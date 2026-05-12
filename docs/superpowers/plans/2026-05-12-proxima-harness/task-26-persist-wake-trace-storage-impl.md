# Task 7.4 — `persist_wake_trace` storage implementation (via Storage trait)

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Wiring shape (load-bearing).** `proxima-core` does not depend on `proxima-storage-pg` — it delegates to backends through the `Storage` trait (`crates/core/src/storage.rs:61`). `Engine::ingest_event_atomic` follows this: `Engine::event_ingest` calls `self.storage.ingest_event_atomic(&draft)` (`crates/core/src/engine/ingest.rs:45-48`). `persist_wake_trace` lands the same way.

The split:

1. **Storage trait method** (`crates/core/src/storage.rs`) — `async fn persist_wake_trace_atomic(&self, registry: &FlavorRegistryFrozen, input: &WakeTracePersistInput) -> Result<WakeTracePersistOutcome, StorageError>`. Registry is passed by reference because the verb must `resolve_relation("core/authored")` and `resolve_relation("core/derived-from")` from the frozen registry to build typed `EdgeDraft`s — the same pattern `append_personality_memories` uses with `req.provenance_relation` (`crates/storage-pg/src/verbs/consolidate/memories.rs:320`).
2. **NoopStorage stub** — returns `StorageError::Internal("NoopStorage rejects writes")` matching the existing pattern (`crates/core/src/storage.rs:457`).
3. **PgStorage impl** — file at `crates/storage-pg/src/verbs/persist_wake_trace.rs` does the 13-row atomic write; the `impl Storage for PgStorage` block in `crates/storage-pg/src/lib.rs` delegates.
4. **Engine method** — `Engine::persist_wake_trace` in `crates/core/src/engine/ingest.rs` (or a new sibling file) clones `Arc<FlavorRegistryFrozen>` off `self`, calls `self.storage.persist_wake_trace_atomic(&self.registry, input).await`, maps `StorageError` to `ProtocolError`.

`Engine` already has `pub(crate) fn storage()` returning `&StorageHandle` and holds `registry: FlavorRegistryFrozen` directly (`crates/core/src/engine/mod.rs:39`). No new fields needed.

**Files:**
- Modify: `crates/core/src/storage.rs` (trait method + NoopStorage stub)
- Modify: `crates/core/src/engine/ingest.rs` (or a new file alongside) — `Engine::persist_wake_trace`
- Create: `crates/storage-pg/src/verbs/persist_wake_trace.rs`
- Modify: `crates/storage-pg/src/verbs/mod.rs` (`pub mod persist_wake_trace;`)
- Modify: `crates/storage-pg/src/lib.rs` (Storage impl block delegates)

- [ ] **Step 1: Add the trait method to `Storage`**

Open `crates/core/src/storage.rs`. Add the imports at the top of the file:

```rust
use crate::verbs::persist_wake_trace::{WakeTracePersistInput, WakeTracePersistOutcome};
use crate::verbs::schema::FlavorRegistryFrozen;
```

Inside the `#[async_trait::async_trait] pub trait Storage: ...` block, add the method (place it near `ingest_event_atomic` for grouping):

```rust
    /// Atomic wake-trace materialization per docs/superpowers/specs/
    /// 2026-05-12-proxima-harness-design.md §"Edge wiring".
    ///
    /// One transaction inserts: cited_objects + cited_wake_trace_jsonl_v1
    /// + source_batches + events + memories (Fact, with the authoring
    /// personality_instance_id) + citation_mappings + citation_wake_trace_v1
    /// + wake_trace_v1 + change_event + core/authored edge (Root P → Fact)
    /// + N core/derived-from edges (Fact → triggering, Fact → root P,
    /// Fact → each active goal entity). Replay (event_id collision)
    /// returns the original outcome with `idempotent_replay = true`.
    ///
    /// `registry` is borrowed for the duration of the call so the
    /// implementation can resolve `core/authored` and `core/derived-from`
    /// to `RegisteredRelation`s and build typed `EdgeDraft`s. Same
    /// pattern as `append_personality_memories`'s `provenance_relation`.
    ///
    /// # Errors
    /// Constraint violations map to `ConstraintViolation`; sqlx
    /// failures map to `Internal`.
    async fn persist_wake_trace_atomic(
        &self,
        registry: &FlavorRegistryFrozen,
        input: &WakeTracePersistInput,
    ) -> Result<WakeTracePersistOutcome, StorageError>;
```

