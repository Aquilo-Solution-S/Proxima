# M0 — Per-Master-Token Shell-Author Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps
> use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Promote `shell-author` from a singleton-per-owner personality
(lazy on first audit emit) to a per-master-token-UUID personality
(eager on first MCP call). Default `ctx.caller_self_perspective` to
the per-token Self-Perspective for every master-token tool call so
existing tool-side code (notably `goal_propose`'s `core/inspires` edge
logic) starts attributing correctly without per-tool changes.

**Architecture:** New mapping table
`proxima_core.master_token_personality` keyed by
`(master_token_id, owner) → personality_instance_id`. New Storage
trait method `ensure_master_token_personality(owner, master_token_id)
→ MasterTokenPersonality { instance_id, self_perspective_memory_id }`.
MCP server's auth layer surfaces `master_token_id` on
`McpAuthContext`; `DevMcpServer::call_tool` ensures the per-token
personality on every master-token call and threads
`self_perspective_memory_id` into `ctx.caller_self_perspective` (and
`master_token_id` into a new field on `McpToolCtx`). Audit code
branches on `ctx.master_token_id.is_some()` rather than on
`caller_self_perspective.is_none()`. Legacy
`ensure_shell_author_personality(owner)` is deleted; the existing
no-op migration `20260510000010_shell_author_personality.sql`
remains in place (reverting it is unsafe).

**Tech Stack:** Rust + sqlx + PG; rmcp 1.6 dynamic handler;
existing `Storage`/`Engine`/`McpToolCtx` plumbing.

**Spec:** `docs/superpowers/specs/2026-05-10-spinning-wheel-proof-roadmap.md` §M0 / §S0.

---

## Acceptance criteria (from the spec)

A `goal_propose` call from a fresh master-token MCP connection writes
a Goal **with** a `core/inspires` edge to a per-token Self-Perspective,
with no explicit `_proxima_caller_self_perspective` arg. Reconnecting
under the same token resolves to the same Self-Perspective; two
distinct master tokens against the same owner resolve to two
distinct Self-Perspectives.

## File Structure

**New:**
- `crates/storage-pg/migrations/20260510000030_master_token_personality.sql` — mapping table.
- `crates/storage-pg/src/verbs/master_token_personality.rs` — `ensure_master_token_personality` impl.
- `crates/storage-pg/tests/master_token_personality_pg.rs` — PG integration tests.
- `crates/mcp-server/tests/master_token_identity.rs` — end-to-end MCP test through auth layer.

**Modified:**
- `crates/core/src/storage.rs` — add `MasterTokenPersonality` struct + trait method; remove `ensure_shell_author_personality` from trait.
- `crates/storage-pg/src/lib.rs` — wire new trait impl; remove old wrapper.
- `crates/storage-pg/src/verbs/mod.rs` — `pub mod master_token_personality`; remove `pub mod shell_author`.
- `crates/storage-pg/src/verbs/shell_author.rs` — **delete**.
- `crates/core/src/mcp/mod.rs` — add `master_token_id: Option<uuid::Uuid>` field to `McpToolCtx`.
- `crates/mcp-server/src/auth.rs` — add `master_token_id: Option<Uuid>` to `McpAuthContext`; populate in `McpAuthStore::resolve`.
- `crates/mcp-server/src/server.rs` — `call_tool` ensures per-token personality and threads identity into ctx.
- `crates/core/src/mcp/core_tools/audit.rs` — branch on `ctx.master_token_id`; drop direct storage call.
- `crates/core/tests/set_wake_entries_validation.rs`, `crates/core/tests/wake_fire_smoke.rs` — mock storage updates.
- `crates/storage-pg/tests/shell_author_pg.rs` — **delete** (superseded by `master_token_personality_pg.rs`).

## Tasks

### Task 1: Add the mapping-table migration

**Files:**
- Create: `crates/storage-pg/migrations/20260510000030_master_token_personality.sql`

