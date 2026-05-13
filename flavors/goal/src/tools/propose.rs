use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState};
use proxima_core::{EdgeId, GoalId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util::{
    GoalPayloadInput, append_inspires_edge, emit_goal_proposed_fact, insert_goal_in_tx,
    insert_motivated_by_edges, map_storage, request_id, target_personality_root,
    validate_evidence_in_owner,
};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProposeArgs {
    pub payload: GoalPayloadInput,
    #[serde(default)]
    pub evidence: Vec<String>,
    pub target_personality: Option<String>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProposeOutput {
    pub handle: String,
    pub edge_handles: Vec<String>,
    pub inspires_edge_handle: Option<String>,
}

#[derive(Debug)]
pub struct ProposeTool;

impl McpTool for ProposeTool {
    const NAME: &'static str = "proxima-goal/goal_propose";
    const DESCRIPTION: &'static str =
        "Propose a Goal with Fact or Abstraction evidence for user review.";
    type Args = ProposeArgs;
    type Output = ProposeOutput;

    fn call(
        ctx: McpToolCtx,
        args: ProposeArgs,
    ) -> futures::future::BoxFuture<'static, Result<ProposeOutput, McpToolError>> {
        Box::pin(async move {
            let encoded = args.payload.encode(&ctx.registry)?;
            let mut tx = ctx.pool.begin().await.map_err(map_storage)?;
            let evidence = validate_evidence_in_owner(&mut tx, &ctx, &args.evidence).await?;
            let draft = GoalDraft {
                owner: ctx.owner.clone(),
                schema_id: encoded.schema_id.clone(),
                schema_version: encoded.schema_version,
                title: encoded.title.clone(),
                text: encoded.text.clone(),
                payload: encoded.bytes.clone(),
                state: GoalState::Proposed,
                parent_goal_ids: Vec::new(),
                supersedes_goal_id: None,
                authorship: GoalAuthorship::External,
                request_id: request_id("goal_propose", args.idempotency_key),
            };
            let goal_id = insert_goal_in_tx(&mut tx, &ctx, &draft, &encoded).await?;
            emit_goal_proposed_fact(&mut tx, &ctx, goal_id, &encoded).await?;
            let target_root = match args.target_personality.as_deref() {
                Some(handle) => Some(target_personality_root(&mut tx, &ctx, handle).await?),
                None => ctx.caller_self_perspective,
            };
            let inspires_edge_id = match target_root {
                Some(self_memory_id) => Some(
                    append_inspires_edge(&mut tx, &ctx, goal_id, self_memory_id, "ExternalAgent")
                        .await?,
                ),
                None => None,
            };
            let edge_uuids =
                insert_motivated_by_edges(&mut tx, &ctx, goal_id, &evidence, "ExternalAgent")
                    .await?;
            tx.commit().await.map_err(map_storage)?;

            let edge_handles = edge_uuids
                .into_iter()
                .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id)))
                .collect();
            let inspires_edge_handle = inspires_edge_id
                .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id)));
            Ok(ProposeOutput {
                handle: ctx.format_goal(GoalId::new(goal_id)),
                edge_handles,
                inspires_edge_handle,
            })
        })
    }
}
