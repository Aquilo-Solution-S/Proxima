//! Host-facing facade exports.

pub use proxima_core::mcp::parse_wire_uuid;
/// Transport adapters for hosts; flavor implementations use `flavor::Tool`.
pub use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError, McpToolErrorKind};
pub use proxima_core::operator_label;
pub use proxima_core::{memory_store_number, new_memory_uuid};

pub use crate::app::{AppContext, AppInfo, Authz, FlavorApp};
pub use crate::core_mcp::{CoreMcpError, CoreMcpErrorKind, CoreMcpTools, CoreToolInfo};
pub use crate::features::FeatureControl;
pub use crate::features::{BootReport, Feature, FeatureDecision, FeatureState};
pub use crate::mcp_edge::{McpEdge, layered_router_mcp_only};
pub use crate::migrations::{
    CoreAdoption, LedgerConflict, MigrationError, MigrationRunReport, MigrationStorage,
    MissingMigrationPolicy, NamedMigrator, PendingMigration, flavor_ledger_table,
    is_flavor_ledger_id, pending_migrations, pending_migrations_with_policy,
    preflight_without_migrations, preflight_without_migrations_with_policy,
    run_core_and_flavor_migrations, run_core_and_flavor_migrations_with_policy,
};
pub use crate::owner_access::{ForwarderPolicy, OwnerRegistry};
pub use crate::proxima_host::ProximaHost;
pub use crate::runtime::{
    BuiltProxima, Proxima, RunningProxima, layered_router, layered_router_with_revalidation, run,
    serve,
};
/// The type of the public field [`RuntimeConfig::auth`].
pub use crate::runtime_config::RuntimeAuthState;
pub use crate::runtime_config::{
    Cause, McpSettings, PlatformAuthContext, PlatformAuthenticatorFactory, ProximaError,
    RuntimeBuilder, RuntimeConfig, RuntimeParts, cause_with, embedding_runtime_policy_from_lookup,
    migration_database_url_from_lookup, missing_migration_policy_from_lookup, parse_store_number,
};
/// The S3-backed cited-blob lane.
///
/// [`ProximaHost::blobs`] returns `Option<&CitedBlobStore>`
/// and [`crate::Proxima::s3`] is a `pub` method taking `S3RuntimeConfig`, so
/// both types were already part of the public surface — just not nameable
/// from `proxima`. A flavor could reach them by inference and could not
/// write either one in a signature, store one in a struct, or configure S3
/// programmatically; `S3RuntimeConfig::from_env` was the only route in, and
/// it reads process environment a library has no business requiring.
///
/// `BlobError` comes with them because `from_env` returns it.
pub use proxima_blob_s3::{BlobError, CitedBlobStore, S3KeyPair, S3RuntimeConfig};
/// The Postgres schemas a frozen registry stores into, sorted. Boot derives
/// its runtime-grant plan, owner-RLS census and platform scope from this
/// function, so a host that freezes a registry without booting runs the same
/// checks or grants on the same set.
pub use proxima_core::composed_schema_names;
pub use proxima_core::cursor::Cursor;
/// The read verb a flavor searches its own corpus with.
///
/// [`proxima_core::Engine::search`] was already public, but every type in
/// its signature was off the facade — so a flavor could write a corpus and
/// had no sanctioned way to query it. Its own MCP tools would have had to
/// re-implement search against raw SQL, which is exactly the coupling the
/// tiered facade exists to prevent.
///
/// Declared comparable projections participate without tags. A `schema_id`
/// selects one schema, while [`MemorySearchRequest::tags`] narrows rows through
/// its declared tag column; for example, a book tag selects one book's corpus.
pub use proxima_core::engine::{
    FactWrite, HostStateCommand, HostStateMaintenanceAuthority, HostStateOutcome,
    HostStateUnitOfWork, SearchReadRequest, SearchReadResponse, UnitOfWork,
};
/// The owner-authorized batch Memory read already exposed by
/// [`proxima_core::Engine::get_memories`].
///
/// Keeping the request, response, and snapshot on the host facade makes the
/// existing Engine signature nameable without reaching through the facade to
/// `proxima-core`. Authorization and the absent/invisible collapse remain in
/// the Engine verb; this export adds no storage access.
pub use proxima_core::engine::{
    GetMemoriesReadRequest, GetMemoriesReadResponse, GetMemoryReadRequest, GetMemoryReadResponse,
    GoalCreatePayloadWriteRequest, GoalDecomposeRequest, GoalMarkAchievedRequest,
    GoalModifyRequest, GoalTransitionRequest,
};
pub use proxima_core::error::{ErrorCode, ProtocolError};
/// The disposition inside `EraseRule::HostState { whole_owner, source,
/// exact_fact }`. [`EraseRule`] is a Flavor SDK type
/// and that variant cannot be built without this one, so it is exported to both
/// tiers; it is a `Copy` contract enum and carries no authority.
pub use proxima_core::flavor::HostStateEraseDisposition;
/// Core's renderer bands for projections declaring [`BandComparability::CoreBands`].
pub use proxima_core::flavor0::{BAND_EXACT, BAND_RESCUE, BAND_SUBSTRING};
/// Explicitly select the deployment's lexical configuration for a memory write.
pub use proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT;
pub use proxima_core::llm;
/// [`EmbedCaps`] is the second parameter of
/// [`OpenAiCompatEmbeddingClient::new`], so without it on the facade that
/// constructor is unspellable for a host depending on `proxima` alone. That
/// rules out OpenAI-compatible endpoints needing `matryoshka` to return a
/// supported [`llm::EmbeddingDim`] rather than their native width.
pub use proxima_core::models::EmbedCaps;
/// Owner erase surface. Note that [`OwnerEraseTarget`]'s
/// variants take id newtypes rather than bare UUIDs and strings:
/// `GroupOwner` takes a [`GroupId`], the two source-scope variants add a
/// [`SourceId`], and the personal variants take a [`UserId`]. All of
/// those are re-exported below, so every variant of this enum is
/// constructible by a host depending on `proxima` alone — an exported
/// enum whose variants cannot be built is not actually exported.
///
/// The export half comes with it. `Engine::export_owner_bundle` is `pub`
/// and returns a [`OwnerExportBundle`] built from a
/// [`OwnerExportTarget`], and neither type was nameable from
/// `proxima` — so a host could call the verb, could not declare a variable
/// for what it returned, and could not build the argument at all. The
/// portability half of an owner's rights was reachable only by a crate that
/// depended on `proxima-core` directly, which is the coupling the tiered
/// facade exists to prevent.
pub use proxima_core::owner_inverse::{
    OwnerEraseCounts, OwnerEraseOutcome, OwnerEraseRefusal, OwnerEraseRequest, OwnerEraseTarget,
    OwnerExportBundle, OwnerExportRequest, OwnerExportTarget,
};
pub use proxima_core::read_models::MemorySnapshot;
/// The owners a [`HostStateCommand`] payload names. [`PgCommandDispatcher`]
/// compares them with the engine-stamped permit before any handler runs.
/// Host API only; Flavor SDK does not export it.
pub use proxima_core::storage_ports::HostStatePayloadOwners;
/// Other-store ports for [`RuntimeBuilder::other_stores`] and
/// [`Proxima::other_stores`], with every request, answer and bounded-service
/// result nameable by hosts implementing or wiring the port.
pub use proxima_core::storage_ports::other_stores::{
    OtherStores, OtherStoresError, OtherStoresService, ReadAt, ReadOrigin, RemoteHit, RemoteMemory,
    RemoteOriginRead, RemoteRead, RemoteSearch, RemoteSearchResult, StoreCaller, StoreInfo,
    StoreInventory,
};
/// Cited-blob verified-read and reconciliation surfaces.
///
/// Global [`CitedBlobStore::reconcile_all`] requires the booted runtime's
/// [`crate::SystemAuthority`] and returns the operator DTO, including raw
/// locator samples needed for restore work. Flavor tools use the separately
/// authorized owner port/service; its DTO carries cited-object ids and counts
/// but never bucket names or object keys. Verified reads are a separate
/// owner-authorized service with a required byte ceiling and locator-free DTO.
pub use proxima_core::storage_ports::{
    CitedBlobIntegrityMismatch, CitedBlobMissingObject, CitedBlobOwnerMissingObject,
    CitedBlobOwnerReconcileOutcome, CitedBlobOwnerReconcilePort, CitedBlobOwnerReconcileService,
    CitedBlobReadError, CitedBlobReadPort, CitedBlobReadService, CitedBlobReconcileOutcome,
    HostStateEraseReceipt, HostStateEraseRequest, HostStateEraseScope, HostStateEraseTableCount,
    HostStateExportReceipt, HostStateExportRequest, HostStateExportTable,
    HostStateParticipantDescriptor, HostStateParticipantId, HostStateReply, HostStateReplyKind,
    HostStateRequest, HostStateWriteOrigin, HostStateWritePermit, MAX_RECONCILE_SAMPLE,
    StateSurfaceName, VerifiedCitedBlob,
};
pub use proxima_core::verbs::fact_ingest::{
    CitationSpec, FactIngestOutcome, FactReceiptDraft, FactWriteCommand, IngestReceipt,
    OCCURRED_AT_FUTURE_BOUND,
};
pub use proxima_core::verbs::goal_write::{
    ChildGoalDraft, DecomposeGoalOutcome, GoalAssignmentTarget, GoalAuthorship, GoalCreateRequest,
    GoalDependencyRef, GoalEvidenceRef, GoalPayloadWrite, GoalState, GoalTopologyWrite,
    GoalWriteBuildError, GoalWriteOutcome, IdempotencyKey, MAX_GOAL_TEXT_CHARS,
    MAX_GOAL_TITLE_CHARS, OperatorKind, SystemOrigin,
};
/// Flavor-scoped erase for the host's retention schedule: open a unit with
/// `Engine::system_unit_of_work` and call [`UnitOfWork::erase_own_series`].
pub use proxima_core::verbs::own_erase::{
    EraseMode, MAX_ERASE_SERIES_PER_CALL, MAX_ERASE_VERSIONS_PER_CALL, SeriesEraseError,
    SeriesEraseReceipt, SeriesEraseRefusal, SeriesEraseRefusalKind, SeriesSelection,
};
/// Typed derived-memory writes; handles identify series and outcomes identify rows.
pub use proxima_core::{
    DerivationIdentity, DerivedMemory, DerivedMemoryOutcome, MemoryTarget, SeriesHandle,
};
pub use proxima_core::{FeatureControlError, FeatureStatus};
/// Frozen-registry catalog element. [`FlavorRegistryFrozen::list_mcp_tools`]
/// already returns `&[McpToolDescriptor]`; without these names a host
/// depending only on `proxima` cannot write a typed signature or match
/// [`McpToolOrigin`]. `CoreToolInfo` stays the projected list DTO.
pub use proxima_core::{McpToolDescriptor, McpToolOrigin};
/// The `plaintext_hosts` field of [`S3RuntimeConfig`] holds one.
pub use proxima_core::{PlaintextHosts, PlaintextHostsError};
/// Typed dispatch for the host's one [`PgHostStateParticipant`]:
/// [`PgCommandDispatcher`] routes each command type to its
/// [`HostStateHandler`], which only receives an [`AgreedCommand`] (owners
/// agreed with the permit); a bad `register` is a [`CommandRegistrationError`]
/// at boot. Host API only; Flavor SDK does not export them.
pub use proxima_storage_pg::{
    AgreedCommand, CommandRegistrationError, HostStateHandler, PgCommandDispatcher,
};
/// The Postgres pool and query-tuning blocks.
///
/// Both are nameable from the host facade and have programmatic builder
/// methods; hosts do not need process environment to configure either block.
pub use proxima_storage_pg::{HnswIterativeScan, PgPoolConfig, PgTuning};
/// Startup-registered participant that runs host-owned state SQL on the
/// [`crate::UnitOfWork`] transaction. Host API only; Flavor SDK does not export it.
/// Its [`PgHostStateSession`] provides the live transaction and authenticated
/// subject membership reads, returned as read-only [`SubjectMemberships`].
pub use proxima_storage_pg::{
    PgHostStateLifecyclePort, PgHostStateParticipant, PgHostStateSession, SubjectMemberships,
};
// `GoalWriteBuildError`'s variants carry this, so a host that matches on
// them cannot bind the payload without being able to name its type. An
// unnameable type in a public signature is the usual shape of an
// out-of-tree blocker, so it is re-exported beside the error itself.
/// The configured consumers, as [`RuntimeConfig::outbox_consumers`] holds them.
pub use crate::runtime_config::OutboxConsumers;
/// `proxima::host::testkit` kept as a path to [`crate::testkit`].
#[cfg(feature = "testkit")]
pub use crate::testkit;
/// The citation schema id [`CitationSpec`] is written with:
/// `UPLOADED_BLOB_SCHEMA_ID` names the cited object a Fact cites.
///
/// `CitationSpec::v1` takes `impl Into<String>`, so leaving the constant
/// off the facade would not block anything — it would silently push
/// flavors onto bare string literals that no compiler could check against
/// a rename of the constant they duplicate.
pub use proxima_core::UPLOADED_BLOB_SCHEMA_ID;
/// The intake contract: what a host implements to receive captured Facts.
///
/// [`RuntimeBuilder::outbox_consumer`] hands the runtime a [`DurableIntake`]
/// per named consumer ([`RuntimeBuilder::outbox_intake`] is the consumer
/// `default`), and the runtime calls each once per captured event, in this
/// process (docs/host-integration.md). No cargo feature and no broker client is involved.
/// [`SubjectPosition`] is [`ReceivedEvent::position`], the per-series order a
/// consumer applies events in.
pub use proxima_core::publication::intake::{
    CloudEventEnvelope, DurableIntake, EventIdentity, Intake, IntakeError, ReceivedEvent,
};
/// The publication half of the engine configuration (docs/host-integration.md).
///
/// A host that registers a listenable schema must bind a
/// [`PublicationSource`]; the engine refuses the boot otherwise, naming the
/// schemas. [`PublicationLimits`] are the bounds ACTUALLY enforced at
/// capture — they travel with the draft, so there is no second copy to
/// configure and no way for the two to disagree.
pub use proxima_core::publication::{
    PublicationConfig, PublicationError, PublicationLimits, PublicationSource,
    PublicationSourceError,
};
pub use proxima_core::publication::{SubjectPosition, SubjectPositionError};
/// The host-only outbox drain contract.
///
/// The claim, mark and release surface the runtime's own drain uses, exported
/// so a host that needs a different loop (a forwarder that holds claims
/// across its own broker round trip, say) can write one. It is deliberately
/// absent from
/// `StoragePorts`, `Engine` and `ToolCtx`: a flavor that could claim an
/// outbox record could delay or suppress an export.
pub use proxima_core::storage_ports::publication::{
    AckOutcome, ClaimToken, ClaimedPublication, ConsumerName, ConsumerNameError,
    PublicationOutboxPort, PublisherId, PublisherIdError, ReleaseOutcome,
};
/// This installation's identity and the host-only provenance check.
///
/// [`OriginScope`] is what [`ProximaHost::origin_scope_for_host`] returns.
/// [`ProximaHost::publication_origin_eligibility_for_host`] returns a
/// [`PublicationOriginEligibilityPort`], whose methods answer with a
/// [`PublicationOriginEligibility`]. Without these names a host can call both
/// accessors and cannot store, pass on or write the result.
pub use proxima_core::storage_ports::publication::{
    OriginScope, PublicationOriginEligibility, PublicationOriginEligibilityPort,
};
/// Registering and retiring outbox consumers.
///
/// [`BuiltProxima::outbox_consumers`] returns the port. Host-only and held
/// beside the boot authorities, not on [`ProximaHost`]: retiring a consumer
/// cancels every delivery it was still owed.
pub use proxima_core::storage_ports::publication::{PublicationConsumerPort, RetireOutcome};
pub use proxima_core::text_bounds::TrimmedLenViolation;
pub use proxima_core::verbs::query::{
    DEFAULT_HYBRID_SEMANTIC_WEIGHT, EdgeExistsRequest, EdgeExistsResponse, EdgeFilter,
    EdgeReadCursor, EdgeReadRequest, EdgeReadResponse, EntityKind, FactCitationReadback,
    MAX_SEARCH_PAGE_LIMIT, MemoryLineageDirection, MemoryLineageEdge, MemoryLineageNode,
    MemoryLineageRequest, MemoryLineageResponse, MemoryRow, MemorySearchPage, MemorySearchRequest,
    MemorySearchResult, QueryRequest, QueryResponse, SearchCursor, SearchMode, SearchOrder,
    SidecarAtom, SupersessionStatus, TagMatch,
};
pub use proxima_core::verbs::schema::{PayloadKind, SchemaRequest, SchemaResponse};
pub use proxima_core::{
    AccessCeiling, AccessError, AccessKind, ActionName, AuthPath, Authenticator, AuthzContext,
    CapabilitySet, DelegatedAuthorityError, DelegatedAuthorityService, DelegatedCommand,
    DelegatedPhase, DelegationId, DelegationIssued, DelegationRevocation,
    EmbeddingAnnObservability, EmbeddingJobBacklog, EmbeddingOrphanCounts,
    EmbeddingOrphanSweepOutcome, EmbeddingRecallCanary, EmbeddingRuntimePolicy, Engine,
    EngineAuthority, EngineHandle, FlavorRegistryFrozen, FlavorServiceError, FlavorServices,
    ForwarderRole, GroupId, MemoryId, OwnerAccessPort, OwnerId, OwnerIdParseError, OwnerRoles,
    Relation, ResourceKey, Role, SchemaId, ScopeKey, ScopeKeyError, ServedSurface, SourceId,
    StorageError, ToolName, ToolScope, UserId, env_value, provider_safe_tool_name,
};
#[cfg(feature = "openai-compat-embed")]
pub use proxima_llm_openai_compat::{OpenAiCompatConfig, OpenAiCompatEmbeddingClient};
pub use proxima_mcp_server::selfdoc::{build_instructions, how_to_markdown};
pub use proxima_mcp_server::{
    HostAllowlist, MAX_REQUEST_BODY_BYTES, McpAuthContext, McpTransportConfig,
    RequestHeaderAllowlist, ResourceServerMetadata,
};
/// Stable exported Postgres `OwnerAccessPort` adapter for embedding hosts
/// (see [`proxima_storage_pg::PgOwnerAccessResolver`]).
pub use proxima_storage_pg::PgOwnerAccessResolver;
/// Fenced authority facts on a host's existing platform-scoped transaction.
///
/// [`PgPlatformScope::authority_on`] takes the complete [`AuthorityTargets`]
/// before returning a [`PgAuthoritySession`]. Group and person views read fresh
/// direct relations, effective roles, registered owner kinds and group topology;
/// these facts grant no authorization. The host retains SQL, commit and
/// whole-transaction retries.
pub use proxima_storage_pg::{
    AuthorityTargets, DirectGroupMembership, DirectUserMembership, GroupContainment,
    PgAuthorityGroup, PgAuthorityPerson, PgAuthoritySession, RegisteredOwner,
};
/// Owner-registry and membership writes on a host's own transaction.
///
/// No authorization runs, the authorization hooks included: the host vouches
/// for each write as it would for a row it inserts itself. The caller's transaction must be platform-scoped
/// (for example [`PgPlatformScope::begin`]); pass `&mut *tx`. These functions
/// never begin, commit, roll back, retry or set a scope. The caller retries
/// its whole transaction on [`StorageError::Retryable`]. All effects follow
/// from the committed rows; no post-commit action is required.
pub use proxima_storage_pg::{
    MembershipAddOutcome, add_group_member, bootstrap_group_admin, register_group,
    remove_group_member, remove_group_member_relation, replace_group_member_relation,
};
/// Role probes on a host's own transaction (see
/// [`proxima_storage_pg::roles_for_subject`]): the answers
/// [`PgOwnerAccessResolver`] gives, with no transaction of their own.
/// [`direct_group_relations`] is the relations written on the person's own
/// membership rows, neither inherited nor folded into one role.
pub use proxima_storage_pg::{direct_group_relations, group_role, roles_for_subject};
/// The `rmcp` crate Proxima's MCP handler is built on, at the version and
/// feature set the workspace pins (`server`, `transport-streamable-http-server`).
/// A host that implements [`rmcp::ServerHandler`] around [`DynamicHandler`]
/// uses this one rather than declaring its own: two copies would be two
/// unrelated traits. The pin is part of the facade's public API, and so is
/// the exported handler helpers' use of `rmcp`'s request-context and error
/// types. A later `rmcp` major is a breaking change of the facade. Other
/// third-party crates (`sqlx`, `axum`, `tokio`) stay the host's own
/// dependencies.
pub use rmcp;
/// Cancellation token type of [`crate::flavor::FlavorWorkerContext::cancel`].
pub use tokio_util::sync::CancellationToken;

