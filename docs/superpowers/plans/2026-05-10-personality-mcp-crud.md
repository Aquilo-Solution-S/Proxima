# Personality CRUD via MCP — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose personality-config CRUD through MCP so an LLM running as a wake can mutate its own or peers' WakeConfig, with audit-by-construction via a typed Fact memory per write.

**Architecture:** Thin MCP-tool wrappers over the existing `Engine::{instantiate_personality, set_wake_entries, tombstone_personality, register_inference_target, remove_inference_target, bind_inference_tier}` verbs. Handles (`P`/`W` prefixes) replace UUIDs at the tool boundary. Each successful mutation emits one `core/personality_config_changed_v1` Fact memory, provenance attributed to the caller's Root Perspective (or to a substrate-shipped `proxima/shell-author` personality for master-token writes).

**Tech Stack:** Rust 1.x, sqlx (Postgres), rmcp 1.6, schemars, async-trait. Spec: `docs/superpowers/specs/2026-05-10-personality-mcp-crud-design.md` (commits `432d3e8` + `bb7283d` on `road-to-v1`).

---

## File Structure

**Touched:**
- `crates/core/src/mcp/handles.rs` — extend `EntityRef` with `Personality` / `WakeEntry` variants (+ assign / resolve helpers). One file, focused.
- `crates/core/src/mcp/mod.rs` — extend `McpToolCtx` with `engine: Option<Arc<Engine>>`.
- `crates/core/src/flavor.rs` — populate substrate-shipped MCP tools in `FlavorRegistry::default()` via a new helper in `mcp/core_tools/`.
- `crates/core/src/relation.rs` or sibling — declare `proxima/shell-author` constants if needed.

**Created (one file per tool, mirroring `flavors/mcp/src/tools/<verb>.rs` convention):**
- `crates/core/src/mcp/core_tools/mod.rs` — module index + `register_all(registry: &mut FlavorRegistry)` helper.
- `crates/core/src/mcp/core_tools/audit.rs` — `emit_personality_config_changed` shared helper.
- `crates/core/src/mcp/core_tools/payload.rs` — `core/personality_config_changed_v1` payload type + `FactPayload` impl.
- Per-tool files: `list_personalities.rs`, `get_personality.rs`, `instantiate_personality.rs`, `tombstone_personality.rs`, `list_wake_entries.rs`, `set_wake_entries.rs`, `add_wake_entry.rs`, `update_wake_entry.rs`, `remove_wake_entry.rs`, `list_inference_targets.rs`, `register_inference_target.rs`, `remove_inference_target.rs`, `bind_inference_tier.rs`, `list_inference_tier_bindings.rs`, `list_recipes.rs`, `list_substrate_tools.rs`, `list_workspace_tools.rs`, `list_schemas.rs`, `list_edge_types.rs`.

**Storage extensions:**
- `crates/core/src/storage.rs` — add `Storage::ensure_shell_author_personality(owner)` and `Storage::set_wake_entries_within(req, locked = true)` trait method (or equivalent transactional R-M-W primitive). NoopStorage stub returns appropriate test errors.
- `crates/storage-pg/src/lib.rs` + `crates/storage-pg/src/verbs/consolidate.rs` — implement both new methods against Postgres.
- `crates/storage-pg/migrations/<NNNN>_shell_author_personality.sql` — idempotent insert backfill for existing owners.

**Tests:**
- Unit tests inline in each tool file (handle resolution, args validation, error paths) — use `NoopStorage` where reads suffice.
- `crates/mcp-server/tests/personality_crud_pg.rs` — Postgres-backed integration test, mirrors `streamable_http_pg.rs` shape.
- `crates/mcp-server/tests/personality_crud_audit_pg.rs` — asserts the audit Fact memory shape per mutation tool.
- `crates/mcp-server/tests/personality_crud_e2e_pg.rs` — self-evolution smoke test.

---

## Task 1: Extend `HandleTable` with `Personality` and `WakeEntry` variants

**Files:**
- Modify: `crates/core/src/mcp/handles.rs`

- [ ] **Step 1: Add the failing test for `Personality` handle assignment**

Append to the `tests` module at the bottom of `crates/core/src/mcp/handles.rs`:

```rust
#[test]
fn personality_handles_use_p_prefix() {
    let table = HandleTable::new();
    let p1 = PersonalityInstanceId::new(uuid::Uuid::now_v7());
    let p2 = PersonalityInstanceId::new(uuid::Uuid::now_v7());
    assert_eq!(table.assign_personality(p1).as_str(), "P1");
    assert_eq!(table.assign_personality(p2).as_str(), "P2");
    assert_eq!(table.assign_personality(p1).as_str(), "P1", "idempotent");
}

#[test]
fn wake_entry_handles_use_w_prefix() {
    let table = HandleTable::new();
    let w1 = uuid::Uuid::now_v7();
    let w2 = uuid::Uuid::now_v7();
    assert_eq!(table.assign_wake_entry(w1).as_str(), "W1");
    assert_eq!(table.assign_wake_entry(w2).as_str(), "W2");
    assert_eq!(table.assign_wake_entry(w1).as_str(), "W1", "idempotent");
}

#[test]
fn resolve_personality_rejects_non_p_handle() {
    let table = HandleTable::new();
    let p = PersonalityInstanceId::new(uuid::Uuid::now_v7());
    let _ = table.assign_personality(p);
    let m = MemoryId::new(uuid::Uuid::now_v7());
    let mh = table.assign_memory(m);
    assert!(table.resolve_personality(mh.as_str()).is_none(),
        "memory handle must not resolve as personality");
}

#[test]
fn malformed_personality_handle_rejected() {
    let table = HandleTable::new();
    assert!(table.resolve_personality("Pfoo").is_none());
    assert!(table.resolve_personality("P").is_none());
    assert!(table.resolve_personality("p1").is_none(), "case-sensitive");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p proxima-core --lib mcp::handles::tests`
Expected: FAIL with "no method named `assign_personality`" / "no method named `assign_wake_entry`" / "no method named `resolve_personality`".

- [ ] **Step 3: Extend `EntityRef` with the two new variants**

In `crates/core/src/mcp/handles.rs`, replace the `EntityRef` enum:

```rust
use crate::PersonalityInstanceId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityRef {
    Memory(MemoryId),
    Edge(EdgeId),
    Goal(GoalId),
    Repo(uuid::Uuid),
    Personality(PersonalityInstanceId),
    WakeEntry(uuid::Uuid),
}
```

- [ ] **Step 4: Extend `HandleTableInner` with two new counters**

```rust
#[derive(Debug, Default)]
struct HandleTableInner {
    memory_counter: u32,
    edge_counter: u32,
    goal_counter: u32,
    repo_counter: u32,
    personality_counter: u32,
    wake_entry_counter: u32,
    by_entity: HashMap<EntityRef, Handle>,
    by_handle: HashMap<String, EntityRef>,
}
```

- [ ] **Step 5: Add `assign_personality` and `assign_wake_entry`**

Insert after `assign_repo` in the `impl HandleTable` block:

```rust
pub fn assign_personality(&self, id: PersonalityInstanceId) -> Handle {
    self.assign(EntityRef::Personality(id), 'P', |inner| {
        &mut inner.personality_counter
    })
}

pub fn assign_wake_entry(&self, id: uuid::Uuid) -> Handle {
    self.assign(EntityRef::WakeEntry(id), 'W', |inner| {
        &mut inner.wake_entry_counter
    })
}
```

- [ ] **Step 6: Add `resolve_personality` and `resolve_wake_entry`**

Insert after `resolve_repo`:

```rust
#[must_use]
pub fn resolve_personality(&self, raw: &str) -> Option<PersonalityInstanceId> {
    match self.resolve(raw)? {
        EntityRef::Personality(id) => Some(id),
        EntityRef::Memory(_) | EntityRef::Edge(_) | EntityRef::Goal(_)
        | EntityRef::Repo(_) | EntityRef::WakeEntry(_) => None,
    }
}

#[must_use]
pub fn resolve_wake_entry(&self, raw: &str) -> Option<uuid::Uuid> {
    match self.resolve(raw)? {
        EntityRef::WakeEntry(id) => Some(id),
        EntityRef::Memory(_) | EntityRef::Edge(_) | EntityRef::Goal(_)
        | EntityRef::Repo(_) | EntityRef::Personality(_) => None,
    }
}
```

- [ ] **Step 7: Update `is_valid_handle_shape` to accept `P` and `W` prefixes**

```rust
fn is_valid_handle_shape(raw: &str) -> bool {
    let mut chars = raw.chars();
    match chars.next() {
        Some('N' | 'E' | 'G' | 'R' | 'P' | 'W') => {}
        _ => return false,
    }
    let rest = chars.as_str();
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test -p proxima-core --lib mcp::handles::tests`
Expected: PASS — all four new tests + the existing five.

- [ ] **Step 9: Commit**

```bash
git add crates/core/src/mcp/handles.rs
git commit -m "feat(core): extend HandleTable with Personality (P) and WakeEntry (W) variants"
```

---

## Task 2: Add `core/personality_config_changed_v1` Fact-memory schema

**Files:**
- Create: `crates/core/src/mcp/core_tools/mod.rs`
- Create: `crates/core/src/mcp/core_tools/payload.rs`
- Modify: `crates/core/src/mcp/mod.rs` (re-export new module)
- Modify: `crates/core/src/flavor.rs` (register schema in `Default::default()`)

- [ ] **Step 1: Create the empty module skeleton**

Create `crates/core/src/mcp/core_tools/mod.rs`:

```rust
//! Substrate-shipped MCP tools for personality config CRUD. Registered
//! into `FlavorRegistry::default()` so they are available in every
//! composite binary.
//!
//! See docs/superpowers/specs/2026-05-10-personality-mcp-crud-design.md.

pub mod payload;

pub use payload::{PersonalityConfigChangedV1, PersonalityConfigChangedSubject,
                  PersonalityConfigChangedCaller, PersonalityConfigChangedVerb};
```

Add to `crates/core/src/mcp/mod.rs` after `pub mod handles;`:

```rust
pub mod core_tools;
```

- [ ] **Step 2: Write the failing payload-validation test**

Create `crates/core/src/mcp/core_tools/payload.rs`:

```rust
//! Payload type for the `core/personality_config_changed_v1` Fact
//! memory emitted alongside every MCP-CRUD mutation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{FactPayload, SchemaId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PersonalityConfigChangedVerb {
    Instantiate,
    Tombstone,
    SetWakeEntries,
    AddWakeEntry,
    UpdateWakeEntry,
    RemoveWakeEntry,
    RegisterInferenceTarget,
    RemoveInferenceTarget,
    BindInferenceTier,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum PersonalityConfigChangedSubject {
    Personality(uuid::Uuid),
    WakeEntry(uuid::Uuid),
    InferenceTarget(String),
    TierBinding(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PersonalityConfigChangedCaller {
    WakePersonality { personality_instance_id: uuid::Uuid },
    MasterToken { shell_author_personality_instance_id: uuid::Uuid },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PersonalityConfigChangedV1 {
    pub verb: PersonalityConfigChangedVerb,
    pub subject: PersonalityConfigChangedSubject,
    /// Opaque snapshot of relevant prior state. `None` on create-style verbs.
    pub before: Option<serde_json::Value>,
    /// Opaque snapshot of relevant new state. `None` on tombstone-style verbs.
    pub after: Option<serde_json::Value>,
    pub caller: PersonalityConfigChangedCaller,
}

impl FactPayload for PersonalityConfigChangedV1 {
    const SCHEMA_ID: &'static str = "core/personality_config_changed_v1";
    const SCHEMA_VERSION: u32 = 1;
    fn schema_id() -> SchemaId { SchemaId::new(Self::SCHEMA_ID.into()) }
    fn sidecar_table() -> &'static str { "proxima_core.personality_config_changed_v1" }
    fn natural_key_columns() -> &'static [&'static str] { &[] }
    fn tombstone() -> Option<crate::FactTombstoneSpec> { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips_through_json() {
        let payload = PersonalityConfigChangedV1 {
            verb: PersonalityConfigChangedVerb::Instantiate,
            subject: PersonalityConfigChangedSubject::Personality(uuid::Uuid::now_v7()),
            before: None,
            after: Some(serde_json::json!({ "display_name": "Engineer" })),
            caller: PersonalityConfigChangedCaller::MasterToken {
                shell_author_personality_instance_id: uuid::Uuid::now_v7(),
            },
        };
        let value = serde_json::to_value(&payload).expect("serialize");
        let back: PersonalityConfigChangedV1 = serde_json::from_value(value).expect("deserialize");
        assert_eq!(payload, back);
    }

    #[test]
    fn schema_id_is_stable() {
        assert_eq!(PersonalityConfigChangedV1::SCHEMA_ID,
                   "core/personality_config_changed_v1");
        assert_eq!(PersonalityConfigChangedV1::SCHEMA_VERSION, 1);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail (or compile error)**

Run: `cargo test -p proxima-core --lib mcp::core_tools::payload`
Expected: FAIL with "no field `sidecar_table`" or compile errors about missing trait items — verify the actual `FactPayload` trait shape and adjust if needed.

- [ ] **Step 4: Verify against the actual `FactPayload` trait**

Run: `grep -nE 'trait FactPayload|fn schema_id|fn sidecar_table|fn natural_key_columns|fn tombstone|FactTombstoneSpec' crates/core/src/payload.rs crates/core/src/lib.rs 2>/dev/null | head -20`
Expected: locates the trait definition. Adjust `impl FactPayload` to match exactly. (The trait may have additional required methods like `to_payload_bytes` or differ in shape; mirror the impl style of `flavors/mcp/src/payloads/agent_note.rs` or similar.)

- [ ] **Step 5: Re-run tests until they pass**

Run: `cargo test -p proxima-core --lib mcp::core_tools::payload`
Expected: PASS — both round-trip and schema-id tests.

- [ ] **Step 6: Register the schema in `FlavorRegistry::default()`**

In `crates/core/src/flavor.rs`, modify the `impl Default for FlavorRegistry` block:

```rust
impl Default for FlavorRegistry {
    fn default() -> Self {
        let mut registry = Self {
            schemas: Vec::new(),
            relations: core_relation_descriptors(),
            validators: Vec::new(),
            mcp_tools: Vec::new(),
            flavors: Vec::new(),
            bundled_recipes: Vec::new(),
            workspace_runners: Vec::new(),
        };
        // Substrate-shipped Fact schema for MCP-CRUD audit.
        registry.add_fact_schema::<crate::mcp::core_tools::PersonalityConfigChangedV1>();
        registry
    }
}
```

- [ ] **Step 7: Add a registration test**

Append to `crates/core/src/flavor.rs` `mod tests`:

```rust
#[test]
fn default_registry_includes_personality_config_changed_schema() {
    let frozen = FlavorRegistry::new().freeze();
    let info = frozen.lookup(
        &crate::SchemaId::new("core/personality_config_changed_v1".into()),
        crate::SchemaVersion::new(1),
    );
    assert!(info.is_some(), "schema must be registered in default registry");
    assert_eq!(info.unwrap().kind, crate::PayloadKind::Fact);
}
```

- [ ] **Step 8: Run all tests**

Run: `cargo test -p proxima-core --lib`
Expected: PASS — including the new registration test. Pre-existing tests using `FlavorRegistry::new()` should continue to pass (the only added behaviour is one more registered schema; no test expects an empty schemas vec).

- [ ] **Step 9: Commit**

```bash
git add crates/core/src/mcp/mod.rs crates/core/src/mcp/core_tools/ crates/core/src/flavor.rs
git commit -m "feat(core): register core/personality_config_changed_v1 Fact schema"
```

---

## Task 3: Add `Storage::ensure_shell_author_personality` + idempotent backfill

**Files:**
- Modify: `crates/core/src/storage.rs` (trait method + NoopStorage stub)
- Modify: `crates/storage-pg/src/lib.rs` (PgStorage impl)
- Create: `crates/storage-pg/migrations/<next>_shell_author_personality.sql`
- Test: `crates/storage-pg/tests/shell_author_pg.rs`

- [ ] **Step 1: Add the trait method to `Storage`**

In `crates/core/src/storage.rs`, add after `instantiate_personality`:

```rust
/// Ensure a substrate-shipped `proxima/shell-author` personality exists
/// for the given owner. Used as the provenance Root Perspective for
/// master-token MCP-CRUD writes. Idempotent: returns the existing
/// instance id on replay, or mints a fresh one with `display_name =
/// "shell-author"`, `purpose = "Substrate authorship for master-token
/// MCP CRUD writes"`, and an empty WakeConfig.
async fn ensure_shell_author_personality(
    &self,
    owner: &Owner,
) -> Result<PersonalityInstanceId, StorageError>;
```

- [ ] **Step 2: NoopStorage stub returns deterministic id**

Append to NoopStorage's `impl Storage` block in the same file:

```rust
async fn ensure_shell_author_personality(
    &self,
    _owner: &Owner,
) -> Result<PersonalityInstanceId, StorageError> {
    Ok(PersonalityInstanceId::new(uuid::Uuid::nil()))
}
```

- [ ] **Step 3: Run core lib tests to confirm trait change compiles**

Run: `cargo test -p proxima-core --lib`
Expected: PASS — pre-existing tests untouched. Compile errors in dependent crates are expected (PgStorage missing impl) — they get fixed in next step.

- [ ] **Step 4: Write the migration**

Find the next migration number:

Run: `ls crates/storage-pg/migrations/ | sort | tail -3`

Create `crates/storage-pg/migrations/<NNNN>_shell_author_personality.sql` with `<NNNN>` one greater than the current max:

```sql
-- Substrate-shipped marker: each owner has a `proxima/shell-author`
-- personality with display_name = 'shell-author' that authors the
-- audit Fact memories emitted by master-token MCP-CRUD calls. Stored
-- in the regular personality table; only distinguished by display_name
-- on lookup. Empty WakeConfig means it never fires.
--
-- This migration adds nothing schema-wise; the shell-author personality
-- is materialized lazily via Storage::ensure_shell_author_personality
-- on the first master-token MCP-CRUD call per owner.

