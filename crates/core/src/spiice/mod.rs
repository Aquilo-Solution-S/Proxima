//! Build-time registry that spiices push into during their
//! `register()` call. Frozen into a `SpiiceRegistryFrozen` once all
//! spiices have run.
//!
//! See docs/host-integration.md

use crate::authz::{AuthorizationHook, OwnerResolver};
use crate::mcp::schema::{mcp_output_schema, mcp_tool_schema};
use crate::mcp::validate_action_args;
use crate::scope::ScopeKind;
use crate::verbs::schema::{
    PayloadKind, ProtocolPayload, ProtocolPayloadIngress, ProtocolPayloadIngressEntry,
    SchemaCapabilityTags, SchemaInfo, SpiiceRegistryFrozen,
};
use crate::{
    AbstractionPayload, CapabilityTag, CitedObjectPayload, FactPayload, GoalPayload, McpCallFn,
    McpTool, McpToolDescriptor, McpToolError, McpToolOrigin, PerspectivePayload, RequestBehavior,
    SchemaId, SchemaVersion, ScopeGateBehavior, SidecarPayload, Tool,
};

use std::collections::BTreeSet;
use std::sync::Arc;

pub mod contract;
mod descriptor;
mod error;
mod freeze;
mod ingress;
mod prefix;
mod registry;
mod registry_mutation;
mod schema_registration;
pub mod spiice0;
mod tool_registration;

#[cfg(test)]
mod tests;

// `BAND_EXACT` and its two siblings are deliberately NOT here: the core
// band values live in spiice #0's declaration (`spiice0::BAND_*`), so that
// a spiice referencing them is making the band-comparability claim rather
// than reaching for module vocabulary that only looked universal.
pub use contract::{
    BAND_NAME_EXACT, BAND_NAME_RESCUE, BAND_NAME_SUBSTRING, Band, BandComparability, CORE_ORDINAL,
    CoreBandName, CoreBandNameParseError, CounterRule, DEFAULT_RANK_WEIGHTS, DbConstraint,
    DbTrigger, EmbedUnit, EmbeddingRecipe, EmbeddingSlot, Enforcement, EraseLeg, EraseRule,
    ExportRule, ForgetLeg, ForgetRule, HostStateEraseDisposition, KeyShape, LanguagePolicy,
    PROJECTION_MEMORY_COLUMN, PROJECTION_MEMORY_FK, PROJECTION_TABLE_NAME, ProjectionDecl,
    ProjectionSpec, Provenance, RankSource, ResolvedEmbedUnit, ResourceContract,
    RetiredFactContract, SLOT_DEFAULT, SchemaContract, SchemaRef, SearchProjectionDecl,
    SpiiceContract, SubstringArm, Surface, TS_RANK_NORMALIZATION_LOG_LENGTH_SCALE,
    TS_RANK_NORMALIZATION_NONE, TS_RANK_NORMALIZATION_SCALE, TSVECTOR_WEIGHT_CLASSES, ToolContract,
    TransferLeg, TransferRule, WEIGHT_UNIFORM, WeightedField,
};
pub use descriptor::{SpiiceDescriptor, SpiiceProvenance};
pub use error::SpiiceRegistryError;
pub use prefix::schema_id_has_prefix;
pub use registry::SpiiceRegistry;
pub use spiice0::SPIICE_0;

pub(crate) use freeze::schema_capability_map;
