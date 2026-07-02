# Public API Reference

## Current Consumption Mode

Workspace packages currently set `publish = false`; consume from git tags or
repo checkouts unless release notes say crates.io publishing is available.

## Supported Tiers

Post-PR9 supported Rust tiers:

| Tier | Import | Use |
|---|---|---|
| Host API | `use proxima::{Proxima, RuntimeBuilder, RuntimeConfig, Engine};` | boot composed binaries; call graph/admin verbs through server-resolved `AuthzContext` |
| Flavor SDK | `use proxima::flavor::{FlavorBundle, FlavorRegistry, FactPayload, pg_sidecar};` | build-time schemas, relations, tools, sidecars |
| Flavor SDK (authorized reads) | `use proxima::flavor::{authorized_memory_ids, authorized_fact_payloads, authorized_fact_payloads_include_tombstones, authorized_abstraction_payloads, authorized_code_chunk_head_candidates};` | typed, authz-filtered candidate/payload reads — see [Authorized Flavor-Read Facade](#authorized-flavor-read-facade) below |

Unsupported:

| Surface | Status |
|---|---|
| raw `sqlx::PgPool` | not stable Host API or Flavor SDK |
| aggregate `Storage` / `StorageHandle` | removed; Engine owns storage ports |
| `proxima-storage-pg` raw write verbs | backend API only; every owner write requires `OwnerWritePermit` minted by `Engine::authorize_owner_write` |
| flavor raw SQL against `proxima_core.*` | denied for every site. The [authorized flavor-read facade](#authorized-flavor-read-facade) replaced the last raw `flavors/code` reads against `proxima_core.*`; `scripts/check-architecture-guardrails.py`'s dated-exemption allowlist is empty, and any new raw `proxima_core.*` site in flavor code fails the guardrail (no temporary exemption path is open) |
| runtime plugin/tool/schema registration | denied; flavor composition is build-time |

## Owner Write Permit Boundary

| Item | Contract |
|---|---|
| `OwnerWritePermit` | sealed storage-tier proof: `(OwnerRef, AccessKind)`; constructor is not public |
| minting path | `Engine::authorize_owner_write(authz, owner, kind)` after server-resolved owner access |
| `AuthPath::System` | cannot mint by public `AuthzContext` shape alone; requires host-held `SystemAuthority` via `Engine::authorize_owner_write_with_system_authority` |
| host witness | `BuiltProxima::system_authority()` / `RunningProxima::system_authority()` expose a borrowed witness to embedding hosts |
| wire/flavor boundary | MCP tools and flavor `ToolCtx` do not receive `SystemAuthority`; normal membership/HostBearer paths need no witness |
| guardrail | `scripts/check-architecture-guardrails.py` fails if listed storage write traits or `storage-pg` write verbs lose `OwnerWritePermit` |

Machine checks:

| Check | Command |
|---|---|
| import tiers | `cargo test -p proxima --test public_api_tiers --locked` |
| architecture ratchets | `python3 scripts/check-architecture-guardrails.py` |
| SQL policy ratchet | `python3 scripts/check-sql-policy.py` |

## Authorized Flavor-Read Facade

`proxima::flavor::{authorized_memory_ids, authorized_fact_payloads,
authorized_fact_payloads_include_tombstones, authorized_abstraction_payloads,
authorized_code_chunk_head_candidates}` give flavor crates typed,
owner/World-authorized candidate filtering and payload projection without
ever holding a raw `sqlx::PgPool` or writing SQL against `proxima_core.*`
themselves.

| Property | Contract |
|---|---|
| authorization path | every helper routes candidate filtering through `proxima_core::Engine::query` — the same owner/group-scoped-plus-World-readable authz path used by every other read |
| shape | narrow a caller-supplied candidate id list down to the visible/typed subset; never a full unauthorized scan |
| bound | the `Engine::query`-backed helpers (`authorized_memory_ids` and the payload fetchers) deduplicate and cap candidate lists at 2,000 ids before they ever reach a query, so a pathological caller cannot force an unbounded `IN (...)`/`ANY($1)` scan. `authorized_code_chunk_head_candidates` is deliberately different: it deduplicates the full input and evaluates EVERY candidate in 2,000-sized batches (bounding each SQL round-trip) without truncating — code-chunk memory ids are deterministic UUIDv5 content hashes, so no truncated window could guarantee the true head survives; silent truncation there would be a correctness bug, not a cap |
| supersession/tombstones | heads-only by default; `authorized_fact_payloads_include_tombstones` also surfaces tombstoned heads (a caller-visible "this was deleted" state, distinct from entity-level compliance tombstoning) |
| the one exception | `authorized_code_chunk_head_candidates` still touches `proxima_core.*` SQL, but from `proxima-storage-pg` (a backend-owned storage adapter, not flavor code) — `AbstractionPayload` has no natural-key/supersession concept to ride on `Engine::query`'s heads-only mode today. It only narrows a candidate id list before the caller's own `authorized_abstraction_payloads` call decides real visibility, so running it without an owner-exact-match restriction is safe by construction |

Source: `crates/proxima/src/flavor/authorized_read.rs`.

## Compliance Erase Host API

Public facade status:

| Type | Import | Status |
|---|---|---|
| `ComplianceEraseRequest` | `proxima::ComplianceEraseRequest` | Host API DTO |
| `ComplianceEraseTarget` | `proxima::ComplianceEraseTarget` | Host API DTO |
| `ComplianceEraseOutcome` | `proxima::ComplianceEraseOutcome` | Host API DTO |
| `ComplianceEraseRefusal` | `proxima::ComplianceEraseRefusal` | Host API DTO |
| `ComplianceEraseCounts` | `proxima::ComplianceEraseCounts` | Host API DTO |

Callers submit requests and inspect outcomes. Callers do not provide
`operation_id`, requester, auth path, request time, audit context, or
abandonment witnesses. Engine derives audit identity from `AuthzContext`,
verifies personal-owner drop proof before minting sealed erase authorization,
and storage rechecks group abandonment in-transaction before hard deletion.
All storage erase paths require sealed `EraseAuthorization` (see
[13 Compliance](../13-compliance.md) and
[14 Compliance Admin Surface](../14-protocol-surface.md#compliance-admin-surface)).

Legal/security holds are host-side owner config:

| Engine verb | Effect |
|---|---|
| `set_legal_hold(authz, owner)` | idempotently activates a per-owner hold; requires compliance-erase operator approval plus owner `Admin` write authority |
| `get_legal_hold(authz, owner)` | returns the active hold flag; requires owner `Admin` |
| `clear_legal_hold(authz, owner)` | clears the hold and returns whether a row existed; requires compliance-erase operator approval plus owner `Admin` write authority |

`set_legal_hold` / `clear_legal_hold` also require an owner `Admin` write
permit for the target owner; compliance authority alone is not an owner write
grant.

While active, the hold suspends substantive owner-memory physical destruction
for exactly the current compliance `erase_*` family. The four destructive
owner/source erase paths return `ComplianceEraseOutcome::Refused { reason:
ComplianceEraseRefusal::LegalHoldActive, .. }` and delete no substantive owner
memory content. `erase_world_owner` remains refusal-only with `WorldOwner`;
reads, ordinary writes, and transient `proxima_core.embedding_jobs`
work-queue consumption are unchanged. Future physical-destruction paths must
inherit the same storage-transaction gate before they can exist.
Operators own the legal judgment; Proxima guarantees only the mechanics.

`ComplianceEraseTarget` has no `World` variant: `WorldOwner` exists only as
a target that is always refused and audited (`crates/core/src/compliance.rs`).
Personal- and group-scoped erasure never reaches a `World`-owned row. See
[Consumer Projector Guidance](#consumer-projector-guidance) below for why
that matters when deciding what to publish.

## Consumer Projector Guidance

Rules for a downstream projector (a host process that writes derived
evidence/activity Facts into Proxima on a tenant's behalf, e.g. an
execution or activity log projector):

| Rule | Contract |
|---|---|
| owner | write under the tenant's `OwnerRef::Group(GroupId)`, not `OwnerRef::Personal(UserId)`. Tenant-shared evidence belongs to the group the tenant's members can read/manage together, not to one operator's personal owner. |
| idempotency keys | Proxima honors a caller-supplied idempotency key verbatim — it never invents a different projector-side key. `core_remember`'s `idempotency_key` deterministically becomes the note id via UUIDv5 over the caller's own bytes (`crates/core/src/mcp/core_tools/memory/remember.rs`); other Fact payload schemas declare their own `natural_key_columns()` from caller-supplied payload fields. Re-ingesting the same key with the same content is a no-op; re-ingesting the same key with changed content writes a new version and advances the head pointer — the identity a projector chooses is the identity Proxima keeps. |
| World publish | reserve `publish_to_world` for deliberate public catalog/trust facts only — never for private execution/activity evidence. Publishing is an irreversible owner **transfer**, not an ACL flag: once transferred, `authorize_write`'s World short-circuit means the row is never a write owner again, and `ComplianceEraseTarget::WorldOwner` is always refused (see above). A published memory permanently exits the personal/group compliance-erase reach — there is no path back through compliance erase if content published this way later turns out to need it. Treat `publish_to_world` as a one-way decision reserved for content the tenant intends to be permanently, publicly, and inerasably visible. |

See [14 Protocol Surface — `core_membership`](../14-protocol-surface.md)
for the `publish_to_world` action itself.

## Embeddings

Embedding contract:

| Item | Contract |
|---|---|
| host wiring | host injects `proxima::llm::EmbeddingClient`; no inference target registry |
| entity tables | no FK from entity rows to embeddings |
| write semantics | re-embedding appends a new `(entity_kind, entity_id, embedding_version, model_id)` row |
| latest pointer | `embedding_heads` metadata, rebuildable from `embeddings` |
| graph authority | similarity is query-time evidence only; embeddings never author edges |

See [07 Vector Store - Independent](../07-storage.md#vector-store--independent).

## Generated Rustdoc

Build locally:

```sh
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked --open
```

CI treats rustdoc warnings as failures.

## Local API Diff Evidence

Install `cargo-public-api` outside tracked source if missing:

```sh
cargo install cargo-public-api --locked --root /tmp/codex-cargo-public-api
```

Generate ignored snapshots:

```sh
mkdir -p .local/architecture-restoration/api
cargo +nightly public-api -p proxima --all-features > .local/architecture-restoration/api/pr9-proxima-public-api.txt
cargo +nightly public-api -p proxima-core --all-features > .local/architecture-restoration/api/pr9-proxima-core-public-api.txt
cargo +nightly public-api -p proxima-storage-pg --all-features > .local/architecture-restoration/api/pr9-proxima-storage-pg-public-api.txt
cargo +nightly public-api -p proxima-code --all-features > .local/architecture-restoration/api/pr9-proxima-code-public-api.txt
```

Summarize reviewer evidence in:

```text
.local/architecture-restoration/pr9-public-api-diff.md
```

Do not track generated API snapshots unless a release process requests a
baseline.
