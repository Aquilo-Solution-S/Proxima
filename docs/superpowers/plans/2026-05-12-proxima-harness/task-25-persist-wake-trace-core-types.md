# Task 7.3 — `persist_wake_trace` verb: core input/outcome types

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Why this task exists:** the existing `EventIngest` verb (`crates/core/src/verbs/event_ingest.rs:13`, `crates/storage-pg/src/verbs/event_ingest.rs:95`) is the wrong tool for emitting the wake-trace Fact for three structural reasons:

1. `EventIngest` inserts the Fact memory with `personality_instance_id = '00000000-...-000'::uuid` (external authorship — verified at `crates/storage-pg/src/verbs/event_ingest.rs:165-167`). The wake-trace Fact must carry the *authoring* `PersonalityInstanceId` so `Memory.personality_instance_id` indexes hit on cross-personality audit queries.
2. `EventIngest` writes only the core rows — it does **not** write the `wake_trace_v1` / `cited_wake_trace_jsonl_v1` / `citation_wake_trace_v1` sidecar payload rows.
3. `EventIngest` writes no edges. The wake-trace Fact needs `root_perspective --core/authored--> wake_trace_fact` plus N `core/derived-from` edges (triggering memory + root-perspective memory + each active goal **entity**). Emitting them in a follow-up call after `EventIngest` would not be atomic with the Fact insert.

A dedicated atomic verb addresses all three. Land it as a peer of `EventIngest`.

**Entity boundary note (load-bearing).** Goals are entities, not a `Memory.kind` — `Storage::list_active_goals` returns `Vec<ActiveGoalSummary>` keyed on `goal_id: GoalId` (`crates/core/src/personality/tools/list_active_goals.rs:36-44`). The wake-trace Fact's "active goals at wake time" provenance therefore points at **Goal entities by `GoalId`**, not at memories. The edge has `source_kind = "Fact"`, `source_memory_id = Some(fact_id)`, `target_kind = "Goal"`, `target_goal_id = Some(goal_id)`, `target_memory_id = None`. Modeling these as `Vec<MemoryId>` would lie about the entity layer and double-bind the goal as both Goal and Memory.

**Files:**
- Create: `crates/core/src/verbs/persist_wake_trace.rs`
- Modify: `crates/core/src/verbs/mod.rs` (`pub mod persist_wake_trace;`)
- Modify: `crates/core/src/lib.rs` (re-export the request/outcome types if the existing `verbs::` pattern does so)

- [ ] **Step 1: Write the failing test**

Create `crates/core/tests/persist_wake_trace_types.rs`:

```rust
use proxima_core::verbs::persist_wake_trace::{
    WakeTracePersistInput, WakeTracePersistOutcome,
};

#[test]
fn input_carries_jsonl_bytes_authoring_instance_and_provenance_targets() {
    let bytes = b"{\"record\":\"start\"}\n".to_vec();
    let hash: [u8; 32] = *blake3::hash(&bytes).as_bytes();

    let input = WakeTracePersistInput {
        owner: test_owner(),
        authoring_personality_instance_id: uuid::Uuid::now_v7(),
        root_perspective_memory_id: uuid::Uuid::now_v7().into(),
        triggering_memory_id: uuid::Uuid::now_v7().into(),
        active_goal_ids: vec![],
        jsonl_bytes: bytes.clone(),
        jsonl_content_hash: hash,
        jsonl_line_count: 1,
        jsonl_truncated: false,
        citation_byte_range: None,
        wake_trace: sample_wake_trace(),
        source_id: proxima_core::SourceId::new("test/wake-trace".into()),
        source_batch_id: proxima_core::SourceBatchId::new(uuid::Uuid::now_v7()),
        observed_at: time::OffsetDateTime::now_utc(),
        occurred_at: time::OffsetDateTime::now_utc(),
    };

    // The struct must be Send + Sync + Clone for the engine path.
    let _: Box<dyn Send> = Box::new(input.clone());

    // Outcome contract.
    let outcome = sample_outcome();
    let _ = outcome.event_id;
    let _ = outcome.fact_memory_id;
    let _ = outcome.cited_object_id;
    let _ = outcome.citation_mapping_id;
    let _ = outcome.change_event_seq;
    let _: bool = outcome.idempotent_replay;
}

fn test_owner() -> proxima_core::Owner { /* construct as elsewhere in tests */ unimplemented!() }
fn sample_wake_trace() -> proxima_core::wake::trace::WakeTracePayload { /* … */ unimplemented!() }
fn sample_outcome() -> WakeTracePersistOutcome { /* … */ unimplemented!() }
```

Run: `cargo test -p proxima-core --test persist_wake_trace_types`
Expected: compile error — `verbs::persist_wake_trace` module does not exist.

- [ ] **Step 2: Add the module**

Create `crates/core/src/verbs/persist_wake_trace.rs`:

