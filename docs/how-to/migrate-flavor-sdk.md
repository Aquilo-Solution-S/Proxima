# Migrate the Flavor SDK

## v0.0.27

Pin all Proxima Rust dependencies to the same `v0.0.27` tag. One additive core
migration, `0025_v031_goal_text.sql`; boot applies it ([migrations
§v0.0.27](migrations.md#v0027)). **Breaking, no shims** (#435): `MemoryReadPort`
gained a required method, `GoalRow` gained fields and `GoalWriteCommand` gained
`text`, so a host or test double that implements the one or builds the others
stops compiling until it adds them; `write_goal` and `write_armed_goal` now need
the `test-fixtures` feature. The MCP tool outputs that carried a preformatted
time string now carry `time::OffsetDateTime` (same wire).

| Removed / changed | Upgrade |
|---|---|
| `impl MemoryReadPort for T` | Add `async fn count_memories(&self, owner_scope: Option<&OwnerScope>, read_owners: &[OwnerRef]) -> Result<MemoryCounts, StorageError>`: current memories by kind over the owners the caller reads, one per series head and one per Goal head. Fill the count with `MemoryCounts::slot(kind)`; a stored kind that is not one of the four is `StorageError::Internal`, never skipped. A fake that holds no memories returns `Ok(MemoryCounts::default())` |
| `GoalRow { .. }` built by hand (a fake `MemoryReadPort`, a test) | Add `text: Option<String>`, `set_at: time::OffsetDateTime` and `changed_at: time::OffsetDateTime`. `text` is this version's prompt text, `None` when unknown and never blank. `set_at` is the time of the series' first version (the least `t` of the `handle`) and is the same on every version; `changed_at` is the time of this version's `id`, equal to `set_at` on the first version. Both serialize as RFC 3339 strings; `text` deserializes to `None` when absent, the two times are required |
| `GoalWriteCommand { .. }`, `write_goal` and `write_armed_goal` (`proxima_storage_pg::verbs::goal_timeseries`, `wake_timeseries`) | `write_goal` and `write_armed_goal` now need the `test-fixtures` feature: production writes go through the Goal write verbs (`create_goal_atomic` and the rest), which fill `text` and carry the prior version's text on a lifecycle write. `GoalWriteCommand` gains `text: Option<String>` after `title`: `Some` is stored as the version's text and must not be blank (the database refuses it with `goal_text_nonblank_chk`; [06 §Goal text](../06-goals-and-self.md#goal-text) defines blank); `None` stores NULL, unknown |
| `SearchMemoryOutput.created_at`, `UploadPrepareOutput.expires_at`, `UploadReadUrlOutput.expires_at` (`proxima_core::mcp::core_tools::{search_memories, upload}`) | Now `time::OffsetDateTime` instead of a preformatted `String`. They serialize with `time::serde::rfc3339` to the same RFC 3339 strings, and the tool output schemas still declare a string, so the wire is unchanged. Code that read the `String` formats the value itself |
| Changed | `proxima://goals` and `proxima://goal/{id}` return `text` (the full text in the listing too; JSON `null` when unknown, the key always present), `set_at` and `changed_at` (RFC 3339 strings) on every goal. A client that rejects unknown fields adds the three ([14 §Core Memory MCP Surface](../14-protocol-surface.md#core-memory-mcp-surface)). The export bundle's `proxima_core.goal` rows carry `text` |
| New | Resource `proxima://memories/counts` (`resource:memory-counts` scope key, REST `GET /v1/resources/memories/counts`), answering `{"counts": {"Fact", "Abstraction", "Perspective", "Goal"}}` with all four keys always present ([14 §Core Memory MCP Surface](../14-protocol-surface.md#core-memory-mcp-surface)). A palette that lists resources by key adds `ScopeKey::Resource(ResourceKey::parse("memory-counts")?)` to expose it. `Engine::memory_counts(&AuthzContext) -> Result<MemoryCounts, ProtocolError>` and `MemoryCounts` are exported from `proxima::host` |

## v0.0.26

Pin all Proxima Rust dependencies to the same `v0.0.26` tag. Boot applies one
additive core migration ([migrations §v0.0.26](migrations.md#v0026)); a
deployment with `PROXIMA_SKIP_MIGRATIONS=true` runs it before the new binary
starts. **Breaking, no shims** (#412): host tools list per request and can
fail, answer content or a tool failure, and be hidden by request behaviors
([12 §Tool Replies](../12-tool-manifest.md#tool-replies)).

| Removed / changed | Upgrade |
|---|---|
| `McpHostTools::list` | `async fn list(&self, &McpAuthContext) -> Result<Vec<McpHostTool>, McpToolError>`. An `Err` fails the `tools/list`, or the `tools/call` that needed the catalog; return `Ok(Vec::new())` to degrade on purpose |
| `McpHostTools::call` | Returns `Result<ToolReply, McpToolError>`: `ToolReply::Structured(value)` for what returned a `Value`; `Content` for blocks (only from a tool without an `output_schema`); `Failure` for a failure the model should read (`isError: true`) |
| `RequestBehavior::handle`, `Next::run`, `TerminalDispatch` | Carry `ToolReply` instead of `Value`. A behavior that rewrites JSON matches `ToolReply::Structured` and passes the other variants on |
| `McpHostTool` struct literal | `#[non_exhaustive]`: `McpHostTool::new(name, description, args_schema, effect)` plus `with_output_schema`, `with_meta`, `with_open_world`. `output_schema` is `Option<Value>` |
| `McpToolHost::host_tools_for` | `async`, returns `Result<Vec<McpHostTool>, McpToolError>` |
| Handler wrapper listing host tools | Delete it; `list` is async now, and `McpHostTools::instructions` adds to the server instructions |
| New | `RequestBehavior::visible(&McpToolCtx, &ToolDescriptorView) -> bool` (default `true`) hides a tool from `tools/list`, the REST catalog and OpenAPI document, `proxima://how-to` and `core/list_substrate_tools`; the call is still refused in `handle`. Listing surfaces now build the request's tool context, so a bad request-services bag fails them as it fails a call |

**Also breaking, no shims** (#421): a palette holds typed scope keys instead of
strings ([12 §Scope keys](../12-tool-manifest.md#scope-keys)). Wire, env and
storage strings are unchanged; a flavor whose dispatcher action names fall
outside `[A-Za-z0-9_.-]` now fails registry freeze with
`FlavorRegistryError::InvalidScopeKey`.

| Removed / changed | Upgrade |
|---|---|
| `ToolScope::Palette(Vec<String>)` | `Palette(Vec<ScopeKey>)`. Build keys with `ScopeKey::parse("core_goal:set")?`, `ScopeKey::Tool(ToolName::parse(name)?)`, `ScopeKey::action(tool, action)?`, `ScopeKey::Resource(ResourceKey::parse(name)?)`; all exported from `proxima::host` |
| `ToolScope::allows(&str)` | `allows(&ScopeKey)`: exact membership. A name that does not parse is in no palette |
| `allows_action`, `allows_group_advertisement`, `allows_tool_advertisement` taking `&str` | Take `&ToolName` / `&ActionName`. New `advertises_action` (listing) and `covers_command` (delegation) name the other two rules |
| `canonical_scope_keys*`, `McpToolDescriptor::{palette_keys, owner_only_keys}`, `tool_palette_excluding` | Return `Vec<ScopeKey>` / `ToolScope`. Text still prints back as before: `key.to_string()` |
| `scope_permits_action(scope, tool, action)` | `McpToolDescriptor::action_advertised_by(scope, action)`; `advertised_by(scope)` for the tool |
| `match ToolScope::Palette(ids)` over strings | Match `ScopeKey::{Tool, Action { tool, action, .. }, Resource}`. `Action` is `#[non_exhaustive]`: only `ScopeKey::action` builds one |
| `DelegatedCommand` | Holds `ToolName` and `Option<ActionName>`: `tool()` / `action()` return them, `as_str()` and `From<GoalWakeToolId>` are gone, `to_string()` prints `tool` or `tool:action` |
| `ResourceContract::scope_key` text lookup | `FlavorContract::resource_by_scope_key(&ResourceKey)`; `ResourceContract::key()` |

**Also breaking, no shims** (#416): the Postgres testkit no longer falls back
to a default server.

| Removed / changed | Upgrade |
|---|---|
| `DEFAULT_ADMIN_URL` (`postgres://proxima:proxima@localhost/proxima`) | Removed. Set `PROXIMA_TEST_PG_URL` to the admin URL of a server you own; the testkit's sweep and template GC act on every matching database behind it |
| `PROXIMA_TEST_PG_URL` unset, empty or blank | `admin_url()` / `db_url()` panic naming the variable; the async entry points return `sqlx::Error::Configuration`. `admin_url_or_skip()` still skips locally and panics under `CI=true` |
| `ensure_template_in` | Returns a `TemplateLease` that holds the template against collection until released |
| Hand-built host template | `proxima::testkit::HostTemplate::new(TemplateFamily::new("my_host_")?, fingerprint, migrators)?`, then `.clone_split_role(prefix, schemas)`; `new` refuses a migrator lineup the runner would refuse |

**Tightened** (#417): flavor ledgers that name one table through different
spellings (case, quoting, schema, whitespace) are refused at boot, as two
identical spellings already were.

**Also breaking, no shims**: public surface that nothing wired is deleted. No
row below changes what is stored: none of these items was persisted or read
back by a shipped path. One was checked and then dropped: a citation mapping
was validated at admission (an unregistered mapping schema was refused) and
never stored, so that refusal goes with it.

| Removed / changed | Upgrade |
|---|---|
| Citation mappings: `CitationMappingPayload`, `InlineCitationMappingDraft`, `AuthorizedInlineCitationMapping`, `CitationMappingHint`, `PayloadKind::CitationMapping`, `FlavorRegistry::try_add_citation_mapping_schema`, `add_citation_mapping_schema_or_panic_for_tests`, `SidecarPayload::citation_mapping`, `SchemaInfo::cited_object_schema`, the core payload types `UploadedBlobWholeV1`, `UploadedBlobPageSpanV1`, `McpCallIoCitationV1`, the `citation_mapping_schemas` / `opaque_citation_mapping_schemas` keys of `proxima_flavor!` and `flavor_bundle!` | Delete your mapping types and registrations. No mapping was ever stored: the citation is `memory.blob_id`. A locator inside the artefact (a page, a region, a byte range) is a field of the citing Fact's payload ([11 §Payload trait](../11-citations.md#payload-trait)) |
| Registered schemas `core/uploaded-blob-whole-v1`, `core/uploaded-blob-page-span-v1`, `core/mcp-call-io-citation-v1`, and the code flavor's eight `proxima-code/*-whole-v1`; their constants `UPLOADED_BLOB_WHOLE_SCHEMA_ID`, `UPLOADED_BLOB_PAGE_SPAN_SCHEMA_ID`, `MCP_CALL_CITATION_SCHEMA` and the code flavor's `*_WHOLE_SCHEMA` | Gone from the registry and `proxima://schemas`; the ids are retired, not reused |
| `Engine::authorize_fact_with_citation(authz, draft, cited_object, mapping, sidecars)`, `_by_ref(.., cited_object_id, mapping, ..)` | Drop the `mapping` argument. By reference, storage checks that the object exists for the Fact's owner; there is no target schema left to compare. The witnesses `AuthorizedFactWithCitation` and `AuthorizedFactWithCitationRef` lose `mapping()`, and the by-reference one `expected_object_schema()` |
| `CitationSpec::v1(object_schema, hash, mapping_schema)`, `v1_for_payload(object_schema, payload, mapping_schema)` | `CitationSpec::v1(object_schema, hash)`, `v1_for_payload(object_schema, payload)`. `Citation` is `{ object }` |
| `FactCitationReadback::{citation_mapping_id, mapping_schema_id, page_span, uploaded_blob}`, `UploadedBlobRef`, and in `citation_of_fact`'s output `page_span`, `document` and `UploadedBlobDocumentOutput` | Read `cited_object_id` and `cited_object_schema_id`; the removed fields were derived from the blob or always `None` |
| `Engine::authorize_citation_attachment`, `CitationAttachmentRequest`, `AuthorizedCitationAttachment`, `citations::kind_may_cite_directly` | Removed; no storage path consumed the witness. Cite at Fact ingest |
| `PgCitedObjectSidecar`, `PgCitationMappingSidecar`, `PgSidecarRegistry::{add_cited_object, add_citation_mapping}`, `CitedObjectPayload::sidecar_table()` | Delete the impls and registrations. Citation schemas have had no sidecar since v0.0.10; no inserter was called |
| `FlavorRegistry::try_add_opaque_schema(id, version, kind) -> Result`, `add_opaque_schema_or_panic_for_tests` | `add_opaque_cited_object_schema(id, version)`, infallible: a cited object is the only kind that may be opaque. A duplicate is still refused at freeze |
| `const SPECIAL_CATEGORY` on `FactPayload`, `AbstractionPayload`, `PerspectivePayload`, `GoalPayload`, `CitedObjectPayload` | Delete it from your impls; nothing read it and the registry did not record it. A host with Art. 9 obligations keeps the classification in its own trait ([13 §Declared metadata](../13-compliance.md#declared-metadata)) |
| `GoalWriteOutcome::lifecycle_memory_id` | Removed; it was always `None`. A terminal transition's close Fact is the Goal row's `close_fact_t` |
| `RegistryProjectionPort`, `RegistryProjectionHandle`, `StoragePortsBuilder::registry_projection`, `read_models::{FactRow, AbstractionRow}`, `proxima_storage_pg::verbs::consolidate::{load_abstraction_heads, load_abstraction_heads_on_connection, load_memory_batch_facts}` | Removed; nothing read them. Use the authorized flavor reads ([public API](../reference/public-api.md#authorized-flavor-read-facade)) |
| `proxima_core::verbs::goal_write::GoalLifecycleFact`, the `goal_lifecycle_fact!` macro (`proxima-storage-pg`), `SCOPE_PREFIX`, `active_goals_for_memory_targets`, `owner_arrays`, `owned_present_chunk_indexes`, the `*_sql_for_tests` helpers, `test_fixtures::{EmbedRefusal, RefusingEmbedding}`, the storage-pg `test_fixtures::operator_proofs` module, `assert_no_serde_json_value_fields` | Removed with no caller. The code flavor's unused ingest chain and run helpers (`append_code_slice{,s}`, `existing_code_chunk_handle`, `resolve_code_chunk_handles`, `code_slice_memory_id_for`, `build_engine_with`, `get_active_run`, `retire_stale_runs`) went with them |

MCP wire, same release: `core_remember`'s `citation` drops `mapping_schema_id`,
`mapping_schema_version` and `mapping_payload`. They used to be validated and
then dropped; now a client that still sends them has them ignored without
validation (nested fields are not refused), so an unregistered mapping schema
is no longer an error.
`core_fact` `citation_of_fact` returns `cited_object_id` and
`cited_object_schema_id` only. `proxima://schemas{?kind}` no longer knows
`CitationMapping`, and Goal write outputs drop `lifecycle_memory`.

**Changed, same release**: a write refused because the publication outbox is
full now reads as backpressure on every surface. Through `Engine` it was
`ErrorCode::Internal` (MCP `-32603`, REST 500); it is the new
`ErrorCode::CapacityExhausted` (MCP `-32000` with
`data.code = "capacity_exhausted"`, REST 503 `capacity-exhausted`), the
class the direct storage path already used ([18 §Capture](../18-fact-outbox.md#capture)).
`ErrorCode` is exhaustive, so a `match` over it needs the new arm.

## v0.0.25

Pin all Proxima Rust dependencies to the same `v0.0.25` tag. No database
change. A member who holds several relations in one group now resolves to
their join, the stronger capability of each, as `Role.join` in
`docs/lean/Causa/Owner.lean` specifies. Before, the last row the resolver
yielded won, and the storage resolver yields `admin` first, so the weakest
relation won (#408).

| Surface | Upgrade |
|---|---|
| `OwnerRoles::for_subject` with one Group named more than once | Joins the roles instead of keeping the last. A host resolver that relied on a later entry overriding an earlier one must pass one entry per group |
| `OwnerRoles::with_group_role` | Unchanged: one whole answer for the group, replacing an earlier entry |
| `Role::join` | New: the least role at least as powerful as both |

## v0.0.24

Pin all Proxima Rust dependencies to the same `v0.0.24` tag. Boot applies
additive core and code-flavor migrations; no reset
([migrations.md §v0.0.24](migrations.md#v0024)). **Breaking, no shims**
(#389): one builder, one host accessor set, and every runtime feature starts
when its config is present
([10 §Runtime features](../10-configuration.md#runtime-features)).

| Removed / changed | Upgrade |
|---|---|
| Non-object tool `Output` (#374) | Return an object struct; use `struct EmptyOutput {}` for no fields. `()`, arrays, scalars and unions with non-object branches fail registration with the tool name. Object-only output unions retain their branches under `type: "object"`. Host schemas follow the same rule; invalid host tools are not listed. |
| `ProximaBuilder`, `EmbeddedProxima`, `EmbedConfig`, `EmbedError` | Boot through `Proxima::<App>::app()…build()` (or `.run()`). `flavor` / `flavor_named` / `bundle` / `pg_sidecars` → the app type's `FlavorBundle` impl; `pg_tuning`, `pg_pool_config`, `embed_client`, `embedding_router`, `embedding_runtime_policy`, `publication`, `host_state_participant`, `skip_migrations`, `runtime_grants(bool)` → the same-named `Proxima<A>` / `RuntimeBuilder` call; `deployment_tool_scope` → `tool_scope`. Errors are `ProximaError` (`EmbedError::X` → `ProximaError::X`) |
| `with_mcp()`, implicit `127.0.0.1:31415` | `mcp_bind(addr)` / `PROXIMA_MCP_BIND`. A flavor that wants MCP by default sets `mcp_bind` in `FlavorApp::configure` |
| `spawn_embedding_worker`, `spawn_publication_publisher`, `spawn_publication_publisher_supervised`, `spawn_publication_copy_cleaner` | Delete the call. The runtime starts each feature in `build()` and `run()` when configured and joins it on `shutdown()`. Cleaner config moves onto the builder: `copy_cleaner(JetStreamCopyCleanerConfig)` or `PROXIMA_COPY_CLEANER_*`. Health: `publisher_health()` / `copy_cleaner_health()` on the runtime handle |
| Public fields of `BuiltProxima` / `RunningProxima` (`engine`, `registry`, `pg_sidecars`, `blobs`, `owner`, `system_authority`, `handle`, `cancel`, `insecure_single_owner`, `service`, `mcp_addr`, `server`) | `.host().engine()` / `.registry()` / `.pg_sidecars_for_host()` / `.blobs()` / `.owner()`; `.system_authority()`; `.service()`; `.mcp_addr()`; `.shutdown().await` replaces `cancel` / `server` / `handle` |
| `BuiltProxima::shutdown` | Now `async`: `.shutdown().await` joins every started task |
| `AppContext` fields and `*_for_host` methods | `ctx.host()` → `ProximaHost`: `engine()`, `blobs()`, `owner()`, `services()`, `clone_pool_for_host()`, `pg_tuning_for_host()`, `platform_scope_for_host()`, `pg_sidecars_for_host()`, `host_state_erase_context_for_host()`, `origin_scope_for_host()`, `publication_origin_eligibility_for_host()`. The same set from `BuiltProxima` / `RunningProxima` via `.host()` |
| `RuntimeConfig` struct literals | Gains `copy_cleaner` (`outbox-nats`). `mcp` is `Some` exactly when a bind address is set |
| `PROXIMA_NATS_*` / `PROXIMA_COPY_CLEANER_*` key without its URL | Now refuses boot, naming the missing URL. `PROXIMA_NATS_URL` / `PROXIMA_COPY_CLEANER_URL` in a build without `outbox-nats` refuses boot |
| New | `boot_report()` → `BootReport`; one INFO line per feature `feature=<name> state=started\|off reason=<rule>` |
| Exhaustive `match` on `SeriesSelection` (#405) | New variants `SidecarEquals { schema, predicates }` and `DeclaredBefore { schema, column, cutoff }` ([13 §Flavor-scoped erase](../13-compliance.md#flavor-scoped-erase)); `proxima::host` now exports `SchemaId` and `SidecarAtom` |

## v0.0.23

Pin all Proxima Rust dependencies to the same `v0.0.23` tag. No database
change. A migration run now refuses a flavor lane that does not continue its
ledger, before the lane applies anything; it used to run it and fail on the
SQL, e.g. `schema "forgejo" already exists`
([migrations.md §Ledger lineage](migrations.md#ledger-lineage)).

| Surface | Upgrade |
|---|---|
| A flavor or host lane on core's `public._sqlx_migrations` (`NamedMigrator::new`) | Unchecked. Build it with `NamedMigrator::flavor(id, migrator)` to have it checked: the next run copies its rows onto `public._sqlx_migrations_<id>` and re-runs nothing |
| `skip_migrations` / `preflight_without_migrations` | Also refuses a flavor lane with a migration its ledger does not record (`LedgerConflict::Unapplied`), as core's preflight already did for core |
| Exhaustive `match` on `MigrationError` | New variants `Ledger { source, ledger, conflict: LedgerConflict }` and `LedgerRead` |

## v0.0.22

Pin all Proxima Rust dependencies to the same `v0.0.22` tag. No database
change. Additive, with one provenance change: a per-request (`_meta`
versioned) MCP call that names no `clientInfo` now records client
`unknown`; before, it recorded `rmcp`, the placeholder rmcp synthesizes for
stateless requests.

| Surface | Upgrade |
|---|---|
| Own `ServerHandler` around `DynamicHandler` | Delegate `supported_protocol_versions`, `discover`, `accepted_subscription_filter` and `listen` to it, like `list_tools`. Otherwise rmcp's defaults admit every revision rmcp knows, `server/discover` loses the per-caller instructions, and `subscriptions/listen` is refused |
| Tool lists that change at runtime | `CoreMcpTools::with_tool_list_notifier(notifier.clone())`, then `notifier.notify(&owner)` on every change to that owner's tools. A wrapping handler that overrides `get_info` or `initialize` keeps `tools.listChanged` and delegates `initialize`, which registers the session |

## v0.0.21

Pin all Proxima Rust dependencies to the same `v0.0.21` tag. No database
change. Not additive: `OidcRoleShape` gains `Host` (exhaustive matches; its
`PartialEq` compares a `Host` shaper by instance); `RuntimeParts` and
`RuntimeConfig` gain public fields (struct literals); a palette built by
`canonical_scope_keys*` / `tool_palette_excluding` now carries every
`tool:action` leaf of an argv-keyed dispatcher (before: the bare name, which
the gate never admitted).

| Surface | Upgrade |
|---|---|
| Host MCP tools | A hand-written `ServerHandler` that lists and dispatches host tools → `RuntimeBuilder::host_tools(Arc<dyn McpHostTools>)`; the tools run through the registry's request behaviors. Copied handler helpers → `proxima::{auth_context, author_from_args, strip_call_context_args, reject_nul_in_args, tool_invocation_error_to_error_data, …}` |
| Own router | A second `RuntimeBuilder::resolve` for allowlists/revalidation/metadata → `BuiltProxima::mcp_edge()`; auth on `/mcp` only → `McpEdge::router(app_router)` or `layered_router_mcp_only` |
| Call log | Host-side `log_mcp_call` on the served path and actor plumbing → `RuntimeBuilder::record_mcp_calls(true)` (actor = verified subject, no body) |
| OIDC | A direct `jsonwebtoken` validator for custom claims → `OidcTokenValidator::validate_with::<C>`; a re-implemented multi-audience loop → `OidcBindingSet` with `OidcRoleShape::Host` |
| System context | An empty bearer through a do-nothing authenticator → `AuthzContext::for_system(&SystemAuthority, OwnerRoles)` |
| Palette keys | Hand-derived `tool:action` lists → `McpToolDescriptor::palette_keys()` / `owner_only_keys()` |
| Platform-scoped resolver | Per-session `app.proxima_scope` in a host pool → `RuntimeBuilder::authenticator_with_platform_scope(\|ctx\| ..)` with the runtime's validated `PgPlatformScope` |

## v0.0.20

Pin all Proxima Rust dependencies to the same `v0.0.20` tag. Hand-written
flavors keep compiling. Not additive: the host-state registration row; the
new `MigrationError::DuplicateLedger` variant (exhaustive matches); and the
code flavor's `execution_plan_v1` policies, now keyed on the plan's own `t`,
so a caller with Fact rights alone no longer reaches plan rows.

| Surface | Upgrade |
|---|---|
| Database | Core `0018_v020_owner_rls_installer.sql` adds `proxima_core.install_owner_rls`; code flavor `20260924000020_v020_owner_rls_installer.sql` re-installs its policies through it. Existing databases upgrade in place. |
| Owner RLS | Replace a flavor's hand-written owner-RLS `DO` block with one `SELECT proxima_core.install_owner_rls(...)` in a **new** migration ([09 §Owner RLS](../09-developing-flavors.md#owner-rls)); never edit the released file. The installer keys an FK-parent table on the FK of its leading primary-key column; a v0.0.15-style block took the first FK by creation order, so diff the policies before and after. A table added later takes one `proxima_core.install_owner_rls_table(schema, table, class)` call in the migration that creates it (core 0024); the other tables' policies stay as they are. |
| Flavor ledger | `NamedMigrator::new(id, m)` → `NamedMigrator::flavor(id, m)`: own ledger `public._sqlx_migrations_<id>` (a migrator that already declares a table keeps it). Rows on core's ledger are copied over on the next migration run and stay there, so neither binary re-runs anything. Two flavors on one ledger refuse. |
| Bundle | `proxima::flavor_bundle! { bundle = …, <proxima_flavor! keys>, migrations = …, app = { … } }` replaces `proxima_flavor!` + `register_pg_sidecars` + `impl FlavorBundle` (+ `FlavorApp::app_info`) ([09 §FlavorBundle](../09-developing-flavors.md#flavorbundle)). |
| Tests | `proxima = { features = ["testkit"] }`: `proxima::testkit::{SplitRoleDb, split_role_urls_for, scoped_authz, assert_trigger_migrations}`; the feature now also enables `proxima-core/test-fixtures`. `proxima::testkit` is a module re-exporting `proxima-pg-testkit`, so existing `proxima::testkit::…` paths still resolve. |
| Side-effect Facts | `proxima::flavor::ingest_fact_detached(&ctx, write, deadline)` / `Engine::ingest_fact_detached` replace hand-rolled spawn + timeout + join. |
| Host state (behavior change) | A second `host_state_participant` registration — same builder, or overlay over `FlavorApp::configure` — now refuses boot (`ProximaError::Config`) instead of replacing the first. `HostStateRequest::is::<C>()` / `try_downcast::<C>()` dispatch without consuming the request. |
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
| Custom `WriteSession` implementations | Implement `apply_host_state`; unsupported backends must refuse before mutation. PostgreSQL hosts register typed participants with `RuntimeBuilder::host_state_participant`. See [transactional host state](../reference/public-api.md#supported-tiers). |
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