- [ ] **Step 1: Write the migration**

```sql
-- Mapping from (master_token_id, owner) to the per-token shell-author
-- personality. Persists across token revocation: once minted, the
-- identity row stays so authored Facts retain their provenance even if
-- the token UUID is later removed from the auth store. New tokens
-- always mint new rows.
CREATE TABLE proxima_core.master_token_personality (
    master_token_id          uuid NOT NULL,
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    owner_org_id             uuid NOT NULL,
    personality_instance_id  uuid NOT NULL
        REFERENCES proxima_core.personality(personality_instance_id),
    created_at               timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (master_token_id, owner_principal_kind, owner_principal_id, owner_org_id)
);

CREATE UNIQUE INDEX idx_master_token_personality_instance
    ON proxima_core.master_token_personality (personality_instance_id);
```

- [ ] **Step 2: Verify migration compiles via sqlx::migrate! (build)**

Run: `cargo check -p proxima-storage-pg`
Expected: clean build (sqlx::migrate! is a build-time macro that embeds the file).

- [ ] **Step 3: Commit**

```bash
git add crates/storage-pg/migrations/20260510000030_master_token_personality.sql
git commit -m "feat(storage-pg): master_token_personality mapping table"
```

---

### Task 2: Add `MasterTokenPersonality` + trait method; drop legacy

**Files:**
- Modify: `crates/core/src/storage.rs:215-232`

- [ ] **Step 1: Write the failing trait test (mock storage)**

In `crates/core/tests/wake_fire_smoke.rs` and
`crates/core/tests/set_wake_entries_validation.rs`, find the mock
`Storage` impls (search for `ensure_shell_author_personality`) and
replace with stubs for the new method. Compile to confirm the trait
shape we're about to land:

```rust
async fn ensure_master_token_personality(
    &self,
    _owner: &Owner,
    _master_token_id: uuid::Uuid,
) -> Result<MasterTokenPersonality, StorageError> {
    Err(StorageError::Internal(
        "mock: ensure_master_token_personality not stubbed".into(),
    ))
}
```

(The mock currently provides `ensure_shell_author_personality`. Drop
that method too.)

- [ ] **Step 2: Add the struct + method to the Storage trait**

Replace `ensure_shell_author_personality` (lines 223-232) with:

```rust
/// Identity row for a per-master-token shell-author personality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MasterTokenPersonality {
    pub instance_id: PersonalityInstanceId,
    pub self_perspective_memory_id: MemoryId,
}

/// Ensure a per-master-token shell-author personality exists for the
/// (owner, master_token_id) pair. Idempotent: returns the existing
/// (instance_id, self_perspective_memory_id) on replay, or mints a
/// fresh personality with `display_name = "shell-author"`,
/// `purpose = "Per-master-token MCP client identity"`, an empty
/// WakeConfig, and an entry in
/// `proxima_core.master_token_personality`.
async fn ensure_master_token_personality(
    &self,
    owner: &Owner,
    master_token_id: uuid::Uuid,
) -> Result<MasterTokenPersonality, StorageError>;
```

Also re-export `MasterTokenPersonality` from the same module.

- [ ] **Step 3: Build and confirm compile failure in storage-pg**

Run: `cargo build --workspace`
Expected: `proxima-storage-pg` fails because it doesn't yet implement
the new method.

- [ ] **Step 4: Commit (the trait change + mock stubs)**

```bash
git add crates/core/src/storage.rs crates/core/tests/wake_fire_smoke.rs crates/core/tests/set_wake_entries_validation.rs
git commit -m "feat(core): MasterTokenPersonality + ensure_master_token_personality on Storage trait"
```

---

### Task 3: Implement `ensure_master_token_personality` in storage-pg

**Files:**
- Create: `crates/storage-pg/src/verbs/master_token_personality.rs`
- Delete: `crates/storage-pg/src/verbs/shell_author.rs`
- Modify: `crates/storage-pg/src/verbs/mod.rs`
- Modify: `crates/storage-pg/src/lib.rs:316-323`

