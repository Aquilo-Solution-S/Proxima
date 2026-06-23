//! `core/list_edge_types` — project `FlavorRegistryFrozen` relations.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListEdgeTypesTool;

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

impl McpTool for ListEdgeTypesTool {
    const NAME: &'static str = "core_list_edge_types";
    const DESCRIPTION: &'static str =
        "List registered edge types. OnEdge triggers reference these.";
    type Args = ListEdgeTypesArgs;
    type Output = ListEdgeTypesOutput;

    fn call(
        ctx: McpToolCtx,
        args: ListEdgeTypesArgs,
    ) -> BoxFuture<'static, Result<ListEdgeTypesOutput, McpToolError>> {
        Box::pin(list_edge_types(ctx, args))
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
        .map(|rel| EdgeTypeItem {
            edge_type: rel.relation.clone(),
            class: rel.class.as_str().to_string(),
        })
        .collect();
    Ok(ListEdgeTypesOutput { edge_types })
}
