//! Flavor SDK exports.

pub use crate::bundle::FlavorBundle;
pub use crate::migrations::NamedMigrator;
/// Background-worker surface for [`FlavorBundle::spawn_workers`]: the
/// runtime handles a spawning flavor receives and the named join handle
/// it returns.
pub use crate::workers::{FlavorWorker, FlavorWorkerContext};
/// MCP tool-authoring surface: implement [`McpTool`] with typed
/// [`McpToolCtx`] / [`McpToolError`] instead of reaching into
/// `proxima_core::mcp`. Mirrors what `docs/tutorials/add-first-mcp-tool.md`
/// imports.
///
/// [`McpToolExtensions`] is the seam for anything core cannot name. A host
/// composes its flavors' services into it by overriding
/// `FlavorApp::mcp_tool_extensions`, and a tool resolves one back with
/// `ctx.extensions.get::<MyService>()` — which is exactly how core's own
/// `core_upload` finds the [`CitedBlobService`] below. It is re-exported
/// here because it is the *return type* of that override: without it an
/// out-of-tree flavor cannot write the signature at all, so its tools have
/// no sanctioned route to a database handle or any other host-owned
/// dependency. See `docs/09-developing-flavors.md` § MCP Tools.
pub use proxima_core::mcp::{
    McpActionArgSpec, McpAuthorContext, McpTool, McpToolAnnotations, McpToolCtx, McpToolError,
    McpToolErrorKind, McpToolExtensions,
};
/// The zero-page-bound rule, shared so a flavor does not have to invent
/// its own.
///
/// Every paged read faces the same question and the in-tree tools once
/// answered it three different ways: one rejected `limit: 0`, one returned
/// a well-formed empty page indistinguishable from "nothing matched", and
/// one clamped to 1 and answered a question nobody asked. The engine has
/// rejected zero from the start; this is how a flavor agrees with it in
/// one line. Upper bounds stay the flavor's own business — clamping those
/// still serves the caller's intent.
pub use proxima_core::reject_zero_limit;
/// Host-wired cited-blob lane, handed to workers as
/// [`FlavorWorkerContext::blobs`]. Present only when the host configured
/// S3; the concrete backend (`proxima-blob-s3`) is never named across
/// this seam, so a flavor codes against [`CitedBlobPort`] and can fake it
/// wholesale in tests.
pub use proxima_core::storage_ports::{
    CitedBlobPort, CitedBlobReadUrl, CitedBlobService, CitedBlobUploadAborted,
    CitedBlobUploadCompleted, CitedBlobUploadHeader, CitedBlobUploadPrepared,
};
/// [`FactTombstone`] is the return type of [`FactPayload::tombstone`], so a
/// flavor that declares a *stateful* Fact schema — one with a head per
/// natural key and an explicit deletion observation — cannot write that
/// override without it. The in-tree precedent (`flavors/code`'s
/// `FileRevisionV1`) reaches it through a direct `proxima-core` dependency
/// an out-of-tree flavor does not have. Without the override a schema can
/// still declare `natural_key_columns`, but storage has no discriminator
/// for `PresentOnly` queries, so a deleted entity stays a live head
/// forever.
pub use proxima_core::{
    AbstractionPayload, AuthorshipKindMask, CapabilitySet, CitationMappingPayload,
    CitedObjectPayload, EdgeId, EdgePayload, EndpointBinding, EntityKindMask, FactPayload,
    FactReceiptId, FactTombstone, FlavorDescriptor, FlavorProvenance, FlavorRegistry,
    FlavorRegistryError, FlavorRegistryFrozen, GoalId, GoalPayload, InputContractId, MemoryId,
    ModelId, OperatorId, PayloadKeyBuilder, PerspectivePayload, PromptVersion, RelationClass,
    RelationDescriptor, SchemaId, SchemaRef, SchemaVersion, SearchProjection,
    SearchProjectionColumnKind, SearchProjectionField, SidecarPayload, Tool, ToolCall, ToolCallFn,
    ToolCtx, ToolDescriptor, ToolError, ToolOrigin, ToolServices, proxima_flavor,
    proxima_schema_id,
};
/// Derived-memory authoring: the request/outcome types of
/// [`proxima_core::Engine::author_derived_authorized`], which is how a
/// flavor writes the Abstractions and Perspectives its
/// [`AbstractionPayload`] / [`PerspectivePayload`] schemas describe.
///
/// Without these the SDK could only *declare* derived schemas, never
/// populate them: an out-of-tree flavor depends on `proxima` alone, and
/// the in-tree precedent (`flavors/code`) reaches the same lane through a
/// direct `proxima-storage-pg` dependency it cannot have.
///
/// [`RegisteredRelation`] is obtained from
/// [`FlavorRegistryFrozen::resolve_relation`] via
/// [`proxima_core::Engine::registry`]; provenance edges back to the source
/// Facts use [`CORE_DERIVED_FROM_RELATION`], and a re-derivation that
/// replaces an earlier output sets `supersedes` (which also writes a
/// [`CORE_SUPERSEDES_RELATION`] edge in the same transaction).
///
/// Note the embedding asymmetry against Facts: a derived memory is
/// embedded *synchronously*, inside the write, so a provider failure
/// fails the write. Facts enqueue a durable job instead. A flavor
/// deriving many memories in a worker should checkpoint per output rather
/// than per batch.
pub use proxima_core::{
    AuthorDerivedAuthorizedOutcome, AuthorDerivedEdgeInput, AuthorDerivedRequestInput,
    CORE_DERIVED_FROM_RELATION, CORE_SUPERSEDES_RELATION, EdgeAuthorshipKind, EntityKind,
    MemoryOperatorKind, RegisteredRelation,
};
pub use proxima_storage_pg::pg_sidecar;
pub use proxima_storage_pg::sidecars::{
    PgCitationMappingSidecar, PgCitedObjectSidecar, PgEdgeSidecar, PgGoalSidecar, PgMemoryPayload,
    PgMemoryPayloadFuture, PgMemorySidecar, PgSidecarFuture, PgSidecarReadCtx,
};
pub use proxima_storage_pg::{
    PgSidecarRegistry, PgSidecarRegistryFrozen, register_core_pg_sidecars,
};

mod authorized_read;
pub use authorized_read::{
    authorized_abstraction_payloads, authorized_code_chunk_head_candidates,
    authorized_fact_payloads, authorized_fact_payloads_include_tombstones, authorized_memory_ids,
    nearest_code_chunk_candidates,
};
pub use proxima_storage_pg::query::{CodeChunkVectorCandidate, CodeChunkVectorFilters};
