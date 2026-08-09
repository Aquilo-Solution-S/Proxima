# Public API Reference

## Current Consumption Mode

Workspace packages currently set `publish = false`; consume from git tags or
repo checkouts unless release notes say crates.io publishing is available.

## Supported Tiers

Post-PR9 supported Rust tiers:

| Tier | Import | Use |
|---|---|---|
| Host API | `use proxima::{Proxima, RuntimeBuilder, RuntimeConfig, Engine, CancellationToken};` | boot composed binaries; call graph/admin/projector verbs through server-resolved `AuthzContext` |
| Flavor SDK | `use proxima::flavor::{FlavorBundle, FlavorRegistry, FactPayload, pg_sidecar};` | build-time schemas, payload references, tools, sidecars |
| Flavor SDK (MCP tools) | `use proxima::flavor::{McpTool, McpToolCtx, McpToolError, McpToolErrorKind, McpToolAnnotations, McpActionArgSpec, McpAuthorContext};` | author flavor MCP tools without reaching into `proxima_core::mcp` — see [add-first-mcp-tool](../tutorials/add-first-mcp-tool.md) |
| Flavor SDK (authorized reads) | `use proxima::flavor::{authorized_memory_ids, authorized_fact_payloads, authorized_fact_payloads_include_tombstones, authorized_abstraction_payloads, authorized_code_chunk_head_candidates};` | typed, authz-filtered candidate/payload reads — see [Authorized Flavor-Read Facade](#authorized-flavor-read-facade) below |
| Flavor SDK (outbound endpoints) | `use proxima::flavor::{validate_endpoint_url, EndpointUrlPolicy};` | enforce HTTPS with the shared, exact loopback-only plaintext exception; never reproduce it with string prefixes |

Unsupported:

| Surface | Status |
|---|---|
| raw `sqlx::PgPool` | not stable Host API or Flavor SDK |
| aggregate `Storage` / `StorageHandle` | removed; Engine owns storage ports |
| `proxima-storage-pg` raw write verbs | backend API only; every owner write requires `OwnerWritePermit` minted by `Engine::authorize_owner_write` |
| flavor raw SQL against `proxima_core.*` | denied for every site. The [authorized flavor-read facade](#authorized-flavor-read-facade) replaced the last raw `flavors/code` reads against `proxima_core.*`; `scripts/check-architecture-guardrails.py`'s dated-exemption allowlist is empty, and any new raw `proxima_core.*` site in flavor code fails the guardrail (no temporary exemption path is open) |
| runtime plugin/tool/schema registration | denied; flavor composition is build-time |

## Owner External Keys

| OwnerRef | External key |
|---|---|
| `OwnerRef::World` | `world:00000000-0000-0000-0000-000000000001` |
| `OwnerRef::Personal(UserId)` | `personal:<uuid>` |
| `OwnerRef::Group(GroupId)` | `group:<uuid>` |

| Helper | Import | Contract |
|---|---|---|
| `OwnerRef::external_key()` | `proxima::OwnerRef` / `proxima_core::OwnerRef` | format the canonical runtime/API key |
| `parse_external_key(&str)` | `proxima::parse_external_key` / `proxima_core::parse_external_key` | parse only canonical keys; bare `world` is invalid |

## Owner Write Permit Boundary

| Item | Contract |
|---|---|
| `OwnerWritePermit` | sealed storage-tier proof: `(OwnerRef, AccessKind)`; constructor is not public |
| minting path | `Engine::authorize_owner_write(authz, owner, kind)` after server-resolved owner access |
| `AuthPath::System` | cannot mint by public `AuthzContext` shape alone; requires host-held `SystemAuthority` via `Engine::authorize_owner_write_with_system_authority` |
| host witness | `BuiltProxima::system_authority()` / `RunningProxima::system_authority()` expose a borrowed witness to embedding hosts |
| wire/flavor boundary | MCP tools and flavor `ToolCtx` do not receive `SystemAuthority`; normal membership/HostBearer paths need no witness |
| target-owner Fact ingest | supported host path: narrow a server-resolved context to one authorized owner with `AuthzContext::narrowed_to_owner(owner)`, then call `Engine::fact_ingest(&owner_authz, draft)`. The engine stamps the write owner from resolved access; `FactWriteCommand` carries no owner field. |
| sidecar-less Fact ingest | supported host path is `Engine::fact_ingest`; backend-only `proxima-storage-pg` helpers such as `ingest_fact_for_owner_plain` are not stable Host API or Flavor SDK. |
| guardrail | `scripts/check-architecture-guardrails.py` fails if listed storage write traits or `storage-pg` write verbs lose `OwnerWritePermit` |

Machine checks:

| Check | Command |
|---|---|
| import tiers | `cargo test -p proxima --test public_api_tiers --locked` |
| architecture ratchets | `python3 scripts/check-architecture-guardrails.py` |
| SQL policy ratchet | `python3 scripts/check-sql-policy.py` |
| schema-id allocation ledger | `python3 scripts/check-schema-ids.py` |
| registry conformance dump | `cargo test -p proxima --test registry_conformance` |

## Registry Conformance Dump

Consumer lockstep check:

1. Build a `FlavorRegistry::new()`.
2. Call `<YourAppOrBundle as FlavorBundle>::register(&mut registry)`.
3. Freeze with `registry.try_freeze()`.
4. Compare sorted schema `(id, version, kind, sidecar_table)`, sidecar tables, tool ids, and flavor ids.

`cargo test -p proxima --test registry_conformance` proves the hosted-app and
embedded-consumer registration paths produce the same deterministic dump.

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

## Compliance Export Host API

Public facade status:

| Type / verb | Import | Status |
|---|---|---|
| `ComplianceExportRequest` | `proxima::ComplianceExportRequest` | Host API DTO |
| `ComplianceExportTarget` | `proxima::ComplianceExportTarget` | Host API DTO |
| `ComplianceExportBundle` | `proxima::ComplianceExportBundle` | Host API DTO |
| `ComplianceExportCounts` | `proxima::ComplianceExportCounts` | Host API DTO |
| `ComplianceExportSidecarRows` | `proxima::ComplianceExportSidecarRows` | Host API DTO |
| `Engine::export_owner_bundle(authz, target)` | `proxima::Engine` | Host API verb |

Contract:

| Field | Rule |
|---|---|
| target | personal/group owner only; no World export target |
| authorization | `AuthPath::System` or `ComplianceAdminPort::may_perform_compliance_export`; default export authorization delegates to erase-family controller approval |
| legal hold | does not block export |
| drop proof | not required; export is non-destructive |
| rows | owner-scoped substrate rows, source cursors, registered sidecars, cited-object blob refs, and matching compliance audit rows |
| serialization | `ComplianceExportBundle::canonical_json_bytes()` emits recursively sorted-key JSON bytes |

## Embedding Ops Host API

Public facade status:

| Type / verb | Import | Status |
|---|---|---|
| `EmbeddingAnnObservability` | `proxima::EmbeddingAnnObservability` | Host API DTO |
| `EmbeddingJobBacklog` | `proxima::EmbeddingJobBacklog` | Host API DTO |
| `EmbeddingOrphanCounts` | `proxima::EmbeddingOrphanCounts` | Host API DTO |
| `EmbeddingOrphanSweepOutcome` | `proxima::EmbeddingOrphanSweepOutcome` | Host API DTO |
| `EmbeddingRecallCanary` | `proxima::EmbeddingRecallCanary` | Host API DTO |
| `Engine::embedding_ann_observability(authz)` | `proxima::Engine` | Host API verb |
| `Engine::sweep_orphan_embedding_rows(authz)` | `proxima::Engine` | Host API verb |

Contract:

| Field | Rule |
|---|---|
| authorization | `AuthPath::System` or `ComplianceAdminPort::may_perform_operator_maintenance`; ordinary owner read/admin roles are insufficient |
| scope | owner-agnostic operational reads over embedding infrastructure |
| observability | rows, relation bytes, HNSW bytes, job backlog, stale processing jobs, orphan rows, recall canary |
| orphan sweep | deletes embeddings, heads, and jobs whose source `memories` / `goals` row no longer exists |
| compliance erase | not dependent on sweep; erase deletes embedding infra synchronously at transaction commit |
| graph authority | embeddings remain engine infrastructure; similarity never authors a connection |

## Cited-Blob Reconcile Host API

Public facade status:

| Type / verb | Import | Status |
|---|---|---|
| `CitedBlobReconcileOutcome` | `proxima::CitedBlobReconcileOutcome` | Host API DTO |
| `CitedBlobMissingObject` | `proxima::CitedBlobMissingObject` | Host API DTO |
| `MAX_RECONCILE_SAMPLE` | `proxima::MAX_RECONCILE_SAMPLE` | Host API constant |
| `CitedBlobStore::reconcile_cited_blobs()` | `proxima::CitedBlobStore` (via `AppContext`/`BuiltProxima`/`RunningProxima`/`EmbeddedProxima`'s `.blobs` field) | Host API verb |

`reconcile_cited_blobs` lists the configured bucket, streams the
`cited_uploaded_blob_v1` rows, and diffs them: rows with no object are
`missing_objects` (a citation that cannot be resolved — `is_intact()` is
false whenever this is non-zero), objects with no row are `orphan_objects`
(cost and retention, not correctness), and rows naming another bucket or a
key outside the canonical prefix are `foreign_locators`. Each count carries
a bounded sample (`missing_sample`/`orphan_sample`/`foreign_sample`, capped
at `MAX_RECONCILE_SAMPLE`) for an operator to recognise the cause without
being handed an unbounded work queue. It reports; it does not repair or
delete.

## Consumer Projector Guidance

Rules for a downstream projector (a host process that writes derived
evidence/activity Facts into Proxima on a tenant's behalf, e.g. an
execution or activity log projector):

| Rule | Contract |
|---|---|
| owner | write under the tenant's `OwnerRef::Group(GroupId)`, not `OwnerRef::Personal(UserId)`. Tenant-shared evidence belongs to the group the tenant's members can read/manage together, not to one operator's personal owner. |
| target-owner ingest | resolve the worker subject to roles, narrow that `AuthzContext` to the tenant `OwnerRef::Group(GroupId)` with `AuthzContext::narrowed_to_owner`, then call `Engine::fact_ingest`. A context that still resolves more than one writable owner is rejected before storage. |
| idempotency keys | Proxima honors a caller-supplied idempotency key verbatim — it never invents a different projector-side key. `core_remember`'s `idempotency_key` deterministically becomes the note id via UUIDv5 over the caller's own bytes (`crates/core/src/mcp/core_tools/memory/remember.rs`); other Fact payload schemas declare their own `natural_key_columns()` from caller-supplied payload fields. Re-ingesting the same key with the same content is a no-op; re-ingesting the same key with changed content writes a new version and advances the head pointer — the identity a projector chooses is the identity Proxima keeps. |
| source cursor bytes | `Cursor` is opaque byte state keyed by `(owner, source)`. A projector may encode `last_event_seq` into it; `store_source_cursor` persists the supplied bytes verbatim, and `load_source_cursor` returns the exact bytes last stored for that owner/source. No Centauri-side `piy_projection_cursor` table is required for that state. |
| projection lag | `Engine::source_cursor_age(authz, owner, source)` returns the age of the owner/source cursor for EVD-012-style lag SLO evidence. It is owner-scoped and read-authorized (`Viewer`); `load_source_cursor` / `store_source_cursor` still require cursor mutation authority (`Ingest`) and do not expose cursor bytes to viewers. |
| World publish | reserve `publish_to_world` for deliberate public catalog/trust facts only — never for private execution/activity evidence. Publishing is an irreversible owner **transfer**, not an ACL flag: once transferred, `authorize_write`'s World short-circuit means the row is never a write owner again, and `ComplianceEraseTarget::WorldOwner` is always refused (see above). A published memory or goal permanently exits the personal/group compliance-erase reach — there is no path back through compliance erase if content published this way later turns out to need it. Treat `publish_to_world` as a one-way decision reserved for content the tenant intends to be permanently, publicly, and inerasably visible. |

See [14 Protocol Surface — `core_publish`](../14-protocol-surface.md)
for the `publish_to_world` action itself.

## Embeddings

Embedding contract:

| Item | Contract |
|---|---|
| host wiring | host injects `proxima::llm::EmbeddingClient`; no inference target registry |
| entity tables | no FK from entity rows to embeddings |
| write semantics | re-embedding appends a new `(entity_kind, entity_id, embedding_version, model_id)` row |
| latest pointer | `embedding_heads` metadata, rebuildable from `embeddings` |
| graph authority | similarity is query-time evidence only; embeddings never author a connection |

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