/// Derive a deployment served surface from the frozen registry, excluding every id
/// in `exclude`. Action-scoped tools expand to `tool:action` granularity
/// (Proxima's scope gate authorizes them at that granularity), so excluding a
/// tool's name also excludes every one of its actions in one step — nothing
/// is emitted for an excluded `tool.name` at all, so an action added later to
/// an excluded tool never enters the palette.
///
/// The configured palette also carries every core resource scope key.
///
/// Every declared read remains served, including Owner-audience reads and
/// resources. Excluding a read removes its configured key, while
/// [`ServedSurface::admits`] still serves it. Callers must independently hold
/// the exact key in their [`ToolScope`], and owner authorization still applies.
#[must_use]
pub fn served_surface_excluding(
    registry: &FlavorRegistryFrozen,
    exclude: &[&str],
) -> ServedSurface {
    ServedSurface::Palette(proxima_core::canonical_scope_keys_excluding(
        registry, exclude,
    ))
}

/// The S3 lane's upload, abort and read-url DTOs, its cold store and the
/// expiry sweep's outcome, which [`CitedBlobStore`]'s `prepare_upload`,
/// `stage_upload`, `abort_upload`, `read_url`, `cold_store` and
/// `sweep_expired_uploads` take or return.
pub use proxima_blob_s3::{
    CitedBlobReadUrlOutcomeTs, CitedBlobReadUrlTs, CitedBlobUploadAbortOutcomeTs,
    CitedBlobUploadAbortTs, CitedBlobUploadCompleteTs, CitedBlobUploadPrepareOutcomeTs,
    CitedBlobUploadPrepareTs, PresignedHeaderTs, S3ColdStore, UploadExpirySweepOutcome,
};
/// The per-call marker behaviors see for host-served MCP tools
/// ([`RuntimeBuilder::host_tools`]).
pub use proxima_core::McpHostToolCall;
/// Stream revalidation cadence: the type of [`McpEdge::revalidation`] and of
/// [`RuntimeConfig::stream_revalidation`], and the parameter of
/// [`layered_router_with_revalidation`], [`layered_router_mcp_only`] and
/// [`mcp_auth_layer_with_metadata`].
pub use proxima_core::authz::RevalidationConfig;
/// Tool names and core-tool metadata for a host that gates or presents its
/// own surface. [`tool_name_matches`] compares a requested name with the
/// canonical or the [`provider_safe_tool_name`] form. [`all_core_resources`]
/// enumerates the core resources' scope keys and [`core_action_meta`] returns
/// the [`CoreActionMeta`] of one core tool action.
pub use proxima_core::mcp::{
    CoreActionMeta, all_core_resources, core_action_meta, tool_name_matches,
};
/// Field types of [`McpToolDescriptor`] and consts of [`McpTool`]; a host
/// partitioning tool surfaces by audience reads them. [`ToolEffect`] is also
/// what an [`McpHostTool`] declares, and [`McpToolAnnotations`] the MCP hints
/// it projects to.
pub use proxima_core::mcp::{
    KeyDeclaration, McpArgvActionSpec, McpToolAnnotations, McpToolAudience, Replay, ToolEffect,
};
/// The request-behavior onion. A behavior wraps [`McpToolCtx`] /
/// [`McpToolError`]; flavors register one through `proxima::flavor`.
pub use proxima_core::mcp::{Next, RequestBehavior, ToolCall};
/// The host-built behavior chain: [`Next::new`] and
/// [`McpToolHost::dispatch_through_behaviors`] take a [`TerminalDispatch`],
/// and [`ScopeGateBehavior`] is the tool-scope gate a host that assembles its
/// own chain names. Host API only; flavors register behaviors, they do not
/// build the chain.
pub use proxima_core::mcp::{ScopeGateBehavior, TerminalDispatch};
/// What a tool call answers ([`ToolReply`] of [`ToolContent`] blocks, which
/// [`McpHostTools::call`] returns and every behavior passes on) and the
/// borrowed view of one tool a behavior's [`RequestBehavior::visible`] reads.
pub use proxima_core::mcp::{ToolContent, ToolDescriptorView, ToolReply, ToolSource};
/// Host-bound `CloudEvents` extension attributes
/// ([`AuthzContext::with_publication_extensions`]); the error and value
/// types are what binding returns and `get` reads.
pub use proxima_core::publication::{
    ExtensionValue, PublicationExtensions, PublicationExtensionsError,
};
/// Ids and payload types public signatures name ([`Engine::ingest_fact`],
/// [`Engine::create_goal`], [`DerivedMemory`], [`DerivationIdentity`],
/// [`SystemOrigin`], [`FactIngestOutcome`], [`MemorySnapshot`],
/// [`Engine::transfer_to_owner`], [`Engine::authorize_fact_ingest`]). The
/// Flavor SDK names some of them under `proxima::flavor` too; these are the
/// host tier's names for them.
pub use proxima_core::{
    AbstractionPayload, CitedObjectPayload, EntityId, FactPayload, FactReceiptId, FactTombstone,
    GoalId, GoalPayload, InputContractId, ModelId, OperatorId, PayloadReference,
    PerspectivePayload, PromptVersion, ReferenceBinding, SchemaVersion, ScopeRef,
    SearchProjectionColumnKind, SidecarPayload, ToolId,
};
/// Request, row and outcome types the storage-port methods take and return
/// ([`FactIngestPort`], [`MemoryAuthoringPort`], [`GoalWritePort`],
/// [`EmbeddingJobPort`] and the other `*Port` traits), so a host or backend that
/// implements a port can write its signatures.
pub use proxima_core::{
    ActiveGoalSummary, AuthorDerivedOutcome, AuthorDerivedRequest, EmbeddableEntityRef,
    EmbeddingJobClaim, EmbeddingJobStatusCounts, EmbeddingWriteOutcome, MembershipRow,
    MemoryGraphIdentity, MemoryKindRow, MemoryOperatorKind, MemorySchemaSpec, OperatorPhase,
    goal_write::AchieveGoalAtomicRequest, goal_write::CreateGoalAtomicRequest,
    goal_write::DecomposeGoalAtomicRequest, goal_write::GoalAtomicContext, goal_write::GoalDraft,
    goal_write::GoalReplayOutcome, goal_write::GoalReplayRequest,
    goal_write::ModifyGoalAtomicRequest, goal_write::TransitionGoalAtomicRequest,
    own_erase::SeriesEraseOutcome, own_erase::SeriesEraseReport, own_erase::SeriesEraseRequest,
};
/// Host authentication: the argument and error types of
/// [`Authenticator::authenticate`], and [`authenticate`], the one mint of the
/// [`OwnerScope`] witness [`AuthzContext::owner_scope`] returns.
pub use proxima_core::{AuthError, Credentials, OwnerScope, WriterAttribution, authenticate};
/// Authorization witnesses, permits and the audit contexts the Engine derives
/// for them, so a signature that takes or returns one can be written down.
/// Naming a witness does not let anyone mint it: only the Engine's authorization
/// gates construct these, their constructors are crate-private (the opt-in
/// `testkit` feature adds `*_for_tests` constructors), and these exports confer
/// no authority.
pub use proxima_core::{
    AuthorizedFactWithCitation, AuthorizedFactWithCitationRef, AuthorizedFactWrite,
    OperatorMaintenanceProof, WritePermit, fact_ingest::AuthorizedInlineCitedObject,
    fact_ingest::AuthorizedNodeLinks, owner_inverse::EraseAuthorization,
    owner_inverse::ExportAuthorization, owner_inverse::OwnerEraseContext,
    owner_inverse::OwnerExportContext, storage_ports::EmbeddingWriteProof,
    storage_ports::MembershipEndpointAuthority, storage_ports::MembershipMemberAuthority,
    storage_ports::MembershipWritePermit, storage_ports::OperatorWriteProof,
    storage_ports::OwnerWritePermit,
};
/// The storage ports and their handles. [`StoragePortsBuilder`]'s setters take the
/// `*Handle` aliases (`Arc<dyn ...Port>`), [`StoragePortsBuilder::try_build`]
/// returns the `StoragePortsBuildError`, and a host that supplies its own backend
/// ([`Engine::with_storage_ports`]) implements the `*Port` traits those handles
/// wrap.
pub use proxima_core::{
    CitationPort, EmbeddingJobPort, EmbeddingMaintenancePort, EmbeddingTextPort,
    EmbeddingWritePort, FactIngestPort, GoalReadPort, GoalWritePort, MemoryAuthoringPort,
    MemoryInspectPort, MemoryReadPort, OwnerAccessReadPort, OwnerDropProofPort,
    OwnerEraseAuthorityPort, OwnerInversePort, OwnerMembershipAdminPort, OwnerTransferPort,
    SourceCursorPort, storage_ports::CitationHandle, storage_ports::EmbeddingJobHandle,
    storage_ports::EmbeddingMaintenanceHandle, storage_ports::EmbeddingTextHandle,
    storage_ports::EmbeddingWriteHandle, storage_ports::FactIngestHandle,
    storage_ports::GoalReadHandle, storage_ports::GoalWriteHandle,
    storage_ports::MemoryAuthoringHandle, storage_ports::MemoryInspectHandle,
    storage_ports::MemoryReadHandle, storage_ports::OwnerAccessReadHandle,
    storage_ports::OwnerDropProofHandle, storage_ports::OwnerEraseAuthorityHandle,
    storage_ports::OwnerInverseHandle, storage_ports::OwnerMembershipAdminHandle,
    storage_ports::OwnerTransferHandle, storage_ports::SourceCursorHandle,
    storage_ports::StoragePortsBuildError, storage_ports::WriteSession,
    storage_ports::WriteSessionFactory, storage_ports::WriteSessionFactoryHandle,
};
/// Rows, cursors and edges the read responses carry ([`QueryRequest`],
/// [`QueryResponse`], [`MemoryLineageRequest`],
/// [`FactCitationReadback`], [`EdgeFilter`], [`FactWriteCommand`]): a host that
/// pages a query, reads the change history or follows an edge names them.
pub use proxima_core::{
    Edge, EdgeEndpoint, EdgeKind, EdgeTargetProjection, EntityRef, query::FactCitationCursor,
    query::GoalRow, query::MemoryLineageCursor, query::QueryCursor, query::QueryPage,
};
/// Request, response and outcome types of the [`Engine`] verbs a host calls:
/// `get_graph`, `read_fact_citation`, `facts_citing_object`,
/// `list_members`, `drain_embedding_jobs`, `reconcile_embeddings`,
/// `embedding_coverage`, `purge_embedding_spaces`, `complete_upload_as_fact`,
/// `inbound_pin_nodes`, `load_sketches`, `memory_counts`,
/// `try_compose` and `with_mcp_listener`. Without them a host can call each verb and cannot
/// write its argument or result in a signature.
pub use proxima_core::{
    EmbeddingDrainOutcome, EmbeddingJobDiagnostic, EmbeddingJobState, EmbeddingPurgeOutcome,
    EmbeddingReconcileOptions, EmbeddingReconcileOutcome, EmbeddingReconcileScope,
    EmbeddingSpaceCounts, EmbeddingSpaceCoverage, EmbeddingSpaceRole, EngineMcpListener,
    FactCitationReadRequest, FactsCitingObjectReadRequest, GetGraphReadRequest,
    GetGraphReadResponse, GroupMemberPage, InboundPinQuery, MAX_EMBEDDING_DIAGNOSTIC_UNITS,
    MemoryCounts, MemoryGraphPayloadRow, MemorySketch, PinNode, RunningMcpListener, StoragePorts,
    UploadCompleted, UploadCompletionExpectation, query::FactCitationPage,
    storage_ports::StoragePortsBuilder,
};
/// What [`AuthzContext`] reports about its caller: the identity and invoking
/// tool [`AuthzContext::identity_for_revalidation`] and
/// [`AuthzContext::invoking_tool`] return, and the refusal
/// [`AuthzContext::with_trusted_model_id`] gives.
pub use proxima_core::{Identity, InvokingTool, TrustedModelIdError};
/// The tool-descriptor and handle vocabulary of [`McpToolDescriptor`],
/// [`McpTool`], [`McpToolError`],
/// [`RequestHeaderAllowlist::extract`]: a
/// host that registers or presents its own tools names them.
pub use proxima_core::{
    McpAuthorContext, McpCallFn, McpUnknownFieldPolicy, MemoryHandleClass, RequestHeaders,
    ToolError, mcp::McpActionArgSpec, mcp::McpActionSchema, mcp::McpDispatcherSchema,
};
/// The publication extension set and plan the Fact write path carries
/// ([`PublicationExtensions::iter`], [`AuthorizedFactWrite`]'s `publication`):
/// a host that binds extensions through
/// [`AuthzContext::with_publication_extensions`] reads them with these.
pub use proxima_core::{
    PublicationDraft, PublicationPlan, publication::ExtensionEntries, publication::ExtensionEntry,
};
/// What [`ToolCtx`] carries into a [`Tool`] call: the caller provenance
/// ([`ToolCaller`], from [`ToolCtx::caller`]) and the service bag ([`ToolServices`])
/// a host fills when it runs a tool directly.
pub use proxima_core::{ToolCaller, ToolCtx, ToolServices};
/// The cited-blob service port's types: [`CitedBlobService`]'s `new`,
/// `prepare_upload`, `abort_upload` and `read_url`, the staged and held blobs
/// [`CitedBlobStore`] returns, and what [`Engine::complete_upload_as_fact`]
/// reports.
pub use proxima_core::{
    UploadedBlobPayload, storage_ports::CitedBlobHeld, storage_ports::CitedBlobPort,
    storage_ports::CitedBlobReadUrl, storage_ports::CitedBlobService,
    storage_ports::CitedBlobStaged, storage_ports::CitedBlobUploadAborted,
    storage_ports::CitedBlobUploadCompleted, storage_ports::CitedBlobUploadHeader,
    storage_ports::CitedBlobUploadPrepared,
};
/// The Fact-with-citation write path: [`FactWriteCommand`],
/// [`Engine::authorize_fact_with_citation`],
/// [`Engine::authorize_fact_with_citation_by_ref`],
/// [`UnitOfWork::read_own_sidecar`] and [`Engine::decompose_goal`] take
/// these drafts and requests and return these outcomes.
pub use proxima_core::{
    fact_ingest::Citation, fact_ingest::CitedObjectHint, fact_ingest::InlineCitedObjectDraft,
    goal_write::DecomposedGoalOutcome, storage_ports::SidecarSessionRead,
};
/// The native MCP handler: what [`CoreMcpTools::into_dynamic_handler`]
/// returns and what [`McpStreamableService`] serves. A host implements
/// [`rmcp::ServerHandler`] around it, with the `rmcp` re-exported below.
pub use proxima_mcp_server::DynamicHandler;
/// What parsing the allowlists ([`OriginAllowlist::parse`],
/// [`RequestHeaderAllowlist::parse`]) and [`McpToolHost::from_database_urls`]
/// return on failure.
pub use proxima_mcp_server::McpServerError;
/// Which protected-resource document [`ResourceServerMetadata`] renders.
pub use proxima_mcp_server::ProtectedResource;
/// What [`McpToolHost`]'s dispatch methods return on failure.
pub use proxima_mcp_server::ToolInvocationError;
/// The state [`McpAuthLayer`] authenticates against.
pub use proxima_mcp_server::security::McpAuthLayerState;
/// The layers [`McpEdge::router`] and [`layered_router_mcp_only`] apply, for
/// a host that builds its own router: the body cap
/// ([`enforce_body_limit`] at the default, [`body_limit_layer`] at a given
/// size), the listener-wide Host guard, and bearer authentication with
/// protected-resource metadata. [`BodyLimitLayer`], [`HostGuardLayer`],
/// [`McpAuthLayer`] (over its [`McpAuthLayerState`]) and [`CorsLayer`] are the
/// types they and [`cors_layer`] return.
pub use proxima_mcp_server::{
    BodyLimitLayer, CorsLayer, HostGuardLayer, McpAuthLayer, body_limit_layer, enforce_body_limit,
    host_guard_layer, mcp_auth_layer_with_metadata,
};
pub use proxima_mcp_server::{
    MAX_HOST_INSTRUCTIONS_CHARS, McpHostTool, McpHostTools, McpStreamableService, McpToolHost,
    ToolListNotifier, auth_context, mcp_tool_error_to_error_data,
};
/// MCP edge wiring [`layered_router`] takes, and the listener CORS layer.
pub use proxima_mcp_server::{McpEdgeAuth, OriginAllowlist, cors_layer};
/// What [`ProximaHost::host_state_erase_context_for_host`] returns. Opaque and
/// authority-free: it grants no erase by itself.
pub use proxima_storage_pg::PgHostStateEraseContext;
/// The storage handle [`run_core_and_flavor_migrations`] and
/// [`preflight_without_migrations`] take. Host API only: the Flavor SDK has
/// no storage handle.
pub use proxima_storage_pg::PgStorage;
/// Trusted host owner registration on its platform transaction. This performs
/// no authorization or transaction control; the host vouches for the write.
pub use proxima_storage_pg::access::owner_columns::ensure_owner_row;
/// Begin a transaction under the caller's [`OwnerScope`], with the pool
/// [`ProximaHost::clone_pool_for_host`] hands out.
///
/// Host API only, for authorized extra-table adapters: every statement of
/// the operation runs on the returned transaction, since pool queries do not
/// inherit its scope. No `proxima_core.*` SQL runs through it or through the
/// pool; Fact and state writes go through [`UnitOfWork`] and
/// [`UnitOfWork::apply_host_state`]. It is not a Flavor SDK name.
pub use proxima_storage_pg::begin_owner_transaction;
/// Compare the stored core definitions with this binary, optionally under the
/// host's [`MissingMigrationPolicy`]. These inspect the caller's connection.
pub use proxima_storage_pg::definitions::{
    current_matches as core_definitions_match,
    current_matches_with_policy as core_definitions_match_with_policy,
};
/// Read one Fact receipt on a transaction the caller already holds.
///
/// Host name for [`crate::flavor::ingest_receipt`]. The Flavor SDK exports
/// the same function: a flavor that holds a shared lock in its transaction
/// calls it there, under that transaction's owner scope.
pub use proxima_storage_pg::ingest_receipt;
/// Generate a projection migration and its inverses from the flavor contract.
/// Also available through the Flavor SDK for migration authors.
pub use proxima_storage_pg::projection::{Artifact, ProjectionArtifacts, projection_artifacts};
/// Migration-only storage and core schema operations for a host's schema lane.
/// [`MigrationStorage`] is the shared bound of the facade migration runners.
pub use proxima_storage_pg::{
    PgMigrationStorage, begin_migration_transaction, core_migrator, ensure_core_schema_current,
    run_core_schema,
};
/// Owner-RLS boot: the runtime-role guard, the platform scope
/// [`ProximaHost::platform_scope_for_host`] returns, and the sqlx →
/// [`StorageError`] classifier for host-state SQL.
pub use proxima_storage_pg::{PgPlatformScope, assert_runtime_rls, is_transient_conflict, map_err};
/// The registry, contract and hook vocabulary a host composes and reads back:
/// [`Engine::try_compose`], [`ProximaError`], [`FlavorApp`],
/// [`FlavorRegistryFrozen`]'s accessors, [`ProximaHost::pg_sidecars_for_host`],
/// the authorization hooks ([`AuthorizationHook`], [`OwnerResolver`]) and the
/// sidecar registry's integrity report. The Flavor SDK names some of these under
/// `proxima::flavor` too.
pub use {
    crate::{bundle::FlavorBundle, workers::FlavorWorker, workers::FlavorWorkerContext},
    proxima_core::{
        AuthorizationHook, AuthzInput, AuthzOperation, AuthzOutcome, AuthzVeto, Band,
        BandComparability, CapabilityTag, CapabilityTagError, CounterRule, DbConstraint, DbTrigger,
        EmbedUnit, EmbeddingRecipe, EmbeddingSlot, Enforcement, EraseLeg, EraseRule, ExportRule,
        FlavorContract, FlavorDescriptor, FlavorProvenance, FlavorRegistry, FlavorRegistryError,
        ForgetLeg, ForgetRule, KeyShape, LanguagePolicy, MembershipChange, OwnerResolver,
        ProjectionDecl, ProjectionSpec, Provenance, RankSource, ResolvedEmbedUnit,
        ResourceContract, SchemaContract, SchemaRef, ScopeDecl, ScopeKind, SearchProjectionDecl,
        SubstringArm, Surface, Tool, ToolContract, TransferLeg, TransferRule, WeightedField,
        schema::MemoryEmbedUnit, schema::MemorySearchProjection,
        schema::MemorySearchProjectionField, schema::ProtocolPayload, schema::RenderBands,
        schema::SchemaInfo, schema::SchemaTombstone,
    },
    proxima_storage_pg::{
        PgSidecarKey, PgSidecarRegistry, PgSidecarRegistryFrozen, integrity::IntegrityFinding,
        integrity::IntegrityReport, integrity::IntegrityViolation, integrity::ProjectedSchema,
        sidecars::PgGoalSidecar, sidecars::PgMemoryPayload, sidecars::PgMemoryPayloadBatchFuture,
        sidecars::PgMemoryPayloadFuture, sidecars::PgMemorySidecar, sidecars::PgSidecarFuture,
        sidecars::PgSidecarReadCtx, sidecars::SidecarInsertPermit, sidecars::SidecarOwnerMode,
    },
};
/// What [`PgStorage`]'s public methods take and return beyond the migration
/// helpers: the cold store, the owner and scope surfaces, the maintenance leases,
/// change-log pruning and cold-purge retry options and outcomes, and the host
/// state erase selection [`HostStateEraseReceipt`] carries. Host API only, like
/// [`PgStorage`].
pub use {
    proxima_core::{
        ColdObjectStore, ObjectStoreId, PurgeDeleted, owner_inverse::CascadedDetail,
        owner_inverse::HostStateLifecycleSurface, owner_inverse::OwnerSurfaces,
        owner_inverse::OwnerSurfacesError, storage_ports::HostStateEraseSelection,
        storage_ports::HostStateFactCopyLocator,
    },
    proxima_storage_pg::{
        ColdPurgeRetryOptions, ColdPurgeRetryOutcome, EmbeddingMaintenanceLock,
        ObjectPurgePassOutcome, StorageMaintenanceLock, StuckPurgeDebt,
        access::scope_surfaces::ScopeSurfaces,
    },
};

