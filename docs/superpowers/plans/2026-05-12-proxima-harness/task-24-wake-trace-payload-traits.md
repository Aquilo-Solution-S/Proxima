# Task 7.2 — Add `CitedObjectPayload` + `CitationMappingPayload` traits

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Why a separate task:** `crates/core/src/payload.rs` today defines only `FactPayload`, `AbstractionPayload`, `PerspectivePayload`, `GoalPayload`, and `EdgePayload`. The `PayloadKind` enum already names `CitedObject` and `CitationMapping` (see `crates/core/src/verbs/schema.rs:36-37`) but no payload trait + matching `FlavorRegistry` helper exists for either. The wake-trace JSONL and citation schemas need typed payload registration — add the two missing traits before writing any payload code that uses them.

**Files:**
- Modify: `crates/core/src/payload.rs`
- Modify: `crates/core/src/lib.rs` (re-export the new traits)
- Modify: `crates/core/src/flavor.rs` (add `add_cited_object_schema` + `add_citation_mapping_schema`)

- [ ] **Step 1: Add the two payload traits**

Append to `crates/core/src/payload.rs`. The trait surface must match docs/11-citations.md §"Trait families" — `CitedObjectPayload::idempotency_key()` and `CitationMappingPayload::cited_object_schema()` are required by docs/11:51 and load-bearing: the engine uses `idempotency_key` to dedup `cited_objects` rows on `(owner, schema_id, content_hash)`, and `cited_object_schema` to validate that a `CitationMapping` annotates a `CitedObject` of the matching schema. Omitting either weakens the citation contract.

```rust
/// Typed payload for a `cited_objects` row, keyed on `cited_object_id`.
/// Cited objects don't participate in F/A/P queries — `sidecar_table`
/// stores the artefact body (e.g. JSONL transcript bytes); the
/// `cited_objects` core row stores ownership and the content-addressed
/// hash. See docs/11 §"Trait families".
pub trait CitedObjectPayload: serde::Serialize + serde::de::DeserializeOwned + 'static {
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// See `FactPayload::SPECIAL_CATEGORY`.
    const SPECIAL_CATEGORY: bool = false;
    fn sidecar_table() -> &'static str;
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }

    /// Stable BLAKE3-32 hash of the artefact content. Re-ingesting the
    /// same artefact (PDF, image, session, JSONL transcript) for the
    /// same Owner produces the same hash, deduplicating the
    /// `cited_objects` row via the `UNIQUE (owner, schema_id,
    /// content_hash)` constraint. Per docs/11:51-54.
    ///
    /// This is row-level dedup for the artefact only — it does NOT
    /// short-circuit verbs that *use* the artefact (e.g. wake-trace
    /// Fact insertion is keyed on `event_id`, not on this hash).
    fn idempotency_key(&self) -> [u8; 32];
}

/// Typed payload for a `citation_mappings` row, keyed on
/// `citation_mapping_id`. Citation mappings pin exactly one Memory to
/// exactly one CitedObject; the sidecar stores any extra mapping
/// metadata (e.g. byte ranges). See docs/11 §"Trait families".
pub trait CitationMappingPayload: serde::Serialize + serde::de::DeserializeOwned + 'static {
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// See `FactPayload::SPECIAL_CATEGORY`.
    const SPECIAL_CATEGORY: bool = false;
    fn sidecar_table() -> &'static str;
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }

    /// Schema id of the `CitedObjectPayload` this mapping is allowed to
    /// annotate. The engine validates that the linked `cited_object_id`
    /// resolves to a `CitedObject` of this schema_id. Per docs/11:63-67.
    fn cited_object_schema() -> SchemaId;
}
```

- [ ] **Step 2: Re-export the traits**

In `crates/core/src/lib.rs`, add to the existing `pub use payload::{...}` line:

```rust
pub use payload::{
    AbstractionPayload, CitationMappingPayload, CitedObjectPayload, EdgePayload, FactPayload,
    FactTombstone, GoalPayload, PerspectivePayload,
};
```

Match the existing line's alphabetical-ish order — the file's existing pattern is what matters; preserve it.

- [ ] **Step 3: Add `FlavorRegistry` helpers**

In `crates/core/src/flavor.rs`, after `add_edge_schema` (around line 187 today), add two new helpers mirroring the `add_fact_schema` pattern:

```rust
pub fn add_cited_object_schema<C: CitedObjectPayload>(&mut self) {
    self.schemas.push(SchemaInfo {
        schema_id: C::schema_id(),
        schema_version: SchemaVersion::new(C::SCHEMA_VERSION),
        kind: PayloadKind::CitedObject,
        filter_keys: vec![],
        sidecar_table: Some(C::sidecar_table().to_string()),
        natural_key_columns: vec![],
        tombstone: None,
        cbor_encoder: Some(encode_payload_cbor::<C>),
    });
    self.validators.push(PayloadValidatorEntry {
        schema_id: C::schema_id(),
        schema_version: SchemaVersion::new(C::SCHEMA_VERSION),
        kind: PayloadKind::CitedObject,
        validate: validate_payload_type::<C>,
    });
}