SELECT 1;  -- intentional no-op; the runtime path handles backfill
```

The runtime call path (lazy backfill) is preferred over a one-shot SQL backfill: `ensure_shell_author_personality` runs on first use, idempotent on subsequent calls. This avoids needing to enumerate owners at migration time.

- [ ] **Step 5: Implement `ensure_shell_author_personality` on PgStorage**

In `crates/storage-pg/src/lib.rs`, locate the `impl Storage for PgStorage` block, add:

```rust
async fn ensure_shell_author_personality(
    &self,
    owner: &Owner,
) -> Result<PersonalityInstanceId, StorageError> {
    verbs::shell_author::ensure_shell_author(&self.pool, owner).await
}
```

- [ ] **Step 6: Create the verb implementation**

Create `crates/storage-pg/src/verbs/shell_author.rs`:

```rust
//! Lazy backfill / idempotent lookup of the `proxima/shell-author`
//! personality. Used as provenance for master-token MCP-CRUD writes.

use proxima_core::{
    InstantiatePersonalityRequest, Owner, PersonalityInstanceId, StorageError,
};

const SHELL_AUTHOR_DISPLAY_NAME: &str = "shell-author";
const SHELL_AUTHOR_PURPOSE: &str =
    "Substrate authorship for master-token MCP CRUD writes";

pub async fn ensure_shell_author(
    pool: &sqlx::PgPool,
    owner: &Owner,
) -> Result<PersonalityInstanceId, StorageError> {
    // Fast path: lookup by owner + display_name.
    let row: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT personality_instance_id
         FROM proxima_core.personality
         WHERE org_id = $1
           AND principal_kind = $2
           AND principal_id = $3
           AND display_name = $4
           AND tombstoned_at IS NULL
         LIMIT 1",
    )
    .bind(owner.org_id.into_inner())
    .bind(owner.principal.kind_str())
    .bind(owner.principal.id_uuid())
    .bind(SHELL_AUTHOR_DISPLAY_NAME)
    .fetch_optional(pool)
    .await
    .map_err(|err| StorageError::Internal(err.to_string()))?;

    if let Some(id) = row {
        return Ok(PersonalityInstanceId::new(id));
    }

    // Slow path: instantiate. The existing instantiate_personality verb
    // handles the canonical personality + Root Perspective + cursor row
    // creation. Empty WakeConfig means dispatcher never fires it.
    let req = InstantiatePersonalityRequest {
        owner: owner.clone(),
        display_name: SHELL_AUTHOR_DISPLAY_NAME.into(),
        purpose: SHELL_AUTHOR_PURPOSE.into(),
    };
    let resp = super::consolidate::instantiate_personality(pool, &req).await?;
    Ok(resp.instance_id)
}
```

Then export it from `crates/storage-pg/src/verbs/mod.rs`:

```rust
pub mod shell_author;
```

The exact `Owner.principal.kind_str()` / `id_uuid()` accessors are the ones used by the existing `consolidate::instantiate_personality` SQL — verify by reading that function and adapt if the column-binding pattern differs.

- [ ] **Step 7: Write the integration test**

Create `crates/storage-pg/tests/shell_author_pg.rs`:

```rust
use proxima_core::storage::Storage;
use proxima_core::{OrgId, Owner, Principal, UserId};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection};

const ADMIN_URL: &str = "postgres://postgres@localhost/postgres";

#[tokio::test]
async fn ensure_shell_author_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else { return Ok(()) };
    let database_url = format!("postgres://postgres@localhost/{db_name}");
    let storage = PgStorage::connect(&database_url).await?;
    storage.run_migrations().await?;
    let owner = Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
        org_id: OrgId::new(uuid::Uuid::now_v7()),
    };

    let first = storage.ensure_shell_author_personality(&owner).await?;
    let second = storage.ensure_shell_author_personality(&owner).await?;
    assert_eq!(first, second, "second call returns the same instance");

    drop_db(&db_name).await?;
    Ok(())
}

async fn create_db() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let mut admin = match PgConnection::connect(ADMIN_URL).await {
        Ok(c) => c,
        Err(_) => return Ok(None),  // Postgres not available; skip.
    };
    let name = format!("proxima_test_{}", uuid::Uuid::now_v7().simple());
    admin.execute(format!("CREATE DATABASE {name}").as_str()).await?;
    Ok(Some(name))
}

async fn drop_db(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut admin = PgConnection::connect(ADMIN_URL).await?;
    admin.execute(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)").as_str()).await?;
    Ok(())
}
```

- [ ] **Step 8: Run the integration test**

Run: `cargo test -p proxima-storage-pg --test shell_author_pg`
Expected: PASS (or SKIPPED with no Postgres available — graceful skip via `create_db` returning `None`).

- [ ] **Step 9: Commit**

```bash
git add crates/core/src/storage.rs crates/storage-pg/ docs/
git commit -m "feat(storage): ensure_shell_author_personality for master-token audit provenance"
```

---

## Task 4: Audit-emit helper

**Files:**
- Create: `crates/core/src/mcp/core_tools/audit.rs`
- Modify: `crates/core/src/mcp/core_tools/mod.rs` (export)

- [ ] **Step 1: Write the helper signature + a unit test**

Create `crates/core/src/mcp/core_tools/audit.rs`:

```rust
//! Emit the `core/personality_config_changed_v1` Fact memory after a
//! successful MCP-CRUD mutation.
//!
//! Provenance:
//! - Wake-token caller: `ctx.caller_self_perspective` (calling personality's Root).
//! - Master-token caller: a substrate-shipped `proxima/shell-author`
//!   personality's Root Perspective, materialized lazily via
//!   `Storage::ensure_shell_author_personality(owner)`.
//!
//! Emit failures are non-fatal: the verb already succeeded.

use crate::McpToolError;
use crate::mcp::McpToolCtx;
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedCaller, PersonalityConfigChangedSubject,
    PersonalityConfigChangedV1, PersonalityConfigChangedVerb,
};

/// Outcome of an audit-emit attempt. Tools surface `Failed` as a
/// non-fatal warning attached to their successful response (the verb
/// already landed; we don't retry).
#[derive(Debug, Clone)]
pub enum AuditEmit {
    Ok,
    Failed { reason: String },
}

pub async fn emit_personality_config_changed(
    ctx: &McpToolCtx,
    verb: PersonalityConfigChangedVerb,
    subject: PersonalityConfigChangedSubject,
    before: Option<serde_json::Value>,
    after: Option<serde_json::Value>,
) -> AuditEmit {
    let caller = match resolve_caller(ctx).await {
        Ok(c) => c,
        Err(reason) => return AuditEmit::Failed { reason },
    };
    let payload = PersonalityConfigChangedV1 { verb, subject, before, after, caller };
    match write_fact(ctx, &payload).await {
        Ok(()) => AuditEmit::Ok,
        Err(reason) => AuditEmit::Failed { reason },
    }
}

async fn resolve_caller(
    ctx: &McpToolCtx,
) -> Result<PersonalityConfigChangedCaller, String> {
    if let Some(self_id) = ctx.caller_self_perspective {
        // Wake-token: caller_self_perspective points at the calling
        // personality's Root Perspective Memory. To name the personality
        // itself we look up the personality whose
        // current_root_perspective_memory_id == self_id.
        let storage = ctx.storage()
            .ok_or_else(|| "engine storage unavailable for wake-token audit".to_string())?;
        let instances = storage
            .list_personality_instances(&ctx.owner, false)
            .await
            .map_err(|e| e.to_string())?;
        let id = instances
            .into_iter()
            .find(|row| row.current_root_perspective_memory_id == self_id)
            .map(|row| row.personality_instance_id.into_inner())
            .ok_or_else(|| {
                format!("no personality matches caller_self_perspective {self_id:?}")
            })?;
        Ok(PersonalityConfigChangedCaller::WakePersonality {
            personality_instance_id: id,
        })
    } else {
        // Master-token: ensure shell-author exists for this owner.
        let storage = ctx.storage()
            .ok_or_else(|| "engine storage unavailable for master-token audit".to_string())?;
        let id = storage
            .ensure_shell_author_personality(&ctx.owner)
            .await
            .map_err(|e| e.to_string())?
            .into_inner();
        Ok(PersonalityConfigChangedCaller::MasterToken {
            shell_author_personality_instance_id: id,
        })
    }
}

