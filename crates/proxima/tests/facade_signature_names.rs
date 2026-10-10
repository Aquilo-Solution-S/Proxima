//! The facade names every type a public signature exposes (#413).
//!
//! The rule: a type that appears in a `pub` signature of the facade, of a path
//! dependency it fronts, or of a trait impl a host calls, must be nameable
//! through `proxima::` (host tier) or `proxima::flavor::` (Flavor SDK), or a
//! host cannot write that signature down. Rust has no stable check for it: the
//! `unreachable_pub`-style lints see one crate, and rustdoc JSON is nightly. This
//! file is the enforcer. Each name below is imported through the facade, so a
//! renamed or dropped re-export fails to compile here, and each name sits in the
//! group of the signature that needs it.
//!
//! Adding a public signature that names a new type means adding the type to
//! `src/host.rs` and to a group here, in the same change. The tiers stay apart:
//! `host_only` names must not appear in `src/flavor.rs`, `both_tiers` names are
//! imported through both paths.
//!
//! Known and open: the Flavor SDK has signature-exposed types it does not name
//! yet; they are listed in `docs/reference/public-api.md` and are not exported.

use std::collections::HashSet;

/// Imports every name through the facade, one `mod` per signature group, and
/// collects the host-only ones for the tier guard.
macro_rules! facade_names {
    ($(
        $(#[$cfg:meta])*
        $group:ident {
            host_only: [$($host:ident),* $(,)?],
            both_tiers: [$($both:ident),* $(,)?] $(,)?
        }
    )+) => {
        $(
            $(#[$cfg])*
            mod $group {
                #[allow(unused_imports)]
                mod host {
                    use proxima::{$($host,)* $($both),*};
                }
                #[allow(unused_imports)]
                mod sdk {
                    use proxima::flavor::{$($both),*};
                }
            }
        )+

        /// Host-tier names the Flavor SDK must not export.
        const HOST_ONLY: &[&str] = &[$($(stringify!($host),)*)+];
    };
}

facade_names! {
    // Needed by `RuntimeConfig::auth`, a public field.
    runtime_auth_state {
        host_only: [RuntimeAuthState],
        both_tiers: [],
    }

    // Needed by `EraseRule::HostState { .. }`: the one name #413 adds to the Flavor SDK.
    host_state_erase_disposition {
        host_only: [],
        both_tiers: [HostStateEraseDisposition],
    }

    // Needed by `ProximaHost::origin_scope_for_host` and
    // `publication_origin_eligibility_for_host`.
    publication_origin {
        host_only: [OriginScope, PublicationOriginEligibility, PublicationOriginEligibilityPort],
        both_tiers: [],
    }

    // Needed by `NatsConsumerConfig`, beside `DEFAULT_SUBJECT_PREFIX`.
    #[cfg(feature = "outbox-nats")]
    outbox_defaults {
        host_only: [DEFAULT_CONSUMER_NAME, DEFAULT_CONSUMER_STREAM],
        both_tiers: [],
    }

    // Needed by an `rmcp::ServerHandler` around `DynamicHandler`: the one rmcp the facade pins.
    rmcp {
        host_only: [rmcp],
        both_tiers: [],
    }

    // Needed by `CitedBlobStore::{prepare_upload, stage_upload, abort_upload, read_url,
    // cold_store}`.
    s3_lane {
        host_only: [CitedBlobReadUrlOutcomeTs, CitedBlobReadUrlTs, CitedBlobUploadAbortOutcomeTs, CitedBlobUploadAbortTs, CitedBlobUploadCompleteTs, CitedBlobUploadPrepareOutcomeTs, CitedBlobUploadPrepareTs, PresignedHeaderTs, S3ColdStore],
        both_tiers: [],
    }

    // Needed by `McpEdge::revalidation`, `RuntimeConfig::stream_revalidation`,
    // `layered_router_with_revalidation`.
    revalidation_config {
        host_only: [RevalidationConfig],
        both_tiers: [],
    }

    // Needed by `core_action_meta`, `all_core_resources`, `tool_name_matches`.
    core_action_meta {
        host_only: [CoreActionMeta, all_core_resources, core_action_meta, tool_name_matches],
        both_tiers: [],
    }

    // Needed by tool-argument ids: `format_prefixed_uuid`, `parse_prefixed_uuid`.
    prefixed_uuid {
        host_only: [PrefixedUuidClass, PrefixedUuidError, format_prefixed_uuid, parse_prefixed_uuid],
        both_tiers: [],
    }

    // Needed by `Next::new`, `McpToolHost::dispatch_through_behaviors`.
    behavior_chain {
        host_only: [ScopeGateBehavior, TerminalDispatch],
        both_tiers: [],
    }

    // Needed by `Engine::ingest_fact`, `create_goal`, `transfer_to_owner`,
    // `authorize_fact_ingest`, `DerivedMemory`, `MemorySnapshot`.
    ids_and_payloads {
        host_only: [],
        both_tiers: [AbstractionPayload, CitationMappingPayload, CitedObjectPayload, EntityId, FactPayload, FactReceiptId, FactTombstone, GoalId, GoalPayload, InputContractId, ModelId, OperatorId, PayloadReference, PerspectivePayload, PromptVersion, ReferenceBinding, SchemaVersion, ScopeRef, SearchProjectionColumnKind, SidecarPayload, ToolId],
    }

    // Needed by the storage-port methods' requests, rows and outcomes (`FactIngestPort`,
    // `GoalWritePort`, ...).
    storage_port_rows {
        host_only: [AbstractionRow, AchieveGoalAtomicRequest, ActiveGoalSummary, AuthorDerivedOutcome, AuthorDerivedRequest, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest, DerivedEmbedding, EmbeddableEntityRef, EmbeddingJobClaim, EmbeddingJobStatusCounts, EmbeddingWriteOutcome, FactRow, GoalAtomicContext, GoalDraft, GoalReplayOutcome, GoalReplayRequest, GoalWakeCandidateRequest, MembershipRow, MemoryGraphIdentity, MemoryKindRow, MemoryOperatorKind, MemorySchemaSpec, ModifyGoalAtomicRequest, OperatorPhase, SeriesEraseOutcome, SeriesEraseReport, SeriesEraseRequest, TransitionGoalAtomicRequest],
        both_tiers: [],
    }

    // Needed by signatures that take or return an authorization witness, permit or audit context;
    // naming one mints nothing.
    witnesses {
        host_only: [AuthorizedCitationAttachment, AuthorizedFactWrite, AuthorizedInlineCitationMapping, AuthorizedInlineCitedObject, AuthorizedNodeLinks, EmbeddingWriteProof, EraseAuthorization, ExportAuthorization, OperatorMaintenanceProof, OperatorWriteProof, OwnerEraseContext, OwnerExportContext, OwnerWritePermit, WritePermit],
        both_tiers: [AuthorizedFactWithCitation, AuthorizedFactWithCitationRef],
    }

    // Needed by `QueryRequest`, `QueryResponse`, `MemoryLineageRequest`, `ChangeHistoryResponse`,
    // `EdgeFilter`.
    read_rows {
        host_only: [ChangeEvent, ChangeEventForWake, ChangeEventKind, FactCitationCursor, MemoryLineageCursor, QueryCursor, QueryPage, UploadedBlobPageSpanV1, UploadedBlobRef],
        both_tiers: [Edge, EdgeEndpoint, EdgeKind, EdgeTargetProjection, EntityRef, GoalRow],
    }

    // Needed by `StoragePortsBuilder`'s setters, `Engine::with_storage_ports`, the `*Port` traits
    // behind the handles.
    storage_ports {
        host_only: [ChangeEventHandle, ChangeEventPort, CitationHandle, CitationPort, EmbeddingJobHandle, EmbeddingJobPort, EmbeddingMaintenanceHandle, EmbeddingMaintenancePort, EmbeddingTextHandle, EmbeddingTextPort, EmbeddingWriteHandle, EmbeddingWritePort, FactIngestHandle, FactIngestPort, GoalReadHandle, GoalReadPort, GoalWakeCandidateHandle, GoalWakeCandidatePort, GoalWriteHandle, GoalWritePort, McpCallReadHandle, McpCallReadPort, MemoryAuthoringHandle, MemoryAuthoringPort, MemoryInspectHandle, MemoryInspectPort, MemoryReadHandle, MemoryReadPort, OwnerAccessReadHandle, OwnerAccessReadPort, OwnerDropProofHandle, OwnerDropProofPort, OwnerEraseAuthorityHandle, OwnerEraseAuthorityPort, OwnerInverseHandle, OwnerInversePort, OwnerMembershipAdminHandle, OwnerMembershipAdminPort, OwnerTransferHandle, OwnerTransferPort, RegistryProjectionHandle, RegistryProjectionPort, SourceCursorHandle, SourceCursorPort, StoragePortsBuildError, WriteSession, WriteSessionFactory, WriteSessionFactoryHandle],
        both_tiers: [],
    }

    // Needed by `Engine::authorize_fact_with_citation`, `authorize_citation_attachment`,
    // `decompose_goal`.
    fact_with_citation {
        host_only: [Citation, CitationMappingHint, CitedObjectHint, DecomposedGoalOutcome],
        both_tiers: [CitationAttachmentRequest, InlineCitationMappingDraft, InlineCitedObjectDraft, SidecarSessionRead],
    }

    // Needed by the arguments and results of the Engine verbs a host calls (`get_graph`,
    // `list_members`, ...).
    engine_verbs {
        host_only: [EmbeddingDrainOutcome, EmbeddingPurgeOutcome, EmbeddingReconcileOptions, EmbeddingReconcileOutcome, EmbeddingReconcileScope, EmbeddingSpaceCounts, EmbeddingSpaceCoverage, EmbeddingSpaceRole, EngineMcpListener, FactCitationPage, FactCitationReadRequest, FactsCitingObjectReadRequest, GetGraphReadRequest, GetGraphReadResponse, GoalWakeConfigRow, GroupMemberPage, InboundPinQuery, ListChangeEventsReadRequest, ListChangeEventsReadResponse, MemoryGraphPayloadRow, MemorySketch, PinNode, RunningMcpListener, StoragePorts, StoragePortsBuilder],
        both_tiers: [EmbeddingMode, UploadCompleted, UploadCompletionExpectation],
    }

    // Needed by `AuthzContext::{identity_for_revalidation, invoking_tool, with_trusted_model_id}`.
    authz_context_reports {
        host_only: [Identity, InvokingTool],
        both_tiers: [TrustedModelIdError],
    }

    // Needed by `McpToolDescriptor`, `McpTool`, `RequestHeaderAllowlist::extract`,
    // `McpCallLogInput`.
    mcp_tool_vocabulary {
        host_only: [McpActionSchema, McpCallFn, McpCallLoggedV1, McpDispatcherSchema, McpUnknownFieldPolicy, MemoryHandleClass],
        both_tiers: [McpActionArgSpec, McpAuthorContext, RequestHeaders, ToolError],
    }

    // Needed by `PublicationExtensions::iter`, `AuthzContext::with_publication_extensions`.
    publication_extensions {
        host_only: [ExtensionEntries, ExtensionEntry, PublicationDraft, PublicationPlan],
        both_tiers: [],
    }

    // Needed by `ToolCtx::{caller, with_caller}` and its constructors.
    tool_context {
        host_only: [],
        both_tiers: [ToolCaller, ToolCtx, ToolServices],
    }

    // Needed by `CitedBlobService`, `CitedBlobStore`, `Engine::complete_upload_as_fact`.
    cited_blob_service {
        host_only: [],
        both_tiers: [CitedBlobHeld, CitedBlobPort, CitedBlobReadUrl, CitedBlobService, CitedBlobStaged, CitedBlobUploadAborted, CitedBlobUploadCompleted, CitedBlobUploadHeader, CitedBlobUploadPrepared, UploadedBlobPayload],
    }

    // Needed by `CoreMcpTools::into_dynamic_handler`, `McpStreamableService`.
    dynamic_handler {
        host_only: [DynamicHandler],
        both_tiers: [],
    }

    // Needed by `OriginAllowlist::parse`, `RequestHeaderAllowlist::parse`,
    // `McpToolHost::from_database_urls`.
    mcp_server_error {
        host_only: [McpServerError],
        both_tiers: [],
    }

    // Needed by `ResourceServerMetadata`.
    protected_resource {
        host_only: [ProtectedResource],
        both_tiers: [],
    }

    // Needed by `McpToolHost`'s dispatch methods, `tool_invocation_error_to_error_data`.
    tool_invocation_error {
        host_only: [ToolInvocationError],
        both_tiers: [],
    }

    // Needed by `McpAuthLayer`.
    mcp_auth_layer_state {
        host_only: [McpAuthLayerState],
        both_tiers: [],
    }

    // Needed by `McpEdge::router`, `layered_router_mcp_only`: a host that builds its own router.
    edge_layers {
        host_only: [BodyLimitLayer, CorsLayer, HostGuardLayer, McpAuthLayer, body_limit_layer, enforce_body_limit, host_guard_layer, mcp_auth_layer_with_metadata],
        both_tiers: [],
    }

    // Needed by `ReferenceConsumer`, `JetStreamPublisher`: `connect_with_hook`, `process_once`,
    // `ReceivedEvent`.
    #[cfg(feature = "outbox-nats")]
    outbox_consumer {
        host_only: [AckAction, AckHook, CloudEventEnvelope, ConsumeReport, ConsumerConnectionState, ConsumerHealth, ConsumerHealthReader, ConsumerPassState, ConsumerTaskState, HookAction, PublishHook],
        both_tiers: [],
    }

    // Needed by `ProximaHost::host_state_erase_context_for_host`.
    host_state_erase_context {
        host_only: [PgHostStateEraseContext],
        both_tiers: [],
    }

    // Needed by `run_core_and_flavor_migrations`, `preflight_without_migrations`: host API only on
    // purpose.
    pg_storage {
        host_only: [PgStorage],
        both_tiers: [],
    }

    // Needed by host API only on purpose: it runs under the caller's `OwnerScope`, never on
    // `proxima_core.*`.
    begin_owner_transaction {
        host_only: [begin_owner_transaction],
        both_tiers: [],
    }

    // Needed by `Engine::try_compose`, `ProximaError`, `FlavorApp`, `FlavorRegistryFrozen`,
    // `ProximaHost::pg_sidecars_for_host`.
    registry_and_contracts {
        host_only: [Artifact, CapabilityTag, CapabilityTagError, IntegrityFinding, IntegrityReport, IntegrityViolation, MemoryEmbedUnit, MemorySearchProjection, MemorySearchProjectionField, PgMemoryPayloadBatchFuture, PgSidecarKey, ProjectedSchema, ProtocolPayload, RenderBands, SchemaInfo, SchemaTombstone],
        both_tiers: [AuthorizationHook, AuthzInput, AuthzOperation, AuthzOutcome, AuthzVeto, Band, BandComparability, CounterRule, DbConstraint, DbTrigger, EmbedUnit, EmbeddingRecipe, EmbeddingSlot, Enforcement, EraseLeg, EraseRule, ExportRule, FlavorBundle, FlavorContract, FlavorDescriptor, FlavorProvenance, FlavorRegistry, FlavorRegistryError, FlavorWorker, FlavorWorkerContext, ForgetLeg, ForgetRule, KeyShape, LanguagePolicy, MembershipChange, OwnerResolver, PgCitationMappingSidecar, PgCitedObjectSidecar, PgGoalSidecar, PgMemoryPayload, PgMemoryPayloadFuture, PgMemorySidecar, PgSidecarFuture, PgSidecarReadCtx, PgSidecarRegistry, PgSidecarRegistryFrozen, ProjectionDecl, ProjectionSpec, Provenance, RankSource, ResolvedEmbedUnit, ResourceContract, SchemaContract, SchemaRef, ScopeDecl, ScopeKind, SearchProjectionDecl, SidecarInsertPermit, SubstringArm, Surface, Tool, ToolContract, TransferLeg, TransferRule, WeightedField],
    }

    // Needed by `PgStorage`'s public methods; host API only like `PgStorage`.
    pg_storage_methods {
        host_only: [CascadedDetail, ChangeEventPruneOptions, ChangeEventPruneOutcome, ColdObjectStore, ColdPurgeRetryOptions, ColdPurgeRetryOutcome, EmbeddingMaintenanceLock, HostStateEraseSelection, HostStateFactCopyLocator, HostStateLifecycleSurface, OwnerSurfaces, OwnerSurfacesError, PruneOwnerOutcome, ScopeSurfaces, StorageMaintenanceLock],
        both_tiers: [],
    }
}

/// The identifiers a Rust source file uses outside comments.
fn code_identifiers(source: &str) -> HashSet<&str> {
    source
        .lines()
        .map(|line| line.split("//").next().unwrap_or_default())
        .flat_map(|code| code.split(|c: char| !(c.is_alphanumeric() || c == '_')))
        .filter(|ident| !ident.is_empty())
        .collect()
}

#[test]
fn host_only_names_stay_out_of_the_flavor_sdk() {
    let sdk = code_identifiers(include_str!("../src/flavor.rs"));
    let leaked: Vec<_> = HOST_ONLY
        .iter()
        .filter(|name| sdk.contains(**name))
        .collect();
    assert!(
        leaked.is_empty(),
        "src/flavor.rs names host-only types {leaked:?}: move them to `both_tiers` only if the \
         Flavor SDK is meant to grow"
    );
}

#[test]
fn the_tier_guard_reads_code_and_not_comments() {
    let source = "pub use proxima_storage_pg::PgStorage;\n// not_exported: NotCode\n/// DocOnly\n";
    let found = code_identifiers(source);
    assert!(found.contains("PgStorage"));
    assert!(!found.contains("NotCode") && !found.contains("DocOnly"));
}

#[test]
fn every_group_names_something() {
    assert!(HOST_ONLY.len() > 100, "the host-only list lost its names");
}
