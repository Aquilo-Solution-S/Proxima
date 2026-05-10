//! `core/list_schemas` — project FlavorRegistryFrozen schemas.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::verbs::schema::PayloadKind;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListSchemasTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListSchemasArgs {
    /// Optional filter. One of "Fact", "Abstraction", "Perspective",
    /// "Goal", "Edge", "CitedObject", "CitationMapping". Omit to return all kinds.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SchemaItem {
    pub schema_id: String,
    pub schema_version: u32,
    pub kind: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListSchemasOutput {
    pub schemas: Vec<SchemaItem>,
}

fn parse_kind(s: &str) -> Option<PayloadKind> {
    match s {
        "Fact" => Some(PayloadKind::Fact),
        "Abstraction" => Some(PayloadKind::Abstraction),
        "Perspective" => Some(PayloadKind::Perspective),
        "Goal" => Some(PayloadKind::Goal),
        "Edge" => Some(PayloadKind::Edge),
        "CitedObject" => Some(PayloadKind::CitedObject),
        "CitationMapping" => Some(PayloadKind::CitationMapping),
        _ => None,
    }
}

fn kind_str(k: PayloadKind) -> &'static str {
    match k {
        PayloadKind::Fact => "Fact",
        PayloadKind::Abstraction => "Abstraction",
        PayloadKind::Perspective => "Perspective",
        PayloadKind::Goal => "Goal",
        PayloadKind::Edge => "Edge",
        PayloadKind::CitedObject => "CitedObject",
        PayloadKind::CitationMapping => "CitationMapping",
    }
}

impl McpTool for ListSchemasTool {
    const NAME: &'static str = "core/list_schemas";
    const DESCRIPTION: &'static str =
        "List registered schemas. Filter by kind for trigger discovery: \
         OnMemory triggers point at Fact schema_ids.";
    type Args = ListSchemasArgs;
    type Output = ListSchemasOutput;

    fn call(
        ctx: McpToolCtx,
        args: ListSchemasArgs,
    ) -> BoxFuture<'static, Result<ListSchemasOutput, McpToolError>> {
        Box::pin(async move {
            let filter = args.kind.as_deref().and_then(parse_kind);
            let schemas = ctx
                .registry
                .list()
                .into_iter()
                .filter(|info| filter.is_none_or(|k| info.kind == k))
                .map(|info| SchemaItem {
                    schema_id: info.schema_id.as_str().to_string(),
                    schema_version: info.schema_version.into_inner(),
                    kind: kind_str(info.kind).to_string(),
                })
                .collect();
            Ok(ListSchemasOutput { schemas })
        })
    }
}
