use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::verbs::event_ingest::EventDraft;
use crate::{
    AgentNoteV1, FactPayload, MemoryAction, MemoryId, Role, SidecarPayload, SourceBatchId,
};

const SOURCE_ID: &str = "core/agent-publish";

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
    require_memory_action(&ctx, &from, MemoryAction::Read)?;
    require_memory_action(&ctx, &from, MemoryAction::Publish)?;
    require_memory_action(&ctx, &to, MemoryAction::Write)?;

    let memory_id = resolve_memory_reference(&ctx, &args.memory)?;
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let sidecars = super::get_memory::sidecar_specs(&ctx);
    let snapshot = storage
        .load_memory_by_id(&from.owner, memory_id, None, &sidecars)
        .await?
        .ok_or_else(|| McpToolError::InvalidInput(format!("memory {memory_id:?} not found")))?;
    if snapshot.schema_id != AgentNoteV1::schema_id() {
        return Err(McpToolError::InvalidInput(
            "core_publish_memory v1 supports only core/agent-note-v1".into(),
        ));
    }
    let Some(payload) = snapshot
        .payload
        .as_ref()
        .and_then(SidecarPayload::downcast_ref::<AgentNoteV1>)
    else {
        return Err(McpToolError::Other("agent note payload missing".into()));
    };
    let tags = if args.tags.is_empty() {
        payload.tags.clone()
    } else {
        super::memory::util::normalize_tags(args.tags)?
    };
    let copied = AgentNoteV1 {
        note_id: uuid::Uuid::now_v7(),
        title: args.title_override.unwrap_or_else(|| payload.title.clone()),
        body: args.body_override.unwrap_or_else(|| payload.body.clone()),
        tags,
        idempotency_key: None,
    };
    let observed_at = time::OffsetDateTime::now_utc();
    let mut draft = EventDraft::from_payload(
        &to.owner,
        SOURCE_ID,
        SourceBatchId::new(uuid::Uuid::now_v7()),
        &copied,
        observed_at,
    );
    if let Some(author) = ctx.author.personality_instance_id {
        draft = draft.author_personality(author);
    }
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::InvalidInput("engine required".into()))?;
    let embedding_client = engine.embed_client();
    let embedding_model_id = embedding_client.as_ref().map(|client| client.model_id());
    let authorized = engine
        .authorize_event_ingest(&ctx.authz, Role::GraphWrite, draft)
        .map_err(McpToolError::from)?;
    let outcome = engine
        .storage()
        .ingest_event_with_typed_sidecar(
            &authorized,
            &SidecarPayload::fact(copied),
            embedding_model_id,
        )
        .await?;

    Ok(PublishMemoryOutput {
        source: ctx.format_fact_memory(snapshot.memory_id),
        published: ctx.format_fact_memory(outcome.memory_id),
        from_space: from.key,
        to_space: to.key,
    })
}

fn require_memory_action(
    ctx: &McpToolCtx,
    space: &super::memory_spaces::ResolvedMemorySpace,
    action: MemoryAction,
) -> Result<(), McpToolError> {
    if ctx.authz.allows_memory_action(&space.owner, action) {
        return Ok(());
    }
    let action_name = match action {
        MemoryAction::Search => "search",
        MemoryAction::Read => "read",
        MemoryAction::Write => "write",
        MemoryAction::Publish => "publish",
        MemoryAction::Admin => "admin",
    };
    Err(crate::error::ProtocolError::forbidden(format!(
        "requires memory.{action_name} on space {}",
        space.key
    ))
    .into())
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