#[cfg(test)]
mod tests {
    use super::{
        ActionName, CapabilitySet, FlavorRegistryFrozen, ScopeKey, ServedSurface, ToolName,
        ToolScope, served_surface_excluding,
    };
    use proxima_core::FlavorRegistry;
    use proxima_core::mcp::McpTool;
    use proxima_core::mcp::core_tools::{CoreGoalTool, SearchMemoriesTool};
    use proxima_core::protocol::resource as protocol_resource;

    // `FlavorRegistry::default()` already registers every substrate tool
    // (see `core_tools::register_all`), including the action-scoped
    // `CoreGoalTool` and the flat `SearchMemoriesTool` used below — no
    // flavor needs to be linked in for this palette-derivation test.
    fn registry() -> FlavorRegistryFrozen {
        FlavorRegistry::new().freeze_or_panic_for_tests()
    }

    fn tool(name: &str) -> ToolName {
        ToolName::parse(name).expect("a tool name")
    }

    fn key(text: &str) -> ScopeKey {
        ScopeKey::parse(text).expect("a scope key")
    }

    fn configured(surface: &ServedSurface, key: &ScopeKey) -> bool {
        match surface {
            ServedSurface::All => true,
            ServedSurface::Palette(keys) => keys.contains(key),
        }
    }

