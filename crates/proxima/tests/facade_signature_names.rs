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
    // Needed by `ForwarderPolicy::wrap` and forwarded-owner access resolution.
    forwarding_policy {
        host_only: [ForwarderPolicy, ForwarderRole, OwnerRegistry],
        both_tiers: [],
    }

    // Host configuration and the canonical bare UUID boundary.
    store_identity {
        host_only: [new_memory_uuid, memory_store_number, parse_store_number, parse_wire_uuid],
        both_tiers: [],
    }

    // Needed by `RuntimeBuilder::other_stores`, `Proxima::other_stores`, the port
    // implementations they accept, and `OtherStoresService`'s bounded answers.
    other_stores {
        host_only: [OtherStores, OtherStoresError, OtherStoresService, ReadOrigin, RemoteHit, RemoteMemory, RemoteOriginRead, RemoteRead, RemoteSearch, StoreCaller, StoreInfo, StoreInventory],
        both_tiers: [],
    }

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

    // Needed by an `rmcp::ServerHandler` around `DynamicHandler`: the one rmcp the facade pins.
    rmcp {
        host_only: [rmcp],
        both_tiers: [],
    }

    // Needed by `CitedBlobStore::{prepare_upload, stage_upload, abort_upload, read_url,
    // cold_store, sweep_expired_uploads}`.
    s3_lane {
        host_only: [CitedBlobReadUrlOutcomeTs, CitedBlobReadUrlTs, CitedBlobUploadAbortOutcomeTs, CitedBlobUploadAbortTs, CitedBlobUploadCompleteTs, CitedBlobUploadPrepareOutcomeTs, CitedBlobUploadPrepareTs, PresignedHeaderTs, S3ColdStore, UploadExpirySweepOutcome],
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

    // Needed by `Next::new`, `McpToolHost::dispatch_through_behaviors`.
    behavior_chain {
        host_only: [ScopeGateBehavior, TerminalDispatch],
        both_tiers: [],
    }

    // Needed by `Engine::ingest_fact`, `create_goal`, `transfer_to_owner`,
    // `authorize_fact_ingest`, `DerivedMemory`, `MemorySnapshot`.
    ids_and_payloads {
        host_only: [],
        both_tiers: [AbstractionPayload, CitedObjectPayload, EntityId, FactPayload, FactReceiptId, FactTombstone, GoalId, GoalPayload, InputContractId, ModelId, OperatorId, PayloadReference, PerspectivePayload, PromptVersion, ReferenceBinding, SchemaVersion, ScopeRef, SearchProjectionColumnKind, SidecarPayload, ToolId],
    }

    // Needed by the storage-port methods' requests, rows and outcomes (`FactIngestPort`,
    // `GoalWritePort`, ...).
    storage_port_rows {
        host_only: [AchieveGoalAtomicRequest, ActiveGoalSummary, AuthorDerivedOutcome, AuthorDerivedRequest, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest, EmbeddableEntityRef, EmbeddingJobClaim, EmbeddingJobStatusCounts, EmbeddingWriteOutcome, GoalAtomicContext, GoalDraft, GoalReplayOutcome, GoalReplayRequest, MembershipRow, MemoryGraphIdentity, MemoryKindRow, MemoryOperatorKind, MemorySchemaSpec, ModifyGoalAtomicRequest, OperatorPhase, SeriesEraseOutcome, SeriesEraseReport, SeriesEraseRequest, TransitionGoalAtomicRequest],
        both_tiers: [],
    }

    // Needed by signatures that take or return an authorization witness, permit or audit context;
    // naming one mints nothing.
    witnesses {
        host_only: [AuthorizedFactWrite, AuthorizedInlineCitedObject, AuthorizedNodeLinks, EmbeddingWriteProof, EraseAuthorization, ExportAuthorization, MembershipEndpointAuthority, MembershipMemberAuthority, MembershipWritePermit, OperatorMaintenanceProof, OperatorWriteProof, OwnerEraseContext, OwnerExportContext, OwnerWritePermit, WritePermit],
        both_tiers: [AuthorizedFactWithCitation, AuthorizedFactWithCitationRef],
    }

    // Needed by `QueryRequest`, `QueryResponse`, `MemoryLineageRequest`,
    // `EdgeFilter`.
    read_rows {
        host_only: [FactCitationCursor, MemoryLineageCursor, QueryCursor, QueryPage],
        both_tiers: [Edge, EdgeEndpoint, EdgeKind, EdgeTargetProjection, EntityRef, GoalRow],
    }

    // The existing typed reads and replay result in the consumer inventory:
    // Engine::{get_memory, get_memories, load_source_cursor, store_source_cursor},
    // Engine::query and UnitOfWork::ingest_fact. Cursor operations use Engine
    // directly; the duplicate root functions are retired.
    inventory_reads {
        host_only: [Cursor, GetMemoryReadRequest, GetMemoryReadResponse, GetMemoriesReadRequest, GetMemoriesReadResponse, MemoryRow, MemorySnapshot],
        both_tiers: [FactIngestOutcome, QueryRequest, QueryResponse, UnitOfWork],
    }

    // A receipt lookup on the caller's transaction, and one row per embedding
    // job of the named memories. Coverage counters are a different read.
    caller_transaction_reads {
        host_only: [],
        both_tiers: [EmbeddingJobDiagnostic, EmbeddingJobState, IngestReceipt, MAX_EMBEDDING_DIAGNOSTIC_UNITS, ingest_receipt],
    }

    // Needed by `StoragePortsBuilder`'s setters, `Engine::with_storage_ports`, the `*Port` traits
    // behind the handles.
    storage_ports {
        host_only: [CitationHandle, CitationPort, EmbeddingJobHandle, EmbeddingJobPort, EmbeddingMaintenanceHandle, EmbeddingMaintenancePort, EmbeddingTextHandle, EmbeddingTextPort, EmbeddingWriteHandle, EmbeddingWritePort, FactIngestHandle, FactIngestPort, GoalReadHandle, GoalReadPort, GoalWriteHandle, GoalWritePort, MemoryAuthoringHandle, MemoryAuthoringPort, MemoryInspectHandle, MemoryInspectPort, MemoryReadHandle, MemoryReadPort, OwnerAccessReadHandle, OwnerAccessReadPort, OwnerDropProofHandle, OwnerDropProofPort, OwnerEraseAuthorityHandle, OwnerEraseAuthorityPort, OwnerInverseHandle, OwnerInversePort, OwnerMembershipAdminHandle, OwnerMembershipAdminPort, OwnerTransferHandle, OwnerTransferPort, SourceCursorHandle, SourceCursorPort, StoragePortsBuildError, WriteSession, WriteSessionFactory, WriteSessionFactoryHandle],
        both_tiers: [],
    }

    // Needed by `Engine::authorize_fact_with_citation`, `decompose_goal`.
    fact_with_citation {
        host_only: [Citation, CitedObjectHint, DecomposedGoalOutcome],
        both_tiers: [InlineCitedObjectDraft, SidecarSessionRead],
    }

    // Needed by the arguments and results of the Engine verbs a host calls (`get_graph`,
    // `list_members`, ...).
    engine_verbs {
        host_only: [EmbeddingDrainOutcome, EmbeddingPurgeOutcome, EmbeddingReconcileOptions, EmbeddingReconcileOutcome, EmbeddingReconcileScope, EmbeddingSpaceCounts, EmbeddingSpaceCoverage, EmbeddingSpaceRole, EngineMcpListener, FactCitationPage, FactCitationReadRequest, FactsCitingObjectReadRequest, GetGraphReadRequest, GetGraphReadResponse, GroupMemberPage, InboundPinQuery, MemoryCounts, MemoryGraphPayloadRow, MemorySketch, PinNode, RunningMcpListener, StoragePorts, StoragePortsBuilder],
        both_tiers: [UploadCompleted, UploadCompletionExpectation],
    }

    // Needed by `AuthzContext::{identity_for_revalidation, invoking_tool, with_trusted_model_id}`.
    authz_context_reports {
        host_only: [Identity, InvokingTool],
        both_tiers: [TrustedModelIdError],
    }

    // Needed by `McpToolDescriptor`, `McpTool`, `RequestHeaderAllowlist::extract`,
    // MCP tool descriptor and request vocabulary.
    mcp_tool_vocabulary {
        host_only: [McpActionSchema, McpCallFn, McpDispatcherSchema, McpUnknownFieldPolicy, MemoryHandleClass],
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
        host_only: [ProtectedResource, ScopeToken, ScopeTokenError],
        both_tiers: [],
    }

    // Needed by `McpToolHost`'s dispatch methods.
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

    // Needed by `RuntimeBuilder::outbox_intake`, `RuntimeConfig::outbox_consumers` and the intake a
    // host implements for it: `DurableIntake::accept` takes a `ReceivedEvent` (and
    // `ReceivedEvent::identity`) and returns `Intake` or `IntakeError`.
    outbox_intake {
        host_only: [CloudEventEnvelope, DurableIntake, EventIdentity, Intake, IntakeError, ReceivedEvent],
        both_tiers: [],
    }

    // Needed by `RuntimeBuilder::outbox_consumer`, `RuntimeConfig::outbox_consumers`,
    // `BuiltProxima::outbox_consumers` (`PublicationConsumerPort::retire` returns
    // `RetireOutcome`) and `ReceivedEvent::position`.
    outbox_consumers {
        host_only: [ConsumerName, ConsumerNameError, OutboxConsumers, PublicationConsumerPort, RetireOutcome, SubjectPosition, SubjectPositionError],
        both_tiers: [],
    }

    // Host API only: the role probes `PgOwnerAccessResolver` answers with, on a host's own
    // transaction.
    role_probes {
        host_only: [group_role, roles_for_subject],
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

    // Host API only: the schemas a frozen registry stores into, which boot's grant plan, RLS
    // census and platform scope derive from.
    composed_schema_names {
        host_only: [composed_schema_names],
        both_tiers: [],
    }

    // Needed by `BuiltProxima::feature_control`, `RunningProxima::feature_control` and the
    // `FeatureControl` they return.
    feature_control {
        host_only: [FeatureControl, FeatureControlError, FeatureStatus],
        both_tiers: [],
    }

    // Needed by `PgCommandDispatcher::register` and the `HostStateHandler` it routes to.
    host_state_command_dispatch {
        host_only: [AgreedCommand, CommandRegistrationError, HostStateHandler, HostStatePayloadOwners, PgCommandDispatcher],
        both_tiers: [],
    }

    // Needed by `PgPlatformScope::authority_on`, its borrowed views, and
    // `add_group_member`'s idempotent outcome.
    host_authority_transactions {
        host_only: [AuthorityTargets, DirectGroupMembership, DirectUserMembership, GroupContainment, MembershipAddOutcome, PgAuthorityGroup, PgAuthorityPerson, PgAuthoritySession, RegisteredOwner],
        both_tiers: [],
    }

    // Needed by exact caller scopes, deployment surfaces, and their shared admission.
    // `McpToolDescriptor::{palette_keys, owner_only_keys}`.
    scope_keys {
        host_only: [],
        both_tiers: [ActionName, CapabilitySet, KeyDeclaration, ResourceKey, ScopeKey, ScopeKeyError, ServedSurface, ToolName, ToolScope],
    }

    // Needed by `pending_migrations`, the read-only plan beside the runner.
    pending_migrations {
        host_only: [VectorStorage, missing_migration_policy_from_lookup, MissingMigrationPolicy, PendingMigration, pending_migrations, pending_migrations_with_lock_timeout, pending_migrations_with_policy, preflight_without_migrations_with_policy, run_core_and_flavor_migrations_with_policy],
        both_tiers: [],
    }

    // Needed by hosts connecting a migration-only lane and managing the core schema.
    host_schema_maintenance {
        host_only: [MigrationStorage, PgMigrationStorage, assert_runtime_rls, is_transient_conflict, begin_migration_transaction, run_core_schema, core_migrator, core_migrator_for, ensure_core_schema_current, ensure_owner_row, core_definitions_match, core_definitions_match_with_policy],
        both_tiers: [],
    }

    // Needed by flavor-authored projection migrations and comparable search declarations.
    projection_migrations {
        host_only: [],
        both_tiers: [Artifact, ProjectionArtifacts, StorageError, projection_artifacts, BAND_EXACT, BAND_RESCUE, BAND_SUBSTRING, LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT],
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
        host_only: [CapabilityTag, CapabilityTagError, IntegrityFinding, IntegrityReport, IntegrityViolation, MemoryEmbedUnit, MemorySearchProjection, MemorySearchProjectionField, PgMemoryPayloadBatchFuture, PgSidecarKey, ProjectedSchema, ProtocolPayload, RenderBands, SchemaInfo, SchemaTombstone],
        both_tiers: [AuthorizationHook, AuthzInput, AuthzOperation, AuthzOutcome, AuthzVeto, Band, BandComparability, CounterRule, DbConstraint, DbTrigger, EmbedUnit, EmbeddingRecipe, EmbeddingSlot, Enforcement, EraseLeg, EraseRule, ExportRule, FlavorBundle, FlavorContract, FlavorDescriptor, FlavorProvenance, FlavorRegistry, FlavorRegistryError, FlavorWorker, FlavorWorkerContext, ForgetLeg, ForgetRule, KeyShape, LanguagePolicy, MembershipChange, OwnerResolver, PgGoalSidecar, PgMemoryPayload, PgMemoryPayloadFuture, PgMemorySidecar, PgSidecarFuture, PgSidecarReadCtx, PgSidecarRegistry, PgSidecarRegistryFrozen, ProjectionDecl, ProjectionSpec, Provenance, RankSource, ResolvedEmbedUnit, ResourceContract, SchemaContract, SchemaRef, ScopeDecl, ScopeKind, SearchProjectionDecl, SidecarInsertPermit, SidecarOwnerMode, SubstringArm, Surface, Tool, ToolContract, TransferLeg, TransferRule, WeightedField],
    }

    // Needed by `PgStorage`'s public methods; host API only like `PgStorage`.
    pg_storage_methods {
        host_only: [CascadedDetail, ColdObjectStore, ColdPurgeRetryOptions, ColdPurgeRetryOutcome, EmbeddingMaintenanceLock, HostStateEraseSelection, HostStateFactCopyLocator, HostStateLifecycleSurface, ObjectStoreId, OwnerSurfaces, OwnerSurfacesError, PurgeDeleted, ScopeSurfaces, StorageMaintenanceLock, StuckPurgeDebt],
        both_tiers: [],
    }

    // Needed by `McpHostTools::call`, `RequestBehavior::handle`, `Next::run` and
    // `TerminalDispatch` (`ToolReply` of `ToolContent`), `RequestBehavior::visible`
    // (`ToolDescriptorView`, `ToolSource`) and `McpHostTools::instructions` (the cap).
    tool_replies_and_visibility {
        host_only: [MAX_HOST_INSTRUCTIONS_CHARS],
        both_tiers: [ToolContent, ToolDescriptorView, ToolReply, ToolSource],
    }
}

/// Flavor-tier reads of the #474 in-tree inventory (issue comment of
/// 2026-10-20 03:15 UTC): natural-key sidecar heads and the owner-id read.
/// They exist only in `proxima::flavor`, so they sit outside the two-tier
/// macro. A trimmed re-export fails to compile here.
#[allow(unused_imports)]
mod inventory_flavor_reads {
    use proxima::flavor::{
        SidecarHead, SidecarHeadsPage, SidecarHeadsRequest, SidecarHeadsScope, read_owner_ids,
        sidecar_heads_by_key,
    };
}

#[test]
fn projection_generator_and_artifact_fields_are_usable_through_the_sdk() {
    use proxima::flavor::{
        Artifact, FlavorContract, ProjectionArtifacts, StorageError, projection_artifacts,
    };

    fn generate(contract: &FlavorContract) -> Result<Option<ProjectionArtifacts>, StorageError> {
        Ok(projection_artifacts(contract)?.map(
            |ProjectionArtifacts {
                 table,
                 index,
                 tag_index,
                 language_trigger,
             }| {
                let Artifact { forward, inverse } = table;
                ProjectionArtifacts {
                    table: Artifact { forward, inverse },
                    index,
                    tag_index,
                    language_trigger,
                }
            },
        ))
    }

    std::hint::black_box(generate);
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
