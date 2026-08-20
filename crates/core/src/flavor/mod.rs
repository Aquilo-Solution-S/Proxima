//! Build-time registry that flavors push into during their
//! `register()` call. Frozen into a `FlavorRegistryFrozen` once all
//! flavors have run.
//!
//! See docs/08 §Registration mechanism.

use crate::authz::{AuthorizationHook, OwnerResolver};
use crate::mcp::schema::{mcp_output_schema, mcp_tool_schema};
use crate::mcp::validate_action_args;
use crate::verbs::schema::{
    FlavorRegistryFrozen, MemorySearchProjection, MemorySearchProjectionField, PayloadKind,
    ProtocolPayload, ProtocolPayloadIngress, ProtocolPayloadIngressEntry, SchemaCapabilityTags,
    SchemaInfo,
};
use crate::{
    AbstractionPayload, CapabilityTag, CitationMappingPayload, CitedObjectPayload, FactPayload,
    GoalPayload, McpCallFn, McpTool, McpToolDescriptor, McpToolError, McpToolOrigin,
    PerspectivePayload, RequestBehavior, SchemaId, SchemaVersion, ScopeGateBehavior,
    SidecarPayload, Tool,
};

use std::collections::BTreeSet;
use std::sync::Arc;

pub mod contract;
mod descriptor;
mod error;
pub mod flavor0;
mod freeze;
mod ingress;
mod prefix;
mod registry;
mod registry_mutation;
mod schema_registration;
mod tool_registration;

#[cfg(test)]
mod tests;

pub use contract::{
    BAND_EXACT, BAND_RESCUE, BAND_SUBSTRING, Band, BandComparability, CORE_ORDINAL,
    DEFAULT_RANK_WEIGHTS, DbConstraint, DbTrigger, EmbedText, EmbedUnit, EmbeddingRecipe,
    EmbeddingSlot, Enforcement, EraseRule, ExportRule, FlavorContract, ForgetRule, KeyShape,
    LanguagePolicy, PROJECTION_MEMORY_FK, PROJECTION_TABLE_NAME, ProjectionDecl, ProjectionSpec,
    Provenance, ResolvedEmbedUnit, ResourceContract, SLOT_DEFAULT, SchemaContract, SchemaRef,
    SearchProjectionDecl, SubstringArm, Surface, TSVECTOR_WEIGHT_CLASSES, ToolContract,
    TransferRule, WEIGHT_UNIFORM, WeightedField,
};
pub use descriptor::{FlavorDescriptor, FlavorProvenance};
pub use error::FlavorRegistryError;
pub use flavor0::FLAVOR_0;
pub use prefix::schema_id_has_prefix;
pub use registry::FlavorRegistry;

pub(crate) use freeze::schema_capability_map;