    #[test]
    fn excluding_an_action_scoped_tool_removes_every_action_entry() {
        let registry = registry();

        let scope = served_surface_excluding(&registry, &[CoreGoalTool::NAME]);

        let ServedSurface::Palette(entries) = &scope else {
            panic!("expected a palette scope")
        };
        assert!(!entries.contains(&ScopeKey::Tool(tool(CoreGoalTool::NAME))));
        assert!(
            !entries.iter().any(|entry| matches!(
                entry,
                ScopeKey::Action { tool, .. } if tool.as_str() == CoreGoalTool::NAME
            )),
            "excluding the tool name must also exclude every tool:action expansion"
        );
        assert!(entries.contains(&ScopeKey::Tool(tool(SearchMemoriesTool::NAME))));
    }

    #[test]
    fn unexcluded_action_scoped_tool_keeps_every_action() {
        let registry = registry();

        let scope = served_surface_excluding(&registry, &[]);

        assert!(configured(
            &scope,
            &ScopeKey::action(
                tool(CoreGoalTool::NAME),
                ActionName::parse("set").expect("an action name")
            )
            .expect("a leaf")
        ));
        assert!(configured(&scope, &key(SearchMemoriesTool::NAME)));
        assert!(
            !configured(&scope, &key(CoreGoalTool::NAME)),
            "flat entry must not leak for an action-scoped tool"
        );
    }