Add the matching stub in `impl Storage for NoopStorage` (search for `async fn ingest_event_atomic` inside that block — add the new method beside it):

```rust
    async fn persist_wake_trace_atomic(
        &self,
        _registry: &FlavorRegistryFrozen,
        _input: &WakeTracePersistInput,
    ) -> Result<WakeTracePersistOutcome, StorageError> {
        Err(StorageError::Internal("NoopStorage rejects writes".into()))
    }
```

Run: `cargo build -p proxima-core`
Expected: clean (the trait method now exists; the PgStorage impl will error in a subsequent build until Step 4 is done).

- [ ] **Step 2: Add the Engine method**

In `crates/core/src/engine/ingest.rs`, add `Engine::persist_wake_trace` next to `Engine::event_ingest`:

```rust
use crate::verbs::persist_wake_trace::{WakeTracePersistInput, WakeTracePersistOutcome};

impl Engine {
    /// docs/superpowers/specs/2026-05-12-proxima-harness-design.md
    /// §"Edge wiring" — atomic wake-trace persistence. Delegates to
    /// `Storage::persist_wake_trace_atomic` with the engine's frozen
    /// registry so the impl can resolve `core/authored` and
    /// `core/derived-from` to `RegisteredRelation`s.
    pub async fn persist_wake_trace(
        &self,
        creds: &Credentials,
        input: WakeTracePersistInput,
    ) -> Result<WakeTracePersistOutcome, ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if !resolved.can_access_owner(&input.owner) {
            return Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ));
        }
        self.storage
            .persist_wake_trace_atomic(&self.registry, &input)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }
}
```

`wake/fire/fire.rs` runs inside the dispatcher with already-authorised credentials (the wake-token store has resolved them); when the emit-trace call site (Task 8.5) cannot pass `Credentials`, expose an inner crate-private path:

```rust
impl Engine {
    /// Internal wake-trace path — called from `fire_wake_entry` which
    /// has already verified the wake-token authorization. Skips the
    /// auth_resolver round-trip.
    pub(crate) async fn persist_wake_trace_internal(
        &self,
        input: WakeTracePersistInput,
    ) -> Result<WakeTracePersistOutcome, StorageError> {
        self.storage
            .persist_wake_trace_atomic(&self.registry, &input)
            .await
    }
}
```

Run: `cargo build -p proxima-core`
Expected: clean.

- [ ] **Step 3: Implement the Postgres verb**

Create `crates/storage-pg/src/verbs/persist_wake_trace.rs`:

