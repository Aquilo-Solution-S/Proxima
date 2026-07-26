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
no backfill.

### v0.0.7 schema lane (core 10→11)

| Source | Files | Notes |
|---|---|---|
| Proxima core | `0011_v007.sql` | `embeddings` primary key rebuilt to include `chunk_index`; STORED generated `search_tsv` on `memories`, `agent_derivation_v1`, `agent_note_v1`; four `proxima_core.lexical_*` functions |
| Code flavor | `20260726000020_v007.sql` | STORED generated `search_tsv` on `code_chunk_v1`, built from `proxima_core.lexical_tsv` (config `english`, changed from `simple`); GIN moves from the old expression index to the column |

**This lane is not online-safe.** Unlike v0.0.6, both files rewrite tables.
`ADD COLUMN ... GENERATED ALWAYS AS ... STORED` rewrites each target and holds
`ACCESS EXCLUSIVE` for the duration, and sqlx runs each file in one
transaction — so every table in a file stays locked until the last one
finishes.
Measured: **54.7s** for a 149k-row `memories` plus a 24.8k-row sidecar, and
it scales with corpus size. A queued `ACCESS EXCLUSIVE` request also blocks
every reader that arrives behind it, so this is a read outage, not just a
write pause.

Plan for it:

- **Large deployments: apply out of band.** Run both files through
  GitOps against a real backup during a maintenance window, then boot with
  `PROXIMA_SKIP_MIGRATIONS=true`. Do not discover the lock window during a
  rolling update.
- **Small deployments** can let boot apply it. Boot migrations now set
  `lock_timeout = 5s`, so a migration that cannot get the lock fails and
  retries on the next pod rather than freezing the table behind a lock queue.

The code-flavor file moves code chunks onto `proxima_core.lexical_tsv`, the
same definition core uses, which means config `english` rather than `simple`.
That does change results, and deliberately.

`simple` neither stems nor drops stopwords, so
`websearch_to_tsquery('simple', ...)` over a question is an AND of every word
in it, function words included. Measured against Proxima's own indexed source,
**0 of 24 natural-language queries returned a single row**. Under `english`
the same question reduces to its content lexemes, which an OR-rescue arm can
work with; the same 24 queries then score hit@1 0.375, hit@10 0.708,
MRR 0.499.

Stemming does fold `parsing`/`parsed`/`parser` together, and `in`, `as`, `if`,
`do`, `no`, `on` are all English stopwords and real keywords. Exact identifier
and keyword lookup moved to the substring arm of the search, which carries a
larger score bonus than any rank, so that precision is relocated rather than
lost — `embed_in_chunks` still matches verbatim.

Sharing the definition is also what lets `CodeChunkV1::search_projection()`
name the column as its `tsv_column`, so `core_search_memories` reads the
stored vector instead of recomputing one. A column built with a different
config could not be substituted: a tsvector carries no record of the config
that produced it, so the mismatch would be silent.

Two behaviour changes ride along, neither of which announces itself:

- **`embeddings` primary key changed shape** to `(entity_kind, entity_id,
  embedding_version, model_id, chunk_index)`. A memory whose text exceeds the
  provider's input limit is now stored as several chunk rows under one
  embedding version, where it previously went un-embedded entirely.
- **Lexical search now stems and drops English stopwords** (`'simple'` →
  `'english'` text-search config), in core and in the code flavor alike.
  Result *sets* change, not just ordering: a query of only stopwords no
  longer matches, and `running` now matches `run`. Re-check any saved query
  or test that pins exact lexical hits.
- **Code chunks carry their body.** `code-chunk-v1` renders as
  `path:start-end` followed by the chunk text, where it used to render the
  header alone. That render is `memories.text`, so it is what gets embedded:
  code-chunk embeddings previously encoded a file path and two line numbers,
  and `core_search_memories` could only retrieve code whose *filename*
  resembled the question. Existing indexes need re-indexing to benefit — see
  §25.

`ProximaBuilder::skip_migrations(true)` boot runs `ensure_core_schema_current`,
which for this release requires core migration version ≥ 11 (lane `version <=
9999`) plus the structural markers for both lanes: v0.0.6's
`embedding_jobs.next_attempt_at` and `memories_enforce_immutable` trigger, the
code-flavor `code_chunk_v1_append_only` trigger when `proxima_code` is
present, and v0.0.7's `memories.search_tsv`, `embeddings.chunk_index`, and
`proxima_core.lexical_tsv(text)`. A database one lane behind now fails at
boot instead of at first query.

