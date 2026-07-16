# Migrating a Proxima host

Runbook for bumping an embedding host (Centauri-style consumer) across a
Proxima tag. Deployment/env reference lives in
[15-deployment.md](docs/15-deployment.md); public API tiers live in
[docs/reference/public-api.md](docs/reference/public-api.md); this file is
the single place for upgrade *mechanics* — don't duplicate either doc here.

## 1. Detect: does the target database need a v0.0.4 reset?

`ProximaBuilder::boot()` / `Proxima::<App>::build()`/`run()` return a typed
error instead of a stringly-typed one when the target database still
carries pre-v0.0.4 Proxima schema artifacts (or a stale baseline
checksum):

```rust
let running = match proxima::Proxima::<MyApp>::app()
    .database_url(url)
    .owner(owner)
    /* ... */
    .run()
    .await
{
    Err(proxima::ProximaError::V004ResetRequired { details }) => {
        eprintln!("database needs a v0.0.4 reset before this host can boot: {details}");
        eprintln!("see MIGRATING.md#2-back-up-then-reset");
        std::process::exit(1);
    }
    Err(other) => return Err(other.into()),
    Ok(running) => running,
};
```

`ProximaBuilder::boot()` callers match `proxima::EmbedError::V004ResetRequired { details }`
the same way — `ProximaError::V004ResetRequired` is just `EmbedError`'s
variant carried through `Proxima::run()`/`build()`. Both variants are
distinct from the generic `Storage(String)` arm precisely so hosts can
match on them instead of parsing an error string
(`crates/proxima/src/lib.rs`, `crates/proxima/src/runtime_config.rs`).

## 2. Back up, then reset

```sh
pg_dump "$DATABASE_URL" -Fc -f pre-v0.0.5-backup.dump
```

Reset with `tools/dev-migrate` (never `sqlx migrate run` — core and flavor
migrators share one `_sqlx_migrations` table, which trips `VersionMissing`
on the second source):

```sh
SQLX_OFFLINE=true cargo build -p proxima-dev-migrate

# target resolution: --database-url first, then DATABASE_URL; always
# printed before anything runs
PROXIMA_V004_RESET_CONFIRM=reset-my-dev-db \
  ./target/debug/dev-migrate --database-url "$DATABASE_URL" --reset
```

`--reset` refuses non-local hosts and protected database names
(`postgres`/`template0`/`template1`) even with the confirm env set — it is
a **local dev tool**, not a production migration path. Never point it at a
shared or production database.

**Production promotion follows GitOps, not this tool.** Apply
`crates/storage-pg/migrations/0008_v005.sql` (the only append-only v0.0.5
migration — `0001_init.sql` is the immutable shipped baseline; migration
versions 2-7 are permanently retired, see
`RETIRED_PRE_V004_MIGRATION_VERSIONS` in `crates/storage-pg/src/lib.rs`)
through your normal deploy pipeline (ArgoCD/Forgejo or equivalent) against
a real backup, not this binary.

### v0.0.6 schema lane (core 9→10 + flavor append-only)

After v0.0.5, apply these migrations through GitOps before booting a
`skip_migrations` host:

| Source | Files | Notes |
|---|---|---|
| Proxima core | `0009_v006.sql`, `0010_v006.sql` | GIN index drops; embedding backoff column; prefix-redundant btree drops; F/A/P append-only triggers |
| Code flavor | `20260709000020_append_only.sql` | Code sidecar append-only triggers |

Online-safe: nullable column add, idempotent index drops, trigger creation —
no backfill. `ProximaBuilder::skip_migrations(true)` boot now also runs
`ensure_core_schema_current`: core migration version ≥ 10 (lane `version <=
9999`), core v0.0.6 structural markers, and the code-flavor
`code_chunk_v1_append_only` trigger when `proxima_code` is present.

## 3. Confirm app restart

```sh
DATABASE_URL="$DATABASE_URL" ./your-host-binary   # or: docker restart <container>
```

Boot succeeds once `ensure_v004_baseline_compatible` sees only the current
baseline version — no more `V004ResetRequired`. Tail logs for the
`{source} migrations applied` lines dev-migrate (or your host's own boot
path) prints, confirming both `proxima-core` and every flavor source ran.

## 4. Embedding / custom hosts: the OIDC group-auth path changed