pub fn add_citation_mapping_schema<M: CitationMappingPayload>(&mut self) {
    self.schemas.push(SchemaInfo {
        schema_id: M::schema_id(),
        schema_version: SchemaVersion::new(M::SCHEMA_VERSION),
        kind: PayloadKind::CitationMapping,
        filter_keys: vec![],
        sidecar_table: Some(M::sidecar_table().to_string()),
        natural_key_columns: vec![],
        tombstone: None,
        cbor_encoder: Some(encode_payload_cbor::<M>),
    });
    self.validators.push(PayloadValidatorEntry {
        schema_id: M::schema_id(),
        schema_version: SchemaVersion::new(M::SCHEMA_VERSION),
        kind: PayloadKind::CitationMapping,
        validate: validate_payload_type::<M>,
    });
}
```

Imports at the top of `flavor.rs` need `CitationMappingPayload` and `CitedObjectPayload` (alongside `FactPayload`, etc.).

- [ ] **Step 4: Implement the three wake-trace payloads**

Create `crates/core/src/wake/trace/mod.rs`:

```rust
//! wake-trace schemas. See spec §"Observability: three layers".

use crate::{CitationMappingPayload, CitedObjectPayload, FactPayload, SchemaId, proxima_schema_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WakeTracePayload {
    pub invocation_id: Uuid,
    pub wake_entry_id: Uuid,
    pub personality_instance_id: Uuid,
    pub model_target_ref: String,
    pub model_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub finished_at: OffsetDateTime,
    pub outcome_kind: String,
    pub failure_reason: Option<String>,
    pub rounds_used: u32,
    pub finish_reason: Option<String>,
    pub total_prompt_tokens: Option<u64>,
    pub total_completion_tokens: Option<u64>,
    pub tool_call_count: u32,
    pub jsonl_truncated: bool,
}

impl FactPayload for WakeTracePayload {
    const SCHEMA_ID: &'static str = proxima_schema_id!("wake-trace-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.wake_trace_v1"
    }

    fn render(&self) -> String {
        format!(
            "Wake {} {} ({} rounds)",
            self.invocation_id, self.outcome_kind, self.rounds_used
        )
    }
}

/// CitedObject payload metadata. The transcript bytes live in
/// `proxima_core.cited_wake_trace_jsonl_v1.body` — the payload struct
/// captures only the shape we care to query plus the content-addressed
/// hash that drives `cited_objects` dedup. Persistence is handled by
/// the dedicated `persist_wake_trace` storage verb (Task 7.4), which
/// writes the bytes into the sidecar directly; the typed payload is
/// the registry-level handle for schema validation and CBOR projection.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WakeTraceJsonlPayload {
    /// BLAKE3-32 of the JSONL bytes. Drives the `cited_objects` UNIQUE
    /// (owner, schema_id, content_hash) row dedup. Returned by
    /// `idempotency_key()`.
    pub content_hash: [u8; 32],
    pub byte_len: u64,
    pub line_count: u64,
    pub truncated: bool,
}

impl CitedObjectPayload for WakeTraceJsonlPayload {
    const SCHEMA_ID: &'static str = proxima_schema_id!("wake-trace-jsonl-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.cited_wake_trace_jsonl_v1"
    }

    fn idempotency_key(&self) -> [u8; 32] {
        self.content_hash
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct WakeTraceCitationPayload {
    pub byte_range_start: Option<u64>,
    pub byte_range_end: Option<u64>,
}

impl CitationMappingPayload for WakeTraceCitationPayload {
    const SCHEMA_ID: &'static str = proxima_schema_id!("wake-trace-citation-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.citation_wake_trace_v1"
    }

    fn cited_object_schema() -> SchemaId {
        SchemaId::new(proxima_schema_id!("wake-trace-jsonl-v1").to_string())
    }
}
```

Add `pub mod trace;` to `crates/core/src/wake/mod.rs`.

- [ ] **Step 5: Register the three schemas in the core flavor**

In `crates/core/src/flavor.rs::FlavorRegistry::default()` (the impl that today calls `registry.add_fact_schema::<PersonalityConfigChangedV1>()`), add:

```rust
registry.add_fact_schema::<crate::wake::trace::WakeTracePayload>();
registry.add_cited_object_schema::<crate::wake::trace::WakeTraceJsonlPayload>();
registry.add_citation_mapping_schema::<crate::wake::trace::WakeTraceCitationPayload>();
```

- [ ] **Step 6: Build**

Run: `cargo build -p proxima-core`
Expected: clean.

- [ ] **Step 7: Test schema registration**

Add to `crates/core/tests/flavor_registry.rs` (create if needed):

```rust
use proxima_core::flavor::FlavorRegistry;
use proxima_core::verbs::schema::PayloadKind;

#[test]
fn wake_trace_schemas_are_registered_in_core_flavor() {
    let frozen = FlavorRegistry::default().freeze();
    // `FlavorRegistryFrozen::list(&self) -> Vec<SchemaInfo>` is the
    // accessor at crates/core/src/verbs/schema.rs:241. (There is no
    // `schemas()` method — use `list()`.)
    let schemas = frozen.list();

    let has = |id: &str, kind: PayloadKind| {
        schemas
            .iter()
            .any(|s| s.schema_id.as_str() == id && s.kind == kind)
    };

    assert!(has("proxima-core/wake-trace-v1", PayloadKind::Fact));
    assert!(has("proxima-core/wake-trace-jsonl-v1", PayloadKind::CitedObject));
    assert!(has(
        "proxima-core/wake-trace-citation-v1",
        PayloadKind::CitationMapping
    ));
}
```

Run: `cargo test -p proxima-core --test flavor_registry`
Expected: passes.

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/payload.rs crates/core/src/flavor.rs crates/core/src/lib.rs \
        crates/core/src/wake/trace crates/core/src/wake/mod.rs crates/core/tests
git commit -m "core(wake_trace): two new payload traits + three registered schemas"
```

