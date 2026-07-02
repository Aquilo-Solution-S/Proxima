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
    .owner(default_owner)              // still required; see docs/01-event-source.md#owner-resolution--the-hosts-trust-boundary
    .authenticator(Arc::new(authenticator))
    .with_mcp()
    .run()
    .await?;
```

Multi-audience composition (branching on `aud` to run more than one
identity class) is just running several `OidcTokenValidator`s that share
one `KeyResolver` and shaping your own `AuthzContext` from
`ValidatedOidcClaims` — see `crates/auth-oidc/tests/custom_host_validation.rs`
and the module doc on `crates/auth-oidc/src/authenticator.rs` for the
worked pattern; that surface didn't change shape in v0.0.5, only the
default `OidcAuthenticator::new` path did.

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

## 5. Single-owner hosts: the smallest correct recipe

For a genuinely single-tenant host — every accepted token maps to one
fixed `Owner`, no per-subject resolution — skip `OidcSubjectMap` and
`OwnerAccessPort` entirely and use `OidcAuthenticator::single_owner`. This
is the mechanical replacement for a fixed-owner `OidcAuthConfig { owner,
.. }` construction (e.g. a legacy `oidc_from_env`):

```rust
use std::sync::Arc;
use proxima::{Owner, UserId};
use proxima_auth_oidc::{HttpJwksResolver, OidcAuthConfig, OidcAuthenticator};

fn single_owner_authenticator(
    issuer: String,
    audience: String,
    owner_user_id: uuid::Uuid,
) -> Result<OidcAuthenticator, Box<dyn std::error::Error>> {
    let config = OidcAuthConfig {
        issuer: issuer.clone(),
        jwks_uri: None,
        audience,
        allowed_subjects: None,
        leeway_secs: 60,
    };
    let keys = Arc::new(HttpJwksResolver::new(issuer, config.jwks_uri.clone())?);
    let owner = Owner::Personal(UserId::new(owner_user_id));
    Ok(OidcAuthenticator::single_owner(config, keys, owner)?)
}
```

**`owner` must be `Owner::Personal`.** A `Group` owner is accepted at
construction (matching the pre-split behavior byte-for-byte) but every
`authenticate()` call then fails closed with `InvalidCredentials` — this
is not a compile error, it silently rejects every request. If your fixed
owner is a group/company, use the host-resolved path in §4 with a
single-entry `OidcSubjectMap` instead.

This shape is exercised end to end by
`crates/auth-oidc/src/authenticator.rs::tests::single_owner_authenticates_one_subject_against_fixed_owner`.

## 6. `proxima-storage-pg` raw write API narrowed (no consumer-visible change expected)

These were never part of the supported Host API or Flavor SDK tiers (see
[public-api.md](docs/reference/public-api.md#supported-tiers)), but if
something depended on them anyway:

| Symbol | Was | Now |
|---|---|---|
| `verbs::fact_embeddings::insert_embedding` | `pub` | `pub(crate)` — use the proof-gated `EmbeddingWritePort` |
| `verbs::fact_embeddings::insert_memory_embedding` | `pub` | `pub(crate)` — use the proof-gated `EmbeddingWritePort` |
| `verbs::fact_embeddings::insert_fact_embedding` / `upsert_fact_embedding` / `upsert_memory_embedding` / `insert_goal_embedding` | `pub` | deleted (zero remaining callers; use the proof-gated port) |
| `verbs::fact_ingest::ingest_fact_command_in_tx` | `pub` | `pub(crate)` |
| `verbs::fact_ingest::ingest_fact_with_derived_sidecar_in_tx` | `pub` | `pub(crate)` |

`ingest_fact` and `ingest_fact_in_tx` (`crates/storage-pg/src/verbs/fact_ingest.rs`)
are unchanged and still `pub` — they're the supported low-level entry
points if you're writing storage-backend code, not flavor code. Flavor and
host code should go through `Engine::fact_ingest` / the facade instead.

## 7. Flavor authors: raw SQL against `proxima_core.*` is guardrail-denied

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

## 8. Owner-transfer: `core_membership:publish_to_world`

Publishing an entity is now an owner **transfer** to `OwnerRef::World`
(`Engine::publish_to_world`), not an ACL flag or a share row. Published
entities become readable by everyone and writable by no one; re-publishing
an already-World entity fails closed with `Forbidden` (the current-owner
lookup resolves to World, which `authorize_write` never accepts). If a
consumer previously modeled "publish" as a copy or a grant, switch it to
the `core_membership:publish_to_world` MCP action / `Engine::publish_to_world`.

## 9. Lock-step version bump

Every Proxima crate this host depends on (`proxima`, `proxima-core`,
`proxima-storage-pg`, `proxima-auth-oidc`, and any flavor crates) moves
together — there is no supported skew between them across a tag. Bump all
of them in the same commit, then run the checks in this file before
merging.

## 10. Lean consumers

If a downstream package requires `docs/lean` as `causa` (e.g. a
`kernel/lakefile.toml` with `require causa rev=...`), bump `rev` in the
same commit as the Cargo tag bump — a Proxima tag bump is a dual
Rust+Lean bump, never just one.

Before bumping `rev`, run `python3 scripts/check-lean-axioms.py` — it
rebuilds `docs/lean` itself and diffs the kernel's current axiom set
against the checked-in allowlist at `scripts/lean-axioms.allowlist.txt`
(Task 8 of this hardening pass). A silent axiom-set change must never be
absorbed into a downstream kernel unnoticed — if the script reports a
diff, that's a stop-and-review signal before the rev bump, not a rubber
stamp.

## Checks before calling an upgrade done

```sh
cargo test -p proxima --lib
cargo test -p proxima-storage-pg --lib
cargo check -p proxima-dev-migrate
cargo clippy -p proxima -p proxima-dev-migrate --all-targets -- -D warnings
python3 scripts/check-architecture-guardrails.py
python3 scripts/check-sql-policy.py
```