```rust
//! Atomic `persist_wake_trace` storage verb.
//!
//! One transaction writes:
//!
//! 1. `cited_objects` row (dedup-keyed on (owner, schema_id, content_hash)).
//! 2. `proxima_core.cited_wake_trace_jsonl_v1` sidecar.
//! 3. `source_batches` upsert.
//! 4. `events` row.
//! 5. `memories` row (Fact, with the authoring `personality_instance_id`).
//! 6. `citation_mappings` row.
//! 7. `proxima_core.citation_wake_trace_v1` sidecar.
//! 8. `proxima_core.wake_trace_v1` sidecar.
//! 9. `change_event` row (`EntityAppend` / `Fact`).
//! 10. `core/authored` edge — Root P → wake-trace Fact.
//! 11. `core/derived-from` edge — Fact → triggering memory.
//! 12. `core/derived-from` edge — Fact → root-perspective memory.
//! 13. `core/derived-from` edge — Fact → each active-goal **entity**
//!     (target_kind = "Goal", target_goal_id, target_memory_id = NULL).
//!
//! Spec: `docs/superpowers/specs/2026-05-12-proxima-harness-design.md`
//! §"Edge wiring".

use proxima_core::verbs::persist_wake_trace::{
    WakeTracePersistInput, WakeTracePersistOutcome,
};
use proxima_core::{
    CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION,
    FlavorRegistryFrozen, MemoryId, Principal, StorageError,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::map_err;
use crate::verbs::edge_append::{EdgeDraft, append_edge_in_tx};

const WAKE_TRACE_FACT_SCHEMA: &str = "proxima-core/wake-trace-v1";
const WAKE_TRACE_JSONL_SCHEMA: &str = "proxima-core/wake-trace-jsonl-v1";
const WAKE_TRACE_CITATION_SCHEMA: &str = "proxima-core/wake-trace-citation-v1";

/// Pool-scoped `persist_wake_trace`. Opens its own transaction. Used
/// by the `impl Storage for PgStorage` block.
///
/// # Errors
/// Constraint violations map to `ConstraintViolation`; sqlx failures
/// map to `Internal`.
pub async fn persist_wake_trace_atomic(
    pool: &PgPool,
    registry: &FlavorRegistryFrozen,
    input: &WakeTracePersistInput,
) -> Result<WakeTracePersistOutcome, StorageError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
    let outcome = persist_wake_trace_in_tx(&mut tx, registry, input).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

#[allow(clippy::too_many_lines)]
pub async fn persist_wake_trace_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    registry: &FlavorRegistryFrozen,
    input: &WakeTracePersistInput,
) -> Result<WakeTracePersistOutcome, StorageError> {
    let event_id = input.event_id();
    let event_id_bytes = event_id.into_inner();

    let (owner_kind, owner_principal_id) = match &input.owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let owner_org_id = input.owner.org_id.into_inner();

    // Replay check on (event_id) unique on memories — same convention
    // as EventIngest at crates/storage-pg/src/verbs/event_ingest.rs:62.
    let existing: Option<(uuid::Uuid, uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
        "SELECT m.memory_id, m.citation_mapping_id, cm.cited_object_id \
         FROM proxima_core.memories m \
         JOIN proxima_core.citation_mappings cm USING (citation_mapping_id) \
         WHERE m.event_id = $1",
    )
    .bind(&event_id_bytes[..])
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;

    if let Some((memory_id, citation_mapping_id, cited_object_id)) = existing {
        let seq: (uuid::Uuid,) = sqlx::query_as(
            "SELECT seq FROM proxima_core.change_event \
             WHERE entity_memory_id = $1 ORDER BY seq ASC LIMIT 1",
        )
        .bind(memory_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_err)?;

        return Ok(WakeTracePersistOutcome {
            event_id,
            fact_memory_id: MemoryId::new(memory_id),
            cited_object_id,
            citation_mapping_id,
            change_event_seq: seq.0,
            idempotent_replay: true,
        });
    }

    let cited_object_id = uuid::Uuid::now_v7();
    let citation_mapping_id = uuid::Uuid::now_v7();
    let memory_id = uuid::Uuid::now_v7();
    let change_seq = uuid::Uuid::now_v7();

    // 1. cited_objects (dedup on (owner, schema_id, content_hash)).
    let cited_id_persisted: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.cited_objects \
            (cited_object_id, schema_id, owner_principal_kind, \
             owner_principal_id, owner_org_id, content_hash) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (owner_principal_kind, owner_principal_id, \
                      owner_org_id, schema_id, content_hash) \
         DO UPDATE SET schema_id = EXCLUDED.schema_id \
         RETURNING cited_object_id",
    )
    .bind(cited_object_id)
    .bind(WAKE_TRACE_JSONL_SCHEMA)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&input.jsonl_content_hash[..])
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;

    // 2. cited_wake_trace_jsonl_v1 sidecar.
    sqlx::query(
        "INSERT INTO proxima_core.cited_wake_trace_jsonl_v1 \
            (cited_object_id, byte_len, line_count, truncated, storage_path, body) \
         VALUES ($1, $2, $3, $4, NULL, $5) \
         ON CONFLICT (cited_object_id) DO NOTHING",
    )
    .bind(cited_id_persisted)
    .bind(i64::try_from(input.jsonl_bytes.len()).unwrap_or(i64::MAX))
    .bind(i64::try_from(input.jsonl_line_count).unwrap_or(i64::MAX))
    .bind(input.jsonl_truncated)
    .bind(&input.jsonl_bytes[..])
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    // 3. source_batches upsert.
    sqlx::query(
        "INSERT INTO proxima_core.source_batches \
            (id, source_id, owner_principal_kind, \
             owner_principal_id, owner_org_id) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(input.source_batch_id.into_inner())
    .bind(input.source_id.as_str())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    // 4. events row.
    sqlx::query(
        "INSERT INTO proxima_core.events \
            (event_id, source_id, source_batch_id, \
             owner_principal_kind, owner_principal_id, owner_org_id, \
             schema_id, schema_version, observed_at, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, $9)",
    )
    .bind(&event_id_bytes[..])
    .bind(input.source_id.as_str())
    .bind(input.source_batch_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(WAKE_TRACE_FACT_SCHEMA)
    .bind(input.observed_at)
    .bind(input.occurred_at)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    // 5. memories (Fact). personality_instance_id = authoring instance
    //    — NOT the external/nil uuid that EventIngest stamps.
    sqlx::query(
        "INSERT INTO proxima_core.memories \
            (memory_id, owner_principal_kind, owner_principal_id, \
             owner_org_id, schema_id, schema_version, event_id, citation_mapping_id, \
             personality_instance_id) \
         VALUES ($1, $2, $3, $4, $5, 1, $6, $7, $8)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(WAKE_TRACE_FACT_SCHEMA)
    .bind(&event_id_bytes[..])
    .bind(citation_mapping_id)
    .bind(input.authoring_personality_instance_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    // 6. citation_mappings.
    sqlx::query(
        "INSERT INTO proxima_core.citation_mappings \
            (citation_mapping_id, schema_id, memory_id, \
             cited_object_id, owner_principal_kind, \
             owner_principal_id, owner_org_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(citation_mapping_id)
    .bind(WAKE_TRACE_CITATION_SCHEMA)
    .bind(memory_id)
    .bind(cited_id_persisted)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    // 7. citation_wake_trace_v1 sidecar.
    let (range_start, range_end) = input
        .citation_byte_range
        .map_or((None, None), |(a, b)| (Some(a as i64), Some(b as i64)));
    sqlx::query(
        "INSERT INTO proxima_core.citation_wake_trace_v1 \
            (citation_mapping_id, byte_range_start, byte_range_end) \
         VALUES ($1, $2, $3)",
    )
    .bind(citation_mapping_id)
    .bind(range_start)
    .bind(range_end)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    // 8. wake_trace_v1 sidecar — columns mirror WakeTracePayload.
    let wt = &input.wake_trace;
    sqlx::query(
        "INSERT INTO proxima_core.wake_trace_v1 \
            (memory_id, invocation_id, wake_entry_id, personality_instance_id, \
             model_target_ref, model_id, started_at, finished_at, \
             outcome_kind, failure_reason, rounds_used, finish_reason, \
             total_prompt_tokens, total_completion_tokens, tool_call_count, \
             jsonl_truncated) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
    )
    .bind(memory_id)
    .bind(wt.invocation_id)
    .bind(wt.wake_entry_id)
    .bind(wt.personality_instance_id)
    .bind(&wt.model_target_ref)
    .bind(&wt.model_id)
    .bind(wt.started_at)
    .bind(wt.finished_at)
    .bind(&wt.outcome_kind)
    .bind(wt.failure_reason.as_deref())
    .bind(i32::try_from(wt.rounds_used).unwrap_or(i32::MAX))
    .bind(wt.finish_reason.as_deref())
    .bind(wt.total_prompt_tokens.map(|t| t as i64))
    .bind(wt.total_completion_tokens.map(|t| t as i64))
    .bind(i32::try_from(wt.tool_call_count).unwrap_or(i32::MAX))
    .bind(wt.jsonl_truncated)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    // 9. change_event (EntityAppend / Fact).
    sqlx::query(
        "INSERT INTO proxima_core.change_event \
            (seq, owner_principal_kind, owner_principal_id, \
             owner_org_id, kind, entity_kind, \
             entity_memory_id, entity_schema_id, entity_schema_version) \
         VALUES ($1, $2, $3, $4, 'EntityAppend', 'Fact', $5, $6, 1)",
    )
    .bind(change_seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(memory_id)
    .bind(WAKE_TRACE_FACT_SCHEMA)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    // 10. core/authored edge — Root P → Fact.
    let authored_relation = registry
        .resolve_relation(CORE_AUTHORED_RELATION)
        .ok_or_else(|| StorageError::Internal(format!(
            "relation {CORE_AUTHORED_RELATION} missing from frozen registry"
        )))?;
    let authored = EdgeDraft {
        edge_id: uuid::Uuid::now_v7(),
        relation: authored_relation,
        source_kind: "Perspective",
        source_memory_id: Some(input.root_perspective_memory_id.into_inner()),
        source_goal_id: None,
        target_kind: "Fact",
        target_memory_id: Some(memory_id),
        target_goal_id: None,
        authorship_kind: "Engine",
        authorship_owner_memory_id: None,
        owner: &input.owner,
    };
    append_edge_in_tx(&mut **tx, &authored, None).await?;

    // 11. core/derived-from edge — Fact → triggering memory.
    let derived_relation = registry
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| StorageError::Internal(format!(
            "relation {CORE_DERIVED_FROM_RELATION} missing from frozen registry"
        )))?;
    let derived_to_trigger = EdgeDraft {
        edge_id: uuid::Uuid::now_v7(),
        relation: derived_relation,
        source_kind: "Fact",
        source_memory_id: Some(memory_id),
        source_goal_id: None,
        target_kind: "Fact",
        target_memory_id: Some(input.triggering_memory_id.into_inner()),
        target_goal_id: None,
        authorship_kind: "Engine",
        authorship_owner_memory_id: None,
        owner: &input.owner,
    };
    append_edge_in_tx(&mut **tx, &derived_to_trigger, None).await?;

    // 12. core/derived-from edge — Fact → root-perspective memory.
    let derived_to_root_p = EdgeDraft {
        edge_id: uuid::Uuid::now_v7(),
        relation: derived_relation,
        source_kind: "Fact",
        source_memory_id: Some(memory_id),
        source_goal_id: None,
        target_kind: "Perspective",
        target_memory_id: Some(input.root_perspective_memory_id.into_inner()),
        target_goal_id: None,
        authorship_kind: "Engine",
        authorship_owner_memory_id: None,
        owner: &input.owner,
    };
    append_edge_in_tx(&mut **tx, &derived_to_root_p, None).await?;

    // 13. core/derived-from edges — Fact → each active goal entity.
    //     target_kind = "Goal", target_goal_id = Some, target_memory_id = None.
    //     Goals are entities (not Memory.kind) — Storage::list_active_goals
    //     returns GoalId, not MemoryId.
    for goal_id in &input.active_goal_ids {
        let derived_to_goal = EdgeDraft {
            edge_id: uuid::Uuid::now_v7(),
            relation: derived_relation,
            source_kind: "Fact",
            source_memory_id: Some(memory_id),
            source_goal_id: None,
            target_kind: "Goal",
            target_memory_id: None,
            target_goal_id: Some(goal_id.into_inner()),
            authorship_kind: "Engine",
            authorship_owner_memory_id: None,
            owner: &input.owner,
        };
        append_edge_in_tx(&mut **tx, &derived_to_goal, None).await?;
    }

    Ok(WakeTracePersistOutcome {
        event_id,
        fact_memory_id: MemoryId::new(memory_id),
        cited_object_id: cited_id_persisted,
        citation_mapping_id,
        change_event_seq: change_seq,
        idempotent_replay: false,
    })
}
```