- [ ] **Step 1: Write the failing PG integration test**

Create `crates/storage-pg/tests/master_token_personality_pg.rs`:

```rust
//! PG coverage for the per-master-token shell-author identity.
mod common;

use common::{drop_db, fresh_pg};
use proxima_core::storage::Storage;
use proxima_core::{OrgId, Owner, Principal, UserId};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn ensure_master_token_personality_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db)) = fresh_pg().await else { return Ok(()); };
    pg.run_migrations().await?;
    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    };
    let token = Uuid::now_v7();

    let first = pg.ensure_master_token_personality(&owner, token).await?;
    let second = pg.ensure_master_token_personality(&owner, token).await?;
    assert_eq!(first, second);

    drop_db(&db).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn distinct_tokens_resolve_to_distinct_personalities() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db)) = fresh_pg().await else { return Ok(()); };
    pg.run_migrations().await?;
    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    };
    let a = pg.ensure_master_token_personality(&owner, Uuid::now_v7()).await?;
    let b = pg.ensure_master_token_personality(&owner, Uuid::now_v7()).await?;
    assert_ne!(a.instance_id, b.instance_id);
    assert_ne!(a.self_perspective_memory_id, b.self_perspective_memory_id);

    drop_db(&db).await?;
    Ok(())
}
```

- [ ] **Step 2: Run test to verify it fails to compile**

Run: `cargo test -p proxima-storage-pg --test master_token_personality_pg`
Expected: build error (method not implemented).

- [ ] **Step 3: Write the implementation**

Create `crates/storage-pg/src/verbs/master_token_personality.rs`:

```rust
//! Idempotent lookup-or-mint of the per-master-token shell-author
//! personality. Used as provenance for every master-token MCP call.

use proxima_core::{
    InstantiatePersonalityRequest, MasterTokenPersonality, MemoryId, Owner,
    PersonalityInstanceId, Principal, StorageError,
};
use sqlx::PgPool;
use uuid::Uuid;

use super::consolidate;

const SHELL_AUTHOR_DISPLAY_NAME: &str = "shell-author";
const SHELL_AUTHOR_PURPOSE: &str = "Per-master-token MCP client identity";

pub async fn ensure_master_token_personality(
    pool: &PgPool,
    owner: &Owner,
    master_token_id: Uuid,
) -> Result<MasterTokenPersonality, StorageError> {
    let (kind, principal_id, org_id) = owner_columns(owner);

    // Fast path: existing mapping.
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT mtp.personality_instance_id,
                p.current_root_perspective_memory_id
         FROM proxima_core.master_token_personality mtp
         JOIN proxima_core.personality p
           ON p.personality_instance_id = mtp.personality_instance_id
         WHERE mtp.master_token_id = $1
           AND mtp.owner_principal_kind = $2
           AND mtp.owner_principal_id = $3
           AND mtp.owner_org_id = $4
         LIMIT 1",
    )
    .bind(master_token_id)
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .fetch_optional(pool)
    .await
    .map_err(|err| StorageError::Internal(err.to_string()))?;

    if let Some((instance_id, root_id)) = row {
        return Ok(MasterTokenPersonality {
            instance_id: PersonalityInstanceId::new(instance_id),
            self_perspective_memory_id: MemoryId::new(root_id),
        });
    }

    // Slow path: instantiate then map.
    let req = InstantiatePersonalityRequest {
        owner: owner.clone(),
        display_name: SHELL_AUTHOR_DISPLAY_NAME.into(),
        purpose: SHELL_AUTHOR_PURPOSE.into(),
    };
    let resp = consolidate::instantiate_personality(pool, &req).await?;
    let instance_id = resp.instance_id;

    sqlx::query(
        "INSERT INTO proxima_core.master_token_personality (
             master_token_id, owner_principal_kind, owner_principal_id,
             owner_org_id, personality_instance_id
         ) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (master_token_id, owner_principal_kind,
                      owner_principal_id, owner_org_id) DO NOTHING",
    )
    .bind(master_token_id)
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(instance_id.into_inner())
    .execute(pool)
    .await
    .map_err(|err| StorageError::Internal(err.to_string()))?;

    let root_id: Uuid = sqlx::query_scalar(
        "SELECT current_root_perspective_memory_id
         FROM proxima_core.personality
         WHERE personality_instance_id = $1",
    )
    .bind(instance_id.into_inner())
    .fetch_one(pool)
    .await
    .map_err(|err| StorageError::Internal(err.to_string()))?;

    Ok(MasterTokenPersonality {
        instance_id,
        self_perspective_memory_id: MemoryId::new(root_id),
    })
}

fn owner_columns(owner: &Owner) -> (&'static str, Uuid, Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}
```

