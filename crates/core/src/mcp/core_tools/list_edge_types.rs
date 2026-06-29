//! `core/list_edge_types` — project `FlavorRegistryFrozen` relations.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::RelationDescriptor;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListEdgeTypesArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EdgePayloadSchemaItem {
    pub schema_id: String,
    pub schema_version: u32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EdgeTypeItem {
    pub edge_type: String,
    pub class: String,
    pub owner_policy: String,
    pub target_access_policy: String,
    pub source_binding: String,
    pub target_binding: String,
    pub source_kind_mask: Vec<String>,
    pub target_kind_mask: Vec<String>,
    pub authorship_mask: Vec<String>,
    pub payload_schema: Option<EdgePayloadSchemaItem>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListEdgeTypesOutput {
    pub edge_types: Vec<EdgeTypeItem>,
}

pub(super) fn edge_type_item(rel: &RelationDescriptor) -> EdgeTypeItem {
    EdgeTypeItem {
        edge_type: rel.relation.clone(),
        class: rel.class.as_str().to_string(),
        owner_policy: rel.owner_policy.as_str().to_string(),
        target_access_policy: rel.target_access_policy.as_str().to_string(),
        source_binding: rel.source_binding.as_str().to_string(),
        target_binding: rel.target_binding.as_str().to_string(),
        source_kind_mask: rel
            .source_kind_mask
            .as_strings()
            .into_iter()
            .map(str::to_string)
            .collect(),
        target_kind_mask: rel
            .target_kind_mask
            .as_strings()
            .into_iter()
            .map(str::to_string)
            .collect(),
        authorship_mask: rel
            .authorship_mask
            .as_strings()
            .into_iter()
            .map(str::to_string)
            .collect(),
        payload_schema: rel
            .payload_schema
            .as_ref()
            .map(|schema| EdgePayloadSchemaItem {
                schema_id: schema.schema_id.as_str().to_string(),
                schema_version: schema.schema_version.into_inner(),
            }),
    }
}

#[allow(clippy::unused_async)]
/// # Errors
///
/// This projection is infallible today; the `Result` shape matches the tool
/// dispatch contract.
pub async fn list_edge_types(
    ctx: McpToolCtx,
    _args: ListEdgeTypesArgs,
) -> Result<ListEdgeTypesOutput, McpToolError> {
    let edge_types = ctx
        .registry
        .list_relations()
        .iter()
        .map(edge_type_item)
        .collect();
    Ok(ListEdgeTypesOutput { edge_types })
}