**Rollback is by image, never by reversing `0011`.** There are no `.down.sql`
files, and `DROP COLUMN search_tsv` is a second full-table rewrite under the
same lock. Rolling the binary back to v0.0.6 against a version-11 database is
safe **only** if no over-limit memory was chunk-embedded — a v0.0.6 binary
assumes one embedding row per `(entity, version, model)`. Check with
`SELECT count(*) FROM proxima_core.embeddings WHERE chunk_index > 0;` and
delete those rows before downgrading if it is non-zero.

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

The rule is about core *data*. As of v0.0.7 a flavor query may call the pure
`proxima_core` SQL functions — `lexical_scrub`, `lexical_tsv`,
`lexical_join`, `lexical_text_array`, `memory_entity_kind` — which are
IMMUTABLE, read no row and enforce no authorization. They exist to be shared:
a flavor that could not call them would have to restate the definition its
own generated column is built from, which is exactly the drift they were
introduced to prevent. The guardrail masks those calls and still fails on any
literal that names a core table or view, including one that does both.

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

## 18. v0.0.7: goal introspection reads and wake truncation signals

Two new read resources make stored intent inspectable:
`proxima://goals{?state,limit,cursor}` (owner-scoped listing with a
closed state vocabulary, opaque keyset cursor bound to the state filter,
and `has_more`) and `proxima://goal/{id}` (single `G:<uuid>` read).
Both read back the goal's stored wake configuration (`trigger_fact` or
`trigger_schema_id`/`version`, `tool_ids`, `prompt`, `hard_memories` as
prefixed ids). `proxima://wake-candidates` output gained `has_more` —
truncation at the 200-candidate cap is now signalled, never silent.

Rust-level changes for embedders: `GoalReadPort` gained
`load_goal_wake_configs(read_owners, goal_ids)` (implement it on custom
ports; returning an empty vec preserves prior behavior),
`ReadVerbStoragePorts` carries a `goal_read` handle,
`ListWakeCandidatesReadResponse`/`ListWakeCandidatesOutput` gained
`has_more`, and `QueryRequest` gained `#[serde(default)] goal_state:
Option<GoalState>` (struct literals must add the field). The MCP wire
change is purely additive.

## 19. v0.0.7: embedding pipeline self-heals; `maintain-embeddings` CLI

The embedding queue now heals its own gaps instead of waiting for an
operator. The MCP wire is unchanged; three things move for embedders and
deployments:

- **CLI rename + wider pass.** `proxima-mcp reconcile-embeddings` is now
  `proxima-mcp maintain-embeddings` (same flags). The pass gained an
  orphan-row sweep before the reconcile enqueue and a health report after
  it (job backlog, orphan counts, ANN recall canary). Passes are
  serialized by a Postgres advisory lock; a run that finds the lock held
  prints a skip notice and exits `0`, so cron overlap is safe. Update
  cron/deploy specs; the old subcommand fails with a message naming the
  new one.
- **Startup catch-up.** When an embedding client is configured, the
  in-process worker (`spawn_embedding_worker`) runs one `missing-only`
  reconcile before its first drain. Facts ingested while no client was
  configured — which get no durable job at ingest — and jobs stuck in the
  `failed` retry dead-end are re-enqueued on the next restart. Degraded
  boots (no client) are unchanged. There is still no recurring in-process
  scheduler; recurring maintenance stays external.
- **Port + types.** `EmbeddingMaintenancePort` gained
  `reconcile_embeddings(options, proof)`; custom implementors must add
  it (forward to storage or return an error — the engine only calls it
  when an embedding client is installed).
  `EmbeddingReconcileScope`/`EmbeddingReconcileOptions`/
  `EmbeddingReconcileOutcome` moved from `proxima-storage-pg` to
  `proxima-core` (storage-pg re-exports them, so existing import paths
  keep compiling). New host verb `Engine::reconcile_embeddings(scope,
  limit)` mirrors `drain_embedding_jobs`: host-invoked, no-op without a
  client. `PgStorage` gained operator-surface inherent methods
  `sweep_orphan_embedding_rows()`, `embedding_ann_observability()`, and
  `try_embedding_maintenance_lock()`.

## 20. v0.0.7: edges become readable on the wire; edge sidecars must be readable

The graph was writable but not traversable by edge: `core_link` returned
an `E:<uuid>` handle no verb could dereference, and its `reason`/
`confidence` payload was write-only. Two new read resources close that
hole: `proxima://edges{?relation,source,target,limit,cursor,payloads}`
(owner-scoped listing — at least one filter required, opaque keyset
cursor bound to the filter, `has_more`, typed payload read-back on by
default) and `proxima://edge/{id}` (single `E:<uuid>` read). Source/
target handles come back kind-correct (`F:`/`A:`/`P:`/`G:`); unreadable
targets stay `redacted target`/`unavailable target` with no id or kind
leakage. The MCP wire change is purely additive.