```rust
//! `persist_wake_trace` verb — typed surface.
//!
//! Atomic write of the wake-trace Fact (`proxima-core/wake-trace-v1`),
//! its JSONL CitedObject (`proxima-core/wake-trace-jsonl-v1`), the
//! CitationMapping (`proxima-core/wake-trace-citation-v1`), all three
//! sidecar payload rows, the `change_event` row, and the canonical
//! authorship + provenance edges. One transaction, atomic with respect
//! to readers.
//!
//! Storage impl: `crates/storage-pg/src/verbs/persist_wake_trace.rs`
//! (lives behind the `Storage::persist_wake_trace_atomic` trait method
//! so `proxima-core` stays backend-neutral — see
//! `crates/core/src/storage.rs`).
//!
//! Spec: `docs/superpowers/specs/2026-05-12-proxima-harness-design.md`
//! §"Edge wiring" (persist_wake_trace block).

use uuid::Uuid;

use crate::wake::trace::WakeTracePayload;
use crate::{EventId, GoalId, MemoryId, Owner, SchemaVersion, SourceBatchId, SourceId};

#[derive(Debug, Clone)]
pub struct WakeTracePersistInput {
    pub owner: Owner,

    /// PersonalityInstanceId stamped on the Fact memory row's
    /// `personality_instance_id` column and used as the source side
    /// of the `core/authored` edge (sourced via the Root Perspective
    /// memory id below).
    pub authoring_personality_instance_id: Uuid,

    /// Source side of `core/authored`. Always the Root Perspective
    /// memory of the authoring personality at wake time.
    pub root_perspective_memory_id: MemoryId,

    /// Target of one `core/derived-from` edge from the wake-trace
    /// Fact — the change-event entity that fired the wake.
    pub triggering_memory_id: MemoryId,

    /// Targets of one `core/derived-from` edge each from the
    /// wake-trace Fact — every Goal **entity** active at wake time.
    /// Sourced from `Storage::list_active_goals(...)` which already
    /// keys on `GoalId`. Order is insignificant; duplicates rejected.
    /// The storage verb writes these as edges with
    /// `target_kind = "Goal"`, `target_goal_id = Some(goal_id)`,
    /// `target_memory_id = None`.
    pub active_goal_ids: Vec<GoalId>,

    /// JSONL transcript bytes. Written into
    /// `proxima_core.cited_wake_trace_jsonl_v1.body`.
    pub jsonl_bytes: Vec<u8>,

    /// BLAKE3 of `jsonl_bytes`. **Two layers of idempotency apply:**
    ///
    /// 1. **Whole-verb replay** is keyed on `event_id()` (which folds
    ///    in `content_hash` *and* `invocation_id` — see below). Two
    ///    distinct wakes producing byte-identical JSONL hash to
    ///    *different* event_ids and both persist; they do not collapse.
    /// 2. **CitedObject row dedup** is keyed on
    ///    `(owner, schema_id, content_hash)` per docs/11 §"Idempotency".
    ///    When two wakes produce identical bytes the second's
    ///    `cited_objects` insert reuses the existing row via
    ///    `ON CONFLICT … RETURNING cited_object_id`; both Facts cite
    ///    the shared CitedObject. This dedups the artefact row only —
    ///    the Fact and CitationMapping are still freshly inserted.
    pub jsonl_content_hash: [u8; 32],

    pub jsonl_line_count: u64,
    pub jsonl_truncated: bool,

    /// Optional byte range into the JSONL artefact for the citation
    /// mapping. `None` means "whole blob".
    pub citation_byte_range: Option<(u64, u64)>,

    /// Typed Fact payload row, written to `proxima_core.wake_trace_v1`.
    pub wake_trace: WakeTracePayload,

    /// EventIngest-equivalent header fields.
    pub source_id: SourceId,
    pub source_batch_id: SourceBatchId,
    pub observed_at: time::OffsetDateTime,
    pub occurred_at: time::OffsetDateTime,
}

impl WakeTracePersistInput {
    /// Canonical event_id (same shape as `EventDraft::event_id`).
    #[must_use]
    pub fn event_id(&self) -> EventId {
        use crate::Principal;
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.source_id.as_str().as_bytes());
        hasher.update(b"\x00");
        let (kind, id) = match &self.owner.principal {
            Principal::User(u) => ("User", u.into_inner()),
            Principal::Group(g) => ("Group", g.into_inner()),
        };
        hasher.update(kind.as_bytes());
        hasher.update(b"\x00");
        hasher.update(id.as_bytes());
        hasher.update(b"\x00");
        hasher.update(self.owner.org_id.into_inner().as_bytes());
        hasher.update(b"\x00");
        hasher.update(&self.jsonl_content_hash);
        hasher.update(b"\x00");
        hasher.update(self.wake_trace.invocation_id.as_bytes());
        EventId::new(*hasher.finalize().as_bytes())
    }

    /// `proxima-core/wake-trace-v1` (Fact).
    #[must_use]
    pub fn fact_schema_version(&self) -> SchemaVersion {
        SchemaVersion::new(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WakeTracePersistOutcome {
    pub event_id: EventId,
    pub fact_memory_id: MemoryId,
    pub cited_object_id: Uuid,
    pub citation_mapping_id: Uuid,
    pub change_event_seq: Uuid,
    /// True iff the whole-verb replay check hit (same `event_id()`).
    /// Distinct from CitedObject row dedup, which is silent — when two
    /// wakes share a `content_hash` but differ on `invocation_id` the
    /// `cited_objects` row is reused but `idempotent_replay = false`
    /// (a *new* Fact and CitationMapping were written this call).
    pub idempotent_replay: bool,
}
```

- [ ] **Step 3: Wire into verb module list**

Edit `crates/core/src/verbs/mod.rs` — add `pub mod persist_wake_trace;` alphabetically. If the existing pattern is to re-export from `crates/core/src/lib.rs`, match it.

- [ ] **Step 4: Build**

Run: `cargo build -p proxima-core`
Expected: clean.

- [ ] **Step 5: Run the type test (now compiles, fixtures still `unimplemented!()`)**

The test from Step 1 will compile but panic at runtime — that's fine; replace the fixture stubs with real constructors in Step 6.

- [ ] **Step 6: Finish the type test**

Replace `unimplemented!()` placeholders with real fixtures. Then run: `cargo test -p proxima-core --test persist_wake_trace_types`
Expected: passes.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/verbs/persist_wake_trace.rs crates/core/src/verbs/mod.rs \
        crates/core/src/lib.rs crates/core/tests/persist_wake_trace_types.rs
git commit -m "core(verbs): persist_wake_trace request + outcome types"
```

