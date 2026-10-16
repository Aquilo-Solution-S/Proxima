# Migrate the Flavor SDK

## Unreleased

No database migration.

| Change | Host action |
|---|---|
| Goal filters | Replace `QueryRequest.goal_state: Option<GoalState>` with `goal_states: BTreeSet<GoalState>`; empty selects all states. `goal_handles` selects series heads/history; `referenced_goal_handles` selects Facts pointing at any readable version of a Goal series ([14 §Query](../14-protocol-surface.md#query)) |
| Goal paging | `QueryCursor::Goal { handle, goal_id }` replaces `{ created_at, goal_id }`; order is `(handle,t) DESC`. Restart resource pagination: old cursor tokens are refused |
| Goal resources | `state=Active,Paused` selects a union. Read `proxima://goal/{id}` with the returned stable `handle`, not version `goal`; the resource returns the current head and its wake configuration |

## v0.0.35

Pin all Proxima dependencies to the same `v0.0.35` tag. No migration.

| Change | Host action |
|---|---|
| `PROXIMA_PLAINTEXT_HOSTS` | Optional: exact host names that may use plaintext `http://` besides loopback, for the S3 endpoint, the embeddings base URL and the HTTP outbox sink with a token ([10 §Plaintext hosts](../10-configuration.md#plaintext-hosts)). Unset ⇒ loopback only. A wildcard, range or IP literal fails boot, even when no lane is configured |
| `S3RuntimeConfig` | **Breaking:** gains `plaintext_hosts: PlaintextHosts`. `from_env` and `from_lookup` read it; a struct literal adds `plaintext_hosts: PlaintextHosts::none()` or a parsed list |
| `OpenAiCompatConfig` | **Breaking:** gains `plaintext_hosts`; `new` sets none. A host that builds its embedding client itself and wants a named host calls `.with_plaintext_hosts(PlaintextHosts::from_lookup(&lookup)?)`; a struct literal adds the field |
| HTTP outbox sink | **Behaviour change:** with `PROXIMA_OUTBOX_HTTP_TOKEN_FILE` set, `PROXIMA_OUTBOX_HTTP_URL` may now be `http://` to loopback or a named host; before, a token required `https://` everywhere |
| Endpoint validation | `PlaintextHosts`, `PlaintextHostsError` and `validate_endpoint_url_for_hosts` are on the flavor tier, `PlaintextHosts` on the host tier. `validate_endpoint_url` is unchanged: it names no host |

## v0.0.34

Pin all Proxima dependencies to the same `v0.0.34` tag. One forward core
migration ([migration policy §v0.0.34](migrations.md#v0034)); no flavor migration.

| Change | Host action |
|---|---|
| Fact occurrence time | **Breaking:** `FactWriteCommand::from_payload(source_id, payload)` loses its time argument; set a time with `.occurred_at(at)` (the new `FactWriteCommand.occurred_at`), or omit it and the Fact occurred at its write time. A caller that passed `now_utc()` passes nothing ([01 §Properties of an Event](../01-event-source.md#properties-of-an-event)) |
| `FactReceiptDraft` | **Breaking:** keeps only `source_id`; its `observed_at` and `occurred_at` are gone — the command carries the time |
| Occurrence bound | An `occurred_at` more than five minutes in the future (`OCCURRED_AT_FUTURE_BOUND`) is refused with `InvalidArgument` on every Fact write path, the library included |
| Reads | `MemoryRow`, `MemorySearchResult` and `MemorySnapshot` gain `occurred_at` (`Some` on every Fact, `None` otherwise); `core_search_memories` hits and `proxima://memory/{id}` return it as RFC 3339, omitted when absent. A struct literal of these types adds the field |
| Outbox envelope `time` | Now the Fact's `occurred_at` ([18 §Publication Contract](../18-fact-outbox.md#publication-contract)); unchanged for a Fact written without one |
| `DerivedMemory::external_origins` | **Breaking:** takes `ReadOrigin`s, which only `OtherStoresService::read_origin` makes, so every recorded remote origin was read as the caller. Remote origins must share the local origins' layer |

## v0.0.33

Pin all Proxima dependencies to the same `v0.0.33` tag. One forward core
migration ([migration policy §v0.0.33](migrations.md#v0033)); no flavor migration.

| Change | Host action |
|---|---|
| Palettes admit reads | **Behaviour change:** a `ToolScope::Palette` narrows writes and owner-audience keys, never a read for every audience ([12 §Scope keys](../12-tool-manifest.md#scope-keys)). A key declared `ToolEffect::ReadOnly` with `McpToolAudience::Shared` — flat tool, dispatcher action, host tool, core `proxima://` resource — is admitted and listed under any palette; owner authorization still applies. A host that relied on a palette (or on `tool_palette_excluding`) to *hide* such a read must stop doing so; declare a flavor tool's effect and audience correctly — an undeclared effect is a write, and an `Owner` read stays gated |
| `ToolScope::admits` | Takes a `KeyDeclaration` (effect + audience): `McpToolDescriptor::declaration(action)`, `ResourceContract::declaration()`, `KeyDeclaration::shared(effect)` for a host tool. `admits_action` judges a dispatcher leaf. `McpToolDescriptor::{open_read_keys, effect_over}` and `canonical_open_read_keys` are new |
| `PROXIMA_TOOL_DENY` | **Breaking:** an entry naming a read for every audience (`core_search_memories`, `core_upload:read_url`, `resource:memory`, …) fails boot: it never disabled anything. Remove it |
| Resource listings | `resources/list`, the REST catalog and `OpenAPI` list a resource only to a caller whose owner role may read it, as tools already were |
| `CoreMcpTools::list_core_tools_for_scope` | A dispatcher's `args_schema` and `effect` cover only the actions the scope admits |
| HTTP outbox sink | Optional: `PROXIMA_OUTBOX_HTTP_URL` (+ `PROXIMA_OUTBOX_HTTP_TOKEN_FILE`, `PROXIMA_OUTBOX_HTTP_CA_FILE`) registers the built-in consumer `http` ([18 §Built-in HTTP sink](../18-fact-outbox.md#built-in-http-sink)). A host that registers its own consumer named `http` and sets the URL fails boot |
| Other stores | Optional: `RuntimeBuilder::other_stores(Arc<dyn OtherStores>)` lets `core_search_memories`, `core_derive`, `core_interpret` and `proxima://memory/{id}` reach other stores by `<store>:<handle>` ([15 §Other stores](../15-deployment.md#other-stores)). A host without the port behaves as before. `McpTool::CALLER_RENDERED_FIELDS` (default empty) is new |

## v0.0.32

Pin all Proxima dependencies to the same `v0.0.32` tag. No migration.

| Change | Host action |
|---|---|
| S3 credentials | **Breaking:** set `PROXIMA_S3_ACCESS_KEY_ID` and `PROXIMA_S3_SECRET_ACCESS_KEY` when enabling the S3 lane. The client uses only this static pair; a host that relied on ambient environment credentials, shared config or credentials files, a profile, web identity, a container or instance role, session token, or refresh provider must configure both variables. |

## v0.0.31

Pin all Proxima dependencies to the same `v0.0.31` tag. One forward core
migration ([migration policy §v0.0.31](migrations.md#v0031)); no flavor migration.

| Change | Host action |
|---|---|
| Membership member | `OwnerId` in add/remove/relation writes and listing cursors; `bootstrap_group_admin` keeps a `UserId` first admin |
| Membership read/port types | `MembershipRow::relation` is replaced by folded `MembershipRow::role`; `OwnerMembershipAdminPort` writes take `MembershipWritePermit` with typed endpoint/member authority |
| Group nesting | A group has at most one parent and one relation (default viewer); cycles/self edges refused; no depth limit |
| Fact outbox legacy records | Before upgrading, drain undelivered pre-v0.0.28 records with a configured consumer, or retire the consumer with `PublicationConsumerPort::retire` (including an unconfigured `default`); the v0.0.31 migration refuses an undelivered legacy lane, then drops the obsolete record-level claim columns in place |
| `ReceivedEvent::position` | Now required as `SubjectPosition`; an envelope missing `subject` or `sequence` is refused at `ReceivedEvent::from_record`. Delivered pre-v0.0.28 records are not parsed again |
| Record state | `publication_outbox.state` uses `publication_record_state` (`pending`/`published`) and cannot be `claimed`; claims remain a lane concern in `publication_delivery` |
| Role resolution | Storage materializes `Role::meet` along ancestor links and `Role::join` across all relations/paths; roles climb only |
| Projection | Database triggers maintain `group_role_projection` in the membership transaction, raw SQL included; hosts never write it |
| Authorization | Engine attach requires manage on both groups; detach either; relation change parent. Host transaction forms continue to run no authorization/hooks |
| Locks | One root-keyed advisory lock per affected tree, two roots in UUID order for attach; roots confirmed after locking and retried if moved |
| Isolation | Membership writes require `READ COMMITTED`; `REPEATABLE READ` and `SERIALIZABLE` refuse with typed `StorageError::MembershipIsolation` |
| Removed | `PgOwnerAccessResolver::has_role_for_owner`; use `roles_for_subject`/`group_role` and authorize from the resulting role |
| Erasure | A subgroup prevents abandonment/erase; group erase removes its parent edge; person erase removes every projected role |

## v0.0.30

Pin all Proxima Rust dependencies to the same `v0.0.30` tag. One additive core
migration and one code-flavor migration; boot applies them ([migrations
§v0.0.30](migrations.md#v0030)). **Breaking, no shims**: an owner is one owner
id, a bare UUID.

| Removed / changed | Upgrade |
|---|---|
| `OwnerRef` / `Owner` enum (`Personal(UserId)`, `Group(GroupId)`) | `OwnerId(Uuid)`. Build it with `OwnerId::from(user_id)`, `OwnerId::from(group_id)` or `OwnerId::new(uuid)`; read it with `into_inner()`, `as_user()`, `as_group()`. A `match` on the kind has no replacement: a role comes from `OwnerRoles` |
| `OwnerRefKind`, `OwnerId::columns()`, `parse_external_key`, `external_key`, `OwnerExternalKeyParseError` | Removed. Bind the owner as one `uuid` column (`owner.into_inner()`); parse and print it with `FromStr` / `Display` (`OwnerIdParseError`) |
| A flavor table keyed `(owner_kind, owner_id, ..)` | Key it on `owner_id` alone in a new migration |
| `ScopeDecl.owner_kind_column` | Removed; a scope declares only its owner id column |
| Roles | One rule: the subject's own id holds `Role::personal()` (never manages); any other owner holds its membership role or none |
| `ForwarderPolicy::wrap(inner)` | `wrap(inner, groups)`, `groups: Arc<dyn GroupRegistry>`; the forwarder role applies only in a registered group |
| Wire | Bare UUIDs everywhere an owner is named; `personal:` / `group:` are refused |
| Hashes | Receipt, derived and interpretation ids hash the bare id: a replay across the upgrade does not deduplicate |
| New host transaction forms | The owner-registry and membership forms in [10 §Roles inside a host transaction](../10-configuration.md#roles-inside-a-host-transaction) take `&mut PgConnection`; call them with `&mut *tx` inside a platform-scoped transaction. The host vouches for the write, so no authorization runs, the authorization hooks included. Group writes share the membership lock. They write registry and membership rows, never control or retry the caller's transaction, and have no post-commit action. |

## v0.0.28

Pin all Proxima Rust dependencies to the same `v0.0.28` tag. One additive core
migration, `0026_v032_outbox_consumers.sql`; boot applies it ([migrations
§v0.0.28](migrations.md#v0028)). **Breaking, no shims**: the Fact outbox
delivers to named consumers and every event carries its place in its series.
A host that only calls `outbox_intake(..)` and implements `DurableIntake`
compiles unchanged, and its single intake is the consumer `default`.

| Removed / changed | Upgrade |
|---|---|
| `RuntimeConfig.outbox_intake: Option<Arc<dyn DurableIntake>>` | Replaced by `outbox_consumers: OutboxConsumers`. A struct literal uses `OutboxConsumers::default()` for none; the builders' `outbox_intake(..)` and the new `outbox_consumer(ConsumerName, ..)` fill it |
| `PublicationOutboxPort::claim`, `mark_published`, `release` (a host's own drain, a test double) | Each takes `consumer: &ConsumerName` first. A drain that stands in for the old single drain passes `&ConsumerName::default()` |
| `ReceivedEvent { .. }` built by hand | v0.0.28 added optional `position`; v0.0.31 requires `SubjectPosition` and refuses envelopes without `subject`/`sequence` (see §v0.0.31) |
| `SealedPublication::seal(plan, t)` | Takes a third argument, `SubjectPosition` |
| `PublicationExtensions::with("sequence", ..)` | Now refused (`SubstrateAttributeName`): the substrate sets `sequence`. Rename the attribute |
| Wire | Every captured event carries `subject` (its series, a bare UUID) and `sequence` (20-digit position, from 1) after `time` ([18 §Ordering](../18-fact-outbox.md#ordering)). An intake that rejects unknown attributes must accept them. Goal-change events carry `subject` = `data.series` and `sequence` = `data.revision` |
| Record state | `publication_outbox.state` is the aggregate of the lanes and is never `claimed`; read `publication_delivery` for one consumer's state. A host that queried `state = 'claimed'` on the record reads the lanes instead |
| New | `RuntimeBuilder::outbox_consumer` / `Proxima::outbox_consumer`; `BuiltProxima::outbox_consumers()` / `RunningProxima::outbox_consumers()` returning `Arc<dyn PublicationConsumerPort>` (`register`, `registered`, `retire -> RetireOutcome`); `ConsumerName`, `ConsumerNameError`, `OutboxConsumers`, `SubjectPosition`, `SubjectPositionError`, all exported from `proxima` ([18 §Consumers](../18-fact-outbox.md#consumers)) |

## v0.0.27

Pin all Proxima Rust dependencies to the same `v0.0.27` tag. One additive core
migration, `0025_v031_goal_text_outbox_receipt.sql`; boot applies it ([migrations
§v0.0.27](migrations.md#v0027)). **Breaking, no shims** (#435): `MemoryReadPort`
gained a required method, `GoalRow` gained fields and `GoalWriteCommand` gained
`text`, so a host or test double that implements the one or builds the others
stops compiling until it adds them; `write_goal` and `write_armed_goal` now need
the `test-fixtures` feature. The MCP tool outputs that carried a preformatted
time string now carry `time::OffsetDateTime` (same wire).

| Removed / changed | Upgrade |
|---|---|
| `impl MemoryReadPort for T` | Add `async fn count_memories(&self, owner_scope: Option<&OwnerScope>, read_owners: &[OwnerId]) -> Result<MemoryCounts, StorageError>`: current memories by kind over the owners the caller reads, one per series head and one per Goal head. Fill the count with `MemoryCounts::slot(kind)`; a stored kind that is not one of the four is `StorageError::Internal`, never skipped. A fake that holds no memories returns `Ok(MemoryCounts::default())` |
| `GoalRow { .. }` built by hand (a fake `MemoryReadPort`, a test) | Add `text: Option<String>`, `set_at: time::OffsetDateTime` and `changed_at: time::OffsetDateTime`. `text` is this version's prompt text, `None` when unknown and never blank. `set_at` is the time of the series' first version (the least `t` of the `handle`) and is the same on every version; `changed_at` is the time of this version's `id`, equal to `set_at` on the first version. Both serialize as RFC 3339 strings; `text` deserializes to `None` when absent, the two times are required |
| `GoalWriteCommand { .. }`, `write_goal` and `write_armed_goal` (`proxima_storage_pg::verbs::goal_timeseries`, `wake_timeseries`) | `write_goal` and `write_armed_goal` now need the `test-fixtures` feature: production writes go through the Goal write verbs (`create_goal_atomic` and the rest), which fill `text` and carry the prior version's text on a lifecycle write. `GoalWriteCommand` gains `text: Option<String>` after `title`: `Some` is stored as the version's text and must not be blank (the database refuses it with `goal_text_nonblank_chk`; [06 §Goal text](../06-goals-and-self.md#goal-text) defines blank); `None` stores NULL, unknown |
| `SearchMemoryOutput.created_at`, `UploadPrepareOutput.expires_at`, `UploadReadUrlOutput.expires_at` (`proxima_core::mcp::core_tools::{search_memories, upload}`) | Now `time::OffsetDateTime` instead of a preformatted `String`. They serialize with `time::serde::rfc3339` to the same RFC 3339 strings, and the tool output schemas still declare a string, so the wire is unchanged. Code that read the `String` formats the value itself |
| Changed | `proxima://goals` and `proxima://goal/{id}` return `text` (the full text in the listing too; JSON `null` when unknown, the key always present), `set_at` and `changed_at` (RFC 3339 strings) on every goal. A client that rejects unknown fields adds the three ([14 §Core Memory MCP Surface](../14-protocol-surface.md#core-memory-mcp-surface)). The export bundle's `proxima_core.goal` rows carry `text` |
| New | Resource `proxima://memories/counts` (`resource:memory-counts` scope key, REST `GET /v1/resources/memories/counts`), answering `{"counts": {"Fact", "Abstraction", "Perspective", "Goal"}}` with all four keys always present ([14 §Core Memory MCP Surface](../14-protocol-surface.md#core-memory-mcp-surface)). A palette that lists resources by key adds `ScopeKey::Resource(ResourceKey::parse("memory-counts")?)` to expose it. `Engine::memory_counts(&AuthzContext) -> Result<MemoryCounts, ProtocolError>` and `MemoryCounts` are exported from `proxima::host` |

**Also breaking, no shims** (#435): the NATS adapter is removed and the Fact
outbox is delivered in process, to an intake the host provides
([18 §Delivery](../18-fact-outbox.md#delivery)). A host that still needs a
broker forwards to it from its own `DurableIntake`
([18 §Forwarding to a broker](../18-fact-outbox.md#forwarding-to-a-broker)).

| Removed / changed | Upgrade |
|---|---|
| Crate `proxima-outbox-nats` (`OutboxPublisher`, `ReferenceConsumer`, the copy cleaner, `asyncapi_document` / `AsyncApiInfo`, `subject_for` / `parse_subject` / `DEFAULT_SUBJECT_PREFIX`, `DrainReport`, `PublishHook`, the `durable_intake` example) | Deleted, with its `async-nats` dependency. Drop the dependency. Deliver through a `DurableIntake` instead; to publish to a broker, do it inside `accept` |
| `proxima` feature `outbox-nats`; `proxima-mcp` feature `nats` | Removed. Delete them from `features = [..]` and from `cargo build --features`; the shipped `proxima-mcp` binary supplies no intake, so its outbox is captured and not drained |
| `RuntimeBuilder::nats(..)`, `copy_cleaner(..)` and the same overlay methods on `Proxima<A>` | Removed. Hand the runtime the intake with `outbox_intake(Arc<dyn DurableIntake>)` on either builder: set, the outbox publisher starts and delivers to it; unset, nothing drains, as before without a broker |
| `RuntimeConfig` struct literals | Lose `nats` and `copy_cleaner`, gain `outbox_intake: Option<Arc<dyn DurableIntake>>` |
| `PROXIMA_NATS_*`, `PROXIMA_COPY_CLEANER_*` | **No longer read, and no longer refused**: a stale `PROXIMA_NATS_URL` in the environment boots without a publisher and without a warning. Delete them from deployment manifests. The drain's batch (64), lease (30 s), poll (500 ms) and `accept` timeout (5 s) are fixed. `PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS`, `PROXIMA_OUTBOX_MAX_PENDING`, `PROXIMA_OUTBOX_MAX_PAYLOAD_BYTES` and `PROXIMA_PUBLICATION_SOURCE` are unchanged |
| `BuiltProxima::publisher_health()`, `RunningProxima::publisher_health()`, `copy_cleaner_health()` | Removed; the drain has no health reader. Read `boot_report()`, `pending_count` and the `attempts` column |
| `Feature::CopyCleaner` | Removed. `Feature::ALL` has five entries and `boot_report()` five decisions; an exhaustive `match` on `Feature` drops the arm. The publisher's off reason is now "no intake configured (outbox_intake)" |
| `PublicationOutboxPort::mark_published(id, claim, &BrokerReceipt)` | `mark_published(&self, id: Uuid, claim: ClaimToken) -> Result<AckOutcome, StorageError>`. An implementation or fake drops the parameter; a caller stops building the receipt. `BrokerReceipt` is removed from `proxima_core::storage_ports::publication` and `proxima::host` |
| `publication_outbox.published_stream`, `published_seq` | Kept, but no longer written: a record published in process leaves both `NULL`. `published_at` alone marks a published record (migration `0025` relaxes `publication_outbox_published_chk`) |
| `proxima_outbox_nats::{DurableIntake, Intake, IntakeError, ReceivedEvent, EventIdentity, CloudEventEnvelope}` (gated by `outbox-nats`) | Now `proxima_core::publication::intake::{..}`, re-exported ungated as `proxima::{..}` and `proxima::host::{..}`. `EventIdentity` and `ReceivedEvent::identity()` are unchanged |
| `ReceivedEvent { subject, stream_sequence, .. }` and `ReceivedEvent::parsed_subject(prefix)` | `subject`, `stream_sequence` and `parsed_subject` are gone. `owner: OwnerId` replaces the subject's owner token, typed from the outbox row rather than parsed back out of a string. `delivered_count: u64` is now the record's `attempts` (this delivery included). A struct literal needs `owner` |
| `IntakeError { retry_after, .. }`, `IntakeError::retry_after(delay)` | Removed: nothing honours a redelivery hint in process. `IntakeError::new(message)` is unchanged; the drain releases the claim and backs off |
| An envelope that does not parse | Was delivered under a synthetic `malformed:<hex>` id. Now logged at error level (event id and error class) and released; the intake is never called with it. It stays `pending` until an operator repairs it |

**Also breaking, no shims** (#435): a goal write reaches the feed as a core
Fact (`core/goal-change-v1`), and a pause says what happens to the work a
scheduler queued for the goal ([18 §Goal changes on the
feed](../18-fact-outbox.md#goal-changes-on-the-feed)). The schema is always
registered; it is listenable only when the runtime has an `outbox_intake`, so a
deployment without one writes no extra Fact. The core migration adds its
sidecar table ([migrations §v0.0.27](migrations.md#v0027)).

| Removed / changed | Upgrade |
|---|---|
| `GoalTransitionRequest { next_state, .. }` (`proxima_core::engine`, `proxima::host`) | `next_state: GoalState` is now `transition: GoalTransitionKind` (`proxima_core::verbs::goal_write`): `Pause { pending: PendingWork }`, `Resume`, `Abandon`. `pending` exists on `Pause` alone, so a resume or abandon that names it does not compile, and there is no `Achieved` variant (achievement is `GoalMarkAchievedRequest`; the engine's runtime refusal of an `Achieved` target is gone with the case). `GoalTransitionKind::next_state(self) -> GoalState` gives the state; `GoalChange::from(kind)` the change. `PendingWork` (`proxima_core::PendingWork`) is `Keep` or `Discard` |
| `TransitionGoalAtomicRequest<'_> { next_state, .. }` (`proxima_core::verbs::goal_write`) | The same replacement: `transition: GoalTransitionKind`. A storage implementer writes the verb's replay declaration with `pending` for a pause |
| `GoalAtomicContext<'a> { .. }` struct literal | Gains `publication: Option<PublicationStamp<'a>>` (`proxima_core::publication::PublicationStamp`, `Copy`). A host or test that builds the context by hand writes `publication: None`, which is right for any registry where `core/goal-change-v1` is not listenable. Storage writes the change Fact exactly when the schema is listenable in `registry`; with it listenable and `publication: None` the write fails with `StorageError::ConstraintViolation` naming the schema. The engine builds the stamp itself from the configured source, the token-certified model id and the host extension attributes |
| `core_goal` `action=transition` | Takes `pending: "keep" \| "discard"`: **required** with `transition: "pause"`, refused with `resume` and `abandon`. A pause without it fails `InvalidInput` naming both values. A client or script that pauses goals adds the field |
| Replay of a transition | The replay declaration of a pause records `pending`. Retrying a pause request id recorded by v0.0.26 (no `pending`) now conflicts instead of replaying; a resume or abandon declaration is unchanged and still replays. A retry of a pause with a different `pending` conflicts |
| New | Fact schema `core/goal-change-v1` (`proxima_core::GoalChangeV1`, `GoalChange`, `GoalChangeKind`, `PendingWork`), sidecar `proxima_core.goal_change_v1` registered by `register_core_pg_sidecars`. `FlavorRegistry::publish_goal_changes(&mut self)` (`#[doc(hidden)]`) turns its `listenable` on; the runtime calls it in `compose_registry` when `outbox_intake` is set, and a host that composes its own registry never needs it. `PublicationStamp::new(&PublicationSource, Option<&str>, &PublicationExtensions, PublicationLimits)` and `PublicationStamp::plan(..) -> PublicationPlan` |
| Boot | With an `outbox_intake` the schema is listenable, so an intake without `PROXIMA_PUBLICATION_SOURCE` now fails boot with a `PublicationConfig::validate_against` error that names `core/goal-change-v1`, as for any listenable schema. Set the source, or drop the intake |
| Wire | With an intake, every goal write adds one event of type `core/goal-change-v1` to the feed, `data` `{series, goal, prior, revision, state, change}` ([18 §Goal changes on the feed](../18-fact-outbox.md#goal-changes-on-the-feed)). An intake that rejects unknown event types must accept it. Deliveries can arrive out of order: key by `series` and apply only an event whose `revision` is greater than the last applied for that series |

**Also breaking, no shims** (#442): a host tool failure can carry a JSON
object as `structuredContent` beside its content blocks ([12 §Tool
Replies](../12-tool-manifest.md#tool-replies)).

| Removed / changed | Upgrade |
|---|---|
| `ToolReply` (`proxima_core::mcp`, `proxima::flavor`) | Gains `StructuredFailure { structured: serde_json::Map<String, Value>, content: Vec<ToolContent> }`, sent as `structuredContent` beside the content blocks with `isError: true`. An exhaustive `match` on `ToolReply` adds the arm; `Failure` is unchanged. A structured failure from a tool that declares an `output_schema` is an internal error (logged, returned redacted) |

**Also breaking, no shims** (#444): a host tool call carries the services of the
catalog entry it was resolved to ([10 §MCP Endpoint and
Authentication](../10-configuration.md#mcp-endpoint-and-authentication)).

| Removed / changed | Upgrade |
|---|---|
| `McpHostTool` (`proxima_mcp_server`, `proxima`) | Gains `services: FlavorServices` and `with_services(FlavorServices)`; the values join the services of a call resolved to that entry. No longer `PartialEq`: compare the fields a test cares about |

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
| NATS subjects | `proxima_outbox_nats::parse_subject(prefix, subject)` inverts `subject_for` (history: both were removed with the crate in [v0.0.27](#v0027)). |

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