Rust-level changes for embedders and flavor authors:

- **`EdgeRow` reshaped.** Gained `source_kind`, `target_kind`
  (`Option<EntityKind>`, populated only for visible targets),
  `created_at`, and its dead `payload: Vec<u8>` (always empty) became
  `payload: Option<SidecarPayload>` mirroring `MemoryRow`. The struct no
  longer derives serde.
- **Edge read pagination.** `EdgeReadRequest` gained `#[serde(default)]`
  `cursor: Option<EdgeReadCursor>` and `include_payloads: bool` (struct
  literals must add the fields); `EdgeReadResponse` gained
  `next_cursor: Option<EdgeReadCursor>` over `(created_at, edge_id)`
  descending keyset order.
- **`EdgeReadPort::read_edges` signature.** Now takes
  `payload_specs: &[EdgePayloadSpec]` (engine-resolved relation → payload
  schema mapping, mirroring `load_memory_by_id`'s `sidecars`). Custom
  ports must accept the parameter; ignoring it preserves lean reads.
- **Edge sidecars must implement read-back.**
  `PgSidecarRegistry::add_edge` now requires `PgEdgePayload` (a batched
  `load_edge_batch(ctx, edge_ids)` loader) alongside `PgEdgeSidecar`, and
  `freeze_against` rejects an edge sidecar without one — an edge payload
  that can be written but never read back is a write-only API hole. Core
  ships readers for `AgentLinkV1` and the code flavor's `EdgeCallsV1`;
  custom edge sidecars add one `SELECT ... WHERE edge_id = ANY($1)`
  loader. `PgSidecarReadCtx` gained `fetch_all_by_edge_ids`.

## 21. v0.0.7: retention is enforced; `change_event` becomes prunable

`owner_fact_retention.retention_seconds` was inert config since the old
sweep was deleted (v0.0.6), and `change_event` grew without bound. Both
are now handled by one operator-scheduled, cron-safe CLI pass:

```sh
proxima-mcp maintain-retention --enforce-fact-retention \
    --prune-change-events-older-than 90d
```

Operational consequences to review before scheduling it:

- **Configured retention windows become real.** Owners with a
  `retention_seconds` value will have Facts older than the window
  tombstoned (hidden from present-only reads; rows and provenance kept —
  physical destruction remains exclusive to the compliance-erase
  family). Audit Facts (`core/mcp-call-logged-v1`) are always excluded.
  If a window was set speculatively and should NOT be enforced, clear it
  before scheduling the pass.
- **Tombstoning now emits `EntityDelete` change events** (the first
  producer of that kind). Forward pollers of `proxima://change-events`
  should already handle the variant — it has been part of the wire enum
  since v0.0.4 — but consumers that only ever matched `EntityAppend`
  should be checked.
- **Pruned change events are gone for every consumer.** A forward
  poller whose `since` cursor predates the prune horizon misses the
  pruned events with no gap signal. Pick a horizon comfortably larger
  than the slowest consumer's lag, or re-baseline lagging consumers via
  cold-start stitching (docs/14 §Change Log).
- **Legal holds gate both halves.** Held owners are skipped and
  reported; the pass never blocks on a hold.
- No MCP wire-surface change: the command is CLI-only, and
  `EntityDelete` was already a legal wire event. Rust embedders gain
  `PgStorage::{enforce_fact_retention, prune_change_events,
  try_retention_maintenance_lock}` (additive).

## 22. v0.0.7: typed resource errors, batch memory read, list pagination sweep

Every bad resource read used to collapse into
`invalid_params: "invalid input: unknown resource {uri}"`; several list
surfaces were unbounded or truncated silently. Wire changes to review:

- **Resource error shapes changed.** Unknown `proxima://` paths now
  return JSON-RPC `resource_not_found` (-32002); bad or missing query
  parameters return `invalid_params` naming the parameter; dereferencing
  a missing or invisible memory/goal/edge through a resource returns
  `resource_not_found` with the wire handle (`memory F:<uuid> not
  found`). The memory case previously surfaced as
  `invalid_request: "Forbidden: entry not found"`; existence stays
  undisclosed — not-exists and not-visible answer identically. Clients
  matching on the old error strings or codes must be updated
  (docs/14 §Resource errors).
- **`Protocol(NotFound)` tool errors shift `-32600` → `-32602`.** The
  new `NotFound` classification maps to `invalid_params` on the tool
  path (it was `invalid_request` when raised via engine protocol
  errors).
- **`core_membership:list_members` output is an envelope.** Was a bare
  array; now `{members, next_cursor, has_more}` with keyset pagination
  (default 50, max 200). `core_fact:facts_citing_object` keeps its
  envelope but gains `next_cursor`/`has_more` and a page cap; both
  accept `limit`/`cursor`.
- **`proxima://memory/{id}/lineage` paginates.** New `cursor` parameter
  and `next_cursor` output; the cursor is bound to memory + direction +
  depth. The output's `truncated` flag is renamed to `has_more` (`true`
  iff `next_cursor` is present) so every paginated surface speaks the
  same pagination vocabulary — clients reading `truncated` must switch.
  `depth=300` now clamps to 8 instead of erroring as "unknown
  resource". An empty walk for a missing/invisible start memory is now
  a `resource_not_found`, not an empty success.
