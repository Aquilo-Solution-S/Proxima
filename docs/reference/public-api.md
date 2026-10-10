# Public API Reference

## Current Consumption Mode

Workspace packages currently set `publish = false`; consume from git tags or
repo checkouts unless release notes say crates.io publishing is available.

Rust upgrade mapping: [Migrate the Flavor SDK](../how-to/migrate-flavor-sdk.md).

## Typed Consumer Operations

Import requests from `proxima::flavor` and `Engine` / `AuthzContext` from `proxima`.

| Intent | Request | Standalone / transaction |
|---|---|---|
| Observe a Fact | `FactWrite::new(owner, source_id, &payload)` | `Engine::ingest_fact(&authz, request)` / `UnitOfWork::ingest_fact(request)` |
| Derive an Abstraction | `DerivedMemory::abstraction(target, owner, text, payload, origins, identity)?` | `derive_memory` on either receiver |
| Derive a Perspective | `DerivedMemory::perspective(target, owner, text, payload, origins, identity)?` | `derive_memory` on either receiver |
| Interpret referenced knowledge | `DerivedMemory::interpretation(target, owner, text, payload)` | `derive_memory` on either receiver |
| Create a typed Goal | `GoalCreateRequest::product(owner, assignment, request_id, title, text, payload)` | `Engine::create_goal(&authz, request)` / `UnitOfWork::create_goal(request)` |
| Host-owned state in the same unit | typed `HostStateCommand` (host-defined) | `UnitOfWork::apply_host_state(command)` then `commit` |

Standalone operations commit before returning. `engine.unit_of_work(&authz).await?`
groups writes; `commit().await?` persists them, dropping the unit rolls them back.
Goal `.with_evidence(...)` accepts admitted Fact/Abstraction row IDs, including
writes earlier in the same transaction. Assignment requires a Perspective
in the Goal's owner space.

Fact `.refs(...)` adds reference pins, never origins. Automatic natural-key
series selection is transactional; an explicit `.handle(SeriesHandle)` wins.
Keep returned `memory_id` / `goal_id` values for reads and references. A series
handle identifies the version stream, not an admitted row.

Advanced host protocol adapters (`fact_ingest`, split authorization/citation
writes, dynamic Goal payload writes) retain their protocol-specific contracts.
Ordinary typed flavor code uses the operations above.

## Supported Tiers

Supported Rust tiers:

| Tier | Import | Use |
|---|---|---|
| Host API | `use proxima::{Proxima, RuntimeBuilder, RuntimeConfig, Engine, CancellationToken, AccessKind, AccessCeiling, OwnerRoles};` | boot composed binaries; call graph/admin/projector verbs through server-resolved `AuthzContext`. `Role::new` / `Role::may_write` / `OwnerRoles::for_subject` name `AccessKind`, `AccessCeiling`, `AccessError`, `OwnerRoles` |
| Host API (runtime handle) | `use proxima::{BuiltProxima, RunningProxima, ProximaHost, BootReport, Feature, FeatureState};` | `Proxima<A>` is the one builder (over `RuntimeBuilder`); `build()` / `run()` start every configured feature ([10 §Runtime features](../10-configuration.md#runtime-features)). Both handles: `host()` → `ProximaHost` (the same accessor set `AppContext::host()` returns), `system_authority()`, `host_state_maintenance_authority()`, `single_owner_authz()`, `boot_report()`, `publisher_health()` / `copy_cleaner_health()` (`outbox-nats`), `shutdown().await` |
| Host extra-table | `AppContext::host()` → `ProximaHost::{clone_pool_for_host, pg_tuning_for_host}` | host `FlavorApp::services` only: wrap the pool and resolved query policy in a flavor-owned store immediately. Tools resolve the store via `FlavorServices`. Not Flavor SDK. No `proxima_core.*` SQL. **Not** the atomic path with Fact writes — that is a second connection |
| Host-state in UnitOfWork | `UnitOfWork::apply_host_state` + `PgHostStateParticipant` | Host API only. Startup-register exactly one typed participant on `RuntimeBuilder` / `Proxima::host_state_participant` (a second refuses boot); dispatch several command types with `HostStateRequest::is` / `try_downcast`. Owner write-gate first; command tables must be `FlavorContract.state_surfaces`. Same backend transaction as Fact ingest; drop/poisoned commit rolls every participant back. Do not hold the unit open across broker/provider I/O |
| Host API (MCP surface) | `use proxima::{McpHostTools, McpHostTool, McpHostToolCall, ToolReply, ToolContent, ToolDescriptorView, McpEdge, layered_router_mcp_only, PlatformAuthContext};` | `RuntimeBuilder::{host_tools, record_mcp_calls, authenticator_with_platform_scope}`, `BuiltProxima::mcp_edge`, and the handler helpers (`auth_context`, `author_from_args`, `strip_call_context_args`, `reject_nul_in_args`, `tool_invocation_error_to_error_data`, …) — [10 §MCP Endpoint and Authentication](../10-configuration.md#mcp-endpoint-and-authentication) |
| Host API (own handler or router) | `use proxima::{DynamicHandler, TerminalDispatch, ScopeGateBehavior, RevalidationConfig, enforce_body_limit, body_limit_layer, host_guard_layer, mcp_auth_layer_with_metadata, tool_name_matches, all_core_resources, core_action_meta, CoreActionMeta};` | a host that assembles its own `ServerHandler` or router. Implement `proxima::rmcp::ServerHandler` around `DynamicHandler` (the rmcp the workspace pins, one copy); apply the layers `McpEdge::router` applies: body cap, Host guard, bearer auth with protected-resource metadata. `RevalidationConfig` is the type of `McpEdge::revalidation` and `RuntimeConfig::stream_revalidation` |
| Host API (tool-argument ids) | `use proxima::{PrefixedUuidClass, PrefixedUuidError, format_prefixed_uuid, parse_prefixed_uuid};` | format and parse the `<prefix>:<uuid>` ids tool arguments carry; the error type travels with the parser |
| Host API (storage handle) | `use proxima::{PgStorage, PgHostStateEraseContext, OriginScope, PublicationOriginEligibility, PublicationOriginEligibilityPort, begin_owner_transaction};` | `PgStorage` is the migration helpers' parameter; `PgHostStateEraseContext` and `OriginScope` are what `ProximaHost::{host_state_erase_context_for_host, origin_scope_for_host}` return. `begin_owner_transaction(&PgPool, &OwnerScope)` is for authorized extra-table adapters: the pool comes from `clone_pool_for_host`, the scope from `authenticate`; every statement runs on the returned transaction; no `proxima_core.*` SQL through it or the pool; Fact and state writes use `UnitOfWork`. Not Flavor SDK |
| Host API (OIDC) | `use proxima::auth::{OidcTokenValidator, ValidatedOidcToken, OidcRejection, OidcRoleShape, OidcRoleShaper, OidcClaimMap};` | `validate_with::<C>` reads host claims from the verified payload; `OidcRoleShape::Host` shapes a binding's context from every claim; requires feature `auth-oidc` |
| Host API (system work) | `AuthzContext::for_system(&SystemAuthority, OwnerRoles)` | sealed `AuthPath::System` context from the runtime's `SystemAuthority`, with no bearer or stub authenticator |
| Host API (role probes) | `use proxima::{roles_for_subject, group_role};` | `roles_for_subject(&mut *tx, subject) -> Result<OwnerRoles, AccessError>`, `group_role(&mut *tx, subject, group) -> Result<Option<Role>, AccessError>` on the host's own connection or transaction (`PgPlatformScope::begin()` included): the answers `PgOwnerAccessResolver` gives, several relations in one group joined. No transaction control, no scope set; rows the connection's scope cannot read are not seen. No bool probe: authorize from the role — [10 §MCP Endpoint and Authentication](../10-configuration.md#mcp-endpoint-and-authentication) |
| Host API (membership relation) | `Engine::{remove_member_relation, replace_member_relation}` | `remove_member_relation(&authz, group, member, relation)` deletes one row (absent is `Ok`); `replace_member_relation(&authz, group, member, from, to)` is one transaction under the membership lock: `from` must be held (else `invalid_argument`, nothing changes), `from == to` is refused, other relations stay, no reader sees the member with neither. Manage gate, hooks (`Membership { change: Remove, relation: from }` then `{ change: Add, relation: to }`, both before the write) and membership locking as `add_member` / `remove_member`. `OwnerMembershipAdminPort` gains `remove_group_member_relation` / `replace_group_member_relation` with refusing defaults |
| Host API (REST OpenAPI) | `use proxima::host::build_openapi_document;` | build the complete registry document with the same generator as `/v1/openapi.json` without depending on `proxima-mcp-server` internals; requires feature `rest` |
| Host API (AsyncAPI) | `use proxima::{asyncapi_document, AsyncApiInfo, DEFAULT_SUBJECT_PREFIX};` | offline AsyncAPI 3.0.0 document of the frozen registry's listenable Facts; channel addresses are the publisher's subjects ([18 §AsyncAPI Catalog](../18-fact-outbox.md#asyncapi-catalog)); requires feature `outbox-nats`; `DEFAULT_CONSUMER_STREAM` and `DEFAULT_CONSUMER_NAME` (the reference consumer's defaults) are exported beside `DEFAULT_SUBJECT_PREFIX` |
| Flavor SDK | `use proxima::flavor::{FlavorBundle, FlavorRegistry, FlavorContract, SchemaContract, Surface, FactPayload, pg_sidecar, InlineCitedObjectDraft, InlineCitationMappingDraft, CitationAttachmentRequest};` | build-time schemas, complete contract declarations, payload references, tools, sidecars. Typed citation drafts and the citation-attachment request + `AuthorizedFactWithCitation{,Ref}` are nameable here; `Engine` stays Host API. `EraseRule::HostState` is built from `flavor::HostStateEraseDisposition`, the SDK's only host-state name |
| Flavor SDK (services) | `use proxima::flavor::{FlavorServices, FlavorServiceError};` | return typed services from `FlavorApp::services`; tuple composition rejects duplicate concrete types and shares one set with MCP, REST, and workers |
| Flavor SDK (tools) | `use proxima::flavor::{Tool, ToolCtx, ToolCaller, ToolError};` | author transport-neutral tools; MCP and REST populate optional caller provenance directly on `ToolCtx` |
| Flavor SDK (authorized reads) | `use proxima::flavor::{authorized_memory_ids, authorized_fact_payloads, authorized_abstraction_payloads, SidecarAtom, QueryRequest, hybrid_degraded_to_lexical};` | typed, authz-filtered candidate/payload reads — see [Authorized Flavor-Read Facade](#authorized-flavor-read-facade) below. `Engine` is Host API (`use proxima::Engine`). Code-series `&PgPool` helpers live in `flavors/code`, not this SDK. |
| Flavor SDK (bundle) | `proxima::flavor_bundle! { bundle = …, name = …, …, migrations = …, app = { … } }`, `NamedMigrator::flavor`, `flavor_ledger_table` | one declaration: `proxima_flavor!` registration, PG sidecars, own ledger `public._sqlx_migrations_<id>`, `FlavorBundle` (+ `FlavorApp`) — [09 §FlavorBundle](../09-developing-flavors.md#flavorbundle) |
| Flavor SDK (after a side effect) | `use proxima::flavor::ingest_fact_detached;` | `ingest_fact_detached(&ctx, write, deadline)`: the Fact recording an upstream write survives client disconnect; `Engine::ingest_fact_detached` for hosts |
| Flavor tests | `proxima = { features = ["testkit"] }` → `proxima::testkit::{SplitRoleDb, split_role_urls_for, scoped_authz, assert_trigger_migrations}` + `proxima-pg-testkit` | [09 §Tests](../09-developing-flavors.md#tests) |
| Flavor SDK (outbound endpoints) | `use proxima::flavor::{validate_endpoint_url, EndpointUrlPolicy};` | enforce HTTPS with the shared, exact loopback-only plaintext exception; never reproduce it with string prefixes |

Reachability rule: a type that appears in a public signature of the facade is
reachable through the facade, in the tier of the signature that exposes it. A
host-tier signature gets its types at the root (`src/host.rs` documents each
group with the signatures that need it: engine verbs, storage ports and their
rows, authorization witnesses, registry and contract vocabulary, the MCP tool
and edge vocabulary, the S3 and outbox lanes, `PgStorage`'s methods). A Flavor
SDK signature gets its types under `proxima::flavor`. Host-only names
(`PgStorage`, `begin_owner_transaction`, the storage ports) stay out of the
SDK. Naming an authorization witness or permit does
not let anyone mint it: the constructors stay crate-private to the Engine's
gates. Third-party crates (`sqlx`, `axum`, `tokio`) stay the host's own
dependencies, except `rmcp`, re-exported as `proxima::rmcp` at the pinned version
and feature set. A later `rmcp` major is a breaking change of the facade.

The enforcer is a compile-checked list, not a scan:
`crates/proxima/tests/facade_signature_names.rs` imports every one of those names
through `proxima::` or `proxima::flavor::`, grouped by the signature that needs
them, so a dropped or renamed re-export fails to compile, and fails when a
host-only name appears in `src/flavor.rs`. Add a type to it, and to `src/host.rs`,
whenever a public signature names a new type. Nothing checks that a new signature's
type is on the list; stable Rust has no tool for that (rustdoc JSON is nightly).
`crates/proxima/tests/facade_one_dependency.rs` builds against `proxima::` alone.

Known and open: the Flavor SDK tier is not complete under this rule. A one-off
source scan (not kept) found 49 types that SDK signatures name and
`proxima::flavor` does not, and they are not exported yet. Through the
`flavor::authorized_read` functions: `AuthzContext`, `Engine`. Through registry,
tool and storage accessors: `AccessKind`, `AccessError`, `Relation`, `Role`,
`Owner`, `OwnerRefKind`, `ToolScope`, `CapabilityTag`, `StorageError`,
`TrimmedLenViolation`, `HostStateCommand`, `HostStateOutcome`. Through the write
path: `WritePermit`, `OwnerWritePermit`, `PublicationPlan`, `Citation`,
`FactReceiptDraft`, `AuthorizedNodeLinks`, `AuthorizedInlineCitedObject`,
`AuthorizedInlineCitationMapping`, `DecomposedGoalOutcome`. Through reads and the
registry: `QueryCursor`, `QueryPage`, `MemoryRow`, `SchemaInfo`, `SchemaRequest`,
`SchemaResponse`, `ProtocolPayload`, `MemorySearchProjection`, `MemoryEmbedUnit`.
Through MCP: `TerminalDispatch`, `McpToolCtx`, `McpToolDescriptor`,
`McpToolError`, `McpToolPresentation`, `McpUnknownFieldPolicy`. Through the PG
sidecar registry: `IntegrityReport`, `IntegrityViolation`, `Artifact`,
`PgSidecarKey`, `PgMemoryPayloadBatchFuture`. Named only by trait-impl generic
arguments (`impl From<..>`), needed by no value a host holds, and left
unexported at the host tier: `CursorError`, `InterpretationSubjectKind`,
`SearchMemoriesKind`, `SearchMemoriesMode`, `SearchMemoriesSupersession`,
`WalkMemoryLineageDirectionArg` (the first five are among the 49).
`AuthorDerivedOutcome` is among the 49 as well: the host tier exports it because
a storage-port method returns it. Each is a maintainer decision about whether the
SDK should name it; none is a regression of this change.

Flavor contract declarations are const-constructible and imported from
`proxima::flavor`. The same module owns `SchemaRef`, `KeyShape`, the erase /
transfer / export / forget / counter rules, projection and embedding
declarations, and tool/resource contracts. Goal write DTOs, including
`GoalCreateRequest<P>` and `GoalTopologyWrite`, use the same facade.
`SidecarSessionRead` remains a bounded, owner-stamped request for an existing
authorized write session; it does not expose a connection or pool.

Cold-memory repair is part of the Host API: `Engine::hydrate_memory` and
`Engine::hydrate_memories` accept an owner plus `MemoryId` values and use the
server-resolved write authority. Their `MemoryHydrationOutcome` values expose
only `Hydrated`, `AlreadyHot`, `NotFound`, `MissingColdObject`,
`UnsupportedColdObject`, `UnsupportedColdSidecar`, `InvalidColdObject`, and
honest batch `NotAttempted` classifications; raw Postgres transactions and
cold-store locators are not public. The set limit is
`MAX_MEMORY_HYDRATION_BATCH` (64), and the set is atomic over owner-visible
cooled items. A row cooled before the integrity witness existed carries no
`cold_digest` and its object predates the stamped cold format; it stays cooled
and reports the unsupported outcome rather than being silently admitted. There
is no repair path and none is planned: the witness cannot be reconstructed
from the object alone, and the append-only trigger refuses to accept one after
the fact. Erase is the only remaining action on such a row.

Unsupported:

| Surface | Status |
|---|---|
| raw `sqlx::PgPool` on Flavor SDK / tools | denied. The Host extra-table bridge is `AppContext::host()` → `ProximaHost::{clone_pool_for_host, pg_tuning_for_host}` (see below) |
| aggregate `Storage` / `StorageHandle` | removed; Engine owns storage ports |
| `proxima-storage-pg` raw write verbs | backend API only; every owner write requires `OwnerWritePermit` minted by `Engine::authorize_owner_write` |
| historical erase witness | internal database metadata only; not a public `Edge`, `PinNode`, export field, transfer field, MCP field, or REST field |
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

## Roles

`Role { read, write, manage }` sets one limit per direction on the ladder
Fact < Abstraction < Perspective < Goal (`write ≤ read`). One rule for both
directions (Lean `Causa.Authorization` `may_read` / `may_write`):

```text
may(role, owner, kind, dir)  :=  role on owner exists  ∧  kind ≤ limit(role, dir)
```

| Role | read / write limit | R F | R A | R P | R G | W F | W A | W P | W G | Upload |
|---|---|:-:|:-:|:-:|:-:|:-:|:-:|:-:|:-:|:-:|
| `Role::viewer()` | G / – | ✓ | ✓ | ✓ | ✓ | · | · | · | · | · |
| `Role::ingest()` | F / F | ✓ | · | · | · | ✓ | · | · | · | ✓ |
| `Role::editor()` | G / P | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | · | ✓ |
| `Role::admin()` | G / G | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `Role::new(Goal, Abstraction, _)` | G / A | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | · | · | ✓ |
| `Role::new(Abstraction, Abstraction, _)` | A / A | ✓ | ✓ | · | · | ✓ | ✓ | · | · | ✓ |

| Surface | Kind checked |
|---|---|
| Fact ingest, upload completion, source cursors, MCP call log, host state | Fact write |
| derive / interpret | the output kind's write |
| Goal create / transition / modify / decompose | Goal write |
| forget / hydrate | the caller's write limit; storage returns `NotFound` for a row above it |
| embedding backfill | the caller's write limit; queues only kinds up to it |
| search, get, lineage, neighbours, change history, Query | Fact read to enter; each row by its own kind (owner RLS). An unreadable row is `NotFound` |
| membership admin, transfer, erase, graph overview | owner admin: `Role::administers()` (write limit Goal); membership and group transfer also `Role::manages()` |

`Relation` is only the stored name of a membership preset; `Relation::role()`
is what authorization reads. `AuthzOperation` (`flavor::AuthorizationHook`
input) is `Read { kind }`, `Write { kind }`, `OwnerAdmin`, `Membership`,
`EntityTransfer`.

Owner RLS (core 0023, code flavor v029) applies the same rule to every
owner-keyed row: memory-attached rows by their memory's kind, Goal-attached
rows as Goal, Fact-only tables as Fact. The migration refuses a
`proxima_core` table it does not classify. Derived Content is keyed by kind,
so Content reuse never crosses a limit.

## Owner Write Permit Boundary

| Item | Contract |
|---|---|
| `OwnerWritePermit` | sealed storage-tier proof: `(OwnerRef, AccessKind)`; constructor is not public |
| minting path | `Engine::authorize_owner_write(authz, owner, kind)` after server-resolved owner access |
| `AuthPath::System` | writes only through a context `AuthzContext::for_system(&SystemAuthority, OwnerRoles)` built from this Engine's witness; any other System context is refused |
| host witness | `BuiltProxima::system_authority()` / `RunningProxima::system_authority()` expose a borrowed witness to embedding hosts |
| wire/flavor boundary | MCP tools and flavor `ToolCtx` do not receive `SystemAuthority`; normal membership/HostBearer paths need no witness |
| typed Fact destination | `Engine::ingest_fact(&authz, FactWrite::new(owner, source_id, &payload))`; the engine authorizes the explicit destination. Keep the full authenticated context so readable foreign references remain available. |
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
| Engine derived memory | `derive_memory` |
| Engine upload completion | `complete_upload_as_fact`, `complete_upload_as_fact_with_expectation` |
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
| facade signature names | `cargo test -p proxima --test facade_signature_names --locked` |
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

## Read Selection

`QueryRequest::readable()` searches the caller's complete authorized owner set.
It accepts kind/schema/history/cursor/ID filters and has no owner selector.
For known IDs, use `Engine::get_memory` / `get_memories`; absent and unreadable
rows have the same response. `Engine::search` retains its explicit corpus owner.

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

Hard deletion retains only an internal, permanent `(t, closed kind)` witness
for each erased Memory or Goal target. It has no owner or payload. Source
Memory rows keep their `origins[]`, `refs[]`, and `goal_refs[]` byte-for-byte;
erase does not cascade or null them. Ordinary new writes require live targets
and cannot use or reuse witnesses. Exact sealed cooled restoration may use a
correctly kinded witness; cooled rows written before that seal are permanently
unsupported and can only be erased.

This metadata is absent from the public Edge/PinNode projections and from
MCP, REST, transfer, forget, and export surfaces. A missing target continues
to use the existing redacted/missing projection; this contract does not add
an `Unavailable` state. Hydration is a storage lifecycle operation exposed by
the owner-authorized Host API, not a flavor tool surface.

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

## Flavor-Scoped Erase API

| Type | Import | Status |
|---|---|---|
| `SeriesSelection`, `EraseMode` | `proxima::flavor::*`, `proxima::host::*` | Flavor SDK / Host API |
| `SchemaId`, `SidecarAtom` (what the schema-naming selections carry) | same | Flavor SDK / Host API |
| `SeriesEraseReceipt` | same | Flavor SDK / Host API |
| `SeriesEraseError`, `SeriesEraseRefusal`, `SeriesEraseRefusalKind` | same | Flavor SDK / Host API |
| `MAX_ERASE_SERIES_PER_CALL`, `MAX_ERASE_VERSIONS_PER_CALL` | same | Flavor SDK / Host API |

| Entry | Authority |
|---|---|
| `engine.unit_of_work(&authz)` → `erase_own_series(flavor_id, owner, selection, mode)` | Admin on `owner`; inside a tool handler, an action whose `ToolEffect` is `Destructive`, of a tool the flavor's contract names |
| `engine.system_unit_of_work(&authority)` → `erase_own_series(..)` | the host's `SystemAuthority` for this boot; the unit admits no other operation |

One entry point for flavors and hosts; the scope rules are the same on both.
Contract: [13 §Flavor-scoped erase](../13-compliance.md#flavor-scoped-erase).

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
unchanged. `RuntimeConfig::pg_pool_config` is the resolved value boot
consumes; it is not re-resolved during canonical boot.

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
| `UploadCompletionExpectation` | `proxima::flavor::UploadCompletionExpectation` | Core-owned, non-serializable immutable upload metadata for expectation-bearing completion |

`Engine::complete_upload_as_fact_with_expectation` stages once, compares the
expected BLAKE3 hash, byte length, MIME, and filename in that order, and only
then enters citation authorization and persistence. A mismatch is a redacted
`InvalidArgument`; it leaves the upload pending and does not call `finish`.
The ordinary `complete_upload_as_fact` method and MCP `complete(upload_id)`
remain unchanged for callers without a separate expectation.

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
| target-owner ingest | resolve the worker subject to roles; call `Engine::ingest_fact(&authz, FactWrite::new(owner, source_id, &payload))`. The explicit destination is authorized without narrowing the caller's read access. |
| idempotency keys | Proxima honors a caller-supplied idempotency key verbatim — it never invents a different projector-side key. `core_remember`'s `idempotency_key` deterministically becomes the note id via UUIDv5 over the caller's own bytes (`crates/core/src/mcp/core_tools/memory/remember.rs`); other Fact payload schemas declare their own `natural_key_columns()` from caller-supplied payload fields. Re-ingesting the same key with the same content is a no-op; re-ingesting the same key with changed content writes a new version and advances the head pointer — the identity a projector chooses is the identity Proxima keeps. |
| source cursor bytes | `Cursor` is opaque byte state keyed by `(owner, source)`. A projector may encode `last_event_seq` into it; `store_source_cursor` persists the supplied bytes verbatim, and `load_source_cursor` returns the exact bytes last stored for that owner/source. No Centauri-side `piy_projection_cursor` table is required for that state. |
| projection lag | `Engine::source_cursor_age(authz, owner, source)` returns the age of the owner/source cursor for EVD-012-style lag SLO evidence. It is owner-scoped and needs Fact read; `load_source_cursor` / `store_source_cursor` need Fact write and do not expose cursor bytes to readers. |
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