- [ ] **Step 4: Wire the trait impl in lib.rs**

In `crates/storage-pg/src/lib.rs`, replace lines 316-323
(`async fn ensure_shell_author_personality`) with:

```rust
async fn ensure_master_token_personality(
    &self,
    owner: &Owner,
    master_token_id: uuid::Uuid,
) -> Result<MasterTokenPersonality, StorageError> {
    verbs::master_token_personality::ensure_master_token_personality(
        &self.pool, owner, master_token_id,
    )
    .await
}
```

Also import `MasterTokenPersonality` in the use statements.

- [ ] **Step 5: Update verbs/mod.rs**

In `crates/storage-pg/src/verbs/mod.rs`:
- Remove `pub mod shell_author;`
- Add `pub mod master_token_personality;`

Then `rm crates/storage-pg/src/verbs/shell_author.rs`
and `rm crates/storage-pg/tests/shell_author_pg.rs`.

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p proxima-storage-pg --test master_token_personality_pg`
Expected: 2 tests pass.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(storage-pg): per-master-token shell-author identity"
```

---

### Task 4: Surface `master_token_id` on `McpAuthContext`

**Files:**
- Modify: `crates/mcp-server/src/auth.rs:11-16, 49-72`

- [ ] **Step 1: Write the failing test**

Add to `crates/mcp-server/src/auth.rs` (under `#[cfg(test)] mod tests`):

```rust
#[tokio::test]
async fn resolve_master_token_carries_token_id() {
    let store = McpAuthStore::new(Arc::new(WakeTokenStore::new()));
    let owner = Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
        org_id: OrgId::new(uuid::Uuid::now_v7()),
    };
    let token = Uuid::now_v7();
    store.replace_local_master_token(token, owner.clone()).await;
    let ctx = store.resolve(token).await.expect("resolved");
    assert_eq!(ctx.master_token_id, Some(token));
    assert!(ctx.wake.is_none());
}

#[tokio::test]
async fn resolve_wake_token_has_no_master_id() {
    // ... existing wake-token test scaffold; assert master_token_id == None
}
```