async fn write_fact(
    ctx: &McpToolCtx,
    payload: &PersonalityConfigChangedV1,
) -> Result<(), String> {
    use crate::FactPayload;
    use crate::verbs::event_ingest::EventDraft;
    use crate::{SchemaId, SchemaVersion, SourceBatchId, SourceId};
    use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;

    let payload_json = serde_json::to_value(payload).map_err(|e| e.to_string())?;
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(&payload, &mut payload_bytes).map_err(|e| e.to_string())?;
    let observed_at = time::OffsetDateTime::now_utc();
    let body_hash = blake3::hash(&payload_bytes);
    let draft = EventDraft {
        source_id: SourceId::new("core/mcp-crud"),
        source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
        owner: ctx.owner.clone(),
        schema_id: SchemaId::new(PersonalityConfigChangedV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(PersonalityConfigChangedV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: observed_at,
        cited_object: crate::verbs::event_ingest::CitedObjectHint {
            schema_id: SchemaId::new("core/personality_config_changed_object_v1".into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *body_hash.as_bytes(),
        },
        citation_mapping: crate::verbs::event_ingest::CitationMappingHint {
            schema_id: SchemaId::new("core/personality_config_changed_whole_v1".into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    let mut tx = ctx.pool.begin().await.map_err(|e| e.to_string())?;
    let _ = ingest_event_in_tx(&mut tx, &draft).await.map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    let _ = payload_json;  // reserved for future direct-projection use
    Ok(())
}
```

Append unit test in the same file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::HandleTable;
    use crate::{FlavorRegistryFrozen, McpAuthorContext, OrgId, Owner, Principal, UserId};
    use std::sync::Arc;

    fn fake_owner() -> Owner {
        Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        }
    }

    #[tokio::test]
    async fn resolve_caller_returns_failed_when_no_storage() {
        // McpToolCtx without engine wired -> ctx.storage() returns None.
        // Verifies the audit returns Failed instead of panicking.
        // Real DB-backed test happens in integration tests.
        let ctx = McpToolCtx {
            pool: sqlx::PgPool::connect_lazy("postgres://placeholder/db")
                .expect("lazy connect"),
            owner: fake_owner(),
            handles: Arc::new(HandleTable::new()),
            registry: Arc::new(FlavorRegistryFrozen::new()),
            author: McpAuthorContext {
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            engine: None,
        };
        let outcome = emit_personality_config_changed(
            &ctx,
            PersonalityConfigChangedVerb::Instantiate,
            PersonalityConfigChangedSubject::Personality(uuid::Uuid::now_v7()),
            None,
            Some(serde_json::json!({})),
        ).await;
        match outcome {
            AuditEmit::Failed { reason } =>
                assert!(reason.contains("storage unavailable"), "got {reason:?}"),
            AuditEmit::Ok => panic!("expected Failed without storage"),
        }
    }
}
```

This test depends on Task 5 (extending `McpToolCtx` with `engine: Option<Arc<Engine>>` and `ctx.storage()` accessor). If running tasks out of order, complete Task 5 first.

- [ ] **Step 2: Export from module index**

In `crates/core/src/mcp/core_tools/mod.rs`:

```rust
pub mod audit;
pub mod payload;

pub use audit::{AuditEmit, emit_personality_config_changed};
pub use payload::{PersonalityConfigChangedCaller, PersonalityConfigChangedSubject,
                  PersonalityConfigChangedV1, PersonalityConfigChangedVerb};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p proxima-core --lib mcp::core_tools::audit`
Expected: PASS (after Task 5).

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/mcp/core_tools/audit.rs crates/core/src/mcp/core_tools/mod.rs
git commit -m "feat(core): audit-emit helper for personality_config_changed_v1"
```

---

## Task 5: Extend `McpToolCtx` with `Option<Arc<Engine>>`

**Files:**
- Modify: `crates/core/src/mcp/mod.rs`
- Modify: `crates/mcp-server/src/server.rs`

- [ ] **Step 1: Add the failing test**

Append to `crates/core/src/mcp/mod.rs` `mod tests`:

```rust
#[cfg(test)]
mod ctx_engine_tests {
    use super::*;
    use crate::auth::NoAuth;
    use crate::verbs::query::MemoryStore;
    use crate::{Engine, FlavorRegistry, OrgId, Owner, Principal, UserId};
    use std::sync::Arc;

    #[test]
    fn ctx_storage_returns_none_when_engine_unwired() {
        let owner = Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        };
        let ctx = McpToolCtx {
            pool: sqlx::PgPool::connect_lazy("postgres://x/x").expect("lazy"),
            owner: owner.clone(),
            handles: Arc::new(HandleTable::new()),
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "t".into(), client_name: "t".into(),
                client_version: "0".into(), caller_self_perspective: None,
            },
            caller_self_perspective: None,
            engine: None,
        };
        assert!(ctx.storage().is_none());
        assert!(ctx.engine().is_none());
    }

    #[test]
    fn ctx_storage_returns_some_when_engine_wired() {
        let owner = Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        };
        let resolver = NoAuth::new(owner.principal.clone(), owner.clone());
        let engine = Arc::new(Engine::new(
            FlavorRegistry::new().freeze(),
            MemoryStore::new(),
            Box::new(resolver),
        ));
        let ctx = McpToolCtx {
            pool: sqlx::PgPool::connect_lazy("postgres://x/x").expect("lazy"),
            owner,
            handles: Arc::new(HandleTable::new()),
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "t".into(), client_name: "t".into(),
                client_version: "0".into(), caller_self_perspective: None,
            },
            caller_self_perspective: None,
            engine: Some(engine.clone()),
        };
        assert!(ctx.engine().is_some());
        assert!(ctx.storage().is_some());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p proxima-core --lib mcp::ctx_engine_tests`
Expected: FAIL with "no field `engine`" / "no method `storage`".

- [ ] **Step 3: Add the field and accessors**

In `crates/core/src/mcp/mod.rs`, modify `McpToolCtx` to:

```rust
#[derive(Clone)]
pub struct McpToolCtx {
    pub pool: sqlx::PgPool,
    pub owner: Owner,
    pub handles: Arc<HandleTable>,
    pub registry: Arc<FlavorRegistryFrozen>,
    pub author: McpAuthorContext,
    pub caller_self_perspective: Option<MemoryId>,
    /// `Some` when the MCP server was constructed with `with_engine`.
    /// Tools that need to call engine verbs (CRUD-via-MCP) require this;
    /// pure read-only / projection tools can ignore it.
    pub engine: Option<Arc<crate::Engine>>,
}

impl McpToolCtx {
    /// `None` when the MCP server is running without a wired engine
    /// (early test scaffolds). Real deployments always wire an engine.
    #[must_use]
    pub fn engine(&self) -> Option<&crate::Engine> {
        self.engine.as_deref()
    }

    /// Convenience: storage handle bound to the engine.
    #[must_use]
    pub fn storage(&self) -> Option<&dyn crate::Storage> {
        self.engine.as_ref().map(|e| e.storage())
    }
}
```

`Engine::storage()` is the existing accessor. If it doesn't exist with that name, search:

Run: `grep -n 'pub fn storage' crates/core/src/engine/mod.rs`
Expected: shows the accessor (or its actual name). Adjust the helper.

- [ ] **Step 4: Update `McpToolHost::ctx` to populate the new field**

In `crates/mcp-server/src/server.rs`, modify `McpToolHost::ctx`:

```rust
#[must_use]
pub fn ctx(&self, author: McpAuthorContext, owner: Option<Owner>) -> McpToolCtx {
    McpToolCtx {
        pool: self.pool.clone(),
        owner: owner.unwrap_or_else(|| self.owner.clone()),
        handles: self.handles.clone(),
        registry: self.registry.clone(),
        caller_self_perspective: author.caller_self_perspective,
        author,
        engine: self.engine.clone(),
    }
}
```

- [ ] **Step 5: Run all tests**

Run: `cargo test -p proxima-core --lib`
Expected: PASS — both new tests + all pre-existing.

Run: `cargo build -p proxima-mcp-server`
Expected: BUILD OK — verify the `ctx` change typechecks.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/mcp/mod.rs crates/mcp-server/src/server.rs
git commit -m "feat(mcp): McpToolCtx carries Option<Arc<Engine>> for verb dispatch"
```

---

## Task 6: `core/list_personalities` tool (read-only)

**Files:**
- Create: `crates/core/src/mcp/core_tools/list_personalities.rs`
- Modify: `crates/core/src/mcp/core_tools/mod.rs` (declare module + export)

- [ ] **Step 1: Write the failing test against `NoopStorage`**

Create `crates/core/src/mcp/core_tools/list_personalities.rs`:

```rust
//! `core/list_personalities` — read-only enumeration of the owner's
//! personalities, returning handles instead of UUIDs.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListPersonalitiesTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListPersonalitiesArgs {
    /// Include tombstoned instances. Default: false.
    #[serde(default)]
    pub include_tombstoned: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListPersonalitiesItem {
    pub handle: String,
    pub display_name: String,
    pub status: String,
    pub root_perspective: String,
    pub wake_entry_count: u32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListPersonalitiesOutput {
    pub personalities: Vec<ListPersonalitiesItem>,
}

impl McpTool for ListPersonalitiesTool {
    const NAME: &'static str = "core/list_personalities";
    const DESCRIPTION: &'static str =
        "List personality instances for the authenticated owner. Returns handles \
         (P-prefixed) usable in subsequent CRUD calls.";
    type Args = ListPersonalitiesArgs;
    type Output = ListPersonalitiesOutput;

    fn call(
        ctx: McpToolCtx,
        args: ListPersonalitiesArgs,
    ) -> BoxFuture<'static, Result<ListPersonalitiesOutput, McpToolError>> {
        Box::pin(async move {
            let storage = ctx.storage().ok_or_else(|| {
                McpToolError::Other("engine storage unavailable".into())
            })?;
            let rows = storage
                .list_personality_instances(&ctx.owner, args.include_tombstoned)
                .await
                .map_err(McpToolError::Storage)?;
            let personalities = rows
                .into_iter()
                .map(|row| {
                    let p_handle = ctx.handles.assign_personality(row.personality_instance_id);
                    let n_handle = ctx.handles.assign_memory(row.current_root_perspective_memory_id);
                    let count = u32::try_from(row.wake_entries.len()).unwrap_or(u32::MAX);
                    ListPersonalitiesItem {
                        handle: p_handle.as_str().to_string(),
                        display_name: row.display_name,
                        status: row.status,
                        root_perspective: n_handle.as_str().to_string(),
                        wake_entry_count: count,
                    }
                })
                .collect();
            Ok(ListPersonalitiesOutput { personalities })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(ListPersonalitiesArgs))
            .expect("ListPersonalitiesArgs schema serializes")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::NoAuth;
    use crate::mcp::HandleTable;
    use crate::verbs::query::MemoryStore;
    use crate::{Engine, FlavorRegistry, McpAuthorContext, OrgId, Owner, Principal, UserId};
    use std::sync::Arc;

    fn make_ctx() -> McpToolCtx {
        let owner = Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        };
        let resolver = NoAuth::new(owner.principal.clone(), owner.clone());
        let engine = Arc::new(Engine::new(
            FlavorRegistry::new().freeze(),
            MemoryStore::new(),
            Box::new(resolver),
        ));
        McpToolCtx {
            pool: sqlx::PgPool::connect_lazy("postgres://x/x").expect("lazy"),
            owner,
            handles: Arc::new(HandleTable::new()),
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "t".into(), client_name: "t".into(),
                client_version: "0".into(), caller_self_perspective: None,
            },
            caller_self_perspective: None,
            engine: Some(engine),
        }
    }

    #[tokio::test]
    async fn list_personalities_against_empty_memory_store_returns_empty() {
        let ctx = make_ctx();
        let out = ListPersonalitiesTool::call(ctx, ListPersonalitiesArgs::default())
            .await
            .expect("ok");
        assert!(out.personalities.is_empty());
    }
}
```

`MemoryStore::list_personality_instances` — verify it returns `Ok(Vec::new())` for an empty store. If not, this test would need adjustment to use a different stub or skip with a note. Run:

`grep -nE 'fn list_personality_instances' crates/core/src/verbs/query.rs`
to confirm.

- [ ] **Step 2: Declare the module + export**

In `crates/core/src/mcp/core_tools/mod.rs`:

```rust
pub mod list_personalities;

pub use list_personalities::ListPersonalitiesTool;
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p proxima-core --lib mcp::core_tools::list_personalities`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/mcp/core_tools/list_personalities.rs crates/core/src/mcp/core_tools/mod.rs
git commit -m "feat(mcp): core/list_personalities tool"
```

---

## Task 7: `core/get_personality` tool

**Files:**
- Create: `crates/core/src/mcp/core_tools/get_personality.rs`
- Modify: `crates/core/src/mcp/core_tools/mod.rs`

- [ ] **Step 1: Write the failing test for handle-not-found**

Create `crates/core/src/mcp/core_tools/get_personality.rs`:

```rust
//! `core/get_personality` — full read of one personality instance,
//! including all wake entries projected with W-handles.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::{
    ModelTier, WakeEntryAuthoredBy, WakeEntryExecutionMode, WakeEntryTriggerKind,
};

#[derive(Debug, Default)]
pub struct GetPersonalityTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetPersonalityArgs {
    /// `P`-prefixed handle previously returned by list_personalities or
    /// instantiate_personality.
    pub personality: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetPersonalityWakeEntry {
    pub handle: String,
    pub trigger_kind: WakeEntryTriggerKind,
    pub trigger_id: String,
    pub label: String,
    pub enabled: bool,
    pub recipe_ref: String,
    pub model_tier: ModelTier,
    pub inference_target_ref: Option<String>,
    pub substrate_tool_palette: Vec<String>,
    pub workspace_tool_palette: Vec<String>,
    pub execution_mode: WakeEntryExecutionMode,
    pub authored_by: WakeEntryAuthoredBy,
    pub probability_promille: u16,
    pub max_rounds: u16,
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetPersonalityOutput {
    pub handle: String,
    pub display_name: String,
    pub status: String,
    pub root_perspective: String,
    pub wake_entries: Vec<GetPersonalityWakeEntry>,
}

impl McpTool for GetPersonalityTool {
    const NAME: &'static str = "core/get_personality";
    const DESCRIPTION: &'static str =
        "Read one personality with all wake entries. Returns W-handles \
         for each entry usable in update/remove calls.";
    type Args = GetPersonalityArgs;
    type Output = GetPersonalityOutput;

    fn call(
        ctx: McpToolCtx,
        args: GetPersonalityArgs,
    ) -> BoxFuture<'static, Result<GetPersonalityOutput, McpToolError>> {
        Box::pin(async move {
            let target_id = ctx
                .handles
                .resolve_personality(&args.personality)
                .ok_or_else(|| McpToolError::UnknownHandle(args.personality.clone()))?;
            let storage = ctx.storage().ok_or_else(|| {
                McpToolError::Other("engine storage unavailable".into())
            })?;
            let rows = storage
                .list_personality_instances(&ctx.owner, true)
                .await
                .map_err(McpToolError::Storage)?;
            let row = rows
                .into_iter()
                .find(|r| r.personality_instance_id == target_id)
                .ok_or_else(|| McpToolError::Other(format!(
                    "personality {} not found for owner",
                    args.personality
                )))?;
            let p_handle = ctx.handles.assign_personality(row.personality_instance_id);
            let n_handle = ctx.handles.assign_memory(row.current_root_perspective_memory_id);
            let wake_entries = row
                .wake_entries
                .into_iter()
                .map(|e| {
                    let w = ctx.handles.assign_wake_entry(e.wake_entry_id);
                    GetPersonalityWakeEntry {
                        handle: w.as_str().to_string(),
                        trigger_kind: e.trigger_kind,
                        trigger_id: e.trigger_id,
                        label: e.label,
                        enabled: e.enabled,
                        recipe_ref: e.recipe_ref,
                        model_tier: e.model_tier,
                        inference_target_ref: e.inference_target_ref,
                        substrate_tool_palette: e.substrate_tool_palette,
                        workspace_tool_palette: e.workspace_tool_palette,
                        execution_mode: e.execution_mode,
                        authored_by: e.authored_by,
                        probability_promille: e.probability_promille,
                        max_rounds: e.max_rounds,
                        disabled_reason: e.disabled_reason,
                    }
                })
                .collect();
            Ok(GetPersonalityOutput {
                handle: p_handle.as_str().to_string(),
                display_name: row.display_name,
                status: row.status,
                root_perspective: n_handle.as_str().to_string(),
                wake_entries,
            })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(GetPersonalityArgs))
            .expect("GetPersonalityArgs schema serializes")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::NoAuth;
    use crate::mcp::HandleTable;
    use crate::verbs::query::MemoryStore;
    use crate::{Engine, FlavorRegistry, McpAuthorContext, OrgId, Owner, Principal, UserId};
    use std::sync::Arc;

    fn make_ctx() -> McpToolCtx {
        let owner = Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        };
        let resolver = NoAuth::new(owner.principal.clone(), owner.clone());
        let engine = Arc::new(Engine::new(
            FlavorRegistry::new().freeze(),
            MemoryStore::new(),
            Box::new(resolver),
        ));
        McpToolCtx {
            pool: sqlx::PgPool::connect_lazy("postgres://x/x").expect("lazy"),
            owner,
            handles: Arc::new(HandleTable::new()),
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "t".into(), client_name: "t".into(),
                client_version: "0".into(), caller_self_perspective: None,
            },
            caller_self_perspective: None,
            engine: Some(engine),
        }
    }

    #[tokio::test]
    async fn get_personality_unknown_handle_returns_unknown_handle_err() {
        let ctx = make_ctx();
        let err = GetPersonalityTool::call(
            ctx,
            GetPersonalityArgs { personality: "P99".into() },
        ).await.unwrap_err();
        assert!(matches!(err, McpToolError::UnknownHandle(_)));
    }

    #[tokio::test]
    async fn get_personality_malformed_handle_returns_unknown_handle_err() {
        let ctx = make_ctx();
        let err = GetPersonalityTool::call(
            ctx,
            GetPersonalityArgs { personality: "not-a-handle".into() },
        ).await.unwrap_err();
        assert!(matches!(err, McpToolError::UnknownHandle(_)));
    }
}
```

- [ ] **Step 2: Export from module**

In `crates/core/src/mcp/core_tools/mod.rs`:

```rust
pub mod get_personality;

pub use get_personality::GetPersonalityTool;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p proxima-core --lib mcp::core_tools::get_personality`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/mcp/core_tools/get_personality.rs crates/core/src/mcp/core_tools/mod.rs
git commit -m "feat(mcp): core/get_personality tool"
```

---

## Task 8: `core/list_wake_entries` tool

**Files:**
- Create: `crates/core/src/mcp/core_tools/list_wake_entries.rs`
- Modify: `crates/core/src/mcp/core_tools/mod.rs`

- [ ] **Step 1: Write the tool**

Create `crates/core/src/mcp/core_tools/list_wake_entries.rs`:

```rust
//! `core/list_wake_entries` — read-only wake-entries projection for one
//! personality, with W-handles assigned.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListWakeEntriesTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListWakeEntriesArgs {
    /// `P`-handle for the personality whose wake entries to list.
    pub personality: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListWakeEntriesItem {
    pub handle: String,
    pub trigger_kind: String,
    pub trigger_id: String,
    pub label: String,
    pub enabled: bool,
    pub recipe_ref: String,
    pub probability_promille: u16,
    pub max_rounds: u16,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListWakeEntriesOutput {
    pub wake_entries: Vec<ListWakeEntriesItem>,
}

impl McpTool for ListWakeEntriesTool {
    const NAME: &'static str = "core/list_wake_entries";
    const DESCRIPTION: &'static str =
        "List wake entries on one personality. Returns W-handles for each \
         entry; use core/get_personality for the full payload.";
    type Args = ListWakeEntriesArgs;
    type Output = ListWakeEntriesOutput;

    fn call(
        ctx: McpToolCtx,
        args: ListWakeEntriesArgs,
    ) -> BoxFuture<'static, Result<ListWakeEntriesOutput, McpToolError>> {
        Box::pin(async move {
            let pid = ctx
                .handles
                .resolve_personality(&args.personality)
                .ok_or_else(|| McpToolError::UnknownHandle(args.personality.clone()))?;
            let storage = ctx.storage().ok_or_else(|| {
                McpToolError::Other("engine storage unavailable".into())
            })?;
            let rows = storage
                .list_personality_instances(&ctx.owner, true)
                .await
                .map_err(McpToolError::Storage)?;
            let row = rows
                .into_iter()
                .find(|r| r.personality_instance_id == pid)
                .ok_or_else(|| McpToolError::Other(format!(
                    "personality {} not found", args.personality
                )))?;
            let wake_entries = row
                .wake_entries
                .into_iter()
                .map(|e| {
                    let w = ctx.handles.assign_wake_entry(e.wake_entry_id);
                    ListWakeEntriesItem {
                        handle: w.as_str().to_string(),
                        trigger_kind: e.trigger_kind.as_str().to_string(),
                        trigger_id: e.trigger_id,
                        label: e.label,
                        enabled: e.enabled,
                        recipe_ref: e.recipe_ref,
                        probability_promille: e.probability_promille,
                        max_rounds: e.max_rounds,
                    }
                })
                .collect();
            Ok(ListWakeEntriesOutput { wake_entries })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(ListWakeEntriesArgs))
            .expect("ListWakeEntriesArgs schema serializes")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::NoAuth;
    use crate::mcp::HandleTable;
    use crate::verbs::query::MemoryStore;
    use crate::{Engine, FlavorRegistry, McpAuthorContext, OrgId, Owner, Principal, UserId};
    use std::sync::Arc;

    #[tokio::test]
    async fn list_wake_entries_unknown_handle_errs() {
        let owner = Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        };
        let resolver = NoAuth::new(owner.principal.clone(), owner.clone());
        let engine = Arc::new(Engine::new(
            FlavorRegistry::new().freeze(),
            MemoryStore::new(),
            Box::new(resolver),
        ));
        let ctx = McpToolCtx {
            pool: sqlx::PgPool::connect_lazy("postgres://x/x").expect("lazy"),
            owner, handles: Arc::new(HandleTable::new()),
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "t".into(), client_name: "t".into(),
                client_version: "0".into(), caller_self_perspective: None,
            },
            caller_self_perspective: None, engine: Some(engine),
        };
        let err = ListWakeEntriesTool::call(
            ctx, ListWakeEntriesArgs { personality: "P99".into() },
        ).await.unwrap_err();
        assert!(matches!(err, McpToolError::UnknownHandle(_)));
    }
}
```

In `crates/core/src/mcp/core_tools/mod.rs`:

```rust
pub mod list_wake_entries;
pub use list_wake_entries::ListWakeEntriesTool;
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p proxima-core --lib mcp::core_tools::list_wake_entries`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/mcp/core_tools/list_wake_entries.rs crates/core/src/mcp/core_tools/mod.rs
git commit -m "feat(mcp): core/list_wake_entries tool"
```

---

## Task 9: Inference list tools (`list_inference_targets`, `list_inference_tier_bindings`)

**Files:**
- Create: `crates/core/src/mcp/core_tools/list_inference_targets.rs`
- Create: `crates/core/src/mcp/core_tools/list_inference_tier_bindings.rs`
- Modify: `crates/core/src/mcp/core_tools/mod.rs`

- [ ] **Step 1: Confirm storage methods + types**

Run: `grep -nE 'fn list_inference_targets|fn list_inference_tier_bindings|InferenceTargetRow|InferenceTierBindingRow' crates/core/src/storage.rs | head -10`
Expected: locates the trait methods and row types. Note `InferenceTargetRow` shape (fields: `target_ref`, `config: ProviderConfig` or similar) and use those exact field names.

- [ ] **Step 2: Write `list_inference_targets.rs`**

```rust
//! `core/list_inference_targets` — read-only enumeration of registered
//! inference targets for the owner.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListInferenceTargetsTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListInferenceTargetsArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InferenceTargetItem {
    pub target_ref: String,
    /// Opaque provider config — surfaced as JSON so flavor-specific
    /// shapes pass through without core-side projection.
    pub config: serde_json::Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListInferenceTargetsOutput {
    pub targets: Vec<InferenceTargetItem>,
}

impl McpTool for ListInferenceTargetsTool {
    const NAME: &'static str = "core/list_inference_targets";
    const DESCRIPTION: &'static str =
        "List inference targets registered for this owner. Use returned \
         target_refs as `inference_target_ref` in WakeEntryDraftInput.";
    type Args = ListInferenceTargetsArgs;
    type Output = ListInferenceTargetsOutput;

    fn call(
        ctx: McpToolCtx,
        _args: ListInferenceTargetsArgs,
    ) -> BoxFuture<'static, Result<ListInferenceTargetsOutput, McpToolError>> {
        Box::pin(async move {
            let storage = ctx.storage().ok_or_else(|| {
                McpToolError::Other("engine storage unavailable".into())
            })?;
            let rows = storage
                .list_inference_targets(&ctx.owner)
                .await
                .map_err(McpToolError::Storage)?;
            let targets = rows
                .into_iter()
                .map(|row| InferenceTargetItem {
                    target_ref: row.target_ref,
                    config: serde_json::to_value(&row.config)
                        .unwrap_or(serde_json::Value::Null),
                })
                .collect();
            Ok(ListInferenceTargetsOutput { targets })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(ListInferenceTargetsArgs))
            .expect("schema serializes")
    })
}
```

- [ ] **Step 3: Write `list_inference_tier_bindings.rs`**

```rust
//! `core/list_inference_tier_bindings` — which inference targets back
//! each model tier for the owner.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::ModelTier;

#[derive(Debug, Default)]
pub struct ListInferenceTierBindingsTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListInferenceTierBindingsArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InferenceTierBindingItem {
    pub tier: ModelTier,
    pub target_ref: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListInferenceTierBindingsOutput {
    pub bindings: Vec<InferenceTierBindingItem>,
}

impl McpTool for ListInferenceTierBindingsTool {
    const NAME: &'static str = "core/list_inference_tier_bindings";
    const DESCRIPTION: &'static str =
        "List tier->target_ref bindings for this owner.";
    type Args = ListInferenceTierBindingsArgs;
    type Output = ListInferenceTierBindingsOutput;

    fn call(
        ctx: McpToolCtx,
        _args: ListInferenceTierBindingsArgs,
    ) -> BoxFuture<'static, Result<ListInferenceTierBindingsOutput, McpToolError>> {
        Box::pin(async move {
            let storage = ctx.storage().ok_or_else(|| {
                McpToolError::Other("engine storage unavailable".into())
            })?;
            let rows = storage
                .list_inference_tier_bindings(&ctx.owner)
                .await
                .map_err(McpToolError::Storage)?;
            let bindings = rows
                .into_iter()
                .map(|row| InferenceTierBindingItem {
                    tier: row.tier,
                    target_ref: row.target_ref,
                })
                .collect();
            Ok(ListInferenceTierBindingsOutput { bindings })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(ListInferenceTierBindingsArgs))
            .expect("schema serializes")
    })
}
```

- [ ] **Step 4: Module declarations**

In `crates/core/src/mcp/core_tools/mod.rs`:

```rust
pub mod list_inference_targets;
pub mod list_inference_tier_bindings;

pub use list_inference_targets::ListInferenceTargetsTool;
pub use list_inference_tier_bindings::ListInferenceTierBindingsTool;
```

- [ ] **Step 5: Run cargo check**

Run: `cargo check -p proxima-core`
Expected: BUILD OK. (Unit tests for these read-only tools live in the integration suite — Task 20 — to avoid mocking InferenceTargetRow shape per-tool.)

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/mcp/core_tools/list_inference_targets.rs \
        crates/core/src/mcp/core_tools/list_inference_tier_bindings.rs \
        crates/core/src/mcp/core_tools/mod.rs
git commit -m "feat(mcp): core/list_inference_{targets,tier_bindings} tools"
```

---

## Task 10: Five discovery tools (`list_recipes`, `list_substrate_tools`, `list_workspace_tools`, `list_schemas`, `list_edge_types`)

**Files:**
- Create: `crates/core/src/mcp/core_tools/list_recipes.rs`
- Create: `crates/core/src/mcp/core_tools/list_substrate_tools.rs`
- Create: `crates/core/src/mcp/core_tools/list_workspace_tools.rs`
- Create: `crates/core/src/mcp/core_tools/list_schemas.rs`
- Create: `crates/core/src/mcp/core_tools/list_edge_types.rs`
- Modify: `crates/core/src/mcp/core_tools/mod.rs`

- [ ] **Step 1: `list_recipes.rs`**

```rust
//! `core/list_recipes` — enumerate flavor-bundled and owner recipes.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListRecipesTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListRecipesArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RecipeItem {
    pub recipe_ref: String,
    /// `"flavor:<flavor_id>"` for bundled recipes; `"owner"` for
    /// recipes found in the engine's owner_recipes_root.
    pub source: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListRecipesOutput {
    pub recipes: Vec<RecipeItem>,
}

impl McpTool for ListRecipesTool {
    const NAME: &'static str = "core/list_recipes";
    const DESCRIPTION: &'static str =
        "List recipes referenceable as recipe_ref in WakeEntryDraftInput.";
    type Args = ListRecipesArgs;
    type Output = ListRecipesOutput;

    fn call(
        ctx: McpToolCtx,
        _args: ListRecipesArgs,
    ) -> BoxFuture<'static, Result<ListRecipesOutput, McpToolError>> {
        Box::pin(async move {
            let mut recipes = Vec::new();
            // Flavor-bundled: list_bundled_recipes returns "<flavor>/<name>"
            // slugs in registration order.
            for slug in ctx.registry.list_bundled_recipes() {
                let flavor = slug.split('/').next().unwrap_or("");
                recipes.push(RecipeItem {
                    recipe_ref: slug.to_string(),
                    source: format!("flavor:{flavor}"),
                });
            }
            // Owner recipes: enumerate engine.owner_recipes_root() if
            // available. The Engine accessor is `owner_recipes_root()`
            // returning Option<&Path>; if it doesn't exist with that
            // exact name, search and adjust.
            if let Some(engine) = ctx.engine() {
                let root = engine.owner_recipes_root();
                if let Some(root) = root {
                    if let Ok(entries) = std::fs::read_dir(root) {
                        for entry in entries.flatten() {
                            let p = entry.path();
                            if p.extension().and_then(|s| s.to_str()) == Some("yaml") {
                                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                                    recipes.push(RecipeItem {
                                        recipe_ref: stem.to_string(),
                                        source: "owner".into(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
            Ok(ListRecipesOutput { recipes })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::to_value(schemars::schema_for!(ListRecipesArgs))
        .expect("schema serializes"))
}
```

`Engine::owner_recipes_root()` accessor — verify it exists. If absent, search:
`grep -n 'owner_recipes_root\|with_owner_recipes_root' crates/core/src/engine/mod.rs`
and adapt.

- [ ] **Step 2: `list_substrate_tools.rs`**

```rust
//! `core/list_substrate_tools` — enumerate substrate-pack tools and
//! flavor-registered MCP tools.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListSubstrateToolsTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListSubstrateToolsArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SubstrateToolItem {
    pub tool_id: String,
    pub source: String,
    pub description: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListSubstrateToolsOutput {
    pub tools: Vec<SubstrateToolItem>,
}

impl McpTool for ListSubstrateToolsTool {
    const NAME: &'static str = "core/list_substrate_tools";
    const DESCRIPTION: &'static str =
        "List tool ids accepted in WakeEntryDraftInput.substrate_tool_palette.";
    type Args = ListSubstrateToolsArgs;
    type Output = ListSubstrateToolsOutput;

    fn call(
        ctx: McpToolCtx,
        _args: ListSubstrateToolsArgs,
    ) -> BoxFuture<'static, Result<ListSubstrateToolsOutput, McpToolError>> {
        Box::pin(async move {
            let mut tools = Vec::new();
            // Substrate pack: hard-coded `core/...` ids.
            for tool in crate::personality::substrate_pack() {
                tools.push(SubstrateToolItem {
                    tool_id: tool.tool_id().to_string(),
                    source: "substrate".into(),
                    description: tool.description().to_string(),
                });
            }
            // Flavor-registered MCP tools: from the frozen registry.
            for desc in ctx.registry.list_mcp_tools() {
                let source = if desc.name.starts_with("core/") {
                    "substrate".into()
                } else {
                    let flavor = desc.name.split('/').next().unwrap_or("flavor");
                    format!("flavor:{flavor}")
                };
                tools.push(SubstrateToolItem {
                    tool_id: desc.name.to_string(),
                    source,
                    description: desc.description.to_string(),
                });
            }
            Ok(ListSubstrateToolsOutput { tools })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::to_value(schemars::schema_for!(ListSubstrateToolsArgs))
        .expect("schema serializes"))
}
```

- [ ] **Step 3: `list_workspace_tools.rs`**

```rust
//! `core/list_workspace_tools` — enumerate workspace tool catalog.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListWorkspaceToolsTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListWorkspaceToolsArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkspaceToolItem {
    pub tool_id: String,
    pub description: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListWorkspaceToolsOutput {
    pub tools: Vec<WorkspaceToolItem>,
}

impl McpTool for ListWorkspaceToolsTool {
    const NAME: &'static str = "core/list_workspace_tools";
    const DESCRIPTION: &'static str =
        "List tool ids accepted in WakeEntryDraftInput.workspace_tool_palette.";
    type Args = ListWorkspaceToolsArgs;
    type Output = ListWorkspaceToolsOutput;

    fn call(
        _ctx: McpToolCtx,
        _args: ListWorkspaceToolsArgs,
    ) -> BoxFuture<'static, Result<ListWorkspaceToolsOutput, McpToolError>> {
        Box::pin(async move {
            // The catalog is a const slice of (id, description) tuples
            // exported by the personality module. Walk it directly.
            let tools = crate::personality::WORKSPACE_TOOL_CATALOG
                .iter()
                .map(|(id, desc)| WorkspaceToolItem {
                    tool_id: (*id).to_string(),
                    description: (*desc).to_string(),
                })
                .collect();
            Ok(ListWorkspaceToolsOutput { tools })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::to_value(schemars::schema_for!(ListWorkspaceToolsArgs))
        .expect("schema serializes"))
}
```

`crate::personality::WORKSPACE_TOOL_CATALOG` — confirm it's `pub` and the tuple shape:
`grep -n 'WORKSPACE_TOOL_CATALOG' crates/core/src/personality/mod.rs`
Expected: `pub const WORKSPACE_TOOL_CATALOG: &[(&str, &str)] = &[...]` or similar. Adjust the iteration accordingly.

- [ ] **Step 4: `list_schemas.rs`**

```rust
//! `core/list_schemas` — project FlavorRegistryFrozen schemas.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::PayloadKind;

#[derive(Debug, Default)]
pub struct ListSchemasTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListSchemasArgs {
    /// Optional filter. One of "Fact", "Abstraction", "Perspective",
    /// "Goal", "Edge". Omit to return all kinds.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SchemaItem {
    pub schema_id: String,
    pub schema_version: u32,
    pub kind: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListSchemasOutput {
    pub schemas: Vec<SchemaItem>,
}

fn parse_kind(s: &str) -> Option<PayloadKind> {
    match s {
        "Fact" => Some(PayloadKind::Fact),
        "Abstraction" => Some(PayloadKind::Abstraction),
        "Perspective" => Some(PayloadKind::Perspective),
        "Goal" => Some(PayloadKind::Goal),
        "Edge" => Some(PayloadKind::Edge),
        "CitedObject" => Some(PayloadKind::CitedObject),
        "CitationMapping" => Some(PayloadKind::CitationMapping),
        _ => None,
    }
}

fn kind_str(k: PayloadKind) -> &'static str {
    match k {
        PayloadKind::Fact => "Fact",
        PayloadKind::Abstraction => "Abstraction",
        PayloadKind::Perspective => "Perspective",
        PayloadKind::Goal => "Goal",
        PayloadKind::Edge => "Edge",
        PayloadKind::CitedObject => "CitedObject",
        PayloadKind::CitationMapping => "CitationMapping",
    }
}

impl McpTool for ListSchemasTool {
    const NAME: &'static str = "core/list_schemas";
    const DESCRIPTION: &'static str =
        "List registered schemas. Filter by kind for trigger discovery: \
         OnMemory triggers point at Fact schema_ids.";
    type Args = ListSchemasArgs;
    type Output = ListSchemasOutput;

    fn call(
        ctx: McpToolCtx,
        args: ListSchemasArgs,
    ) -> BoxFuture<'static, Result<ListSchemasOutput, McpToolError>> {
        Box::pin(async move {
            let filter = args.kind.as_deref().and_then(parse_kind);
            let schemas = ctx
                .registry
                .list()
                .into_iter()
                .filter(|info| filter.map_or(true, |k| info.kind == k))
                .map(|info| SchemaItem {
                    schema_id: info.schema_id.as_str().to_string(),
                    schema_version: info.schema_version.into_inner(),
                    kind: kind_str(info.kind).to_string(),
                })
                .collect();
            Ok(ListSchemasOutput { schemas })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::to_value(schemars::schema_for!(ListSchemasArgs))
        .expect("schema serializes"))
}
```

- [ ] **Step 5: `list_edge_types.rs`**

```rust
//! `core/list_edge_types` — project FlavorRegistryFrozen relations.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListEdgeTypesTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListEdgeTypesArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EdgeTypeItem {
    pub edge_type: String,
    pub class: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListEdgeTypesOutput {
    pub edge_types: Vec<EdgeTypeItem>,
}

impl McpTool for ListEdgeTypesTool {
    const NAME: &'static str = "core/list_edge_types";
    const DESCRIPTION: &'static str =
        "List registered edge types. OnEdge triggers reference these.";
    type Args = ListEdgeTypesArgs;
    type Output = ListEdgeTypesOutput;

    fn call(
        ctx: McpToolCtx,
        _args: ListEdgeTypesArgs,
    ) -> BoxFuture<'static, Result<ListEdgeTypesOutput, McpToolError>> {
        Box::pin(async move {
            let edge_types = ctx
                .registry
                .list_relations()
                .iter()
                .map(|rel| EdgeTypeItem {
                    edge_type: rel.relation.clone(),
                    class: format!("{:?}", rel.class),
                })
                .collect();
            Ok(ListEdgeTypesOutput { edge_types })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::to_value(schemars::schema_for!(ListEdgeTypesArgs))
        .expect("schema serializes"))
}
```

- [ ] **Step 6: Module declarations**

In `crates/core/src/mcp/core_tools/mod.rs`:

```rust
pub mod list_recipes;
pub mod list_substrate_tools;
pub mod list_workspace_tools;
pub mod list_schemas;
pub mod list_edge_types;

pub use list_recipes::ListRecipesTool;
pub use list_substrate_tools::ListSubstrateToolsTool;
pub use list_workspace_tools::ListWorkspaceToolsTool;
pub use list_schemas::ListSchemasTool;
pub use list_edge_types::ListEdgeTypesTool;
```

- [ ] **Step 7: Run cargo check**

Run: `cargo check -p proxima-core`
Expected: BUILD OK. Per-tool unit tests for these are minimal because each is a near-trivial projection; the discovery integration test (Task 20) covers correctness end-to-end.

- [ ] **Step 8: Commit**

```bash
git add crates/core/src/mcp/core_tools/list_recipes.rs \
        crates/core/src/mcp/core_tools/list_substrate_tools.rs \
        crates/core/src/mcp/core_tools/list_workspace_tools.rs \
        crates/core/src/mcp/core_tools/list_schemas.rs \
        crates/core/src/mcp/core_tools/list_edge_types.rs \
        crates/core/src/mcp/core_tools/mod.rs
git commit -m "feat(mcp): five discovery tools (list_recipes, _substrate_tools, _workspace_tools, _schemas, _edge_types)"
```

---

## Task 11: `core/instantiate_personality` tool + audit

**Files:**
- Create: `crates/core/src/mcp/core_tools/instantiate_personality.rs`
- Modify: `crates/core/src/mcp/core_tools/mod.rs`

- [ ] **Step 1: Write the tool with audit emit**

Create `crates/core/src/mcp/core_tools/instantiate_personality.rs`:

```rust
//! `core/instantiate_personality` — wraps `Engine::instantiate_personality`
//! and emits a `core/personality_config_changed_v1` Fact.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::auth::Credentials;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};
use crate::InstantiatePersonalityRequest;

#[derive(Debug, Default)]
pub struct InstantiatePersonalityTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InstantiatePersonalityArgs {
    pub display_name: String,
    pub purpose: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InstantiatePersonalityOutput {
    pub handle: String,
    /// `Some` when the audit Fact emit failed after a successful verb.
    /// Caller may treat this as a soft warning.
    pub audit_emit_failed: Option<String>,
}

impl McpTool for InstantiatePersonalityTool {
    const NAME: &'static str = "core/instantiate_personality";
    const DESCRIPTION: &'static str =
        "Instantiate one inert personality with a Root Perspective and \
         empty WakeConfig. Returns the new P-handle.";
    type Args = InstantiatePersonalityArgs;
    type Output = InstantiatePersonalityOutput;

    fn call(
        ctx: McpToolCtx,
        args: InstantiatePersonalityArgs,
    ) -> BoxFuture<'static, Result<InstantiatePersonalityOutput, McpToolError>> {
        Box::pin(async move {
            let display_name = args.display_name.trim();
            let purpose = args.purpose.trim();
            if display_name.is_empty() {
                return Err(McpToolError::InvalidInput("display_name is empty".into()));
            }
            if purpose.is_empty() {
                return Err(McpToolError::InvalidInput("purpose is empty".into()));
            }
            let engine = ctx.engine().ok_or_else(|| {
                McpToolError::Other("engine unavailable".into())
            })?;
            let req = InstantiatePersonalityRequest {
                owner: ctx.owner.clone(),
                display_name: display_name.to_string(),
                purpose: purpose.to_string(),
            };
            let resp = engine
                .instantiate_personality(&Credentials::None, &req)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            let after = serde_json::json!({
                "personality_instance_id": resp.instance_id.into_inner(),
                "display_name": display_name,
                "purpose": purpose,
            });
            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::Instantiate,
                PersonalityConfigChangedSubject::Personality(resp.instance_id.into_inner()),
                None,
                Some(after),
            ).await;
            let p_handle = ctx.handles.assign_personality(resp.instance_id);
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => {
                    tracing::warn!(reason, "personality_config_changed audit emit failed");
                    Some(reason)
                }
            };
            Ok(InstantiatePersonalityOutput {
                handle: p_handle.as_str().to_string(),
                audit_emit_failed,
            })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::to_value(schemars::schema_for!(InstantiatePersonalityArgs))
        .expect("schema serializes"))
}
```

`Engine::instantiate_personality(&Credentials::None, &req)` — verify the exact signature against `crates/core/src/engine/mod.rs`. If the engine method takes the request directly (no creds arg), drop the `Credentials::None`.

- [ ] **Step 2: Module export**

In `crates/core/src/mcp/core_tools/mod.rs`:

```rust
pub mod instantiate_personality;
pub use instantiate_personality::InstantiatePersonalityTool;
```

- [ ] **Step 3: Run cargo check**

Run: `cargo check -p proxima-core`
Expected: BUILD OK. (Happy-path test runs in the integration suite — Task 20 — because it requires real Postgres for `Engine::instantiate_personality` to actually persist.)

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/mcp/core_tools/instantiate_personality.rs crates/core/src/mcp/core_tools/mod.rs
git commit -m "feat(mcp): core/instantiate_personality with audit"
```

---

## Task 12: `core/tombstone_personality` tool + audit

**Files:**
- Create: `crates/core/src/mcp/core_tools/tombstone_personality.rs`
- Modify: `crates/core/src/mcp/core_tools/mod.rs`

- [ ] **Step 1: Write the tool**

```rust
//! `core/tombstone_personality` — wraps `Engine::tombstone_personality`
//! and emits an audit Fact. Idempotent.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::auth::Credentials;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};
use crate::TombstonePersonalityRequest;

#[derive(Debug, Default)]
pub struct TombstonePersonalityTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TombstonePersonalityArgs {
    /// `P`-handle of the personality to tombstone.
    pub personality: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TombstonePersonalityOutput {
    pub status: String,
    pub idempotent_replay: bool,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for TombstonePersonalityTool {
    const NAME: &'static str = "core/tombstone_personality";
    const DESCRIPTION: &'static str =
        "Tombstone a personality. Idempotent: replay returns idempotent_replay=true.";
    type Args = TombstonePersonalityArgs;
    type Output = TombstonePersonalityOutput;

    fn call(
        ctx: McpToolCtx,
        args: TombstonePersonalityArgs,
    ) -> BoxFuture<'static, Result<TombstonePersonalityOutput, McpToolError>> {
        Box::pin(async move {
            let pid = ctx
                .handles
                .resolve_personality(&args.personality)
                .ok_or_else(|| McpToolError::UnknownHandle(args.personality.clone()))?;
            let engine = ctx.engine().ok_or_else(|| {
                McpToolError::Other("engine unavailable".into())
            })?;
            // Snapshot prior state for the audit `before`.
            let storage = engine.storage();
            let rows = storage
                .list_personality_instances(&ctx.owner, true)
                .await
                .map_err(McpToolError::Storage)?;
            let before_row = rows.iter().find(|r| r.personality_instance_id == pid);
            let before = before_row.map(|r| serde_json::json!({
                "display_name": r.display_name,
                "status": r.status,
                "wake_entry_count": r.wake_entries.len(),
            }));
            let req = TombstonePersonalityRequest {
                owner: ctx.owner.clone(),
                personality_instance_id: pid,
            };
            let resp = engine
                .tombstone_personality(&Credentials::None, &req)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::Tombstone,
                PersonalityConfigChangedSubject::Personality(pid.into_inner()),
                before,
                None,
            ).await;
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => Some(reason),
            };
            Ok(TombstonePersonalityOutput {
                status: resp.status,
                idempotent_replay: resp.idempotent_replay,
                audit_emit_failed,
            })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::to_value(schemars::schema_for!(TombstonePersonalityArgs))
        .expect("schema serializes"))
}
```

- [ ] **Step 2: Module export**

```rust
pub mod tombstone_personality;
pub use tombstone_personality::TombstonePersonalityTool;
```

- [ ] **Step 3: Run cargo check**

Run: `cargo check -p proxima-core`
Expected: BUILD OK.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/mcp/core_tools/tombstone_personality.rs crates/core/src/mcp/core_tools/mod.rs
git commit -m "feat(mcp): core/tombstone_personality with audit"
```

---

## Task 13: Storage primitive `set_wake_entries_within` (transactional R-M-W)

**Files:**
- Modify: `crates/core/src/storage.rs`
- Modify: `crates/storage-pg/src/lib.rs`
- Modify: `crates/storage-pg/src/verbs/consolidate.rs`
- Test: `crates/storage-pg/tests/set_wake_entries_within_pg.rs`

- [ ] **Step 1: Add the trait method**

In `crates/core/src/storage.rs`, add after `set_wake_entries`:

```rust
/// Transactional read-modify-write over a personality's WakeConfig.
/// Acquires a row-level lock on the personality (SELECT ... FOR UPDATE),
/// reads its current entries, calls `mutate` to compute the new entry
/// list, and then commits the replacement in the same transaction.
///
/// `mutate` is called with the current `Vec<WakeEntryDraft>` (with
/// `wake_entry_id`s already populated) and returns the new list. If
/// `mutate` returns an error, the transaction is rolled back and the
/// error is mapped to `StorageError::Internal`.
async fn set_wake_entries_within<F>(
    &self,
    owner: &Owner,
    personality_instance_id: PersonalityInstanceId,
    mutate: F,
) -> Result<SetWakeEntriesResponse, StorageError>
where
    F: for<'a> FnOnce(&'a [WakeEntryDraft]) -> Result<Vec<WakeEntryDraft>, String>
        + Send + 'static;
```

The lifetime/`Send` bounds may need refinement — start with this signature; if generic `FnOnce` is awkward in the trait object context, narrow `mutate` to a concrete enum of patches (`Append`, `Replace`, `RemoveBy`) instead. Pick the simplest shape that compiles.

- [ ] **Step 2: NoopStorage stub**

Add to NoopStorage:

```rust
async fn set_wake_entries_within<F>(
    &self,
    _owner: &Owner,
    _personality_instance_id: PersonalityInstanceId,
    _mutate: F,
) -> Result<SetWakeEntriesResponse, StorageError>
where
    F: for<'a> FnOnce(&'a [WakeEntryDraft]) -> Result<Vec<WakeEntryDraft>, String>
        + Send + 'static,
{
    Err(StorageError::Internal("NoopStorage rejects writes".into()))
}
```

- [ ] **Step 3: PgStorage impl**

In `crates/storage-pg/src/lib.rs` (PgStorage impl block):

```rust
async fn set_wake_entries_within<F>(
    &self,
    owner: &Owner,
    personality_instance_id: PersonalityInstanceId,
    mutate: F,
) -> Result<SetWakeEntriesResponse, StorageError>
where
    F: for<'a> FnOnce(&'a [WakeEntryDraft]) -> Result<Vec<WakeEntryDraft>, String>
        + Send + 'static,
{
    verbs::consolidate::set_wake_entries_within(
        &self.pool,
        owner,
        personality_instance_id,
        mutate,
    ).await
}
```

In `crates/storage-pg/src/verbs/consolidate.rs`, add a new function:

```rust
pub async fn set_wake_entries_within<F>(
    pool: &sqlx::PgPool,
    owner: &Owner,
    personality_instance_id: PersonalityInstanceId,
    mutate: F,
) -> Result<SetWakeEntriesResponse, StorageError>
where
    F: for<'a> FnOnce(&'a [WakeEntryDraft]) -> Result<Vec<WakeEntryDraft>, String>
        + Send + 'static,
{
    let mut tx = pool.begin().await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

    // Lock the personality row to serialize concurrent granular ops.
    let _: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT personality_instance_id
         FROM proxima_core.personality
         WHERE personality_instance_id = $1
           AND org_id = $2
           AND principal_kind = $3
           AND principal_id = $4
         FOR UPDATE",
    )
    .bind(personality_instance_id.into_inner())
    .bind(owner.org_id.into_inner())
    .bind(owner.principal.kind_str())
    .bind(owner.principal.id_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?
    .ok_or(StorageError::NotFound)?;

    // Read current entries inside the locked transaction.
    let current = read_wake_entries_in_tx(&mut *tx, owner, personality_instance_id).await?;

    // Run user-provided mutation.
    let new_entries = mutate(&current).map_err(StorageError::Internal)?;

    // Build the canonical SetWakeEntriesRequest and call the existing
    // replace-all writer inside the same transaction.
    let req = SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id,
        entries: new_entries,
    };
    let resp = set_wake_entries_in_tx(&mut tx, &req).await?;

    tx.commit().await.map_err(|e| StorageError::Internal(e.to_string()))?;
    Ok(resp)
}

async fn read_wake_entries_in_tx(
    tx: &mut sqlx::PgConnection,
    owner: &Owner,
    pid: PersonalityInstanceId,
) -> Result<Vec<WakeEntryDraft>, StorageError> {
    // Read by joining the personality + active wake_entry tables.
    // Mirror the columns the existing list_personality_instances query
    // already projects; the row-to-WakeEntryDraft mapping should match.
    todo!("project WakeEntryDraft rows from proxima_core.personality_wake_entry \
           where personality_instance_id = pid AND tombstoned_at IS NULL");
}
```

The `read_wake_entries_in_tx` body and `set_wake_entries_in_tx` extraction depend on how the existing `set_wake_entries` is structured. Steps:

1. Read `crates/storage-pg/src/verbs/consolidate.rs` around line 248 (the existing `set_wake_entries` entry point).
2. Extract a `set_wake_entries_in_tx(tx, req)` that takes a `&mut Transaction` and does the body work.
3. Extract a sibling `read_wake_entries_in_tx(tx, owner, pid)` that mirrors the SQL used by `list_personality_instances` to project `WakeEntryDraft` rows.
4. Refactor the existing `set_wake_entries` to call `pool.begin()` then `set_wake_entries_in_tx` then commit.

This is mechanical refactor work. The replace-all semantics + validation contract stay unchanged.

- [ ] **Step 4: Write the integration test**

Create `crates/storage-pg/tests/set_wake_entries_within_pg.rs`:

```rust
use proxima_core::storage::Storage;
use proxima_core::{
    InstantiatePersonalityRequest, ModelTier, OrgId, Owner, PersonalityInstanceId,
    Principal, UserId, WakeEntryAuthoredBy, WakeEntryDraft, WakeEntryTriggerKind,
};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection};

const ADMIN_URL: &str = "postgres://postgres@localhost/postgres";

#[tokio::test]
async fn set_wake_entries_within_appends_one() -> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else { return Ok(()) };
    let database_url = format!("postgres://postgres@localhost/{db_name}");
    let storage = PgStorage::connect(&database_url).await?;
    storage.run_migrations().await?;
    let owner = Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
        org_id: OrgId::new(uuid::Uuid::now_v7()),
    };
    // Instantiate a personality to mutate.
    let inst = storage.instantiate_personality(&InstantiatePersonalityRequest {
        owner: owner.clone(),
        display_name: "test".into(),
        purpose: "rmw fixture".into(),
    }).await?;
    // Append one entry via the new R-M-W primitive.
    let _ = storage.set_wake_entries_within(&owner, inst.instance_id, move |current| {
        assert!(current.is_empty(), "fresh personality has no entries");
        let new_entry = WakeEntryDraft::new(
            uuid::Uuid::now_v7(),
            inst.instance_id,
            WakeEntryTriggerKind::OnMemory,
            "core/personality_config_changed_v1".to_string(),
            "rmw-test".to_string(),
            WakeEntryAuthoredBy::Any,
            10,
            "proxima-code/engineer".to_string(),
            ModelTier::Standard,
            None,
            vec!["core/fetch_memory".into()],
            3,
        ).expect("draft");
        Ok(vec![new_entry])
    }).await?;
    // Verify via list path.
    let rows = storage.list_personality_instances(&owner, false).await?;
    let row = rows.into_iter().find(|r| r.personality_instance_id == inst.instance_id)
        .expect("found");
    assert_eq!(row.wake_entries.len(), 1);
    assert_eq!(row.wake_entries[0].label, "rmw-test");
    drop_db(&db_name).await?;
    Ok(())
}

async fn create_db() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let mut admin = match PgConnection::connect(ADMIN_URL).await {
        Ok(c) => c, Err(_) => return Ok(None),
    };
    let name = format!("proxima_test_{}", uuid::Uuid::now_v7().simple());
    admin.execute(format!("CREATE DATABASE {name}").as_str()).await?;
    Ok(Some(name))
}

async fn drop_db(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut admin = PgConnection::connect(ADMIN_URL).await?;
    admin.execute(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)").as_str()).await?;
    Ok(())
}
```

- [ ] **Step 5: Run the test**

Run: `cargo test -p proxima-storage-pg --test set_wake_entries_within_pg`
Expected: PASS (or skip if no Postgres).

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/storage.rs crates/storage-pg/
git commit -m "feat(storage): set_wake_entries_within transactional R-M-W primitive"
```

---

## Task 14: `core/set_wake_entries` tool + audit

**Files:**
- Create: `crates/core/src/mcp/core_tools/set_wake_entries.rs`
- Create: `crates/core/src/mcp/core_tools/wake_entry_input.rs` — shared `WakeEntryDraftInput` type
- Modify: `crates/core/src/mcp/core_tools/mod.rs`

- [ ] **Step 1: Define the shared input type**

Create `crates/core/src/mcp/core_tools/wake_entry_input.rs`:

```rust
//! Wire-shape for `WakeEntryDraft` input via MCP tools. Strips the
//! engine-allocated `personality_instance_id` (filled in by the tool
//! layer) and converts `wake_entry_id: Uuid` to `wake_entry_id:
//! Option<W-handle>`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{HandleTable, McpToolError};
use crate::{
    ModelTier, PersonalityInstanceId, WakeEntryAuthoredBy, WakeEntryDraft,
    WakeEntryTriggerKind, WakeExecutionMode,
};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WakeEntryDraftInput {
    /// Optional W-handle. Omit for new entries (UUID is allocated);
    /// pass an existing handle to preserve identity in a bulk replace.
    #[serde(default)]
    pub wake_entry_id: Option<String>,
    pub trigger_kind: WakeEntryTriggerKind,
    pub trigger_id: String,
    pub label: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_execution_mode")]
    pub execution_mode: WakeExecutionMode,
    #[serde(default)]
    pub authored_by: WakeEntryAuthoredBy,
    #[schemars(range(min = 0, max = 1000))]
    pub probability_promille: u16,
    pub recipe_ref: String,
    #[serde(default = "default_model_tier")]
    pub model_tier: ModelTier,
    #[serde(default)]
    pub inference_target_ref: Option<String>,
    #[serde(default)]
    pub substrate_tool_palette: Vec<String>,
    #[serde(default)]
    pub workspace_tool_palette: Vec<String>,
    #[schemars(range(min = 1))]
    pub max_rounds: u16,
}

fn default_enabled() -> bool { true }
fn default_execution_mode() -> WakeExecutionMode { WakeExecutionMode::SubstrateOnly }
fn default_model_tier() -> ModelTier { ModelTier::Standard }

impl WakeEntryDraftInput {
    /// Resolve into a `WakeEntryDraft`. Allocates a fresh UUID when
    /// `wake_entry_id` is `None`; resolves through `HandleTable` when
    /// `Some`.
    pub fn into_draft(
        self,
        handles: &HandleTable,
        personality_instance_id: PersonalityInstanceId,
    ) -> Result<WakeEntryDraft, McpToolError> {
        let wake_entry_id = match self.wake_entry_id {
            None => uuid::Uuid::now_v7(),
            Some(handle) => handles
                .resolve_wake_entry(&handle)
                .ok_or_else(|| McpToolError::UnknownHandle(handle))?,
        };
        Ok(WakeEntryDraft {
            wake_entry_id,
            personality_instance_id,
            trigger_kind: self.trigger_kind,
            trigger_id: self.trigger_id,
            label: self.label,
            enabled: self.enabled,
            execution_mode: self.execution_mode,
            authored_by: self.authored_by,
            probability_promille: self.probability_promille,
            recipe_ref: self.recipe_ref,
            model_tier: self.model_tier,
            inference_target_ref: self.inference_target_ref,
            substrate_tool_palette: self.substrate_tool_palette,
            workspace_tool_palette: self.workspace_tool_palette,
            max_rounds: self.max_rounds,
        })
    }
}
```

`WakeEntryAuthoredBy` needs `Default` impl for `#[serde(default)]`. If it doesn't have one, add `#[derive(Default)]` and mark `Any` as default in the original enum at `crates/core/src/personality/mod.rs`.

- [ ] **Step 2: Write the tool**

Create `crates/core/src/mcp/core_tools/set_wake_entries.rs`:

```rust
//! `core/set_wake_entries` — replace-all bulk write of a personality's
//! wake entries. Mirrors `Engine::set_wake_entries`.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::auth::Credentials;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::core_tools::wake_entry_input::WakeEntryDraftInput;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::SetWakeEntriesRequest;

#[derive(Debug, Default)]
pub struct SetWakeEntriesTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetWakeEntriesArgs {
    pub personality: String,
    pub entries: Vec<WakeEntryDraftInput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetWakeEntriesOutput {
    pub active_entries: u32,
    pub entry_handles: Vec<String>,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for SetWakeEntriesTool {
    const NAME: &'static str = "core/set_wake_entries";
    const DESCRIPTION: &'static str =
        "Replace all wake entries for a personality. Carry-over entries \
         keep their identity by passing the W-handle from list_wake_entries \
         in wake_entry_id; omit wake_entry_id for new entries.";
    type Args = SetWakeEntriesArgs;
    type Output = SetWakeEntriesOutput;

    fn call(
        ctx: McpToolCtx,
        args: SetWakeEntriesArgs,
    ) -> BoxFuture<'static, Result<SetWakeEntriesOutput, McpToolError>> {
        Box::pin(async move {
            let pid = ctx
                .handles
                .resolve_personality(&args.personality)
                .ok_or_else(|| McpToolError::UnknownHandle(args.personality.clone()))?;
            let engine = ctx.engine().ok_or_else(|| {
                McpToolError::Other("engine unavailable".into())
            })?;

            // Snapshot before for audit.
            let before_rows = engine.storage()
                .list_personality_instances(&ctx.owner, true)
                .await
                .map_err(McpToolError::Storage)?;
            let before = before_rows.iter()
                .find(|r| r.personality_instance_id == pid)
                .map(|r| serde_json::json!({
                    "wake_entry_count": r.wake_entries.len(),
                    "wake_entry_ids": r.wake_entries.iter()
                        .map(|e| e.wake_entry_id).collect::<Vec<_>>(),
                }));

            // Resolve inputs into drafts.
            let drafts = args.entries.into_iter()
                .map(|input| input.into_draft(&ctx.handles, pid))
                .collect::<Result<Vec<_>, _>>()?;

            let req = SetWakeEntriesRequest {
                owner: ctx.owner.clone(),
                personality_instance_id: pid,
                entries: drafts.clone(),
            };
            let resp = engine
                .set_wake_entries(&Credentials::None, &req)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;

            // Assign W-handles for each (possibly new) entry.
            let entry_handles: Vec<String> = drafts.iter()
                .map(|d| ctx.handles.assign_wake_entry(d.wake_entry_id).as_str().to_string())
                .collect();

            let after = serde_json::json!({
                "wake_entry_count": drafts.len(),
                "wake_entry_ids": drafts.iter().map(|d| d.wake_entry_id).collect::<Vec<_>>(),
            });
            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::SetWakeEntries,
                PersonalityConfigChangedSubject::Personality(pid.into_inner()),
                before,
                Some(after),
            ).await;
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => Some(reason),
            };
            Ok(SetWakeEntriesOutput {
                active_entries: resp.active_entries,
                entry_handles,
                audit_emit_failed,
            })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::to_value(schemars::schema_for!(SetWakeEntriesArgs))
        .expect("schema serializes"))
}
```

- [ ] **Step 3: Module exports**

```rust
pub mod wake_entry_input;
pub mod set_wake_entries;
pub use wake_entry_input::WakeEntryDraftInput;
pub use set_wake_entries::SetWakeEntriesTool;
```

- [ ] **Step 4: Run cargo check**

Run: `cargo check -p proxima-core`
Expected: BUILD OK.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/mcp/core_tools/wake_entry_input.rs \
        crates/core/src/mcp/core_tools/set_wake_entries.rs \
        crates/core/src/mcp/core_tools/mod.rs
git commit -m "feat(mcp): core/set_wake_entries with audit"
```

---

## Task 15: `core/add_wake_entry` tool + audit

**Files:**
- Create: `crates/core/src/mcp/core_tools/add_wake_entry.rs`
- Modify: `crates/core/src/mcp/core_tools/mod.rs`

- [ ] **Step 1: Write the tool**

Create `crates/core/src/mcp/core_tools/add_wake_entry.rs`:

```rust
//! `core/add_wake_entry` — granular append to a personality's
//! WakeConfig. Read-modify-write inside one transaction via
//! Storage::set_wake_entries_within.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::core_tools::wake_entry_input::WakeEntryDraftInput;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct AddWakeEntryTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddWakeEntryArgs {
    pub personality: String,
    pub entry: WakeEntryDraftInput,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AddWakeEntryOutput {
    pub handle: String,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for AddWakeEntryTool {
    const NAME: &'static str = "core/add_wake_entry";
    const DESCRIPTION: &'static str =
        "Append one wake entry to a personality. Conflicts with an existing \
         (trigger_kind, trigger_id) on the personality return an error.";
    type Args = AddWakeEntryArgs;
    type Output = AddWakeEntryOutput;

    fn call(
        ctx: McpToolCtx,
        args: AddWakeEntryArgs,
    ) -> BoxFuture<'static, Result<AddWakeEntryOutput, McpToolError>> {
        Box::pin(async move {
            let pid = ctx
                .handles
                .resolve_personality(&args.personality)
                .ok_or_else(|| McpToolError::UnknownHandle(args.personality.clone()))?;
            let storage = ctx.storage().ok_or_else(|| {
                McpToolError::Other("engine storage unavailable".into())
            })?;
            // Resolve input now so handle errors fail fast (before tx).
            let new_draft = args.entry.into_draft(&ctx.handles, pid)?;
            let new_id = new_draft.wake_entry_id;
            let new_trigger = (new_draft.trigger_kind, new_draft.trigger_id.clone());
            let _resp = storage.set_wake_entries_within(
                &ctx.owner, pid,
                move |current| {
                    if current.iter().any(|e| {
                        (e.trigger_kind, e.trigger_id.clone()) == new_trigger
                    }) {
                        return Err(format!(
                            "wake entry with trigger ({:?}, {}) already exists",
                            new_trigger.0, new_trigger.1
                        ));
                    }
                    let mut next: Vec<_> = current.to_vec();
                    next.push(new_draft);
                    Ok(next)
                },
            ).await.map_err(|e| McpToolError::Other(e.to_string()))?;

            let after = serde_json::json!({ "wake_entry_id": new_id });
            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::AddWakeEntry,
                PersonalityConfigChangedSubject::WakeEntry(new_id),
                None,
                Some(after),
            ).await;
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => Some(reason),
            };
            let w_handle = ctx.handles.assign_wake_entry(new_id);
            Ok(AddWakeEntryOutput {
                handle: w_handle.as_str().to_string(),
                audit_emit_failed,
            })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::to_value(schemars::schema_for!(AddWakeEntryArgs))
        .expect("schema serializes"))
}
```

- [ ] **Step 2: Module export**

```rust
pub mod add_wake_entry;
pub use add_wake_entry::AddWakeEntryTool;
```

- [ ] **Step 3: Run cargo check**

Run: `cargo check -p proxima-core`
Expected: BUILD OK.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/mcp/core_tools/add_wake_entry.rs crates/core/src/mcp/core_tools/mod.rs
git commit -m "feat(mcp): core/add_wake_entry with audit (R-M-W via set_wake_entries_within)"
```

---

## Task 16: `core/update_wake_entry` tool + audit (with `WakeEntryPatch`)

**Files:**
- Create: `crates/core/src/mcp/core_tools/update_wake_entry.rs`
- Modify: `crates/core/src/mcp/core_tools/mod.rs`

- [ ] **Step 1: Define `WakeEntryPatch` and the tool**

Create `crates/core/src/mcp/core_tools/update_wake_entry.rs`:

```rust
//! `core/update_wake_entry` — granular update with a partial-fields
//! patch. trigger_kind/trigger_id are immutable; change them via
//! remove + add.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};
use crate::{ModelTier, WakeEntryAuthoredBy, WakeExecutionMode};

#[derive(Debug, Default)]
pub struct UpdateWakeEntryTool;

#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WakeEntryPatch {
    #[serde(default)] pub label: Option<String>,
    #[serde(default)] pub enabled: Option<bool>,
    #[serde(default)] pub recipe_ref: Option<String>,
    #[serde(default)] pub model_tier: Option<ModelTier>,
    #[serde(default)] pub inference_target_ref: Option<Option<String>>,
    #[serde(default)] pub substrate_tool_palette: Option<Vec<String>>,
    #[serde(default)] pub workspace_tool_palette: Option<Vec<String>>,
    #[serde(default)] #[schemars(range(min = 0, max = 1000))]
    pub probability_promille: Option<u16>,
    #[serde(default)] #[schemars(range(min = 1))]
    pub max_rounds: Option<u16>,
    #[serde(default)] pub execution_mode: Option<WakeExecutionMode>,
    #[serde(default)] pub authored_by: Option<WakeEntryAuthoredBy>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateWakeEntryArgs {
    pub wake_entry: String,
    pub patch: WakeEntryPatch,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UpdateWakeEntryOutput {
    pub handle: String,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for UpdateWakeEntryTool {
    const NAME: &'static str = "core/update_wake_entry";
    const DESCRIPTION: &'static str =
        "Update one wake entry. Only the fields present in `patch` change. \
         To change trigger_kind/trigger_id, use remove_wake_entry + add_wake_entry.";
    type Args = UpdateWakeEntryArgs;
    type Output = UpdateWakeEntryOutput;

    fn call(
        ctx: McpToolCtx,
        args: UpdateWakeEntryArgs,
    ) -> BoxFuture<'static, Result<UpdateWakeEntryOutput, McpToolError>> {
        Box::pin(async move {
            let wid = ctx
                .handles
                .resolve_wake_entry(&args.wake_entry)
                .ok_or_else(|| McpToolError::UnknownHandle(args.wake_entry.clone()))?;
            let storage = ctx.storage().ok_or_else(|| {
                McpToolError::Other("engine storage unavailable".into())
            })?;

            // Locate the personality owning this wake entry. We need
            // `pid` to call set_wake_entries_within; the only owner-
            // scoped lookup is via list_personality_instances.
            let rows = storage
                .list_personality_instances(&ctx.owner, true)
                .await
                .map_err(McpToolError::Storage)?;
            let pid = rows.iter()
                .find(|r| r.wake_entries.iter().any(|e| e.wake_entry_id == wid))
                .map(|r| r.personality_instance_id)
                .ok_or_else(|| McpToolError::Other(format!(
                    "wake entry {} not found for owner", args.wake_entry
                )))?;
            let patch = args.patch.clone();
            let before_handle = args.wake_entry.clone();
            let _ = before_handle; // for diagnostics if needed
            let _ = storage.set_wake_entries_within(
                &ctx.owner, pid,
                move |current| {
                    let mut next: Vec<_> = current.to_vec();
                    let entry = next.iter_mut()
                        .find(|e| e.wake_entry_id == wid)
                        .ok_or_else(|| format!("wake entry {wid} no longer present"))?;
                    if let Some(v) = patch.label { entry.label = v; }
                    if let Some(v) = patch.enabled { entry.enabled = v; }
                    if let Some(v) = patch.recipe_ref { entry.recipe_ref = v; }
                    if let Some(v) = patch.model_tier { entry.model_tier = v; }
                    if let Some(v) = patch.inference_target_ref {
                        entry.inference_target_ref = v;
                    }
                    if let Some(v) = patch.substrate_tool_palette {
                        entry.substrate_tool_palette = v;
                    }
                    if let Some(v) = patch.workspace_tool_palette {
                        entry.workspace_tool_palette = v;
                    }
                    if let Some(v) = patch.probability_promille {
                        entry.probability_promille = v;
                    }
                    if let Some(v) = patch.max_rounds { entry.max_rounds = v; }
                    if let Some(v) = patch.execution_mode { entry.execution_mode = v; }
                    if let Some(v) = patch.authored_by { entry.authored_by = v; }
                    Ok(next)
                },
            ).await.map_err(|e| McpToolError::Other(e.to_string()))?;

            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::UpdateWakeEntry,
                PersonalityConfigChangedSubject::WakeEntry(wid),
                Some(serde_json::json!({ "wake_entry_id": wid, "patch_applied": true })),
                Some(serde_json::Value::Null),
            ).await;
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => Some(reason),
            };
            let w_handle = ctx.handles.assign_wake_entry(wid);
            Ok(UpdateWakeEntryOutput {
                handle: w_handle.as_str().to_string(),
                audit_emit_failed,
            })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::to_value(schemars::schema_for!(UpdateWakeEntryArgs))
        .expect("schema serializes"))
}
```

- [ ] **Step 2: Module export**

```rust
pub mod update_wake_entry;
pub use update_wake_entry::UpdateWakeEntryTool;
```

- [ ] **Step 3: Run cargo check**

Run: `cargo check -p proxima-core`
Expected: BUILD OK.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/mcp/core_tools/update_wake_entry.rs crates/core/src/mcp/core_tools/mod.rs
git commit -m "feat(mcp): core/update_wake_entry with WakeEntryPatch + audit"
```

---

## Task 17: `core/remove_wake_entry` tool + audit

**Files:**
- Create: `crates/core/src/mcp/core_tools/remove_wake_entry.rs`
- Modify: `crates/core/src/mcp/core_tools/mod.rs`

- [ ] **Step 1: Write the tool**

```rust
//! `core/remove_wake_entry` — granular delete via R-M-W.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct RemoveWakeEntryTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveWakeEntryArgs {
    pub wake_entry: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RemoveWakeEntryOutput {
    pub removed: bool,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for RemoveWakeEntryTool {
    const NAME: &'static str = "core/remove_wake_entry";
    const DESCRIPTION: &'static str =
        "Remove one wake entry. Idempotent: returns removed=false if the \
         entry was already absent.";
    type Args = RemoveWakeEntryArgs;
    type Output = RemoveWakeEntryOutput;

    fn call(
        ctx: McpToolCtx,
        args: RemoveWakeEntryArgs,
    ) -> BoxFuture<'static, Result<RemoveWakeEntryOutput, McpToolError>> {
        Box::pin(async move {
            let wid = ctx
                .handles
                .resolve_wake_entry(&args.wake_entry)
                .ok_or_else(|| McpToolError::UnknownHandle(args.wake_entry.clone()))?;
            let storage = ctx.storage().ok_or_else(|| {
                McpToolError::Other("engine storage unavailable".into())
            })?;
            let rows = storage
                .list_personality_instances(&ctx.owner, true)
                .await
                .map_err(McpToolError::Storage)?;
            let Some(row) = rows.iter()
                .find(|r| r.wake_entries.iter().any(|e| e.wake_entry_id == wid))
            else {
                // Idempotent: not present anywhere -> removed=false, no audit.
                return Ok(RemoveWakeEntryOutput {
                    removed: false, audit_emit_failed: None,
                });
            };
            let pid = row.personality_instance_id;

            let _ = storage.set_wake_entries_within(
                &ctx.owner, pid,
                move |current| {
                    let next: Vec<_> = current.iter()
                        .filter(|e| e.wake_entry_id != wid)
                        .cloned()
                        .collect();
                    Ok(next)
                },
            ).await.map_err(|e| McpToolError::Other(e.to_string()))?;

            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::RemoveWakeEntry,
                PersonalityConfigChangedSubject::WakeEntry(wid),
                Some(serde_json::json!({ "wake_entry_id": wid })),
                None,
            ).await;
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => Some(reason),
            };
            Ok(RemoveWakeEntryOutput { removed: true, audit_emit_failed })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::to_value(schemars::schema_for!(RemoveWakeEntryArgs))
        .expect("schema serializes"))
}
```

- [ ] **Step 2: Module export**

```rust
pub mod remove_wake_entry;
pub use remove_wake_entry::RemoveWakeEntryTool;
```

- [ ] **Step 3: Run cargo check**

Run: `cargo check -p proxima-core`
Expected: BUILD OK.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/mcp/core_tools/remove_wake_entry.rs crates/core/src/mcp/core_tools/mod.rs
git commit -m "feat(mcp): core/remove_wake_entry with audit"
```

---

## Task 18: Inference admin write tools (`register_inference_target`, `remove_inference_target`, `bind_inference_tier`)

**Files:**
- Create: `crates/core/src/mcp/core_tools/register_inference_target.rs`
- Create: `crates/core/src/mcp/core_tools/remove_inference_target.rs`
- Create: `crates/core/src/mcp/core_tools/bind_inference_tier.rs`
- Modify: `crates/core/src/mcp/core_tools/mod.rs`

- [ ] **Step 1: Confirm engine signatures + types**

Run: `grep -nE 'fn register_inference_target|fn remove_inference_target|fn bind_inference_tier|RegisterInferenceTargetRequest|RemoveInferenceTargetRequest|BindInferenceTierRequest' crates/core/src/engine/mod.rs crates/core/src/personality/mod.rs crates/core/src/lib.rs 2>/dev/null | head -20`
Expected: locates request struct fields. Mirror them in the tool args.

- [ ] **Step 2: Write `register_inference_target.rs`**

```rust
//! `core/register_inference_target` — wraps Engine's same-name verb.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::auth::Credentials;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};
use crate::RegisterInferenceTargetRequest;

#[derive(Debug, Default)]
pub struct RegisterInferenceTargetTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RegisterInferenceTargetArgs {
    pub target_ref: String,
    /// Opaque provider config — passed through to the engine.
    pub config: serde_json::Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RegisterInferenceTargetOutput {
    pub target_ref: String,
    pub idempotent_replay: bool,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for RegisterInferenceTargetTool {
    const NAME: &'static str = "core/register_inference_target";
    const DESCRIPTION: &'static str =
        "Register an inference target. Idempotent on target_ref.";
    type Args = RegisterInferenceTargetArgs;
    type Output = RegisterInferenceTargetOutput;

    fn call(
        ctx: McpToolCtx,
        args: RegisterInferenceTargetArgs,
    ) -> BoxFuture<'static, Result<RegisterInferenceTargetOutput, McpToolError>> {
        Box::pin(async move {
            let engine = ctx.engine().ok_or_else(|| {
                McpToolError::Other("engine unavailable".into())
            })?;
            let target_ref = args.target_ref.clone();
            let config: crate::InferenceProviderConfig =
                serde_json::from_value(args.config.clone())
                    .map_err(|e| McpToolError::InvalidInput(format!("config: {e}")))?;
            let req = RegisterInferenceTargetRequest {
                owner: ctx.owner.clone(),
                target_ref: target_ref.clone(),
                config,
            };
            let resp = engine
                .register_inference_target(&Credentials::None, &req)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::RegisterInferenceTarget,
                PersonalityConfigChangedSubject::InferenceTarget(target_ref.clone()),
                None,
                Some(args.config.clone()),
            ).await;
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => Some(reason),
            };
            Ok(RegisterInferenceTargetOutput {
                target_ref: resp.target_ref,
                idempotent_replay: resp.idempotent_replay,
                audit_emit_failed,
            })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::to_value(schemars::schema_for!(RegisterInferenceTargetArgs))
        .expect("schema serializes"))
}
```

`crate::InferenceProviderConfig` — the actual engine type name may differ. Adjust to whatever the engine method takes. Run:
`grep -nE 'pub struct.*Config\|RegisterInferenceTargetRequest' crates/core/src/personality/mod.rs crates/core/src/lib.rs 2>/dev/null | head -10`

- [ ] **Step 3: Write `remove_inference_target.rs`**

```rust
//! `core/remove_inference_target` — wraps Engine's same-name verb.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::auth::Credentials;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};
use crate::RemoveInferenceTargetRequest;

#[derive(Debug, Default)]
pub struct RemoveInferenceTargetTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveInferenceTargetArgs {
    pub target_ref: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RemoveInferenceTargetOutput {
    pub removed: bool,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for RemoveInferenceTargetTool {
    const NAME: &'static str = "core/remove_inference_target";
    const DESCRIPTION: &'static str =
        "Remove an inference target by ref. Idempotent.";
    type Args = RemoveInferenceTargetArgs;
    type Output = RemoveInferenceTargetOutput;

    fn call(
        ctx: McpToolCtx,
        args: RemoveInferenceTargetArgs,
    ) -> BoxFuture<'static, Result<RemoveInferenceTargetOutput, McpToolError>> {
        Box::pin(async move {
            let engine = ctx.engine().ok_or_else(|| {
                McpToolError::Other("engine unavailable".into())
            })?;
            let target_ref = args.target_ref.clone();
            let req = RemoveInferenceTargetRequest {
                owner: ctx.owner.clone(),
                target_ref: target_ref.clone(),
            };
            let resp = engine
                .remove_inference_target(&Credentials::None, &req)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::RemoveInferenceTarget,
                PersonalityConfigChangedSubject::InferenceTarget(target_ref),
                Some(serde_json::Value::Null),
                None,
            ).await;
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => Some(reason),
            };
            Ok(RemoveInferenceTargetOutput {
                removed: resp.removed,
                audit_emit_failed,
            })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::to_value(schemars::schema_for!(RemoveInferenceTargetArgs))
        .expect("schema serializes"))
}
```

- [ ] **Step 4: Write `bind_inference_tier.rs`**

```rust
//! `core/bind_inference_tier` — wraps Engine's same-name verb.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::auth::Credentials;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};
use crate::{BindInferenceTierRequest, ModelTier};

#[derive(Debug, Default)]
pub struct BindInferenceTierTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BindInferenceTierArgs {
    pub tier: ModelTier,
    pub target_ref: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct BindInferenceTierOutput {
    pub tier: ModelTier,
    pub target_ref: String,
    pub idempotent_replay: bool,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for BindInferenceTierTool {
    const NAME: &'static str = "core/bind_inference_tier";
    const DESCRIPTION: &'static str =
        "Bind a model tier to an inference target_ref.";
    type Args = BindInferenceTierArgs;
    type Output = BindInferenceTierOutput;

    fn call(
        ctx: McpToolCtx,
        args: BindInferenceTierArgs,
    ) -> BoxFuture<'static, Result<BindInferenceTierOutput, McpToolError>> {
        Box::pin(async move {
            let engine = ctx.engine().ok_or_else(|| {
                McpToolError::Other("engine unavailable".into())
            })?;
            let req = BindInferenceTierRequest {
                owner: ctx.owner.clone(),
                tier: args.tier,
                target_ref: args.target_ref.clone(),
            };
            let resp = engine
                .bind_inference_tier(&Credentials::None, &req)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            let subject_id = format!("{:?}::{}", args.tier, args.target_ref);
            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::BindInferenceTier,
                PersonalityConfigChangedSubject::TierBinding(subject_id),
                None,
                Some(serde_json::json!({
                    "tier": format!("{:?}", args.tier),
                    "target_ref": args.target_ref,
                })),
            ).await;
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => Some(reason),
            };
            Ok(BindInferenceTierOutput {
                tier: resp.tier,
                target_ref: resp.target_ref,
                idempotent_replay: resp.idempotent_replay,
                audit_emit_failed,
            })
        })
    }
}

fn _args_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::to_value(schemars::schema_for!(BindInferenceTierArgs))
        .expect("schema serializes"))
}
```

- [ ] **Step 5: Module exports**

```rust
pub mod register_inference_target;
pub mod remove_inference_target;
pub mod bind_inference_tier;
pub use register_inference_target::RegisterInferenceTargetTool;
pub use remove_inference_target::RemoveInferenceTargetTool;
pub use bind_inference_tier::BindInferenceTierTool;
```

- [ ] **Step 6: Run cargo check**

Run: `cargo check -p proxima-core`
Expected: BUILD OK.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/mcp/core_tools/register_inference_target.rs \
        crates/core/src/mcp/core_tools/remove_inference_target.rs \
        crates/core/src/mcp/core_tools/bind_inference_tier.rs \
        crates/core/src/mcp/core_tools/mod.rs
git commit -m "feat(mcp): three inference admin write tools with audit"
```

---

## Task 19: Substrate registration in `FlavorRegistry`

**Files:**
- Modify: `crates/core/src/flavor.rs`
- Modify: `crates/core/src/mcp/core_tools/mod.rs`

- [ ] **Step 1: Add `add_substrate_mcp_tool` helper to `FlavorRegistry`**

In `crates/core/src/flavor.rs`, after the `add_mcp_tool` method, add:

```rust
/// Register a substrate-shipped MCP tool. Asserts the name starts
/// with `"core/"` (no flavor prefix). Used in `Default::default()`
/// to wire the personality-config-CRUD tools into every composite
/// binary.
pub(crate) fn add_substrate_mcp_tool<T: McpTool>(&mut self) {
    assert!(
        T::NAME.starts_with("core/"),
        "substrate McpTool::NAME {:?} must start with 'core/'",
        T::NAME,
    );
    let schema = schemars::schema_for!(T::Args);
    let args_schema = serde_json::to_value(schema).expect("JsonSchema serializes");
    let call: McpCallFn = |ctx, args| {
        Box::pin(async move {
            let typed: T::Args = serde_json::from_value(args)
                .map_err(|e| McpToolError::InvalidInput(e.to_string()))?;
            let output = T::call(ctx, typed).await?;
            serde_json::to_value(output).map_err(|e| McpToolError::InvalidInput(e.to_string()))
        })
    };
    self.mcp_tools.push(McpToolDescriptor {
        name: T::NAME,
        description: T::DESCRIPTION,
        args_schema,
        call,
    });
}
```

- [ ] **Step 2: Add a `register_all` helper in `core_tools/mod.rs`**

In `crates/core/src/mcp/core_tools/mod.rs`:

```rust
/// Register every substrate-shipped MCP tool into the FlavorRegistry.
/// Called from `FlavorRegistry::default()`.
pub(crate) fn register_all(registry: &mut crate::FlavorRegistry) {
    registry.add_substrate_mcp_tool::<ListPersonalitiesTool>();
    registry.add_substrate_mcp_tool::<GetPersonalityTool>();
    registry.add_substrate_mcp_tool::<InstantiatePersonalityTool>();
    registry.add_substrate_mcp_tool::<TombstonePersonalityTool>();
    registry.add_substrate_mcp_tool::<ListWakeEntriesTool>();
    registry.add_substrate_mcp_tool::<SetWakeEntriesTool>();
    registry.add_substrate_mcp_tool::<AddWakeEntryTool>();
    registry.add_substrate_mcp_tool::<UpdateWakeEntryTool>();
    registry.add_substrate_mcp_tool::<RemoveWakeEntryTool>();
    registry.add_substrate_mcp_tool::<ListInferenceTargetsTool>();
    registry.add_substrate_mcp_tool::<ListInferenceTierBindingsTool>();
    registry.add_substrate_mcp_tool::<RegisterInferenceTargetTool>();
    registry.add_substrate_mcp_tool::<RemoveInferenceTargetTool>();
    registry.add_substrate_mcp_tool::<BindInferenceTierTool>();
    registry.add_substrate_mcp_tool::<ListRecipesTool>();
    registry.add_substrate_mcp_tool::<ListSubstrateToolsTool>();
    registry.add_substrate_mcp_tool::<ListWorkspaceToolsTool>();
    registry.add_substrate_mcp_tool::<ListSchemasTool>();
    registry.add_substrate_mcp_tool::<ListEdgeTypesTool>();
}
```

- [ ] **Step 3: Wire into `FlavorRegistry::default()`**

In `crates/core/src/flavor.rs`, modify `Default::default()`:

```rust
impl Default for FlavorRegistry {
    fn default() -> Self {
        let mut registry = Self {
            schemas: Vec::new(),
            relations: core_relation_descriptors(),
            validators: Vec::new(),
            mcp_tools: Vec::new(),
            flavors: Vec::new(),
            bundled_recipes: Vec::new(),
            workspace_runners: Vec::new(),
        };
        registry.add_fact_schema::<crate::mcp::core_tools::PersonalityConfigChangedV1>();
        crate::mcp::core_tools::register_all(&mut registry);
        registry
    }
}
```

- [ ] **Step 4: Add a registration test**

Append to `crates/core/src/flavor.rs` `mod tests`:

```rust
#[test]
fn default_registry_includes_all_19_substrate_mcp_tools() {
    let frozen = FlavorRegistry::new().freeze();
    let names: std::collections::HashSet<_> =
        frozen.list_mcp_tools().iter().map(|d| d.name).collect();
    let expected = [
        "core/list_personalities", "core/get_personality",
        "core/instantiate_personality", "core/tombstone_personality",
        "core/list_wake_entries", "core/set_wake_entries",
        "core/add_wake_entry", "core/update_wake_entry", "core/remove_wake_entry",
        "core/list_inference_targets", "core/list_inference_tier_bindings",
        "core/register_inference_target", "core/remove_inference_target",
        "core/bind_inference_tier",
        "core/list_recipes", "core/list_substrate_tools",
        "core/list_workspace_tools", "core/list_schemas", "core/list_edge_types",
    ];
    for name in expected {
        assert!(names.contains(name), "missing tool {name}");
    }
}
```

- [ ] **Step 5: Run all core tests**

Run: `cargo test -p proxima-core --lib`
Expected: PASS — including the new registration test. Pre-existing tests calling `FlavorRegistry::new()` should still pass (registry now contains 19 tools instead of 0; no test asserts emptiness).

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/flavor.rs crates/core/src/mcp/core_tools/mod.rs
git commit -m "feat(mcp): register 19 personality-CRUD tools in FlavorRegistry::default"
```

---

## Task 20: Discovery → mutation flow integration test

**Files:**
- Create: `crates/mcp-server/tests/personality_crud_pg.rs`

- [ ] **Step 1: Write the integration test**

Create `crates/mcp-server/tests/personality_crud_pg.rs`:

```rust
//! End-to-end MCP-CRUD flow over real Postgres + the streamable-http
//! transport. Mirrors the shape of `streamable_http_pg.rs`. Asserts an
//! LLM with no prior config knowledge can author a working wake entry
//! using only the discovery tools' output.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use proxima_core::wake::token_store::WakeTokenStore;
use proxima_core::{FlavorRegistry, OrgId, Owner, Principal, UserId};
use proxima_mcp_server::{McpToolHost, McpAuthStore, default_allowlist, serve_streamable_http};
use serde_json::{Value, json};
use sqlx::{Connection, Executor, PgConnection};

const ADMIN_URL: &str = "postgres://postgres@localhost/postgres";

#[tokio::test]
async fn discovery_to_mutation_smoke() -> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else { return Ok(()) };
    let database_url = format!("postgres://postgres@localhost/{db_name}");
    let owner = Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
        org_id: OrgId::new(uuid::Uuid::now_v7()),
    };
    let registry = FlavorRegistry::new();
    let server = McpToolHost::from_database_url(&database_url, owner.clone(), registry).await?;
    let store = Arc::new(WakeTokenStore::new(Duration::from_mins(5)));
    let auth_store = Arc::new(McpAuthStore::new(store));
    let master_token = uuid::Uuid::now_v7();
    auth_store.replace_local_master_token(master_token, owner.clone()).await;

    let (handle, addr) = serve_streamable_http(
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
        server, default_allowlist(), auth_store,
    ).await?;

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let bearer = format!("Bearer {master_token}");
    let session = initialize(&client, &url, &bearer).await?;
    initialized(&client, &url, &session, &bearer).await?;

    // 1. Discovery: list_recipes (will be empty in this fresh DB,
    //    but the call itself must succeed).
    let _ = call_tool(&client, &url, &session, &bearer,
        "core/list_recipes", json!({})).await?;

    // 2. Discovery: list_substrate_tools includes the substrate pack
    //    plus the substrate-registered MCP tools.
    let tools = call_tool(&client, &url, &session, &bearer,
        "core/list_substrate_tools", json!({})).await?;
    let arr = tools["tools"].as_array().expect("tools array");
    let names: std::collections::HashSet<_> = arr.iter()
        .map(|t| t["tool_id"].as_str().unwrap().to_string()).collect();
    assert!(names.contains("core/fetch_memory"), "substrate pack tool present");
    assert!(names.contains("core/list_personalities"), "MCP CRUD tool present");

    // 3. Mutation: instantiate_personality.
    let inst = call_tool(&client, &url, &session, &bearer,
        "core/instantiate_personality",
        json!({"display_name": "TestSubject", "purpose": "smoke test"})).await?;
    let p_handle = inst["handle"].as_str().expect("P handle").to_string();
    assert!(p_handle.starts_with('P'), "P-prefixed handle, got {p_handle}");

    // 4. Read-after-write: list_personalities returns it.
    let list = call_tool(&client, &url, &session, &bearer,
        "core/list_personalities", json!({})).await?;
    let items = list["personalities"].as_array().expect("array");
    assert!(items.iter().any(|p| {
        p["display_name"].as_str() == Some("TestSubject")
    }));

    // 5. Tombstone, idempotent.
    let t1 = call_tool(&client, &url, &session, &bearer,
        "core/tombstone_personality", json!({"personality": p_handle})).await?;
    assert_eq!(t1["idempotent_replay"], json!(false));
    let t2 = call_tool(&client, &url, &session, &bearer,
        "core/tombstone_personality", json!({"personality": p_handle})).await?;
    assert_eq!(t2["idempotent_replay"], json!(true));

    handle.abort();
    let _ = handle.await;
    drop_db(&db_name).await?;
    Ok(())
}

async fn create_db() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let mut admin = match PgConnection::connect(ADMIN_URL).await {
        Ok(c) => c, Err(_) => return Ok(None),
    };
    let name = format!("proxima_test_{}", uuid::Uuid::now_v7().simple());
    admin.execute(format!("CREATE DATABASE {name}").as_str()).await?;
    Ok(Some(name))
}

async fn drop_db(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut admin = PgConnection::connect(ADMIN_URL).await?;
    admin.execute(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)").as_str()).await?;
    Ok(())
}

// Helpers `initialize`, `initialized`, `post_rpc`, `call_tool`:
// copy from crates/mcp-server/tests/streamable_http_pg.rs verbatim
// (they are private helpers in that test file; either move them to a
// shared `tests/common.rs` module or duplicate here — the test crate
// is small enough that duplication is acceptable for v1).
async fn initialize(client: &reqwest::Client, url: &str, bearer: &str) -> Result<String, Box<dyn std::error::Error>> {
    todo!("copy from streamable_http_pg.rs initialize helper")
}
async fn initialized(client: &reqwest::Client, url: &str, session: &str, bearer: &str) -> Result<(), Box<dyn std::error::Error>> {
    todo!("copy from streamable_http_pg.rs initialized helper")
}
async fn call_tool(
    client: &reqwest::Client, url: &str, session: &str, bearer: &str,
    name: &str, args: Value,
) -> Result<Value, Box<dyn std::error::Error>> {
    todo!("copy from streamable_http_pg.rs post_rpc + extract content[0].text JSON")
}
```

- [ ] **Step 2: Copy or share the test helpers**

Replace each `todo!()` body with the corresponding helper from
`crates/mcp-server/tests/streamable_http_pg.rs`. Either:

(a) Copy the helper bodies directly (acceptable for v1 — small test crate).
(b) Move them to `crates/mcp-server/tests/common/mod.rs` (preferred for long-term maintenance).

If choosing (b), create the common module and include it in both test files via `mod common;`.

- [ ] **Step 3: Run the integration test**

Run: `cargo test -p proxima-mcp-server --test personality_crud_pg`
Expected: PASS (or skip with no Postgres).

- [ ] **Step 4: Commit**

```bash
git add crates/mcp-server/tests/personality_crud_pg.rs
git commit -m "test(mcp): discovery -> mutation flow integration over Postgres"
```

---

## Task 21: E2E self-evolution smoke test

**Files:**
- Create: `crates/mcp-server/tests/personality_crud_e2e_pg.rs`

This task is the high-bar smoke test described in the spec: a wake invocation in personality A calls `core/add_wake_entry` on its own `PersonalityInstanceId`, the next dispatch tick picks up the new entry, and firing it produces the expected output.

- [ ] **Step 1: Sketch the test scaffolding**

Create `crates/mcp-server/tests/personality_crud_e2e_pg.rs`:

```rust
//! E2E self-evolution: a wake-token-bearing client (simulating a
//! goose-recipe wake) calls core/add_wake_entry on its own personality;
//! a subsequent dispatcher tick picks up the new entry; the change
//! persists.
//!
//! v1 shape: simulate the wake-token by minting one against the test
//! personality and using it as the MCP bearer token. Skip the full
//! goose-loop — the engine integration is exercised by Task 20's
//! mutation flow already; this test focuses on the
//! `caller_self_perspective` → audit-Fact-provenance path.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use proxima_core::wake::token_store::{WakeTokenContext, WakeTokenStore};
use proxima_core::{
    FlavorRegistry, InstantiatePersonalityRequest, OrgId, Owner, Principal,
    UserId,
};
use proxima_mcp_server::{McpToolHost, McpAuthStore, default_allowlist, serve_streamable_http};
use serde_json::json;
use sqlx::{Connection, Executor, PgConnection};

const ADMIN_URL: &str = "postgres://postgres@localhost/postgres";

#[tokio::test]
async fn wake_token_caller_audit_fact_provenance_walks_to_self_root()
    -> Result<(), Box<dyn std::error::Error>>
{
    let Some(db_name) = create_db().await? else { return Ok(()) };
    // Setup: make engine, instantiate a personality, mint a wake token
    // that points at it, install the token, drive an
    // add_wake_entry call, query the audit Fact, walk its provenance,
    // assert it lands on the personality's Root Perspective.
    todo!("Implement once Engine::start hooks are concrete enough; \
           for v1 this can be a unit-style test that constructs ctx \
           directly + calls AddWakeEntryTool::call without HTTP.")
}

async fn create_db() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let mut admin = match PgConnection::connect(ADMIN_URL).await {
        Ok(c) => c, Err(_) => return Ok(None),
    };
    let name = format!("proxima_test_{}", uuid::Uuid::now_v7().simple());
    admin.execute(format!("CREATE DATABASE {name}").as_str()).await?;
    Ok(Some(name))
}

async fn drop_db(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut admin = PgConnection::connect(ADMIN_URL).await?;
    admin.execute(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)").as_str()).await?;
    Ok(())
}
```

- [ ] **Step 2: Implement the test body**

Replace the `todo!()` with concrete steps:
1. Build a `McpToolHost.with_engine(engine)`.
2. Instantiate a personality via the engine API.
3. Mint a `WakeTokenContext` whose `current_root_perspective_memory_id` matches the personality's Root.
4. Construct an `McpToolCtx` with `caller_self_perspective: Some(root)`.
5. Call `AddWakeEntryTool::call(ctx, args)`.
6. Query Postgres directly for the resulting `core/personality_config_changed_v1` Fact memory:
   ```sql
   SELECT memory_id, payload_json
   FROM proxima_core.fact_memory
   WHERE schema_id = 'core/personality_config_changed_v1'
   ORDER BY observed_at DESC LIMIT 1
   ```
7. Assert the payload's `caller.kind == "wake_personality"` and `caller.personality_instance_id` matches the test personality.

If the storage projection of payload_json is non-trivial (CBOR-encoded), use `engine.query()` or whatever read-path verb is canonical to fetch + decode the payload.

- [ ] **Step 3: Run the test**

Run: `cargo test -p proxima-mcp-server --test personality_crud_e2e_pg`
Expected: PASS (or skip with no Postgres).

- [ ] **Step 4: Commit**

```bash
git add crates/mcp-server/tests/personality_crud_e2e_pg.rs
git commit -m "test(mcp): E2E self-evolution audit-Fact provenance"
```

---

## Self-Review

**Spec coverage:**
- ✅ Handle layer extension — Task 1.
- ✅ Audit Fact memory schema — Task 2.
- ✅ `proxima/shell-author` substrate personality — Task 3.
- ✅ Audit-emit helper — Task 4.
- ✅ McpToolCtx engine access — Task 5.
- ✅ Personality CRUD tools — Tasks 6, 7, 11, 12.
- ✅ WakeEntry CRUD tools — Tasks 8, 14, 15, 16, 17.
- ✅ Inference admin tools — Tasks 9, 18.
- ✅ Discovery surface — Task 10.
- ✅ Storage R-M-W primitive — Task 13.
- ✅ Substrate registration — Task 19.
- ✅ Discovery → mutation integration test — Task 20.
- ✅ E2E self-evolution test — Task 21.
- ✅ Auth/scopes — handled implicitly via existing `McpToolScope::Palette` mechanism, surfaced through wake-config palette declarations + the master-token `All` scope. No new enum changes needed; documented in the spec but the implementation reuses the existing scope shape.

**Type consistency:**
- `WakeEntryDraftInput` (Task 14) → consumed by Tasks 14, 15, 16, 17.
- `WakeEntryPatch` (Task 16) → consumed only by Task 16 (its scope is intentionally limited).
- `PersonalityConfigChangedV1` (Task 2) → consumed by audit helper (Task 4) and indirectly by every mutation tool.
- `Engine` accessor names (`storage()`, `instantiate_personality(creds, req)`, etc.) — flagged in each task as "verify against the actual signature; adjust if names differ." This keeps the plan honest about the engine surface I haven't directly inspected line-by-line.

**Open implementation choices flagged in the plan (not blockers, but the implementer needs to resolve):**
1. The exact `FactPayload` trait shape (Task 2 Step 4) — the implementer reads the trait def and adjusts the `impl FactPayload` body.
2. `Engine::owner_recipes_root()` accessor name (Task 10) — confirm exact name.
3. `Engine` verb signatures with vs. without `Credentials` arg (Tasks 11, 12, 18) — verify and adapt.
4. `Storage::set_wake_entries_within` lifetime/Send bounds on the closure parameter (Task 13) — may need to narrow to a concrete enum if generic FnOnce in a trait is awkward.
5. `read_wake_entries_in_tx` SQL body (Task 13) — mirror `list_personality_instances`'s SQL for the wake-entry projection columns.
6. Test helpers in Task 20/21 — copy from `streamable_http_pg.rs` or share via `tests/common/mod.rs`.

These are all "read the existing code and adapt" choices that don't change the plan's structure.

---

Plan complete and saved to `docs/superpowers/plans/2026-05-10-personality-mcp-crud.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
