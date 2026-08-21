//! Build-time registry that flavors push into during their
//! `register()` call. Frozen into a `FlavorRegistryFrozen` once all
//! flavors have run.
//!
//! See docs/08 §Registration mechanism.

use crate::authz::{AuthorizationHook, OwnerResolver};
use crate::mcp::schema::{mcp_output_schema, mcp_tool_schema};
use crate::mcp::validate_action_args;
use crate::verbs::schema::{
    FlavorRegistryFrozen, PayloadKind, ProtocolPayload, ProtocolPayloadIngress,
    ProtocolPayloadIngressEntry, SchemaCapabilityTags, SchemaInfo,
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

// `BAND_EXACT` and its two siblings are deliberately NOT here: the core
// band values live in flavor #0's declaration (`flavor0::BAND_*`), so that
// a flavor referencing them is making the band-comparability claim rather
// than reaching for module vocabulary that only looked universal.
pub use contract::{
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
pub use descriptor::{FlavorDescriptor, FlavorProvenance};
pub use error::FlavorRegistryError;
pub use flavor0::FLAVOR_0;
pub use prefix::schema_id_has_prefix;
pub use registry::FlavorRegistry;

pub(crate) use freeze::schema_capability_map;