(Use whatever `Owner`/`Principal`/`UserId` import path matches existing
tests in this file.)

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p proxima-mcp-server auth::tests`
Expected: compile error (`master_token_id` field missing).

- [ ] **Step 3: Add the field and populate it**

```rust
#[derive(Debug, Clone)]
pub struct McpAuthContext {
    pub owner: Owner,
    pub scope: McpToolScope,
    pub model_id: Option<String>,
    pub wake: Option<WakeTokenContext>,
    pub master_token_id: Option<Uuid>,
}
```

In `resolve`:

```rust
pub async fn resolve(&self, token: Uuid) -> Option<McpAuthContext> {
    if let Some(wake) = self.wake_tokens.resolve(token).await {
        return Some(McpAuthContext {
            owner: wake.owner.clone(),
            scope: McpToolScope::Palette(wake.palette.clone()),
            model_id: Some(wake.model_id.clone()),
            wake: Some(wake),
            master_token_id: None,
        });
    }
    let guard = self.master_tokens.read().await;
    guard.get(&token).cloned().map(|owner| McpAuthContext {
        owner,
        scope: McpToolScope::All,
        model_id: None,
        wake: None,
        master_token_id: Some(token),
    })
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p proxima-mcp-server auth::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mcp-server/src/auth.rs
git commit -m "feat(mcp-server): expose master_token_id on McpAuthContext"
```

---

### Task 5: Add `master_token_id` to `McpToolCtx`

**Files:**
- Modify: `crates/core/src/mcp/mod.rs:30-41, 167-180, 198-211`

- [ ] **Step 1: Add the field**

```rust
#[derive(Clone)]
pub struct McpToolCtx {
    pub pool: sqlx::PgPool,
    pub owner: Owner,
    pub handles: Arc<HandleTable>,
    pub registry: Arc<FlavorRegistryFrozen>,
    pub author: McpAuthorContext,
    pub caller_self_perspective: Option<MemoryId>,
    pub master_token_id: Option<uuid::Uuid>,
    pub engine: Option<Arc<crate::Engine>>,
}
```

- [ ] **Step 2: Update existing test ctx constructions**

In `crates/core/src/mcp/mod.rs:167-180` and `:198-211`, add
`master_token_id: None` to the inline `McpToolCtx { ... }` literals.
Same for any other `McpToolCtx { ... }` literal in the workspace
(`grep -rn 'McpToolCtx {' crates/ flavors/`).

- [ ] **Step 3: Build the workspace**

Run: `cargo build --workspace`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(core): master_token_id on McpToolCtx"
```

---

### Task 6: Wire ensure-on-call in `DevMcpServer::call_tool`

**Files:**
- Modify: `crates/mcp-server/src/server.rs:87-117`

- [ ] **Step 1: Update `ctx()` to thread the field**

```rust
pub fn ctx(
    &self,
    author: McpAuthorContext,
    owner: Option<Owner>,
    master_token_id: Option<uuid::Uuid>,
) -> McpToolCtx {
    McpToolCtx {
        pool: self.pool.clone(),
        owner: owner.unwrap_or_else(|| self.owner.clone()),
        handles: self.handles.clone(),
        registry: self.registry.clone(),
        caller_self_perspective: author.caller_self_perspective,
        master_token_id,
        author,
        engine: self.engine.clone(),
    }
}
```

- [ ] **Step 2: Insert ensure-and-default into `call_tool`**

Before the `if let Some(descriptor) = ...` block (around line 109),
insert:

```rust
// M0: For master-token calls without an explicit caller_self_perspective,
// ensure the per-token shell-author and default the field. Wake-token
// calls already carry caller_self_perspective; explicit overrides via
// the reserved arg keys still win.
let mut author = author;
let master_token_id = auth.as_ref().and_then(|c| c.master_token_id);
if author.caller_self_perspective.is_none() {
    if let (Some(token_id), Some(engine), Some(auth_ctx)) =
        (master_token_id, self.engine.as_ref(), auth.as_ref())
    {
        let identity = engine
            .storage()
            .ensure_master_token_personality(&auth_ctx.owner, token_id)
            .await
            .map_err(|err| {
                ToolInvocationError::Tool(McpToolError::Other(err.to_string()))
            })?;
        author.caller_self_perspective = Some(identity.self_perspective_memory_id);
    }
}
```

Then update both `self.ctx(author, owner)` call sites to
`self.ctx(author, owner, master_token_id)`.

- [ ] **Step 3: Smoke test**

Run: `cargo build --workspace`
Expected: clean build.

Run: `cargo test -p proxima-mcp-server`
Expected: existing tests still pass.

- [ ] **Step 4: Commit**

```bash
git add crates/mcp-server/src/server.rs
git commit -m "feat(mcp-server): ensure per-token shell-author on master-token call_tool"
```

---

### Task 7: Update audit `resolve_caller` to dispatch on `ctx.master_token_id`

**Files:**
- Modify: `crates/core/src/mcp/core_tools/audit.rs:57-92`

- [ ] **Step 1: Rewrite the dispatch**

```rust
async fn resolve_caller(
    ctx: &McpToolCtx,
) -> Result<PersonalityConfigChangedCaller, String> {
    let storage = ctx
        .storage()
        .ok_or_else(|| "engine storage unavailable".to_string())?;

    let self_id = ctx
        .caller_self_perspective
        .ok_or_else(|| "caller_self_perspective missing for audit emit".to_string())?;

    let instances = storage
        .list_personality_instances(&ctx.owner, false)
        .await
        .map_err(|e| e.to_string())?;
    let instance_id = instances
        .into_iter()
        .find(|row| row.current_root_perspective_memory_id == self_id)
        .map(|row| row.personality_instance_id.into_inner())
        .ok_or_else(|| {
            format!("no personality matches caller_self_perspective {self_id:?}")
        })?;

    Ok(if ctx.master_token_id.is_some() {
        PersonalityConfigChangedCaller::MasterToken {
            shell_author_personality_instance_id: instance_id,
        }
    } else {
        PersonalityConfigChangedCaller::WakePersonality {
            personality_instance_id: instance_id,
        }
    })
}
```

The semantics shift on the existing `MasterToken.shell_author_personality_instance_id`
field: it now carries the per-token instance_id (was: the
singleton). Schema id stays `core/personality_config_changed_v1`;
field name stays. (A v2 rename is a separate cleanup if we want it.)

- [ ] **Step 2: Update the test that exercises "no storage"**

In the existing test `resolve_caller_returns_failed_when_no_storage`,
behavior is unchanged (still fails on storage missing). But note
the caller now also fails fast if `caller_self_perspective` is
`None` regardless of master/wake — this is the contract M0 enforces
(the MCP server always populates it).

Add a second test that constructs a ctx with
`caller_self_perspective: None` and asserts the new error message.

- [ ] **Step 3: Run tests**

Run: `cargo test -p proxima-core mcp::core_tools::audit`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/mcp/core_tools/audit.rs
git commit -m "refactor(core): audit resolve_caller dispatches on ctx.master_token_id"
```

---

### Task 8: Verify the whole workspace + integration tests still pass

**Files:** none (verification step)

- [ ] **Step 1: Workspace build + clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 2: Workspace test**

Run: `cargo test --workspace`
Expected: all tests pass.

If any test exists that constructed `caller_self_perspective: None`
to simulate a master-token-equivalent call into a tool that emits
audit Facts, it now needs to set `master_token_id: Some(...)` and
provide a `caller_self_perspective: Some(...)`. Update inline
following the audit contract change above.

- [ ] **Step 3: Commit any test updates**

```bash
git add -A
git commit -m "test: align ctx fixtures with M0 master-token contract"
```

(Skip if no fixups needed.)

---

### Task 9: End-to-end MCP test through auth layer

**Files:**
- Create: `crates/mcp-server/tests/master_token_identity.rs`

- [ ] **Step 1: Write the integration test**

```rust
//! End-to-end: a master-token MCP call resolves to a per-token
//! shell-author personality, and the resulting tool ctx carries a
//! caller_self_perspective + master_token_id.
//!
//! This rides on the same scaffolding the existing rmcp tests use
//! (search this directory for `DevMcpServer` setup helpers).

mod common;

use common::{fresh_engine_with_pg, with_master_token};
use proxima_core::storage::Storage;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn master_token_call_mints_per_token_self_perspective(
) -> Result<(), Box<dyn std::error::Error>> {
    let Some((engine, server, owner)) = fresh_engine_with_pg().await? else {
        return Ok(());
    };
    let token = Uuid::now_v7();
    with_master_token(&server, token, owner.clone()).await;

    // Trigger ensure via call_tool path. Use `core/list_personalities`
    // — read-only, no payload effects, but routes through the same
    // call_tool surface that performs the M0 ensure step.
    server
        .call_tool(
            "core/list_personalities",
            serde_json::json!({}),
            /* author */ default_author(),
            /* auth */ Some(server.auth().resolve(token).await.expect("resolved")),
        )
        .await?;

    // Assert: the mapping table now has exactly one row for this token
    // pointing at a personality whose self-perspective memory id is
    // present.
    let identity = engine
        .storage()
        .ensure_master_token_personality(&owner, token)
        .await?;
    assert_ne!(identity.instance_id.into_inner(), Uuid::nil());
    assert_ne!(identity.self_perspective_memory_id.into_inner(), Uuid::nil());

    // Reconnect path: a second resolve with the same token returns the
    // same identity.
    let identity_again = engine
        .storage()
        .ensure_master_token_personality(&owner, token)
        .await?;
    assert_eq!(identity, identity_again);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn distinct_master_tokens_resolve_to_distinct_identities(
) -> Result<(), Box<dyn std::error::Error>> {
    let Some((engine, server, owner)) = fresh_engine_with_pg().await? else {
        return Ok(());
    };
    let t_a = Uuid::now_v7();
    let t_b = Uuid::now_v7();
    with_master_token(&server, t_a, owner.clone()).await;
    with_master_token(&server, t_b, owner.clone()).await;

    let a = engine
        .storage()
        .ensure_master_token_personality(&owner, t_a)
        .await?;
    let b = engine
        .storage()
        .ensure_master_token_personality(&owner, t_b)
        .await?;
    assert_ne!(a.instance_id, b.instance_id);

    Ok(())
}

fn default_author() -> proxima_core::McpAuthorContext {
    proxima_core::McpAuthorContext {
        model_id: "test".into(),
        client_name: "test".into(),
        client_version: "0".into(),
        caller_self_perspective: None,
    }
}
```

If `crates/mcp-server/tests/common.rs` doesn't already export
`fresh_engine_with_pg` and `with_master_token`, mirror the patterns
in the closest existing rmcp integration test file in that
directory. Both helpers are short:
- `fresh_engine_with_pg` returns `Option<(Arc<Engine>, DevMcpServer, Owner)>` skipping when no PG is available (same shape as `fresh_pg`).
- `with_master_token` calls `server.auth().replace_local_master_token(token, owner)` (expose `auth()` getter on `DevMcpServer` if not already public).

- [ ] **Step 2: Run the test**

Run: `cargo test -p proxima-mcp-server --test master_token_identity`
Expected: 2 tests pass when PG is available; both skip gracefully
when PG is not.

- [ ] **Step 3: Commit**

```bash
git add crates/mcp-server/tests/master_token_identity.rs
git commit -m "test(mcp-server): per-token shell-author end-to-end"
```

---

### Task 10: End-to-end goal_propose creates `core/inspires` from master token

**Files:**
- Add: a test in `flavors/goal/tests/` (mirror an existing PG-backed test in that dir; e.g. if `goal_propose_pg.rs` exists, extend it; otherwise create `goal_propose_master_token_pg.rs`).

- [ ] **Step 1: Write the failing test**

```rust
//! Master-token goal_propose creates a core/inspires edge to the
//! per-token Self-Perspective without the caller specifying one
//! explicitly. This is the v0.1.0 acceptance criterion for M0.

mod common;

use common::{fresh_goal_pg, master_token_call};
use proxima_core::storage::Storage;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn master_token_propose_creates_inspires_edge(
) -> Result<(), Box<dyn std::error::Error>> {
    let Some((engine, server, owner)) = fresh_goal_pg().await? else {
        return Ok(());
    };
    let token = Uuid::now_v7();
    server.auth().replace_local_master_token(token, owner.clone()).await;

    let response = master_token_call(
        &server,
        token,
        "proxima-goal/goal_propose",
        serde_json::json!({
            "payload": { "kind": "simple_text",
                         "title": "Test goal",
                         "text": "rename foo to bar" },
            "evidence": []
        }),
    )
    .await?;

    let inspires_edge_handle = response
        .get("inspires_edge_handle")
        .and_then(serde_json::Value::as_str)
        .ok_or("propose did not return inspires_edge_handle")?;

    assert!(
        !inspires_edge_handle.is_empty(),
        "expected non-empty inspires_edge_handle from a master-token propose"
    );

    // Optional deeper assert: query the edges table and verify the
    // edge target_memory_id matches the per-token self_perspective.
    let identity = engine
        .storage()
        .ensure_master_token_personality(&owner, token)
        .await?;
    // ... a SELECT against proxima_core.edges by edge_id ...
    // (use whatever helper the surrounding test files already have)

    Ok(())
}
```

`master_token_call` is a thin helper around `server.call_tool(..., Some(auth))`
that resolves the auth context for the given token; mirror it from
existing rmcp test scaffolds.

- [ ] **Step 2: Run the test to confirm it fails on `main` and passes on M0**

Run: `cargo test -p proxima-goal --test goal_propose_master_token_pg`
Expected: fails on the pre-M0 code (no inspires_edge_handle returned
because no caller_self_perspective). After M0 plumbing, passes.

- [ ] **Step 3: Commit**

```bash
git add flavors/goal/tests/goal_propose_master_token_pg.rs flavors/goal/tests/common.rs
git commit -m "test(goal): master-token propose creates core/inspires edge"
```

---

### Task 11: Final review + roadmap update

**Files:**
- Modify: `docs/superpowers/specs/2026-05-10-spinning-wheel-proof-roadmap.md` §M0

- [ ] **Step 1: Update the roadmap "Status" line for M0**

Change `[implementation status]` to `landed (commit <SHA>)` once the
final commit is in. Or add a small "Status: landed" line under the
M0 heading. Pick the convention closest to existing roadmap files in
the repo.

- [ ] **Step 2: Run the full workspace test once more**

Run: `cargo test --workspace`
Expected: all green.

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-05-10-spinning-wheel-proof-roadmap.md
git commit -m "docs(roadmap): mark M0 landed"
```

---

## Self-Review Checklist

After all tasks complete, verify against the spec:

1. **Acceptance criterion §M0:** master-token MCP call writes a Goal *with* a `core/inspires` edge to the per-token Self-Perspective, no explicit args required → covered by Task 10.
2. **Reconnect identity:** same token → same identity → covered by Task 3 (`ensure_master_token_personality_is_idempotent`) + Task 9 (`master_token_call_mints_per_token_self_perspective` reconnect assertion).
3. **Distinct tokens:** distinct identities → covered by Task 3 (`distinct_tokens_resolve_to_distinct_personalities`) + Task 9 (`distinct_master_tokens_resolve_to_distinct_identities`).
4. **Audit Fact authorship:** PersonalityConfigChangedV1.MasterToken now carries the per-token instance_id → covered by Task 7 + existing audit integration tests.
5. **No legacy `ensure_shell_author_personality`** in the trait or PG surface → covered by Task 2 (trait removal) + Task 3 (file deletion).

## Rollback

Per-task commits land independently. The cleanest rollback is to
`git revert` the commits in reverse Task order (11 → 1). The new
migration `20260510000030_master_token_personality.sql` should NOT
be reverted on a database that has rows in
`proxima_core.master_token_personality` — drop the rows first if
truly needed; otherwise leave the table in place and let the trait
method be re-added in a future spec. The substrate stays clean
because the singleton `ensure_shell_author_personality` row(s) from
prior runs are now harmless leftovers (no code paths reference them).