Notes:
- The `resolve_relation` method on `FlavorRegistryFrozen` returns `Option<RegisteredRelation<'_>>`. If the actual method is named differently, mirror the real name — `crates/core/src/wake/fire/fire.rs:432` already uses `engine.registry().resolve_relation(crate::CORE_AUTHORED_RELATION)`, so the method exists and the name is correct.
- `RegisteredRelation` is `Clone` + `Copy` (see `crates/core/src/relation.rs:152-156`), so the same `derived_relation` value can be passed into each `EdgeDraft` without re-resolving.

- [ ] **Step 4: Wire the verb into the `impl Storage for PgStorage` block**

Open `crates/storage-pg/src/lib.rs`. Find the existing `async fn ingest_event_atomic(...) -> Result<EventIngestOutcome, StorageError>` inside the `impl Storage for PgStorage` block. Add the new method next to it:

```rust
    async fn persist_wake_trace_atomic(
        &self,
        registry: &proxima_core::FlavorRegistryFrozen,
        input: &proxima_core::verbs::persist_wake_trace::WakeTracePersistInput,
    ) -> Result<
        proxima_core::verbs::persist_wake_trace::WakeTracePersistOutcome,
        StorageError,
    > {
        crate::verbs::persist_wake_trace::persist_wake_trace_atomic(
            self.pool(),
            registry,
            input,
        )
        .await
    }
```

Add `pub mod persist_wake_trace;` to `crates/storage-pg/src/verbs/mod.rs`.

- [ ] **Step 5: Build**

Run: `cargo build -p proxima-core -p proxima-storage-pg`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/storage.rs crates/core/src/engine/ingest.rs \
        crates/storage-pg/src/verbs/persist_wake_trace.rs \
        crates/storage-pg/src/verbs/mod.rs crates/storage-pg/src/lib.rs
git commit -m "$(cat <<'EOF'
storage(persist_wake_trace): atomic Fact + sidecars + edges via Storage trait

Trait method on proxima_core::Storage; PgStorage impl writes 13 rows
in one transaction (cited_objects + JSONL sidecar + events + memories
with authoring personality_instance_id + citation_mappings + citation
sidecar + wake_trace_v1 + change_event + core/authored Root-P→Fact
edge + N core/derived-from edges). Goal-entity provenance uses
target_kind="Goal" + target_goal_id (not target_memory_id).
EOF
)"
```