`OidcAuthConfig { owner, .. }` — a config struct that pinned every
accepted token to one fixed `Owner` — **no longer has an `owner` field**.
Identity mapping is now a separate, explicit step. Two issuer-branching
`OidcAuthConfig` construction sites (e.g. one per Zitadel issuer) become:

`OidcAuthConfig`/`OidcAuthenticator`/`OidcSubjectMap`/`HttpJwksResolver`
live in `proxima-auth-oidc`, not the `proxima` facade — add it as a direct
Cargo dependency (see `apps/proxima-mcp/Cargo.toml` /
`apps/proxima-mcp/src/lib.rs::oidc_from_env` for the in-repo reference
host that already does this). `?` below stands in for whatever error type
your host maps each fallible step into (`oidc_from_env` maps each one into
its own `CliError` variant — copy that pattern, not the bare `?`):

```rust
use std::sync::Arc;
use proxima_auth_oidc::{HttpJwksResolver, OidcAuthConfig, OidcAuthenticator, OidcSubjectMap};

// 1. Validation-only config: no identity mapping.
let oidc_config = OidcAuthConfig {
    issuer: issuer.clone(),
    jwks_uri,              // None => discover via {issuer}/.well-known/openid-configuration
    audience,
    allowed_subjects,       // unchanged: still an optional `sub` allowlist
    leeway_secs: 60,
};
let keys = Arc::new(HttpJwksResolver::new(issuer.clone(), oidc_config.jwks_uri.clone())?);

// 2. (iss, sub) -> UserId, explicit and issuer-aware (replaces whatever
//    identity source `owner` used to encode).
let subject_map = OidcSubjectMap::from_json(&subject_map_json)?; // or ::from_legacy_shorthand for one issuer

// 3. Exported OwnerAccessPort — drop any hand-rolled resolver that raw-SQLs
//    proxima_core.group_memberships; PgOwnerAccessResolver wraps the same
//    table.
let owner_access: Arc<dyn proxima::OwnerAccessPort> =
    Arc::new(proxima::PgOwnerAccessResolver::connect_lazy(&database_url)?);

// 4. Composes the same shape `AuthzContext::server_resolved(roles,
//    AuthPath::HostBearer)` used to be assembled by hand.
let authenticator = OidcAuthenticator::new(oidc_config, keys, subject_map, owner_access)?;
```

`OidcAuthenticator::authenticate` builds the `AuthzContext` internally —
hosts don't call `AuthzContext::server_resolved(...)` themselves for this
path. Wire the result into the runtime the same way as before:

```rust
proxima::Proxima::<MyApp>::app()
    .database_url(database_url)
    .owner_access(owner_access.clone())
    .authenticator(Arc::new(authenticator))
    .with_mcp()
    .run()
    .await?;
```

Embedded hosts that do not serve MCP may still configure a boot owner for
host-owned direct calls. MCP serving ignores that boot owner and requires
`OwnerAccessPort`.

Multi-audience composition (branching on `aud` to run more than one
identity class) can now use `OidcBindingSet`: register one `OidcBinding`
per `(issuer, audience, subject-map, role-shape)` route. Construction
rejects duplicate `(issuer, audience)` routes; authentication rejects
tokens unless exactly one binding validates. The lower-level
`OidcTokenValidator` / `ValidatedOidcClaims` surface remains available for
fully custom hosts — see `crates/auth-oidc/tests/custom_host_validation.rs`.

Hand-rolled agent tool-palette filtering is replaced by one call. `built`
below is `Proxima::<App>::build()`'s result (`BuiltProxima`), whose
`registry: Arc<FlavorRegistryFrozen>` field this reads:

```rust
let scope = proxima::tool_palette_excluding(&built.registry, &["dangerous_tool_id"]);
let authz = /* ... */.with_tool_scope(scope);
```

`tool_palette_excluding` expands action-scoped tools to `tool:action`
granularity itself, so excluding a tool name also excludes every one of
its actions — no partial-exclusion gap when a tool grows a new action.

## 5. v0.0.6 MCP serving: fixed-owner serving removed

`proxima-mcp --owner-user` is removed. `OidcAuthenticator::single_owner`
and `IdentityResolution::FixedOwner` are removed. Serving has one path:

```text
bearer -> UserId -> OwnerAccessPort::resolve_roles_for_subject -> OwnerRoles
```

MCP owner selection:

| Step | Contract |
|---|---|
| initialize | client sends `X-Proxima-Owner: personal:<uuid>` / `group:<uuid>` / `world:00000000-0000-0000-0000-000000000001` |
| session | server binds selected owner to `Mcp-Session-Id` |
| later calls | no owner argument; bound owner is rechecked against fresh roles |
| revocation | membership removal denies the next request |

Loopback master-token auth is removed. MCP serving requires a host
`Authenticator` plus `OwnerAccessPort`; stale `Bearer pxm_*` credentials
fail closed and are not forwarded to host auth.

`McpToolHost` no longer has a default owner. Embedded direct MCP calls
must pass the owner explicitly per call through the existing direct-call
API.

## 6. AuthorizationHook membership direction

`AuthzOperation::Membership` is now directional:

```rust
AuthzOperation::Membership {
    change: MembershipChange::Add | MembershipChange::Remove,
    group,
    member,
    relation,
}
```

Any `AuthorizationHook` that pattern-matches membership mutations must add
the `change` field. This is a breaking hook-input change so veto consumers
can distinguish group membership grants from removals; Centauri-style
router vetoes that previously consumed the membership shape around
`router/mod.rs:84-88` should branch on `MembershipChange` instead of
inferring direction from the called tool.

## 7. `proxima-storage-pg` raw write API requires `OwnerWritePermit`

These were never part of the supported Host API or Flavor SDK tiers (see
[public-api.md](docs/reference/public-api.md#supported-tiers)), but if
something depended on them anyway:

| Symbol | Was | Now |
|---|---|---|
| `verbs::fact_ingest::ingest_fact` / `ingest_fact_in_tx` / `ingest_fact_for_owner` | engine/authz/owner arguments | `&OwnerWritePermit` + payload + optional embedding model |
| `verbs::derive_append::append_derived_with_edges_in_tx` | raw owner in `DerivedDraft` was enough | `&OwnerWritePermit` + `DerivedDraft` + operator edge proofs |
| `verbs::edge_write::append_owner_checked_*` | raw `&Owner` authority | `&OwnerWritePermit` authority |
| `verbs::close_batch::close_batch`, `verbs::persist_mcp_call::persist_mcp_call_atomic`, source cursor / retention / legal-hold write verbs | raw owner authority | `&OwnerWritePermit` authority |
| `verbs::fact_embeddings::insert_embedding` | `pub` | `pub(crate)` — use the proof-gated `EmbeddingWritePort` |
| `verbs::fact_embeddings::insert_memory_embedding` | `pub` | `pub(crate)` — use the proof-gated `EmbeddingWritePort` |
| `verbs::fact_embeddings::insert_fact_embedding` / `upsert_fact_embedding` / `upsert_memory_embedding` / `insert_goal_embedding` | `pub` | deleted (zero remaining callers; use the proof-gated port) |
| `verbs::fact_ingest::ingest_fact_command_in_tx` | `pub` | `pub(crate)` |
| `verbs::fact_ingest::ingest_fact_with_derived_sidecar_in_tx` | `pub` | `pub(crate)` |

Permit minting is an engine operation:

```rust
let permit = engine
    .authorize_owner_write(&authz, &owner, proxima_core::AccessKind::Fact)
    .await?;

proxima_storage_pg::verbs::fact_ingest::ingest_fact_in_tx(
    &mut tx,
    &permit,
    &payload,
    None,
    |tx, outcome| Box::pin(async move { /* sidecar write */ Ok(()) }),
)
.await?;
```

`AuthPath::System` no longer mints storage write permits by shape alone.
Hosts that intentionally need System writes hold
`BuiltProxima::system_authority()` / `RunningProxima::system_authority()`
and call `Engine::authorize_owner_write_with_system_authority(...)`.
Flavor tools and MCP-wire code do not receive this witness.

## 8. Flavor authors: raw SQL against `proxima_core.*` is guardrail-denied

New flavor code may not run raw SQL against `proxima_core.*` tables
(`scripts/check-architecture-guardrails.py` fails the build on new sites).
Migrate reads onto the exported facade:

```rust
use proxima::flavor::{authorized_memory_ids, authorized_fact_payloads};
```

See the module doc on `crates/proxima/src/flavor/authorized_read.rs` for
the full helper set (`authorized_memory_ids`, `authorized_fact_payloads`,
`authorized_fact_payloads_include_tombstones`) — all route through
`Engine::query`, the same owner/group/`World` visibility path every other
authorized read uses.

## 9. Owner-transfer: `core_publish:publish_to_world`

Publishing an entity is now an owner **transfer** to `OwnerRef::World`
(`Engine::publish_to_world`), not an ACL flag or a share row. Published
entities become readable by everyone and writable by no one; re-publishing
an already-World entity fails closed with `Forbidden` (the current-owner
lookup resolves to World, which `authorize_write` never accepts). If a
consumer previously modeled "publish" as a copy or a grant, switch it to
the `core_publish:publish_to_world` MCP action / `Engine::publish_to_world`.
The previous `core_membership:publish_to_world` action key is removed; update
tool-scope allow/deny entries and MCP clients to the new `core_publish`
dispatcher.

## 10. Code flavor repo erase is physical and rebuildable

`proxima_code::erase_repo(pool, owner, repo_id, schemas)` no longer
returns the old "deferred to PR9" storage error. It deletes the selected
repo record, repo ingestion runs via FK cascade, code-flavor sidecars,
selected owner-scoped substrate memories, source receipts/batches,
citations, edges, and embeddings, then returns `RepoEraseReceipt`.

Unlike compliance owner/source erasure, repo erase does not write
suppression keys. Re-registering and re-ingesting the same repo is allowed.

## 11. Lock-step version bump

Every Proxima crate this host depends on (`proxima`, `proxima-core`,
`proxima-storage-pg`, `proxima-auth-oidc`, and any flavor crates) moves
together — there is no supported skew between them across a tag. Bump all
of them in the same commit, then run the checks in this file before
merging.

Migration version lanes:

| Source | Reserved versions |
|---|---|
| Proxima core | `1..=9999`; `2..=7` retired pre-v0.0.4 rows |
| example/host migrators | timestamp versions ending `00..=19` |
| first-party flavors | timestamp versions ending `20..=39` |
| downstream host composition | timestamp versions ending `60..=99`; if a host composes migrators outside `run_core_and_flavor_migrations`, it owns collision avoidance before touching the database |

Run `python3 scripts/check-migration-ranges.py` after adding or bumping any
in-repo migration.

## 12. Lean consumers

If a downstream package requires `docs/lean` as `causa` (e.g. a
`kernel/lakefile.toml` with `require causa rev=...`), bump `rev` in the
same commit as the Cargo tag bump — a Proxima tag bump is a dual
Rust+Lean bump, never just one.

Before bumping `rev`, run `python3 scripts/check-lean-axioms.py` — it
rebuilds `docs/lean` itself and diffs the kernel's current axiom set
against the checked-in allowlist at `scripts/lean-axioms.allowlist.txt`.
A silent axiom-set change must never be
absorbed into a downstream kernel unnoticed — if the script reports a
diff, that's a stop-and-review signal before the rev bump, not a rubber
stamp.

## 13. `RuntimeBuilder::tool_scope` is now required

`RuntimeBuilder`/`Proxima::<App>::app()` no longer defaults an unset tool
scope to `ToolScope::All`. An embedding host that never called
`.tool_scope(...)` used to silently advertise the full MCP tool surface —
including `core_publish` (irreversible owner transfer to World) and
`core_membership` — to every token. `build()`/`run()` now return
`ProximaError::Config("tool_scope is required: ...")` at `resolve()` time
until the host makes an explicit choice:

```rust
// one-line fix — restores the previous full-surface behavior explicitly
Proxima::<App>::app()
    .tool_scope(proxima::ToolScope::All)
    // ...
    .run()
    .await?;
```

Agent-facing hosts should prefer a narrow palette instead:
`.tool_scope(proxima::ToolScope::Palette(vec!["core_search_memories".into(), /* ... */]))`.
This applies even when the host never enables MCP (`.with_mcp()`) — the
check is unconditional in the builder, not gated on transport wiring.
`apps/proxima-mcp` already always resolves and passes an explicit scope,
so it is unaffected.

## 14. `layered_router`/`layered_router_with_revalidation` now cap body size

These two `crates/proxima::runtime` composition-seam routers previously
had no `DefaultBodyLimit`/`enforce_body_limit` layer, unlike `build_router`
and the streamable transport (`crates/mcp-server/src/transport.rs`) — an
embedding host serving `layered_router` network-facing had no cap on
inbound request body size. Both now carry the same
`proxima_mcp_server::enforce_body_limit` layer, outermost, matching
`build_router`'s order (body limit runs before auth). No caller-visible
signature change; hosts composing their own router around
`layered_router`'s output get the cap for free.

## 15. v0.0.7: `goal_wake_candidate` is a new required storage port

`StoragePortsBuilder` gained a required `goal_wake_candidate` handle
(`GoalWakeCandidatePort`) backing the new wake-candidate admission read
(`Engine::list_goal_wake_candidates`, MCP
`proxima://wake-candidates{?fact,limit}`). Hosts assembling ports via
`PgStorage::storage_ports()` are unaffected. Custom port assemblers must add
one builder line (`.goal_wake_candidate(backend.clone())`) and implement the
one-method port, or `try_build()` reports it missing. No schema change: the
port reads the existing `proxima_core.goal_wake_config` table. Wake config is
now also writable over MCP (`core_goal` `set`/`decompose` `wake`, `modify`
`wake`/`clear_wake`); tool-scope palettes that should expose the new resource
must include `resource:wake-candidates` (profile `memory` includes it).

Admission additionally intersects the engine's composed deployment
tool-surface profile: `Engine::with_deployment_tool_scope` (default
`ToolScope::All`). The `proxima` runtime facade forwards its required
`tool_scope` automatically; hosts composing `Engine` directly should pass
their deployment palette so Host-API wake reads cannot exceed the deployed
tool surface even under an `AuthzContext` with `ToolScope::All`.

## 16. v0.0.7: one MCP reference grammar — prefixed ids only

The MCP presentation tier collapsed to the single canonical wire form:
typed prefixed uuids (`F:`/`A:`/`P:`/`G:`/`E:<uuid>`; flavor objects use
their registered uppercase prefix). `OutputMode`, `HandleTable`, `Handle`,
the mcp-level `EntityKind`/`EntityRef`, and `McpToolError::Resolve` /
`ResolveError` are removed; `McpToolCtx` lost its `handles`/`mode` fields
and `McpToolPresentation` is now stateless (`new()` takes no arguments).
Deployed MCP clients are unaffected: production servers already spoke
prefixed ids exclusively, and every `format_*`/`resolve_*` helper keeps its
signature. Two wire-visible tightenings: `core_get_memory` no longer
accepts a bare uuid for `memory` (pass the `F:`/`A:`/`P:` form it emits),
and resolve errors now always report the prefixed-id grammar
(`expected Fact id (F:<uuid>), got prefix 'A' in '…'`). Test harnesses
that projected session-scoped handles (`F1`, `G7`) must format references
with `format_prefixed_uuid`/`parse_prefixed_uuid`
(`proxima_core::mcp`), which remain public alongside
`PrefixedUuidClass`, `PrefixedUuidError`, and `MemoryHandleClass`.

## 17. v0.0.7: memory search returns pages and takes retrieval knobs

`MemoryReadPort::search_memories` now returns
`verbs::query::MemorySearchPage { results, has_more }` instead of
`Vec<MemorySearchResult>`; port implementors and mocks must wrap their
row vectors (`has_more: false` preserves prior semantics), and
`engine::SearchReadResponse` gained a `has_more` field.
`MemorySearchRequest` gained three `#[serde(default)]` fields:
`min_score: Option<f32>` (post-fusion relevance floor, `0..=1`),
`semantic_weight: Option<f32>` (hybrid fusion weight on the semantic
component; `None` keeps `DEFAULT_HYBRID_SEMANTIC_WEIGHT` = 0.6), and
`after: Option<SearchCursor>` (typed keyset resume point whose variant
must match `order`; relevance depth is bounded by
`MAX_RELEVANCE_SEARCH_DEPTH`). Struct-literal construction sites must add
the fields; serde consumers are unaffected. The former inline `50` result
cap is now `verbs::query::MAX_SEARCH_PAGE_LIMIT` and applies per page.

On the MCP wire the change is additive: `core_search_memories` accepts
optional `min_score`, `semantic_weight` (hybrid only), and `cursor`
(opaque, from the previous response's `next_cursor`), and its output
gained `next_cursor` and `has_more`. Cursors are fingerprint-bound to
the query shape — replaying one with any changed argument except `limit`
fails closed with `InvalidInput`.

## Checks before calling an upgrade done

```sh
cargo test -p proxima --lib
cargo test -p proxima-storage-pg --lib
cargo check -p proxima-dev-migrate
cargo clippy -p proxima -p proxima-dev-migrate --all-targets -- -D warnings
python3 scripts/check-architecture-guardrails.py
python3 scripts/check-sql-policy.py
python3 scripts/check-migration-ranges.py
```
