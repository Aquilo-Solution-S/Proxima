//! Flavor SDK exports.

pub use crate::bundle::FlavorBundle;
pub use crate::migrations::NamedMigrator;
/// Background-worker surface for [`FlavorBundle::spawn_workers`]: the
/// runtime handles a spawning flavor receives and the named join handle
/// it returns.
pub use crate::workers::{FlavorWorker, FlavorWorkerContext};
/// The typed artefact inside [`CitedBlobStaged`], and the outcome of
/// [`proxima_core::Engine::complete_upload_as_fact`].
///
/// NAMING A TYPE IS NOT ENOUGH TO RETURN ONE. `stage_upload` returns
/// `CitedBlobStaged`, whose `payload` field is an
/// [`UploadedBlobPayload`] — a struct with no constructor, so an
/// out-of-tree flavor that could name the outer type still could not
/// build one, and the port was unimplementable for exactly as long as
/// this line was missing. That is the recurring shape of a facade gap
/// here: the blocker is never the trait, it is a field type one level
/// down that no `use` can reach. The tier test below constructs the
/// struct rather than only naming it, because only construction
/// exercises the difference.
///
/// [`UploadCompleted`] rides along for the caller's half of the same
/// verb: without it the result of a completion cannot be bound to a
/// named local or returned from a flavor's own function.
pub use proxima_core::citations::UploadedBlobPayload;
pub use proxima_core::engine::{FactWrite, UnitOfWork};
/// Goal write DTOs, including the nested topology and wake declarations.
pub use proxima_core::engine::{
    GoalDecomposeRequest, GoalMarkAchievedRequest, GoalModifyRequest, GoalTransitionRequest,
};
pub use proxima_core::engine::{UploadCompleted, UploadCompletionExpectation};
pub use proxima_core::error::{ErrorCode, ProtocolError};
/// Build-time flavor declaration vocabulary. These are const-constructible
/// contract values; no runtime registry or storage handle crosses the SDK.
pub use proxima_core::flavor::{
    BAND_NAME_EXACT, BAND_NAME_RESCUE, BAND_NAME_SUBSTRING, Band, BandComparability, CORE_ORDINAL,
    CounterRule, DEFAULT_RANK_WEIGHTS, DbConstraint, DbTrigger, EmbedUnit, EmbeddingRecipe,
    EmbeddingSlot, Enforcement, EraseLeg, EraseRule, ExportRule, FlavorContract, ForgetLeg,
    ForgetRule, KeyShape, LanguagePolicy, PROJECTION_MEMORY_COLUMN, PROJECTION_MEMORY_FK,
    PROJECTION_TABLE_NAME, ProjectionDecl, ProjectionSpec, Provenance, RankSource,
    ResolvedEmbedUnit, ResourceContract, SLOT_DEFAULT, SchemaContract, SchemaRef,
    SearchProjectionDecl, SubstringArm, Surface, TS_RANK_NORMALIZATION_LOG_LENGTH_SCALE,
    TS_RANK_NORMALIZATION_NONE, TS_RANK_NORMALIZATION_SCALE, TSVECTOR_WEIGHT_CLASSES, ToolContract,
    TransferLeg, TransferRule, WEIGHT_UNIFORM, WeightedField,
};
/// Tool metadata and the MCP reference presentation bridge.
/// Implement [`Tool`] with [`ToolCtx`] / [`ToolError`]; MCP and REST adapt it.
/// Import [`McpPresentationExt`] to format and parse MCP references on `ToolCtx`.
/// Resolve dependencies with `ctx.service::<T>()` and model labels with
/// [`ToolCtx::operator_label`]. The authenticated model identity takes precedence.
pub use proxima_core::mcp::{
    McpActionArgSpec, McpAuthorContext, McpPresentationExt, McpToolAnnotations,
};
/// Host-wired cited-blob lane, resolved by tools and workers from
/// [`FlavorServices`]. Present only when the host configured S3; the concrete
/// backend (`proxima-blob-s3`) is never named across this seam.
///
/// [`CitedBlobHeld`] and [`MAX_HELD_BLOB_DIGESTS`] are the two halves of
/// `find_held_blobs`, and both have to cross this seam for the same reason:
/// a flavor faking the port must be able to RETURN the outcome type, and a
/// flavor batching its digests must be able to read the bound it is being
/// held to rather than hardcode a copy that drifts from it.
pub use proxima_core::storage_ports::{
    CitedBlobHeld, CitedBlobIntegrityMismatch, CitedBlobOwnerMissingObject,
    CitedBlobOwnerReconcileOutcome, CitedBlobOwnerReconcilePort, CitedBlobOwnerReconcileService,
    CitedBlobPort, CitedBlobReadError, CitedBlobReadPort, CitedBlobReadService, CitedBlobReadUrl,
    CitedBlobService, CitedBlobStaged, CitedBlobUploadAborted, CitedBlobUploadCompleted,
    CitedBlobUploadHeader, CitedBlobUploadPrepared, MAX_HELD_BLOB_DIGESTS, VerifiedCitedBlob,
};
/// Transaction-scoped, owner-stamped sidecar precondition reads.
pub use proxima_core::storage_ports::{SIDECAR_SESSION_READ_MAX_ROWS, SidecarSessionRead};
/// Typed inline citation drafts and the Engine admission witnesses they
/// produce. `authorize_fact_with_citation` takes the drafts plus a sidecar
/// slice; without these names an out-of-tree flavor can only spell
/// `CitationSpec` (opaque hash) or take `proxima-core`.
pub use proxima_core::verbs::fact_ingest::{
    AuthorizedFactWithCitation, AuthorizedFactWithCitationRef, CitationAttachmentRequest,
    CitationSpec, FactIngestOutcome, FactWriteCommand, InlineCitationMappingDraft,
    InlineCitedObjectDraft,
};
pub use proxima_core::verbs::goal_write::{
    ChildGoalDraft, DecomposeGoalOutcome, GoalAssignmentTarget, GoalAuthorship, GoalCreateRequest,
    GoalDependencyRef, GoalEvidenceRef, GoalPayloadWrite, GoalState, GoalTopologyWrite,
    GoalWakeConfigWrite, GoalWakeToolId, GoalWakeTrigger, GoalWriteBuildError, GoalWriteOutcome,
    IdempotencyKey, OperatorKind, SystemOrigin,
};
pub use proxima_core::verbs::query::{
    GoalRow, QueryRequest, QueryResponse, SearchMode, SidecarAtom, SupersessionStatus,
    hybrid_degraded_to_lexical,
};
pub use proxima_core::verbs::schema::PayloadKind;
/// [`FactTombstone`] is the return type of [`FactPayload::tombstone`], so a
/// flavor that declares a *stateful* Fact schema — one with a head per
/// natural key and an explicit deletion observation — cannot write that
/// override without it. The in-tree precedent (`flavors/code`'s
/// `FileRevisionV1`) reaches it through a direct `proxima-core` dependency
/// an out-of-tree flavor does not have. Without the override a schema can
/// still declare `natural_key_columns`, but registry consumers cannot
/// identify which hot-head payload value represents a deletion observation.
/// Core Query does not filter tombstone heads.
pub use proxima_core::{
    AbstractionPayload, CapabilitySet, CitationMappingPayload, CitedObjectPayload,
    DelegatedAuthorityError, DelegatedAuthorityService, DelegatedCommand, DelegatedPhase,
    DelegationId, DelegationIssued, DelegationRevocation, EndpointUrlError, EndpointUrlPolicy,
    EngineAuthority, FactPayload, FactReceiptId, FactTombstone, FlavorDescriptor, FlavorProvenance,
    FlavorRegistry, FlavorRegistryError, FlavorRegistryFrozen, FlavorServiceError, FlavorServices,
    GoalId, GoalPayload, GroupId, InputContractId, MAX_MEMORY_HYDRATION_BATCH,
    MemoryHydrationBatchOutcome, MemoryHydrationOutcome, MemoryHydrationStatus, MemoryId, ModelId,
    OperatorId, OwnerRef, PayloadKeyBuilder, PerspectivePayload, PromptVersion, SchemaId,
    SchemaVersion, SearchProjectionColumnKind, SidecarPayload, Tool, ToolCaller, ToolCtx,
    ToolError, ToolId, ToolServices, TrustedModelIdError, UserId, is_loopback_endpoint,
    is_loopback_host, proxima_flavor, proxima_schema_id, validate_endpoint_url,
};
/// Query / ingest types a flavor names next to [`crate::Engine`]
/// (Host API). `Engine` itself stays off this module (`docs/14`).
/// `AuthorizedFactWrite` is Engine-internal (UoW-first).
///
/// [`AuthorizationHook`] is the advertised extension. Implementing `veto`
/// or `observe` requires the input/outcome types; [`OwnerResolver`] is the
/// sibling remap trait the registry already accepts. Naming the trait
/// alone is not enough (see the `UploadedBlobPayload` note below).
pub use proxima_core::{
    AuthorizationHook, AuthzInput, AuthzOperation, AuthzOutcome, AuthzVeto, EntityId,
    MembershipChange, OwnerResolver,
};
/// Typed derived-memory writes. `DerivedMemory` infers kind/schema from its payload;
/// `Engine::derive_memory` resolves actual origin kinds and validates provenance.
/// `MemoryTarget` distinguishes a new series from a revision. Always retain the returned row ID.
/// `UnitOfWork::derive_memories` pre-embeds a batch before opening its transaction;
/// an already-open transaction defers embedding, reported in `DerivedMemoryOutcome`.
pub use proxima_core::{
    DerivationIdentity, DerivedMemory, DerivedMemoryOutcome, EntityKind, MemoryTarget, SeriesHandle,
};
/// The connection vocabulary a flavor is allowed to speak (docs/16-edges.md).
///
/// A flavor never writes an edge, so none of these is a write surface.
/// [`PayloadReference`] is the whole of what a payload declares — the field
/// it was read from, the binding, and the target — and the defaulted
/// `references()` on every payload trait returns a `Vec` of them, so a
/// schema that points at another node cannot be written without naming this
/// type. [`ReferenceBinding`] is a property of the field, decided once by
/// the schema author. The only binding is [`ReferenceBinding::Pin`].
///
/// [`EdgeEndpoint`] is the address form the constructors below mint
/// (`memory`, `goal`); [`EdgeKind`] is exported to be *read*
/// — off a listed [`Edge`], or when filtering — never passed to a writer,
/// because the kind follows the operation.
pub use proxima_core::{
    Edge, EdgeEndpoint, EdgeKind, EdgeTargetProjection, EntityRef, PayloadReference,
    ReferenceBinding,
};
/// Shared argument rules for search and paged reads, so a flavor does not
/// have to invent its own — and so the in-tree tools cannot drift apart.
///
/// [`reject_zero_limit`]: every paged read faces the same question, and
/// the in-tree tools once answered it three different ways: one rejected
/// `limit: 0`, one returned a well-formed empty page indistinguishable
/// from "nothing matched", and one clamped to 1 and answered a question
/// nobody asked. The engine has rejected zero from the start; this is how
/// a flavor agrees with it in one line. Upper bounds stay the flavor's own
/// business — clamping those still serves the caller's intent.
///
/// [`validate_search_query`] and [`MAX_QUERY_CHARS`]: three tools carried
/// a byte-identical copy of the same check with `512` inlined in each.
///
/// [`MAX_TEXT_CAP_CHARS`] is the ceiling on a caller-supplied cap over
/// returned text (`body_max_chars`, `snippet_max_chars`). The *default*
/// under it is deliberately not shared — how much of a code chunk versus a
/// memory body is worth returning is a property of the object.
pub use proxima_core::{
    MAX_QUERY_CHARS, MAX_TEXT_CAP_CHARS, reject_zero_limit, validate_search_query,
};
/// The flavor-owned lifecycle scope: the declaration a flavor writes, and
/// the erase-side fence that declaration makes available.
///
/// [`ScopeDecl`] goes in the flavor's [`FlavorContract::scopes`], and
/// [`ScopeKind`] names it from the payloads that belong to it. Declaring
/// the scope is the whole of the admission side — the Engine takes the
/// fence and runs the liveness probe on EVERY write of a payload that
/// names the scope, whether the caller is the flavor or a host writing
/// straight through [`proxima_core::Engine`], and there is no opt-in and
/// no opt-out.
///
/// [`lock_scope_fence_exclusive_tx`] is the other half, and the only half
/// a flavor spells: a scope erase takes it in its own transaction BEFORE
/// it reads what it intends to delete, so the footprint it computes is
/// exact by construction. [`lock_scope_fence_shared_tx`] is for the
/// flavor-owned writes that are NOT payload admissions — a run row, a
/// cursor — which the Engine never sees and so cannot fence for you.
///
/// [`FlavorContract::scopes`]: proxima_core::FlavorContract::scopes
pub use proxima_core::{ScopeDecl, ScopeKind, ScopeRef};
pub use proxima_storage_pg::access::owner_columns::{
    lock_scope_fence_exclusive_tx, lock_scope_fence_shared_tx,
};
pub use proxima_storage_pg::pg_sidecar;
pub use proxima_storage_pg::sidecars::{
    PgCitationMappingSidecar, PgCitedObjectSidecar, PgGoalSidecar, PgMemoryPayload,
    PgMemoryPayloadFuture, PgMemorySidecar, PgSidecarFuture, PgSidecarReadCtx, SidecarInsertPermit,
};
pub use proxima_storage_pg::{
    PgSidecarRegistry, PgSidecarRegistryFrozen, register_core_pg_sidecars,
};

mod authorized_read;
pub use authorized_read::{
    authorized_abstraction_payloads, authorized_fact_payloads, authorized_memory_ids,
    read_owner_ids,
};
