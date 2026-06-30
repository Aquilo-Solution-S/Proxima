//! Flavor SDK exports.

pub use crate::bundle::FlavorBundle;
pub use crate::migrations::NamedMigrator;
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
