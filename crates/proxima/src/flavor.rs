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
pub use proxima_core::mcp::{
    McpActionArgSpec, McpAuthorContext, McpTool, McpToolAnnotations, McpToolCtx, McpToolError,
    McpToolErrorKind,
};
/// Host-wired cited-blob lane, handed to workers as
/// [`FlavorWorkerContext::blobs`]. Present only when the host configured
/// S3; the concrete backend (`proxima-blob-s3`) is never named across
/// this seam, so a flavor codes against [`CitedBlobPort`] and can fake it
/// wholesale in tests.
pub use proxima_core::storage_ports::{
    CitedBlobPort, CitedBlobReadUrl, CitedBlobService, CitedBlobUploadAborted,
    CitedBlobUploadCompleted, CitedBlobUploadHeader, CitedBlobUploadPrepared,
};
pub use proxima_core::{
    AbstractionPayload, AuthorshipKindMask, CapabilitySet, CitationMappingPayload,
    CitedObjectPayload, EdgeId, EdgePayload, EndpointBinding, EntityKindMask, FactPayload,
    FactReceiptId, FlavorDescriptor, FlavorProvenance, FlavorRegistry, FlavorRegistryError,
    FlavorRegistryFrozen, GoalId, GoalPayload, InputContractId, MemoryId, ModelId, OperatorId,
    PayloadKeyBuilder, PerspectivePayload, PromptVersion, RelationClass, RelationDescriptor,
    SchemaId, SchemaRef, SchemaVersion, SearchProjection, SearchProjectionColumnKind,
    SearchProjectionField, SidecarPayload, Tool, ToolCall, ToolCallFn, ToolCtx, ToolDescriptor,
    ToolError, ToolOrigin, ToolServices, proxima_flavor, proxima_schema_id,
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
