# Public API Reference

## Current Consumption Mode

Workspace packages currently set `publish = false`; consume from git tags or
repo checkouts unless release notes say crates.io publishing is available.

## Supported Tiers

Post-PR9 supported Rust tiers:

| Tier | Import | Use |
|---|---|---|
| Host API | `use proxima::{Proxima, RuntimeBuilder, RuntimeConfig, Engine, CancellationToken, AccessKind, AccessCeiling, OwnerRoles};` | boot composed binaries; call graph/admin/projector verbs through server-resolved `AuthzContext`. `Role::new` / `Role::may_write` / `OwnerRoles::for_subject` name `AccessKind`, `AccessCeiling`, `AccessError`, `OwnerRoles` |
| Host extra-table | `AppContext::{clone_pool_for_host, pg_tuning_for_host}` | host `FlavorApp::services` only: wrap the pool and resolved query policy in a flavor-owned store immediately. Tools resolve the store via `FlavorServices`. Not Flavor SDK. No `proxima_core.*` SQL |
| Host API (REST OpenAPI) | `use proxima::host::build_openapi_document;` | build the complete registry document with the same generator as `/v1/openapi.json` without depending on `proxima-mcp-server` internals; requires feature `rest` |
| Flavor SDK | `use proxima::flavor::{FlavorBundle, FlavorRegistry, FactPayload, pg_sidecar, InlineCitedObjectDraft, InlineCitationMappingDraft};` | build-time schemas, payload references, tools, sidecars. Typed citation drafts + `AuthorizedFactWithCitation{,Ref}` are nameable here; `Engine` stays Host API |
| Flavor SDK (services) | `use proxima::flavor::{FlavorServices, FlavorServiceError};` | return typed services from `FlavorApp::services`; tuple composition rejects duplicate concrete types and shares one set with MCP, REST, and workers |
| Flavor SDK (generic tools) | `use proxima::flavor::{Tool, ToolCtx, ToolCaller, ToolError};` | author transport-neutral tools; MCP and REST populate optional caller provenance directly on `ToolCtx` |
| Flavor SDK (MCP tools) | `use proxima::flavor::{McpTool, McpToolCtx, McpToolError, McpToolErrorKind, McpToolAnnotations, McpActionArgSpec, McpAuthorContext};` | author flavor MCP tools without reaching into `proxima_core::mcp` — see [add-first-mcp-tool](../tutorials/add-first-mcp-tool.md) |
| Flavor SDK (authorized reads) | `use proxima::flavor::{authorized_memory_ids, authorized_fact_payloads, authorized_abstraction_payloads, SidecarAtom, QueryRequest, hybrid_degraded_to_lexical};` | typed, authz-filtered candidate/payload reads — see [Authorized Flavor-Read Facade](#authorized-flavor-read-facade) below. `Engine` is Host API (`use proxima::Engine`). Code-series `&PgPool` helpers live in `flavors/code`, not this SDK. |
| Flavor SDK (outbound endpoints) | `use proxima::flavor::{validate_endpoint_url, EndpointUrlPolicy};` | enforce HTTPS with the shared, exact loopback-only plaintext exception; never reproduce it with string prefixes |

Unsupported:

| Surface | Status |
|---|---|
| raw `sqlx::PgPool` on Flavor SDK / tools | denied. The Host extra-table bridge is `AppContext::{clone_pool_for_host, pg_tuning_for_host}` (see below) |
| aggregate `Storage` / `StorageHandle` | removed; Engine owns storage ports |
| `proxima-storage-pg` raw write verbs | backend API only; every owner write requires `OwnerWritePermit` minted by `Engine::authorize_owner_write` |
| flavor raw SQL against `proxima_core.*` | denied for every site. The [authorized flavor-read facade](#authorized-flavor-read-facade) replaced the last raw `flavors/code` reads against `proxima_core.*`; `scripts/check-architecture-guardrails.py`'s dated-exemption allowlist is empty, and any new raw `proxima_core.*` site in flavor code fails the guardrail (no temporary exemption path is open) |
| runtime plugin/tool/schema registration | denied; flavor composition is build-time |

## Owner External Keys

| OwnerRef | External key |
|---|---|
| `OwnerRef::Personal(UserId)` | `personal:<uuid>` |
| `OwnerRef::Group(GroupId)` | `group:<uuid>` |

Personal and Group are the only owner kinds. Every owner carries a UUID —
there is no id-less owner — so `OwnerRef::columns()` returns
`(OwnerRefKind, Uuid)` and `OwnerRefKind::with_uuid(Uuid)` is total.

| Helper | Import | Contract |
|---|---|---|
| `OwnerRef::external_key()` | `proxima::OwnerRef` / `proxima_core::OwnerRef` | format the canonical runtime/API key |
| `parse_external_key(&str)` | `proxima::parse_external_key` / `proxima_core::parse_external_key` | parse only canonical `personal:`/`group:` keys; any other prefix or a bare kind is invalid |

## Owner Write Permit Boundary

| Item | Contract |
|---|---|
| `OwnerWritePermit` | sealed storage-tier proof: `(OwnerRef, AccessKind)`; constructor is not public |
| minting path | `Engine::authorize_owner_write(authz, owner, kind)` after server-resolved owner access |
| `AuthPath::System` | cannot mint by public `AuthzContext` shape alone; requires host-held `SystemAuthority` via `Engine::authorize_owner_write_with_system_authority` |
| host witness | `BuiltProxima::system_authority()` / `RunningProxima::system_authority()` expose a borrowed witness to embedding hosts |
| wire/flavor boundary | MCP tools and flavor `ToolCtx` do not receive `SystemAuthority`; normal membership/HostBearer paths need no witness |
| target-owner Fact ingest | supported host path: narrow a server-resolved context to one authorized owner with `AuthzContext::narrowed_to_owner(owner)`, then call `Engine::fact_ingest(&owner_authz, draft)`. The engine stamps the write owner from resolved access; `FactWriteCommand` carries no owner field. |
| sidecar-less Fact ingest | supported host path is `Engine::fact_ingest`. `proxima-storage-pg`'s write verbs are `pub(crate)` implementation detail of its port impls — there is no second entry point to reach past the engine with. |
| guardrail | `scripts/check-architecture-guardrails.py` fails if listed storage write traits or `storage-pg` write verbs lose `OwnerWritePermit` |

## Delegated Worker Authority

Supported facade:

| Type | Import | Contract |
|---|---|---|
| `DelegationId` | `proxima::*` / `proxima::flavor::*` | redeemable queue handle; persist it as a credential and do not log/export it |
| `DelegatedCommand` | `proxima::*` / `proxima::flavor::*` | canonical registered flat tool or exact dispatcher action; parsing delegates to `GoalWakeToolId` |
| `DelegationIssued` | `proxima::*` / `proxima::flavor::*` | `{ id, expires_at }` returned after HostBearer issuance |
| `DelegatedAuthorityService` | `proxima::*` / `proxima::flavor::*` | shared runtime service: `issue`, `redeem_phase`, `revoke`; absent when no authenticator is configured |
| `DelegatedPhase` | `proxima::*` / `proxima::flavor::*` | opaque, non-cloneable, non-serializable authority for one claimed phase |
| `EngineAuthority` | `proxima::*` / `proxima::flavor::*` | sealed argument trait implemented only by `AuthzContext` and `DelegatedPhase` |

Queue redemption checks exact owner/id/command, current registry and deployment
tool profile, grant revocation/expiry, current owner membership and recorded role
ceiling, and `current_auth_epoch` when the host authenticator implements epoch
revocation. The built-in OIDC authenticators currently use epoch `0`; bearer
expiry and current membership are their production revocation bounds.

After redemption, each delegated-capable Engine/blob operation checks the
same-runtime binding, exact owner/role ceiling, and finite expiry. A later
revoke, epoch bump, or membership change denies the next redemption; it does
not cancel an already-redeemed phase. Redeem at job claim and every phase
boundary. The exact command binds issuance, queue routing, and redemption; the
linked worker implementation remains trusted to choose among the allowed
operations. This is not an in-process sandbox.

Delegated-capable operations are closed and explicit:

| Surface | Delegated-capable operation |
|---|---|
| Engine Fact | `fact_ingest` |
| Engine Fact split write | `authorize_fact_ingest` → `ingest_fact_with_typed_sidecar`; the returned witness rechecks runtime binding and expiry at commit |
| Engine inline citation Fact | `authorize_fact_with_citation` → `ingest_fact_with_citation_and_typed_sidecar`; commit rechecks the witness |
| Engine cited-object-reference Fact | `authorize_fact_with_citation_by_ref` → `ingest_fact_with_citation_ref_and_typed_sidecar`; commit rechecks the witness |
| Engine derived memory | `author_derived_authorized` |
| Engine upload completion | `complete_upload_as_fact` |
| `CitedBlobService` | `prepare_upload`, `stage_upload`, `finish_upload`, `abort_upload`, `read_url`, `find_held_blobs` |
| `CitedBlobReadService` | `collect_verified` |

Every other Engine/service API rejects a raw
`AuthzContext { auth_path: Delegated, .. }`; notably query, owner-inverse/admin,
and owner reconciliation are not delegated-capable. `CitedBlobService`,
`CitedBlobReadService`, and `CitedBlobOwnerReconcileService` keep their backend
ports private. Direct `proxima-core` hosts are the trusted composition root and
can extract runtime authorities; the standard `proxima` boot path extracts and
withholds the delegation runtime authority from MCP tools, REST tools, and
workers.

Direct `CitedBlob*Port` or concrete-backend calls are trusted, unsupported
adapter/composition seams. Delegated workers must use the runtime-bound
`CitedBlobService` and `CitedBlobReadService` wrappers.

`DelegationGrant`, `DelegationGrantStorage`, `DelegationMutationPermit`,
`DelegationStorePort`, `DelegatedAuthorityService::new`, and
`PgDelegationStore` are doc-hidden backend composition/persistence APIs, not
supported Host API or Flavor SDK.

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
authorized_abstraction_payloads}`
take `&Engine`, not `&PgPool`. They give flavor crates typed,
owner-authorized candidate filtering and payload projection without
writing SQL against `proxima_core.*`. Code-chunk ANN / file-revision head
helpers that need a pool stay in `flavors/code` (backend-owned).

| Property | Contract |
|---|---|
| authorization path | every helper routes candidate filtering through `proxima_core::Engine::query` — the same owner/group-scoped authz path used by every other read |
| shape | narrow a caller-supplied candidate id list down to the visible/typed subset; never a full unauthorized scan |
| bound | helpers deduplicate and cap candidate lists at 2,000 ids before they ever reach a query, so a pathological caller cannot force an unbounded `IN (...)`/`ANY($1)` scan |
| versions | heads-only (`memory_head`); a flavor-defined tombstone payload is itself a hot head and remains visible |

`Engine::owned_series_handle` looks up the current owned handle by
sidecar columns. It takes `Engine` + `AuthzContext`, not `PgPool`. After
`transfer_to_owner` the prior owner misses and mints. Code-chunk ingest
lists one file's series via `owned_chunk_series_heads` (store /
`code_series_heads`), not N Engine NK lookups.

`GoalRow` projects `assignment` and `evidence`. `QueryRequest` can
narrow Goals by `assignment` and `evidence_contains`.

Source: `crates/proxima/src/flavor/authorized_read.rs`,
`Engine::owned_series_handle`, `GoalRow`.

## Owner Erase Host API

Public facade status:

| Type | Import | Status |
|---|---|---|
| `OwnerEraseRequest` | `proxima::OwnerEraseRequest` | Host API DTO |
| `OwnerEraseTarget` | `proxima::OwnerEraseTarget` | Host API DTO |
| `OwnerEraseOutcome` | `proxima::OwnerEraseOutcome` | Host API DTO |
| `OwnerEraseRefusal` | `proxima::OwnerEraseRefusal` | Host API DTO |
| `OwnerEraseCounts` | `proxima::OwnerEraseCounts` | Host API DTO |

| Engine verb | Scope |
|---|---|
| `erase_group_owner(authz, group_id)` | one group owner |
| `erase_personal_owner(authz, user_id, drop_event_id)` | one personal owner |
| `erase_group_source_scope(authz, group_id, source_id)` | one source inside a group owner |
| `erase_personal_source_scope(authz, user_id, source_id, drop_event_id)` | one source inside a personal owner |

Callers submit requests and inspect outcomes. Callers do not provide
`operation_id`, requester, auth path, request time, audit context, or
abandonment witnesses. Engine derives the operation identity from
`AuthzContext`, verifies personal-owner drop proof before minting a sealed
`EraseAuthorization`, and storage rechecks group abandonment in-transaction
under the membership lock before hard deletion.

`OwnerEraseCounts` is a name→count map, not a fixed struct: its key set is
exactly the `counter` names the frozen flavor contracts declare, seeded to
zero before the first delete. A flavor that declares a new counter gets it in
the receipt without a change here.

Core keeps no record of the operation. There is no audit table, no retention
window and no legal hold — the last two were removed outright, because a
retention schedule and a litigation hold are judgements about a hosting
application's obligations rather than facts about a store. A host that owes
its users an erasure right calls these verbs when its own rules say to, and
records the returned receipt if its own rules say to. See
[13 The Inverses of Storing](../13-compliance.md) and
[14 Compliance Admin Surface](../14-protocol-surface.md#compliance-admin-surface).

`OwnerEraseTarget` is personal/group only
(`crates/core/src/owner_inverse.rs`). Every row has a personal or group
owner, so every row is within some owner's erase reach; a transfer moves that
reach to the destination owner. See
[Consumer Projector Guidance](#consumer-projector-guidance) below for what
that means when deciding where to send a memory.

## Who may erase — the provider seam

`OwnerEraseAuthorityPort` is the seam, and the only place the question is
asked.

| Method | Contract |
|---|---|
| `may_erase_owner(authz, target)` | yes/no for one target; no reason, no deadline, no policy |
| `may_export_owner(authz, target)` | defaults to asking the erase question; override for a looser portability rule |
| `may_perform_operator_maintenance(authz)` | defaults to `false`; gates the owner-agnostic maintenance verbs |

Wiring nothing is a valid deployment and refuses every erase and every
export: fail-closed, because the failure mode of guessing wrong is
unrecoverable. `AuthPath::System` bypasses the port; `AuthPath::Delegated`
can never reach it.

## Owner Export Host API

Public facade status:

| Type / verb | Import | Status |
|---|---|---|
| `OwnerExportRequest` | `proxima::OwnerExportRequest` | Host API DTO |
| `OwnerExportTarget` | `proxima::OwnerExportTarget` | Host API DTO |
| `OwnerExportBundle` | `proxima::OwnerExportBundle` | Host API DTO |
| `Engine::export_owner_bundle(authz, target)` | `proxima::Engine` | Host API verb |

Contract:

| Field | Rule |
|---|---|
| target | personal/group owner only |
| authorization | `AuthPath::System` or `OwnerEraseAuthorityPort::may_export_owner` |
| drop proof | not required; export is non-destructive |
| shape | `tables: BTreeMap<String, Vec<Value>>` — one entry per surface the frozen contracts declare exportable, present even when empty — plus `edges` projected from the exported memory rows, plus derived `counts` |
| rows | `ExportRule::Rows` exports the whole row; `ExportRule::Allowlist` exactly its named fields (grant export omits the redeemable `delegation_id`); `ExportRule::Excluded` exports nothing and says why |
| order | the surface's declared key columns |
| serialization | `OwnerExportBundle::canonical_json_bytes()` emits recursively sorted-key JSON bytes |

## PostgreSQL Runtime Configuration

| Type / method | Import | Contract |
|---|---|---|
| `PgPoolConfig` | `proxima::PgPoolConfig` | Five finite pool/connection defaults; `from_lookup` resolves the `PROXIMA_PG_{MAX_CONNECTIONS,STATEMENT_TIMEOUT_MS,ACQUIRE_TIMEOUT_SECS,IDLE_TIMEOUT_SECS,MAX_LIFETIME_SECS}` block through an injected source |
| `RuntimeBuilder::pg_pool_config(config)` | `proxima::RuntimeBuilder` | Programmatic pool policy; explicit config outranks the environment layer |
| `Proxima::pg_pool_config(config)` | `proxima::Proxima<App>` | Host-facade passthrough to `RuntimeBuilder` |
| `Proxima::from_lookup(lookup)` | `proxima::Proxima<App>` | Resolve the whole environment layer once from host-injected lookup; canonical storage boot does not fall back to process env |
| `PgTuning` | `proxima::PgTuning` | Separate query/search policy; unchanged by pool configuration |

`PgPoolConfig::default()` is `10` max connections, `300000ms` statement
timeout, `5s` acquire timeout, `600s` idle timeout, and `1800s` max lifetime.
`max_connections = 0` is invalid. Zero duration values preserve the env
contract: statement timeout is omitted; SQLx pool durations receive zero
unchanged. `RuntimeConfig::pg_pool_config` is the resolved value consumed by
`ProximaBuilder`; it is not re-resolved during canonical boot.

## Embedding Ops Host API

Public facade status:

| Type / verb | Import | Status |
|---|---|---|
| `EmbeddingAnnObservability` | `proxima::EmbeddingAnnObservability` | Host API DTO |
| `EmbeddingJobBacklog` | `proxima::EmbeddingJobBacklog` | Host API DTO |
| `EmbeddingOrphanCounts` | `proxima::EmbeddingOrphanCounts` | Host API DTO |
| `EmbeddingOrphanSweepOutcome` | `proxima::EmbeddingOrphanSweepOutcome` | Host API DTO |
| `EmbeddingRecallCanary` | `proxima::EmbeddingRecallCanary` | Host API DTO |
| `EmbeddingRuntimePolicy` | `proxima::EmbeddingRuntimePolicy` | Validated whole-second host policy; programmatic equivalent of generic `PROXIMA_EMBED_*` runtime variables |
| `RuntimeBuilder::embedding_runtime_policy(policy)` | `proxima::RuntimeBuilder` | Installs provider batch width, enforced request timeout, worker cadence, and claim lifecycle as one block |
| `Engine::embedding_ann_observability(authz)` | `proxima::Engine` | Host API verb |
| `Engine::sweep_orphan_embedding_rows(authz)` | `proxima::Engine` | Host API verb |

Contract:

| Field | Rule |
|---|---|
| authorization | `AuthPath::System` or `OwnerEraseAuthorityPort::may_perform_operator_maintenance`; ordinary owner read/admin roles are insufficient |
| scope | owner-agnostic operational reads over embedding infrastructure |
| observability | rows, relation bytes, HNSW bytes, job backlog, stale processing jobs, orphan rows, recall canary |
| orphan sweep | deletes embeddings, heads, and jobs whose source `memories` / `goals` row no longer exists |
| owner erase | not dependent on sweep; erase deletes embedding infra synchronously at transaction commit |
| graph authority | embeddings remain engine infrastructure; similarity never authors a connection |

## Cited-Blob Read and Reconciliation APIs

Public facade status:

| Type / verb | Import | Status |
|---|---|---|
| `CitedBlobReconcileOutcome` | `proxima::CitedBlobReconcileOutcome` | Host API DTO |
| `CitedBlobMissingObject` | `proxima::CitedBlobMissingObject` | Host API DTO |
| `MAX_RECONCILE_SAMPLE` | `proxima::MAX_RECONCILE_SAMPLE` | Host API constant |
| `CitedBlobStore::reconcile_all(&SystemAuthority)` | `proxima::CitedBlobStore` + booted runtime's `system_authority()` | Host/operator verb |
| `CitedBlobReadService` / `Port` | `proxima::flavor::*` and `proxima::*` | Bounded verified-byte service |
| `VerifiedCitedBlob` / `CitedBlobReadError` / `CitedBlobIntegrityMismatch` | `proxima::flavor::*` and `proxima::*` | Locator-free result + typed failure taxonomy |
| `CitedBlobOwnerReconcileService` / `Port` | `proxima::flavor::*` and `proxima::*` | Typed flavor service |
| `CitedBlobOwnerReconcileOutcome` / `CitedBlobOwnerMissingObject` | `proxima::flavor::*` and `proxima::*` | Redacted owner DTO |

| Lane | Authority | Scope | Samples |
|---|---|---|---|
| Global | same-boot `SystemAuthority`; foreign-engine witnesses fail before I/O | configured bucket + every locator row | bounded raw missing/orphan/foreign locators for restore operations |
| Owner | ordinary `AuthzContext::may_read(owner, Fact)`; raw delegated contexts rejected | exact owner rows, each probed for its own object | missing cited-object id, byte length, filename; no bucket/object key or orphan/foreign locator samples. `orphan_objects` is structurally 0 here: keys carry no owner, so an unclaimed object has no owner to attribute it to — orphans are a Global-lane finding |
| Verified bytes | ordinary `AuthzContext` or same-runtime `DelegatedPhase`, then Fact-read | exact owner row + canonical object | required `NonZeroU64` ceiling; length+BLAKE3+SHA-256; no partial bytes or locator |

The Global and Owner reconciliation lanes report `missing_objects`,
`orphan_objects`, and `foreign_locators`. `is_intact()` is false exactly when
`missing_objects` is non-zero. Both are report-only: no repair or deletion
occurs.

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
| owner transfer | `transfer_to_owner` is an owner **move**, not an ACL flag or a copy: the series leaves the prior owner's view entirely and lands under the destination. The destination must be a group, and the caller must hold admin on the source (plus group-manage when the source is a group) and admin + group-manage on the destination — that receiving-side manage authority is the destination's consent, which is why a personal destination is refused. Transfer is memory-only: goals do not transfer. |
| transfer and erase reach | a transferred memory moves *between* erase reaches, it does not leave them. The destination owner can erase it under `OwnerEraseTarget::GroupOwner`; the source owner no longer can. Send tenant evidence only to a group whose operators should own its deletion decision, because after the transfer they do — the source's erasure obligation for those rows lands on the destination. |
| transfer and audit sidecars | `mcp_call_logged_v1` is **owner-pinned**: it carries `actor_upn` plus its own `owner_id`, stamped at write time with the owner that made the call, and describes who acted rather than what the memory says. A transfer leaves those rows with the source. The destination receives the memory without its call log — the payload hydrate joins the memory's owner to the row's, so `get_memory`/`get_memories`/`query_memories` return nothing for them — while `read_mcp_call_history`, the export bundle, and Art. 17 erase all stay with the source, which keeps both the history and the obligation to delete it. Every other registered sidecar follows the memory. |

See [14 Protocol Surface — `core_transfer`](../14-protocol-surface.md)
for the `transfer_to_owner` action itself.

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
