use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::relation::CORE_INSPIRES_RELATION;
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState};
use proxima_core::{EdgeId, GoalId};
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
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
            let inspires_edge_id = match ctx.caller_self_perspective {
                Some(self_memory_id) => {
                    let edge_id = uuid::Uuid::now_v7();
                    let relation = ctx
                        .registry
                        .resolve_relation(CORE_INSPIRES_RELATION)
                        .ok_or_else(|| {
                            McpToolError::Other(format!(
                                "relation {CORE_INSPIRES_RELATION} not registered"
                            ))
                        })?;
                    let self_memory_uuid = self_memory_id.into_inner();
                    let draft = EdgeDraft {
                        edge_id,
                        relation,
                        source_kind: "Goal",
                        source_memory_id: None,
                        source_goal_id: Some(goal_id),
                        target_kind: "Perspective",
                        target_memory_id: Some(self_memory_uuid),
                        target_goal_id: None,
                        authorship_kind: "ExternalAgent",
                        authorship_owner_memory_id: Some(self_memory_uuid),
                        owner: &ctx.owner,
                    };
                    append_edge_in_tx(&mut tx, &draft, None)
                        .await
                        .map_err(McpToolError::Storage)?;
                    Some(edge_id)
                }
                None => None,
            };
            let edge_uuids =
                insert_motivated_by_edges(&mut tx, &ctx, goal_id, &evidence, "ExternalAgent")
                    .await?;
            tx.commit().await.map_err(map_storage)?;

            let handle = ctx.handles.assign_goal(GoalId::new(goal_id));
            let edge_handles = edge_uuids
                .into_iter()
                .map(|edge_id| {
                    ctx.handles
                        .assign_edge(EdgeId::new(edge_id))
                        .as_str()
                        .to_string()
                })
                .collect();
            let inspires_edge_handle = inspires_edge_id.map(|edge_id| {
                ctx.handles
                    .assign_edge(EdgeId::new(edge_id))
                    .as_str()
                    .to_string()
            });
            Ok(ProposeOutput {
                handle: handle.as_str().to_string(),
                edge_handles,
                inspires_edge_handle,
            })
        })
    }
}
