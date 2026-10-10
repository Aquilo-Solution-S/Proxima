//! Host-facing facade exports.

/// Transport adapters for hosts; flavor implementations use `flavor::Tool`.
pub use proxima_core::mcp::{
    McpTool, McpToolCtx, McpToolError, McpToolErrorKind, McpToolPresentation,
};
pub use proxima_core::operator_label;

pub use crate::app::{AppContext, AppInfo, Authz, FlavorApp};
pub use crate::core_mcp::{CoreMcpError, CoreMcpErrorKind, CoreMcpTools, CoreToolInfo};
pub use crate::features::{BootReport, Feature, FeatureDecision, FeatureState};
pub use crate::health::{HEALTHZ_PATH, READYZ_PATH};
pub use crate::mcp_edge::{McpEdge, layered_router_mcp_only};
pub use crate::migrations::{
    LedgerConflict, MigrationError, MigrationRunReport, NamedMigrator, flavor_ledger_table,
    is_flavor_ledger_id, preflight_without_migrations, run_core_and_flavor_migrations,
};
pub use crate::owner_access::ForwarderPolicy;
pub use crate::proxima_host::ProximaHost;
pub use crate::runtime::{
    BuiltProxima, Proxima, RunningProxima, layered_router, layered_router_with_revalidation, run,
    serve,
};
/// The type of the public field [`RuntimeConfig::auth`].
pub use crate::runtime_config::RuntimeAuthState;
pub use crate::runtime_config::{
    McpSettings, PlatformAuthContext, PlatformAuthenticatorFactory, ProximaError, RuntimeBuilder,
    RuntimeConfig, RuntimeParts, embedding_runtime_policy_from_lookup,
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
pub use proxima_blob_s3::{BlobError, CitedBlobStore, S3RuntimeConfig};
pub use proxima_core::cursor::Cursor;
/// The read verb a flavor searches its own corpus with.
///
/// [`proxima_core::Engine::search`] was already public, but every type in
/// its signature was off the facade — so a flavor could write a corpus and
/// had no sanctioned way to query it. Its own MCP tools would have had to
/// re-implement search against raw SQL, which is exactly the coupling the
/// tiered facade exists to prevent.
///
/// [`MemorySearchRequest::tags`] is the only predicate that narrows a
/// search to a subset of a corpus. `schema_id` is exact-match and there is
/// no per-column filter, so a flavor that wants "search inside this book"
/// declares a `tag_column` on its projection and filters here.
pub use proxima_core::engine::{
    FactWrite, HostStateCommand, HostStateMaintenanceAuthority, HostStateOutcome,
    HostStateUnitOfWork, ListWakeCandidatesReadRequest, ListWakeCandidatesReadResponse,
    SearchReadRequest, SearchReadResponse, UnitOfWork,
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
pub use proxima_core::verbs::change_history::{ChangeHistoryRequest, ChangeHistoryResponse};
pub use proxima_core::verbs::fact_ingest::{
    CitationSpec, FactIngestOutcome, FactReceiptDraft, FactWriteCommand,
};
pub use proxima_core::verbs::goal_write::{
    ChildGoalDraft, DecomposeGoalOutcome, GoalAssignmentTarget, GoalAuthorship, GoalCreateRequest,
    GoalDependencyRef, GoalEvidenceRef, GoalPayloadWrite, GoalState, GoalTopologyWrite,
    GoalWakeConfigWrite, GoalWakeToolId, GoalWakeTrigger, GoalWriteBuildError, GoalWriteOutcome,
    IdempotencyKey, MAX_GOAL_TEXT_CHARS, MAX_GOAL_TITLE_CHARS, MAX_WAKE_TOOL_ID_CHARS,
    OperatorKind, SystemOrigin,
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
/// Typed result of the owner-authorized cold-memory hydration command. The
/// facade exposes only ids and classifications; Postgres transactions,
/// locators, and cold-store keys remain backend implementation details.
pub use proxima_core::{
    MAX_MEMORY_HYDRATION_BATCH, MemoryHydrationBatchOutcome, MemoryHydrationOutcome,
    MemoryHydrationStatus,
};
/// Frozen-registry catalog element. [`FlavorRegistryFrozen::list_mcp_tools`]
/// already returns `&[McpToolDescriptor]`; without these names a host
/// depending only on `proxima` cannot write a typed signature or match
/// [`McpToolOrigin`]. `CoreToolInfo` stays the projected list DTO.
pub use proxima_core::{McpToolDescriptor, McpToolOrigin};
/// The Postgres pool and query-tuning blocks.
///
/// Both are nameable from the host facade and have programmatic builder
/// methods; hosts do not need process environment to configure either block.
pub use proxima_storage_pg::{HnswIterativeScan, PgPoolConfig, PgTuning};
/// Startup-registered participant that runs host-owned state SQL on the
/// [`crate::UnitOfWork`] transaction. Host API only; Flavor SDK does not export it.
pub use proxima_storage_pg::{PgHostStateLifecyclePort, PgHostStateParticipant};
// `GoalWriteBuildError`'s variants carry this, so a host that matches on
// them cannot bind the payload without being able to name its type. An
// unnameable type in a public signature is the usual shape of an
// out-of-tree blocker, so it is re-exported beside the error itself.
/// `proxima::host::testkit` kept as a path to [`crate::testkit`].
#[cfg(feature = "testkit")]
pub use crate::testkit;
/// The publication half of the engine configuration (docs/18).
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
/// The host-only outbox drain contract.
///
/// Exported so a host can implement its own publisher against a broker this
/// workspace ships no adapter for. It is deliberately absent from
/// `StoragePorts`, `Engine` and `ToolCtx`: a flavor that could claim an
/// outbox record could delay or suppress an export.
pub use proxima_core::storage_ports::publication::{
    AckOutcome, BrokerReceipt, ClaimToken, ClaimedPublication, PublicationOutboxPort, PublisherId,
    PublisherIdError, ReleaseOutcome,
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
pub use proxima_core::text_bounds::{TrimmedLenViolation, check_trimmed_len};
pub use proxima_core::verbs::mcp_call_history::{
    MAX_MCP_CALL_HISTORY_LIMIT, McpCallHistoryRequest, McpCallHistoryResponse, McpCallRecord,
};
pub use proxima_core::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
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
    AccessCeiling, AccessError, AccessKind, AuthPath, Authenticator, AuthzContext,
    DelegatedAuthorityError, DelegatedAuthorityService, DelegatedCommand, DelegatedPhase,
    DelegationId, DelegationIssued, DelegationRevocation, EmbeddingAnnObservability,
    EmbeddingJobBacklog, EmbeddingOrphanCounts, EmbeddingOrphanSweepOutcome, EmbeddingRecallCanary,
    EmbeddingRuntimePolicy, Engine, EngineAuthority, EngineHandle, FlavorRegistryFrozen,
    FlavorServiceError, FlavorServices, GoalWakeCandidate, GoalWakeHardMemory, GroupId, MemoryId,
    Owner, OwnerAccessPort, OwnerExternalKeyParseError, OwnerRef, OwnerRefKind, OwnerRoles,
    Relation, Role, SchemaId, SourceId, StorageError, ToolScope, UserId, canonical_json_bytes,
    env_value, parse_external_key, provider_safe_tool_name,
};
/// The three citation schema ids [`CitationSpec`] is written with:
/// `UPLOADED_BLOB_SCHEMA_ID` names the cited object, and the other two
/// name the locator mapping through which a Fact cites it — the whole
/// object, or a page span within it.
///
/// A flavor citing an uploaded blob names a mapping id in every
/// `CitationSpec::v1` call. `CitationSpec::v1` takes `impl Into<String>`,
/// so leaving two of the three off the facade did not block anything —
/// it silently pushed flavors onto bare string literals that no compiler
/// could check against a rename of the constant they duplicate.
pub use proxima_core::{
    UPLOADED_BLOB_PAGE_SPAN_SCHEMA_ID, UPLOADED_BLOB_SCHEMA_ID, UPLOADED_BLOB_WHOLE_SCHEMA_ID,
};
#[cfg(feature = "openai-compat-embed")]
pub use proxima_llm_openai_compat::{OpenAiCompatConfig, OpenAiCompatEmbeddingClient};
pub use proxima_mcp_server::selfdoc::{build_instructions, how_to_markdown};
pub use proxima_mcp_server::{
    HostAllowlist, MAX_REQUEST_BODY_BYTES, McpAuthContext, McpTransportConfig,
    RequestHeaderAllowlist, ResourceServerMetadata,
};
/// The reference consumer's defaults ([`NatsConsumerConfig`]), beside
/// [`DEFAULT_SUBJECT_PREFIX`]. The crate defines them in `config` and does not
/// re-export them at its root.
#[cfg(feature = "outbox-nats")]
pub use proxima_outbox_nats::config::{DEFAULT_CONSUMER_NAME, DEFAULT_CONSUMER_STREAM};
/// The shipped NATS `JetStream` publisher and its reference consumer
/// (docs/18, `crates/outbox-nats`).
///
/// Behind the `outbox-nats` feature: a deployment that registers no
/// listenable schema should not link a broker client. The boot guarantee
/// (listenable schema ⇒ bound source) holds without this feature.
#[cfg(feature = "outbox-nats")]
pub use proxima_outbox_nats::{
    AsyncApiError, AsyncApiInfo, ConfigError as NatsConfigError, ConsumerError,
    DEFAULT_SUBJECT_PREFIX, DrainReport, DrainSummary, DurableIntake, EventIdentity, Intake,
    IntakeError, JetStreamPublisher, NatsAuth, NatsConsumerConfig, NatsPublisherConfig,
    ParsedSubject, PublisherError, ReceivedEvent, ReferenceConsumer, SubjectParseError,
    asyncapi_document, parse_subject, subject_for,
};
/// The retained-copy cleaner's config ([`RuntimeBuilder::copy_cleaner`]) and
/// the health views [`BuiltProxima::publisher_health`] /
/// [`BuiltProxima::copy_cleaner_health`] return, with every type their
/// snapshots carry.
#[cfg(feature = "outbox-nats")]
pub use proxima_outbox_nats::{
    CleanerConfigError, CopyCleanerConnectionState, CopyCleanerFailure, CopyCleanerHealth,
    CopyCleanerHealthReader, CopyCleanerScanState, CopyCleanerTaskState, InboxPrefix,
    JetStreamCopyCleanerConfig, PublisherConnectionState, PublisherDrainState, PublisherHealth,
    PublisherHealthReader, PublisherTaskState,
};
/// Stable exported Postgres `OwnerAccessPort` adapter for embedding hosts
/// (see [`proxima_storage_pg::PgOwnerAccessResolver`]).
pub use proxima_storage_pg::PgOwnerAccessResolver;
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

/// Build the complete REST `OpenAPI` document from a frozen registry.
///
/// This offline projection contains every registered tool and core resource.
/// The served `/v1/openapi.json` route uses the same generator with its
/// caller-scoped authorization context. Core resources are included
/// automatically; callers never assemble transport-internal descriptor
/// slices or depend on `proxima-mcp-server` directly.
#[cfg(feature = "rest")]
#[must_use]
pub fn build_openapi_document(
    registry: &FlavorRegistryFrozen,
    public_url: Option<&str>,
) -> serde_json::Value {
    proxima_mcp_server::rest::openapi::document_from_registry(registry, public_url, None)
}

/// Derive an agent-safe MCP tool palette from the frozen registry, excluding
/// every id in `exclude`. Action-scoped tools expand to `tool:action`
/// granularity (Proxima's scope gate authorizes them at that granularity),
/// so excluding a tool's name also excludes every one of its actions in one
/// step — nothing is emitted for an excluded `tool.name` at all, so a newly
/// added action on an already-excluded tool can never silently bypass the
/// exclusion list.
///
/// The palette also carries every core resource scope key. `read_resource`
/// runs through the same flat scope gate as a tool call, so a palette built
/// from tools alone denies every `proxima://` read outright rather than
/// merely leaving it unadvertised. Exclude a resource by its exact scope key.
#[must_use]
pub fn tool_palette_excluding(registry: &FlavorRegistryFrozen, exclude: &[&str]) -> ToolScope {
    ToolScope::Palette(proxima_core::canonical_scope_keys_excluding(
        registry, exclude,
    ))
}

/// The S3 lane's upload, abort and read-url DTOs and its cold store, which
/// [`CitedBlobStore`]'s `prepare_upload`, `stage_upload`, `abort_upload`,
/// `read_url` and `cold_store` take or return.
pub use proxima_blob_s3::{
    CitedBlobReadUrlOutcomeTs, CitedBlobReadUrlTs, CitedBlobUploadAbortOutcomeTs,
    CitedBlobUploadAbortTs, CitedBlobUploadCompleteTs, CitedBlobUploadPrepareOutcomeTs,
    CitedBlobUploadPrepareTs, PresignedHeaderTs, S3ColdStore,
};
/// Host-served MCP tools ([`RuntimeBuilder::host_tools`]), the per-call
/// marker behaviors see, and the handler helpers a host transport reuses
/// instead of copying: auth/peer extraction, author reconciliation, the
/// reserved-argument strip, the NUL guard and the JSON-RPC error mapping.
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
    McpArgvActionSpec, McpToolAnnotations, McpToolAudience, Replay, ToolEffect,
};
/// The request-behavior onion. A behavior wraps [`McpToolCtx`] /
/// [`McpToolError`]; flavors register one through `proxima::flavor`.
pub use proxima_core::mcp::{Next, RequestBehavior, ToolCall};
/// The `<prefix>:<uuid>` ids tool arguments carry. [`PrefixedUuidError`]
/// is what [`parse_prefixed_uuid`] returns, so it travels with the parser.
pub use proxima_core::mcp::{
    PrefixedUuidClass, PrefixedUuidError, format_prefixed_uuid, parse_prefixed_uuid,
};
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
    AbstractionPayload, CitationMappingPayload, CitedObjectPayload, EntityId, FactPayload,
    FactReceiptId, FactTombstone, GoalId, GoalPayload, InputContractId, ModelId, OperatorId,
    PayloadReference, PerspectivePayload, PromptVersion, ReferenceBinding, SchemaVersion, ScopeRef,
    SearchProjectionColumnKind, SidecarPayload, ToolId,
};
/// Request, row and outcome types the storage-port methods take and return
/// ([`FactIngestPort`], [`MemoryAuthoringPort`], [`GoalWritePort`],
/// [`EmbeddingJobPort`] and the other `*Port` traits), so a host or backend that
/// implements a port can write its signatures.
pub use proxima_core::{
    AbstractionRow, ActiveGoalSummary, AuthorDerivedOutcome, AuthorDerivedRequest,
    DerivedEmbedding, EmbeddableEntityRef, EmbeddingJobClaim, EmbeddingJobStatusCounts,
    EmbeddingWriteOutcome, FactRow, GoalWakeCandidateRequest, MembershipRow, MemoryGraphIdentity,
    MemoryKindRow, MemoryOperatorKind, MemorySchemaSpec, OperatorPhase,
    goal_write::AchieveGoalAtomicRequest, goal_write::CreateGoalAtomicRequest,
    goal_write::DecomposeGoalAtomicRequest, goal_write::GoalAtomicContext, goal_write::GoalDraft,
    goal_write::GoalReplayOutcome, goal_write::GoalReplayRequest,
    goal_write::ModifyGoalAtomicRequest, goal_write::TransitionGoalAtomicRequest,
    own_erase::SeriesEraseOutcome, own_erase::SeriesEraseReport, own_erase::SeriesEraseRequest,
};
/// Host authentication: the argument and error types of
/// [`Authenticator::authenticate`], and [`authenticate`], the one mint of the
/// [`OwnerScope`] witness [`AuthzContext::owner_scope`] returns.
pub use proxima_core::{AuthError, Credentials, OwnerScope, authenticate};
/// Authorization witnesses, permits and the audit contexts the Engine derives
/// for them, so a signature that takes or returns one can be written down.
/// Naming a witness does not let anyone mint it: only the Engine's authorization
/// gates construct these, their constructors are crate-private (the opt-in
/// `testkit` feature adds `*_for_tests` constructors), and these exports confer
/// no authority.
pub use proxima_core::{
    AuthorizedFactWithCitation, AuthorizedFactWithCitationRef, AuthorizedFactWrite,
    OperatorMaintenanceProof, WritePermit, fact_ingest::AuthorizedCitationAttachment,
    fact_ingest::AuthorizedInlineCitationMapping, fact_ingest::AuthorizedInlineCitedObject,
    fact_ingest::AuthorizedNodeLinks, owner_inverse::EraseAuthorization,
    owner_inverse::ExportAuthorization, owner_inverse::OwnerEraseContext,
    owner_inverse::OwnerExportContext, storage_ports::EmbeddingWriteProof,
    storage_ports::OperatorWriteProof, storage_ports::OwnerWritePermit,
};
/// Rows, cursors and edges the read responses carry ([`QueryRequest`],
/// [`QueryResponse`], [`MemoryLineageRequest`], [`ChangeHistoryResponse`],
/// [`FactCitationReadback`], [`EdgeFilter`], [`FactWriteCommand`]): a host that
/// pages a query, reads the change history or follows an edge names them.
pub use proxima_core::{
    ChangeEvent, ChangeEventForWake, ChangeEventKind, Edge, EdgeEndpoint, EdgeKind,
    EdgeTargetProjection, EntityRef, UploadedBlobPageSpanV1, query::FactCitationCursor,
    query::GoalRow, query::MemoryLineageCursor, query::QueryCursor, query::QueryPage,
    query::UploadedBlobRef,
};
/// The storage ports and their handles. [`StoragePortsBuilder`]'s setters take the
/// `*Handle` aliases (`Arc<dyn ...Port>`), [`StoragePortsBuilder::try_build`]
/// returns the `StoragePortsBuildError`, and a host that supplies its own backend
/// ([`Engine::with_storage_ports`]) implements the `*Port` traits those handles
/// wrap.
pub use proxima_core::{
    ChangeEventPort, CitationPort, EmbeddingJobPort, EmbeddingMaintenancePort, EmbeddingTextPort,
    EmbeddingWritePort, FactIngestPort, GoalReadPort, GoalWritePort, McpCallReadPort,
    MemoryAuthoringPort, MemoryInspectPort, MemoryReadPort, OwnerAccessReadPort,
    OwnerDropProofPort, OwnerEraseAuthorityPort, OwnerInversePort, OwnerMembershipAdminPort,
    OwnerTransferPort, RegistryProjectionPort, SourceCursorPort, storage_ports::ChangeEventHandle,
    storage_ports::CitationHandle, storage_ports::EmbeddingJobHandle,
    storage_ports::EmbeddingMaintenanceHandle, storage_ports::EmbeddingTextHandle,
    storage_ports::EmbeddingWriteHandle, storage_ports::FactIngestHandle,
    storage_ports::GoalReadHandle, storage_ports::GoalWakeCandidateHandle,
    storage_ports::GoalWakeCandidatePort, storage_ports::GoalWriteHandle,
    storage_ports::McpCallReadHandle, storage_ports::MemoryAuthoringHandle,
    storage_ports::MemoryInspectHandle, storage_ports::MemoryReadHandle,
    storage_ports::OwnerAccessReadHandle, storage_ports::OwnerDropProofHandle,
    storage_ports::OwnerEraseAuthorityHandle, storage_ports::OwnerInverseHandle,
    storage_ports::OwnerMembershipAdminHandle, storage_ports::OwnerTransferHandle,
    storage_ports::RegistryProjectionHandle, storage_ports::SourceCursorHandle,
    storage_ports::StoragePortsBuildError, storage_ports::WriteSession,
    storage_ports::WriteSessionFactory, storage_ports::WriteSessionFactoryHandle,
};
/// The Fact-with-citation write path: [`FactWriteCommand`],
/// [`Engine::authorize_fact_with_citation`],
/// [`Engine::authorize_fact_with_citation_by_ref`],
/// [`Engine::authorize_citation_attachment`], [`UnitOfWork::read_own_sidecar`]
/// and [`Engine::decompose_goal`] take these drafts and requests and return
/// these outcomes.
pub use proxima_core::{
    CitationAttachmentRequest, fact_ingest::Citation, fact_ingest::CitationMappingHint,
    fact_ingest::CitedObjectHint, fact_ingest::InlineCitationMappingDraft,
    fact_ingest::InlineCitedObjectDraft, goal_write::DecomposedGoalOutcome,
    storage_ports::SidecarSessionRead,
};
/// Request, response and outcome types of the [`Engine`] verbs a host calls:
/// `get_graph`, `list_change_events`, `read_fact_citation`, `facts_citing_object`,
/// `list_members`, `drain_embedding_jobs`, `reconcile_embeddings`,
/// `embedding_coverage`, `purge_embedding_spaces`, `complete_upload_as_fact`,
/// `inbound_pin_nodes`, `load_sketches`, `read_goal_wake_configs`, `try_compose`
/// and `with_mcp_listener`. Without them a host can call each verb and cannot
/// write its argument or result in a signature.
pub use proxima_core::{
    EmbeddingDrainOutcome, EmbeddingMode, EmbeddingPurgeOutcome, EmbeddingReconcileOptions,
    EmbeddingReconcileOutcome, EmbeddingReconcileScope, EmbeddingSpaceCounts,
    EmbeddingSpaceCoverage, EmbeddingSpaceRole, EngineMcpListener, FactCitationReadRequest,
    FactsCitingObjectReadRequest, GetGraphReadRequest, GetGraphReadResponse, GoalWakeConfigRow,
    GroupMemberPage, InboundPinQuery, ListChangeEventsReadRequest, ListChangeEventsReadResponse,
    MemoryGraphPayloadRow, MemorySketch, PinNode, RunningMcpListener, StoragePorts,
    UploadCompleted, UploadCompletionExpectation, query::FactCitationPage,
    storage_ports::StoragePortsBuilder,
};
/// What [`AuthzContext`] reports about its caller: the identity and invoking
/// tool [`AuthzContext::identity_for_revalidation`] and
/// [`AuthzContext::invoking_tool`] return, and the refusal
/// [`AuthzContext::with_trusted_model_id`] gives.
pub use proxima_core::{Identity, InvokingTool, TrustedModelIdError};
/// The tool-descriptor and handle vocabulary of [`McpToolDescriptor`],
/// [`McpTool`], [`McpToolPresentation`], [`McpToolError`],
/// [`RequestHeaderAllowlist::extract`] and the call log ([`McpCallLogInput`]): a
/// host that registers or presents its own tools names them.
pub use proxima_core::{
    McpAuthorContext, McpCallFn, McpUnknownFieldPolicy, MemoryHandleClass, RequestHeaders,
    ToolError, mcp::McpActionArgSpec, mcp::McpActionSchema, mcp::McpDispatcherSchema,
    persist_mcp_call::McpCallLoggedV1,
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
/// What [`McpToolHost`]'s dispatch methods return on failure, and what the
/// exported [`tool_invocation_error_to_error_data`] takes.
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
    ToolListNotifier, auth_context, author_from_args, mcp_tool_error_to_error_data,
    peer_implementation, reject_nul_in_args, strip_call_context_args,
    tool_invocation_error_to_error_data,
};
/// MCP edge wiring [`layered_router`] takes, and the listener CORS layer.
pub use proxima_mcp_server::{McpEdgeAuth, OriginAllowlist, cors_layer};
/// The reference consumer's and the publisher's types that
/// `ReferenceConsumer::connect_with_hook`, `process_once`, `into_observed_parts`,
/// `ReceivedEvent` and `JetStreamPublisher::connect_with_hook` name. Behind the
/// `outbox-nats` feature.
#[cfg(feature = "outbox-nats")]
pub use proxima_outbox_nats::{
    AckAction, AckHook, CloudEventEnvelope, ConsumeReport, ConsumerConnectionState, ConsumerHealth,
    ConsumerHealthReader, ConsumerPassState, ConsumerTaskState, HookAction, PublishHook,
};
/// What [`ProximaHost::host_state_erase_context_for_host`] returns. Opaque and
/// authority-free: it grants no erase by itself.
pub use proxima_storage_pg::PgHostStateEraseContext;
/// The storage handle [`run_core_and_flavor_migrations`] and
/// [`preflight_without_migrations`] take. Host API only: the Flavor SDK has
/// no storage handle.
pub use proxima_storage_pg::PgStorage;
/// Begin a transaction under the caller's [`OwnerScope`], with the pool
/// [`ProximaHost::clone_pool_for_host`] hands out.
///
/// Host API only, for authorized extra-table adapters: every statement of
/// the operation runs on the returned transaction, since pool queries do not
/// inherit its scope. No `proxima_core.*` SQL runs through it or through the
/// pool; Fact and state writes go through [`UnitOfWork`] and
/// [`UnitOfWork::apply_host_state`]. It is not a Flavor SDK name.
pub use proxima_storage_pg::begin_owner_transaction;
/// Owner-RLS boot: the runtime-role guard, the platform scope
/// [`ProximaHost::platform_scope_for_host`] returns, and the sqlx →
/// [`StorageError`] classifier for host-state SQL.
pub use proxima_storage_pg::{PgPlatformScope, assert_runtime_rls, map_err};
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
        projection::Artifact, sidecars::PgCitationMappingSidecar, sidecars::PgCitedObjectSidecar,
        sidecars::PgGoalSidecar, sidecars::PgMemoryPayload, sidecars::PgMemoryPayloadBatchFuture,
        sidecars::PgMemoryPayloadFuture, sidecars::PgMemorySidecar, sidecars::PgSidecarFuture,
        sidecars::PgSidecarReadCtx, sidecars::SidecarInsertPermit,
    },
};
/// What [`PgStorage`]'s public methods take and return beyond the migration
/// helpers: the cold store, the owner and scope surfaces, the maintenance locks,
/// change-log pruning and cold-purge retry options and outcomes, and the host
/// state erase selection [`HostStateEraseReceipt`] carries. Host API only, like
/// [`PgStorage`].
pub use {
    proxima_core::{
        ColdObjectStore, owner_inverse::CascadedDetail, owner_inverse::HostStateLifecycleSurface,
        owner_inverse::OwnerSurfaces, owner_inverse::OwnerSurfacesError,
        storage_ports::HostStateEraseSelection, storage_ports::HostStateFactCopyLocator,
    },
    proxima_storage_pg::{
        ChangeEventPruneOptions, ChangeEventPruneOutcome, ColdPurgeRetryOptions,
        ColdPurgeRetryOutcome, EmbeddingMaintenanceLock, PruneOwnerOutcome, StorageMaintenanceLock,
        access::scope_surfaces::ScopeSurfaces,
    },
};