- **New `proxima://memories{?ids}` batch read** (at most 100
  comma-separated prefixed ids): found memories in request order plus a
  `missing` list. Wake-candidate `hard_memories` hydration no longer
  needs one round trip per id.
- **Neighbor edges name their edge reference `edge`.** The
  `neighbor_edges` items returned by `core_search_memories` and
  `core_get_memory` used to carry the `E:<uuid>` reference under
  `handle`; it is now `edge`, matching `core_read_edges`, lineage, and
  change events. Clients reading `handle` must switch.
- **One idempotency-key contract on every write surface.** The memory
  append tools (`core_remember`, `core_record_utterance`,
  `core_derive`) now parse `idempotency_key` through the same type the
  goal tools use: trimmed, then 1..=180 chars (was untrimmed 1..=200).
  Keys longer than 180 chars are rejected, and a key with surrounding
  whitespace now dedups identically to its trimmed spelling — replays
  of such keys recorded before this release produce a new memory
  instead of an idempotent replay.
- **Arg ergonomics, non-breaking:** `title` on
  `core_remember`/`core_derive` widens to 240 chars, matching goal
  titles; the `core_search_memories` `kind` filter and the
  `core_membership` `relation` arg fold case like every other
  enum-like string arg; oversized `spaces`/`tags` filter lists on
  `core_search_memories` (over 16 entries) are rejected instead of
  fanned out.
- **Code flavor:** `proxima-code_list_repos` accepts `limit`/`cursor`
  and returns `{repos, next_cursor, has_more}` (was unbounded);
  `proxima-code_search_chunks` gains `has_more`;
  `proxima-code_search_commits` gains `commits_has_more` /
  `summaries_has_more`.
- Rust embedders: `MemoryInspectPort` gains `load_memories_by_ids`,
  `CitationPort::facts_citing_object` takes `after`/`limit` and returns
  `FactCitationPage`, `OwnerMembershipAdminPort` gains
  `list_group_members_page`, `Engine::list_members` takes
  `limit`/`after` and returns `GroupMemberPage`, and
  `MemoryLineageRequest`/`MemoryLineageResponse` carry
  `after`/`next_cursor`, and `Engine::backfill_fact_embeddings` returns
  `ProtocolError` instead of `StorageError` so an authorization denial
  keeps its `Forbidden` category. Custom port implementations must add
  the new methods; cursor plumbing is shared via
  `proxima_core::mcp::cursor`.

## 23. v0.0.7: `SearchProjection` and `MemorySearchProjection` gain `tsv_column`

Stored lexical vectors moved `to_tsvector` off the read path into STORED
generated columns (`0011_v007.sql`). A projection now declares whether its
sidecar has such a column, so both structs gained a field:

- `proxima_core::SearchProjection` (re-exported on the Flavor SDK tier as
  `proxima::flavor::SearchProjection`) gains
  `tsv_column: Option<&'static str>`.
- `proxima_core::MemorySearchProjection` gains `tsv_column: Option<String>`.

Neither is `#[non_exhaustive]` and neither derives `Default`, so
out-of-tree struct literals fail to compile with E0063 until the field is
added:

```rust
fn search_projection() -> Option<SearchProjection> {
    Some(SearchProjection {
        fields: &[/* ... */],
        tag_column: None,
        tsv_column: None, // <- add this
    })
}
```

**Use `None` unless you also add a stored column.** `None` keeps the
v0.0.6 behaviour exactly: the builder computes the vector inline from the
projected search text, through the same `proxima_core.lexical_tsv`
definition the generated columns use, so scoring is identical either way.