    /// The canonical configured keys include each core resource, even though
    /// deployment read admission also follows its declaration.
    #[test]
    fn palette_admits_every_core_resource() {
        let registry = registry();

        let scope = served_surface_excluding(&registry, &[]);

        for resource in proxima_core::all_core_resources() {
            assert!(
                configured(&scope, &key(resource.scope_key)),
                "palette must admit resource scope key {}",
                resource.scope_key
            );
        }
    }

    /// Excluding a resource drops its key from the palette, but a core
    /// resource is a read, and the scope gate admits reads under every
    /// palette: the exclusion no longer narrows it.
    #[test]
    fn excluding_a_resource_drops_its_key_but_not_the_read() {
        let registry = registry();

        let scope = served_surface_excluding(&registry, &[protocol_resource::MEMORY]);

        assert!(!configured(&scope, &key(protocol_resource::MEMORY)));
        let memory = proxima_core::all_core_resources()
            .find(|resource| resource.scope_key == protocol_resource::MEMORY)
            .expect("the memory resource");
        assert!(
            scope.admits(&key(protocol_resource::MEMORY), memory.declaration()),
            "a read-only resource is admitted whatever the palette holds"
        );
        assert!(
            configured(&scope, &key(protocol_resource::SCHEMAS)),
            "excluding one resource must not remove the others"
        );
    }