#[cfg(test)]
mod tests {
    use super::{FlavorRegistryFrozen, ToolScope, tool_palette_excluding};
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

    #[test]
    fn excluding_an_action_scoped_tool_removes_every_action_entry() {
        let registry = registry();

        let scope = tool_palette_excluding(&registry, &[CoreGoalTool::NAME]);

        let ToolScope::Palette(entries) = &scope else {
            panic!("expected a palette scope")
        };
        assert!(!entries.iter().any(|entry| entry == CoreGoalTool::NAME));
        let action_prefix = format!("{}:", CoreGoalTool::NAME);
        assert!(
            !entries
                .iter()
                .any(|entry| entry.starts_with(&action_prefix)),
            "excluding the tool name must also exclude every tool:action expansion"
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry == SearchMemoriesTool::NAME)
        );
    }

    #[test]
    fn unexcluded_action_scoped_tool_keeps_every_action() {
        let registry = registry();

        let scope = tool_palette_excluding(&registry, &[]);

        assert!(scope.allows_action(CoreGoalTool::NAME, "set"));
        assert!(scope.allows(SearchMemoriesTool::NAME));
        assert!(
            !scope.allows(CoreGoalTool::NAME),
            "flat entry must not leak for an action-scoped tool"
        );
    }

    /// `read_resource` runs through the same flat scope gate as a tool call,
    /// with the resource's scope key standing in for a tool name. A palette
    /// built from tools alone therefore *denies* every `proxima://` read
    /// instead of merely leaving it unadvertised, so a host using
    /// `tool_palette_excluding` without resource keys loses resource reads
    /// entirely.
    #[test]
    fn palette_admits_every_core_resource() {
        let registry = registry();

        let scope = tool_palette_excluding(&registry, &[]);

        for resource in proxima_core::all_core_resources() {
            assert!(
                scope.allows(resource.scope_key),
                "palette must admit resource scope key {}",
                resource.scope_key
            );
        }
    }

    #[test]
    fn a_resource_can_be_excluded_by_its_scope_key() {
        let registry = registry();

        let scope = tool_palette_excluding(&registry, &[protocol_resource::MEMORY]);

        assert!(!scope.allows(protocol_resource::MEMORY));
        assert!(
            scope.allows(protocol_resource::SCHEMAS),
            "excluding one resource must not remove the others"
        );
    }
}
