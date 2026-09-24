# Migrate the Flavor SDK

## v0.0.20

Pin all Proxima Rust dependencies to the same `v0.0.20` tag. Hand-written
flavors keep compiling. Not additive: the host-state registration row; the
new `MigrationError::DuplicateLedger` variant (exhaustive matches); and the
code flavor's `execution_plan_v1` policies, now keyed on the plan's own `t`,
so a caller with Fact rights alone no longer reaches plan rows.

| Surface | Upgrade |
|---|---|
| Database | Core `0018_v020_owner_rls_installer.sql` adds `proxima_core.install_owner_rls`; code flavor `20260924000020_v020_owner_rls_installer.sql` re-installs its policies through it. Existing databases upgrade in place. |
| Owner RLS | Replace a flavor's hand-written owner-RLS `DO` block with one `SELECT proxima_core.install_owner_rls(...)` in a **new** migration ([09 §Owner RLS](../09-developing-flavors.md#owner-rls)); never edit the released file. The installer keys an FK-parent table on the FK of its leading primary-key column; a v0.0.15-style block took the first FK by creation order, so diff the policies before and after. |
| Flavor ledger | `NamedMigrator::new(id, m)` → `NamedMigrator::flavor(id, m)`: own ledger `public._sqlx_migrations_<id>` (a migrator that already declares a table keeps it). Rows on core's ledger are copied over on the next migration run and stay there, so neither binary re-runs anything. Two flavors on one ledger refuse. |
| Bundle | `proxima::flavor_bundle! { bundle = …, <proxima_flavor! keys>, migrations = …, app = { … } }` replaces `proxima_flavor!` + `register_pg_sidecars` + `impl FlavorBundle` (+ `FlavorApp::app_info`) ([09 §FlavorBundle](../09-developing-flavors.md#flavorbundle)). |
| Tests | `proxima = { features = ["testkit"] }`: `proxima::testkit::{SplitRoleDb, split_role_urls_for, scoped_authz, assert_trigger_migrations}`; the feature now also enables `proxima-core/test-fixtures`. `proxima::testkit` is a module re-exporting `proxima-pg-testkit`, so existing `proxima::testkit::…` paths still resolve. |
| Side-effect Facts | `proxima::flavor::ingest_fact_detached(&ctx, write, deadline)` / `Engine::ingest_fact_detached` replace hand-rolled spawn + timeout + join. |
| Host state (behavior change) | A second `host_state_participant` registration — same builder, or overlay over `FlavorApp::configure` — now refuses boot (`ProximaError::Config` / `EmbedError::Config`) instead of replacing the first. `HostStateRequest::is::<C>()` / `try_downcast::<C>()` dispatch without consuming the request. |
| NATS subjects | `proxima_outbox_nats::parse_subject(prefix, subject)` inverts `subject_for` ([18 §The `type_token` rule](../18-fact-outbox.md#the-type_token-rule)). |

## v0.0.14

Pin all Proxima Rust dependencies to the same `v0.0.14` tag. Cargo package
versions remain unpublished; MCP initialization and REST OpenAPI report `0.0.14`.

| Surface | Upgrade |
|---|---|
| Database | No core or flavor migration ships in v0.0.14. Existing v0.0.13 databases remain compatible and boot without a reset. |
| Host-bound publication extensions (additive Rust API) | `AuthzContext::with_publication_extensions(PublicationExtensions)` binds validated `CloudEvents` extension attributes onto every Fact that context captures. Host-only, additive, never reachable from a payload or a transport. A host that binds nothing is unaffected and its envelopes are byte-identical. See [18 §Host-bound extension attributes](../18-fact-outbox.md#host-bound-extension-attributes). |
| `PublicationDraft::new` (breaking Rust API — test/host constructors only) | Gained a `PublicationExtensions` parameter between `model_id` and `data`. The engine fills it from the authorization context; code that builds a draft directly passes `PublicationExtensions::new()` for the previous behavior. |
| Consumer envelope view (breaking Rust API — struct literals only) | `proxima_outbox_nats::CloudEventEnvelope` gained `extensions: BTreeMap<String, serde_json::Value>`, collecting every context attribute its named fields do not claim. Parsing is unchanged; a struct literal of that type needs the field. |
| Wire contract | Additive. No MCP or REST change; no new required attribute. |

## v0.0.13

Pin all Proxima Rust dependencies to the same `v0.0.13` tag. Cargo package
versions remain unpublished; MCP initialization and REST OpenAPI report `0.0.13`.

| Surface | Upgrade |
|---|---|
| Database | No core or flavor migration ships in v0.0.13. Existing v0.0.12 databases remain compatible and boot without a reset. |
| Forwarder hosts | A trusted subject may act for a selected Group owner per request. The host resolves that Group role through `OwnerAccessPort::resolve_group_role`; the caller still selects only the owner. Existing ports retain the eager role-map default, while the shipped Postgres runtime resolves one missing Group per request. |
| Wire contract | No MCP or REST wire change. Existing owner selection and server-resolved authorization rules remain in force. |

## v0.0.12

Pin all Proxima Rust dependencies to the same `v0.0.12` tag. Cargo package
versions remain unpublished; MCP initialization and REST OpenAPI report `0.0.12`.

| Surface | Upgrade |
|---|---|
| Database | Normal facade boot applies additive core migration `0011_v012_fact_outbox.sql`; no reset of a compatible v0.0.11 database. See [migration policy](migrations.md#v0012). |
| Cold operations (breaking behavior) | Database-only facade hosts no longer use in-memory cold storage. Configure durable S3 for forget/hydration; unavailable storage preserves hot data and pending external purge debts. Hot ingest/query remain available. See [S3 configuration](../10-configuration.md). |
| Split Fact authorization/persistence (breaking Rust API) | Supply sidecars to `authorize_fact_ingest*`; the authorized value now owns them. Remove the sidecar argument from Engine/`FactIngestPort` `ingest_fact_with_*typed_sidecar` calls and `WriteSession::ingest_fact_with_typed_sidecar`. Backend implementations read `authorized.sidecar_payloads()`. |
| Custom `WriteSession` implementations | Implement `apply_host_state`; unsupported backends must refuse before mutation. PostgreSQL hosts register typed participants with `ProximaBuilder::host_state_participant`. See [transactional host state](../reference/public-api.md#supported-tiers). |
| Typed flavor writes | `ingest_fact(FactWrite::new(...))` and UoW `ingest_fact` retain their entry points. |
| Fact outbox (opt-in) | `FactPayload::LISTENABLE = true` requires a publication source at facade boot (`PROXIMA_PUBLICATION_SOURCE` or builder configuration). Capture is atomic with admission; without NATS configuration, records remain pending. Enable the host `nats` feature and configure the deployment-owned broker topology to deliver them. See [setup](fact-outbox.md). |
| MCP/REST schemas | Schema projections add `listenable`; `proxima://schema/{schema_id}/{schema_version}` resolves one registered JSON Schema. Existing tool names remain. |

## v0.0.11

Rust source changes for v0.0.11. Existing database migrations, payload key
bytes, and served MCP/REST tool schemas were unchanged by that SDK overhaul.

| Previous Rust API | Replacement |
|---|---|
| `TypedFactIngest`, `ingest_typed_fact`, `ingest_typed_fact_with`, UoW `ingest_typed` | `FactWrite::new(owner, source_id, &payload)` and `ingest_fact` |
| typed Fact `.derived_from(...)` | `.refs(memory_ids)`; Facts have no derivation origins |
| raw `FactWriteCommand.derived_from` / `with_derived_from` | `additional_references` / `with_additional_references` |
| `AuthorDerivedRequestInput`, `author_derived_authorized`, UoW `author_derived` / `author_derived_all` | `DerivedMemory`, `derive_memory` / `derive_memories` |
| caller-selected `MemoryOperatorKind` | inferred from authorized origin rows and typed output payload |
| raw output schema/kind/version and generic `model_id` | typed payload metadata; generic `model_id` was unused |
| derived request's `memory_id` / `supersedes` | `MemoryTarget::Series(SeriesHandle)` / `MemoryTarget::Revision(prior_t)` |
| UoW `create_goal(dynamic_request, write_act_t)` | `create_goal(GoalCreateRequest<P>)`; episode write-act attachment stays internal |
| flavor `GoalCreatePayloadWriteRequest` | typed `GoalCreateRequest<P>`; dynamic protocol DTO remains Host API |
| `QueryRequest::for_owner(owner)` | `QueryRequest::readable()`; full authorized readable set |
| Query `owner` / `read_owners`; edge-read `owner` | removed inert request fields; Engine supplies authorization separately |
| authorized candidate/payload read helpers' `owner` argument | removed; candidate IDs are filtered by the caller's readable set |
| flavor `McpTool` / `McpToolCtx` / `McpToolError` | `Tool` / `ToolCtx` / `ToolError` |
| flavor `operator_label(&ctx, value)` | `ctx.operator_label(value)` |
| flavor `McpToolPresentation` | host wiring supplies presentation; flavor tools import `McpPresentationExt` |

## Identity and transactions

- Keep returned `memory_id` and `goal_id` for references and reads. A `SeriesHandle` is a version stream, not a row.
- Independent conclusions use distinct series handles. A→A derivation retains both conclusions. Revision advances the selected series and retains its history; it still requires origins.
- `Series(handle)` replays matching head metadata/pins. Changed origins append a version; unchanged origins with changed refs conflict. `Revision(prior_t)` appends on every successful call.
- Natural-key Fact series selection runs inside the storage transaction, including concurrent writes and earlier uncommitted observations. `.handle(...)` overrides automatic selection. Receipt replay still returns the original row.
- `FactWrite` selects an authorized destination without narrowing the caller's readable target set.
- Standalone writes commit before return. A `UnitOfWork` commits explicitly; drop rolls back all its writes. Pending Goal assignments must be Perspectives in the Goal's owner space; evidence may reference readable Facts or Abstractions.
- Split host Fact authorization followed by automatic natural-key admission must supply the typed payload during authorization. Storage verifies the same natural-key values at persistence.

## Reads and tools

`QueryRequest::readable()` has no owner selector. Legacy incoming JSON owner
fields are ignored; they cannot affect authorization. Known-ID reads use
`get_memory` / `get_memories`. Search retains its explicit corpus-owner selector.
Candidate helpers reject a zero limit, including an empty candidate list.

Implement one `Tool` trait; the existing adapters serve it over MCP and REST.
Keep `mcp_tools = [...]` build-time registration and existing wire names.
`McpPresentationExt` supplies MCP reference parsing/formatting on `ToolCtx`.
Host transport adapters remain available under `proxima::host`.

Complete examples: [typed operations](../reference/public-api.md#typed-consumer-operations),
[derived memory](../09-developing-flavors.md#deriving-abstractions),
[tool authoring](../tutorials/add-first-mcp-tool.md).
