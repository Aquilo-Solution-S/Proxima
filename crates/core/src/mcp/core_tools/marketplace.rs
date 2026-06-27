use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{CoreActionMeta, McpActionArgSpec, McpTool, McpToolCtx, McpToolError};
use crate::personality::MemorySnapshot;

use super::READ_ONLY;

const CORE_MARKETPLACE_BROWSE_SCOPE_KEY: &str = "core_marketplace:browse";
const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 100;

pub const CORE_MARKETPLACE_ACTIONS: &[CoreActionMeta] = &[CoreActionMeta {
    tool: CoreMarketplaceTool::NAME,
    action: "browse",
    scope_key: CORE_MARKETPLACE_BROWSE_SCOPE_KEY,
    description: "Browse public marketplace memories.",
    produces_schema_ids: &[],
    annotations: READ_ONLY,
}];

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MarketplaceBrowseArgs {
    #[serde(default = "default_limit")]
    #[schemars(
        description = "Maximum number of public memories. Defaults to 20; clamped to 1..=100."
    )]
    pub limit: u32,
}

#[derive(Debug, Serialize)]
pub struct MarketplaceBrowseOutput {
    pub memories: Vec<MarketplaceMemoryOutput>,
}

#[derive(Debug, Serialize)]
pub struct MarketplaceMemoryOutput {
    pub memory: String,
    pub kind: String,
    pub schema_id: String,
    pub schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authoring_personality_instance_id: Option<String>,
    pub text: Option<String>,
    pub wake_chain_depth: u16,
    pub payload: serde_json::Value,
    pub title: Option<String>,
    pub body: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Default)]
pub struct CoreMarketplaceTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CoreMarketplaceArgs {
    Browse(MarketplaceBrowseArgs),
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CoreMarketplaceOutput {
    Browse(MarketplaceBrowseOutput),
}

impl McpTool for CoreMarketplaceTool {
    const NAME: &'static str = "core_marketplace";
    const DESCRIPTION: &'static str = "Marketplace dispatcher — browse public memories.";
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[McpActionArgSpec {
        action: "browse",
        allowed_fields: &["limit"],
        required_fields: &[],
    }];
    type Args = CoreMarketplaceArgs;
    type Output = CoreMarketplaceOutput;

    fn call(
        ctx: McpToolCtx,
        args: CoreMarketplaceArgs,
    ) -> BoxFuture<'static, Result<CoreMarketplaceOutput, McpToolError>> {
        Box::pin(async move {
            match args {
                CoreMarketplaceArgs::Browse(args) => {
                    browse(ctx, args).await.map(CoreMarketplaceOutput::Browse)
                }
            }
        })
    }
}

async fn browse(
    ctx: McpToolCtx,
    args: MarketplaceBrowseArgs,
) -> Result<MarketplaceBrowseOutput, McpToolError> {
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let limit = args.limit.clamp(1, MAX_LIMIT);
    let memories = engine
        .browse_marketplace(&ctx.authz, i64::from(limit))
        .await?
        .into_iter()
        .map(|snapshot| marketplace_memory_output(&ctx, snapshot))
        .collect::<Result<Vec<_>, McpToolError>>()?;
    Ok(MarketplaceBrowseOutput { memories })
}

fn marketplace_memory_output(
    ctx: &McpToolCtx,
    snapshot: MemorySnapshot,
) -> Result<MarketplaceMemoryOutput, McpToolError> {
    let class = super::get_memory::memory_class(&snapshot.kind)?;
    let payload = super::get_memory::snapshot_payload_value(snapshot.payload.as_ref())?;
    let title = super::get_memory::payload_string(&payload, "title")
        .or_else(|| super::get_memory::payload_string(&payload, "conversation_id"));
    let body = super::get_memory::payload_string(&payload, "body")
        .or_else(|| super::get_memory::payload_string(&payload, "text"))
        .or_else(|| snapshot.text.clone());
    let tags = super::get_memory::payload_tags(&payload);
    Ok(MarketplaceMemoryOutput {
        memory: ctx.format_memory_with_class(snapshot.memory_id, class),
        kind: snapshot.kind,
        schema_id: snapshot.schema_id.as_str().to_string(),
        schema_version: snapshot.schema_version.into_inner(),
        authoring_personality_instance_id: super::get_memory::format_authoring_personality(
            ctx,
            snapshot.authoring_personality_instance_id,
        ),
        text: snapshot.text,
        wake_chain_depth: snapshot.wake_chain_depth.into_inner(),
        payload,
        title,
        body,
        tags,
    })
}

const fn default_limit() -> u32 {
    DEFAULT_LIMIT
}
