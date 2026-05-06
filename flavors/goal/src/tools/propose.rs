use proxima_core::GoalId;
use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util::{
    GoalPayloadInput, insert_goal_in_tx, insert_motivated_by_edges, map_storage, request_id,
    validate_evidence_in_owner,
};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProposeArgs {
    pub payload: GoalPayloadInput,
    #[serde(default)]
    pub evidence: Vec<String>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProposeOutput {
    pub handle: String,
    pub uuid: uuid::Uuid,
    pub edge_uuids: Vec<uuid::Uuid>,
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
            let edge_uuids =
                insert_motivated_by_edges(&mut tx, &ctx, goal_id, &evidence, "ExternalAgent")
                    .await?;
            tx.commit().await.map_err(map_storage)?;

            let handle = ctx.handles.assign_goal(GoalId::new(goal_id));
            Ok(ProposeOutput {
                handle: handle.as_str().to_string(),
                uuid: goal_id,
                edge_uuids,
            })
        })
    }
}