    #[test]
    fn excluding_an_owner_read_keeps_it_served_without_granting_the_caller_its_key() {
        let registry = registry();
        let name = "core_membership:list_members";
        let scope = served_surface_excluding(&registry, &[name]);
        let declared = registry
            .mcp_tool("core_membership")
            .expect("the membership dispatcher")
            .declaration(Some("list_members"));
        assert!(!configured(&scope, &key(name)));
        assert!(scope.admits(&key(name), declared));
        let capabilities = CapabilitySet {
            tool_scope: ToolScope::Palette(Vec::new()),
            served_surface: scope,
        };
        assert!(!capabilities.admits(&key(name), declared));
    }

    /// A host extends the canonical palette with its own flat tools by their
    /// `ToolName`; the typed key cannot be mistaken for a leaf or a resource.
    #[test]
    fn a_host_adds_its_flat_tools_to_the_canonical_palette_by_name() {
        let ServedSurface::Palette(mut entries) = served_surface_excluding(&registry(), &[]) else {
            panic!("expected a palette scope")
        };
        entries.push(ScopeKey::Tool(tool("host_echo")));
        let scope = ServedSurface::Palette(entries);

        assert!(configured(&scope, &key("host_echo")));
        assert!(configured(&scope, &key(SearchMemoriesTool::NAME)));
        assert!(ToolName::parse("host_echo:set").is_err());
        assert!(ToolName::parse("resource:host_echo").is_err());
    }
}
