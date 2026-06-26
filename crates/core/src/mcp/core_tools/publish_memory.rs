use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::{MemoryId, PublishMemoryRequestInput};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PublishMemoryArgs {
    pub memory: String,
    pub from_space: String,
    pub to_space: String,
    #[serde(default)]
    pub title_override: Option<String>,
    #[serde(default)]
    pub body_override: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Serialize)]
pub struct PublishMemoryOutput {
    pub source: String,
    pub published: String,
    pub from_space: String,
    pub to_space: String,
}

#[derive(Debug)]
pub struct PublishMemoryTool;

impl McpTool for PublishMemoryTool {
    const NAME: &'static str = "core_publish_memory";
    const DESCRIPTION: &'static str = "Copy a core AgentNote memory from one authorized memory space to another. v1 never moves owners and never creates cross-owner edges.";
    type Args = PublishMemoryArgs;
    type Output = PublishMemoryOutput;

    fn call(
        ctx: McpToolCtx,
        args: PublishMemoryArgs,
    ) -> BoxFuture<'static, Result<PublishMemoryOutput, McpToolError>> {
        Box::pin(async move { publish_memory(ctx, args).await })
    }
}

async fn publish_memory(
    ctx: McpToolCtx,
    args: PublishMemoryArgs,
) -> Result<PublishMemoryOutput, McpToolError> {
    if !args.confirm {
        return Err(crate::error::ProtocolError::forbidden(
            "confirm=true is required to publish memory",
        )
        .into());
    }
    let from = super::memory_spaces::resolve_space_owner(
        &ctx,
        Some(args.from_space.as_str()),
        super::memory_spaces::SpaceDefault::Current,
    )?;
    let to = super::memory_spaces::resolve_space_owner(
        &ctx,
        Some(args.to_space.as_str()),
        super::memory_spaces::SpaceDefault::Current,
    )?;

    let memory_id = resolve_memory_reference(&ctx, &args.memory)?;
    let tags = if args.tags.is_empty() {
        Vec::new()
    } else {
        super::memory::util::normalize_tags(args.tags)?
    };
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::InvalidInput("engine required".into()))?;
    let outcome = engine
        .publish_memory(
            &ctx.authz,
            PublishMemoryRequestInput {
                source_owner: from.owner.clone(),
                target_owner: to.owner.clone(),
                memory_id,
                title_override: args.title_override,
                body_override: args.body_override,
                tags,
                author_personality_instance_id: ctx.author.personality_instance_id,
            },
        )
        .await?;

    Ok(PublishMemoryOutput {
        source: ctx.format_fact_memory(outcome.source_memory_id),
        published: ctx.format_fact_memory(outcome.published_memory_id),
        from_space: from.key,
        to_space: to.key,
    })
}

fn resolve_memory_reference(ctx: &McpToolCtx, raw: &str) -> Result<MemoryId, McpToolError> {
    match ctx.resolve_memory(raw) {
        Ok(memory_id) => Ok(memory_id),
        Err(resolve_err) => raw
            .parse::<uuid::Uuid>()
            .map(MemoryId::new)
            .map_err(|_| resolve_err),
    }
}
