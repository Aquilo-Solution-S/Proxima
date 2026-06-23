//! `core/list_edge_types` — project `FlavorRegistryFrozen` relations.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListEdgeTypesArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EdgeTypeItem {
    pub edge_type: String,
    pub class: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListEdgeTypesOutput {
    pub edge_types: Vec<EdgeTypeItem>,
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
        .map(|rel| EdgeTypeItem {
            edge_type: rel.relation.clone(),
            class: rel.class.as_str().to_string(),
        })
        .collect();
    Ok(ListEdgeTypesOutput { edge_types })
}