Set `tsv_column: Some("search_tsv")` only after your own sidecar migration
adds a matching column, which must be generated from the same
concatenation the projection emits:

```sql
ALTER TABLE my_flavor.my_sidecar
    ADD COLUMN search_tsv tsvector
    GENERATED ALWAYS AS (
        proxima_core.lexical_tsv(proxima_core.lexical_join(
            NULLIF(title, ''),
            NULLIF(body, ''),
            proxima_core.lexical_text_array(tags)))) STORED;
```

A `tsv_column` naming a column that does not exist surfaces as a Postgres
error on the first lexical or hybrid search against that schema, not at
boot — so exercise one search after adding it. Getting the generated
expression *wrong* is worse: it fails silently, scoring that sidecar
differently from every other. Pin it with a test in the shape of
`crates/storage-pg/tests/integration/search_pg/stored_tsv.rs`, which
asserts the stored column equals `lexical_tsv` over the projection's own
concatenation.

## 24. v0.0.7: `Engine::backfill_fact_embeddings` is now `backfill_missing_embeddings`

Renamed, and widened to match the name: it enqueues missing embeddings for
Facts **and** derived memories (Abstractions, Perspectives), owner-scoped and
idempotent as before.

```rust
// before
engine.backfill_fact_embeddings(&authz, &owner, limit).await?;
// after
engine.backfill_missing_embeddings(&authz, &owner, limit).await?;
```

The Fact-only filter was a real gap, not a naming detail. A flavor that
materializes derived memories through its own sidecar path — as
`proxima-code`'s repository ingest does for every `code-chunk-v1`
Abstraction — has no embedding client in scope at write time and enqueues
nothing. Those rows were then invisible to the owner-scoped backfill too, so
an indexed repository stayed lexically searchable and semantically empty
until an operator happened to run a *global* `maintain-embeddings` pass.

Custom `EmbeddingJobPort` implementations need no change: the port method
`enqueue_missing_embedding_jobs` keeps its name and signature. If yours
filters to Facts internally, widen it to match, or derived memories will
still be skipped.

## 25. v0.0.7: code indexes must be re-indexed; `proxima-code_erase_repo`

Two v0.0.7 changes alter how a repository is chunked and rendered, and neither
reaches an index that already exists.

- The chunker no longer drops comments. In the Rust grammar a doc comment is a
  *sibling* of the item it documents, and the merge step skipped comment nodes
  — so every `///` and `//!` block was excluded from the corpus. Measured over
  Proxima's own 444 indexed Rust files, chunk spans covered 95.3% of source
  bytes overall but only 14.2% of `flavors/code/src/migrations.rs` and 65.9%
  of `crates/core/src/llm.rs`: the loss landed exactly on the files that carry
  their reasoning in prose. After the fix, 99.2% and 99.9% respectively.
- `code-chunk-v1` renders as `path:start-end` plus the chunk body rather than
  the header alone, so chunk embeddings encode code instead of a file path.

A HEAD snapshot re-derives only files whose blob hash moved, so a repository
that has not changed keeps its old chunks, and files that never change keep
them permanently. That skip cannot simply be bypassed: a derived Abstraction
must carry the same `source_batch_id` as the Facts it was derived from, and
re-deriving an unchanged file would stamp new chunks with a batch its
already-receipted Fact does not belong to.

The supported path is therefore erase and re-ingest, which produces fresh
Facts in fresh batches — the model working as intended rather than around:

```
proxima-code_erase_repo   { repo_handle, confirm_canonical_path }
proxima-code_register_repo { path }
proxima-code_ingest_head_snapshot { repo_handle }
```

`proxima-code_erase_repo` is new in v0.0.7, and it is also the first supported
way to remove an indexed repository at all. The storage verb behind it has
existed and been tested since the code flavor shipped, but was reachable only
through `proxima_code::testkit`, which is `cfg(debug_assertions)` — in a
release build, `proxima-code_register_repo` upserts and keeps the cursor, so a
repository once indexed was permanent.

It deletes every Fact, Abstraction, edge, embedding, receipt, citation mapping
and cited object derived from that repository, and returns a receipt counting
each. It is irreversible and requires `confirm_canonical_path` to match the
repo's stored path exactly; a mismatch is rejected and changes nothing.

Budget for the re-embed. Chunk embeddings now cover real content, so the
queue after a re-ingest is proportional to corpus size rather than to a few
bytes per chunk: Proxima's own 620-file tree enqueues 4,083 jobs.

Rust embedders: `proxima_code::repos::erase_repo` lost its unused `schemas`
parameter.

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
