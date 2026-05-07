#![allow(clippy::missing_errors_doc)]

use proxima_core::GoalId;
use proxima_core::mcp::{EntityRef, McpTool, McpToolCtx, McpToolError};
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util::{
    GoalPayloadInput, insert_goal_in_tx, insert_motivated_by_edges, load_goal_payload, map_storage,
    outgoing_motivated_by_evidence, request_id, validate_evidence_in_owner,
};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AcceptArgs {
    pub proposal: String,
    pub payload: Option<GoalPayloadInput>,
    pub evidence: Option<Vec<String>>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AcceptOutput {
    pub handle: String,
    pub uuid: uuid::Uuid,
    pub supersedes: uuid::Uuid,
    pub edge_uuids: Vec<uuid::Uuid>,
}

#[derive(Debug)]
pub struct AcceptTool;

impl McpTool for AcceptTool {
    const NAME: &'static str = "proxima-goal/goal_accept";
    const DESCRIPTION: &'static str =
        "Accept a Proposed Goal, optionally overriding payload or evidence.";
    type Args = AcceptArgs;
    type Output = AcceptOutput;

    fn call(
        ctx: McpToolCtx,
        args: AcceptArgs,
    ) -> futures::future::BoxFuture<'static, Result<AcceptOutput, McpToolError>> {
        Box::pin(async move { accept_goal(ctx, args, GoalState::Active).await })
    }
}

pub async fn accept_goal(
    ctx: McpToolCtx,
    args: AcceptArgs,
    state: GoalState,
) -> Result<AcceptOutput, McpToolError> {
    let proposal_id = match ctx
        .handles
        .resolve(&args.proposal)
        .ok_or_else(|| McpToolError::UnknownHandle(args.proposal.clone()))?
    {
        EntityRef::Goal(id) => id,
        EntityRef::Memory(_) | EntityRef::Edge(_) => {
            return Err(McpToolError::InvalidInput(
                "proposal must resolve to a Goal handle".into(),
            ));
        }
    };

    let mut tx = ctx.pool.begin().await.map_err(map_storage)?;
    let payload = match args.payload {
        Some(payload) => payload,
        None => load_goal_payload(&mut tx, proposal_id).await?,
    };
    let encoded = payload.encode(&ctx.registry)?;
    let evidence = match args.evidence {
        Some(evidence) => validate_evidence_in_owner(&mut tx, &ctx, &evidence).await?,
        None => outgoing_motivated_by_evidence(&mut tx, &ctx, proposal_id).await?,
    };
    let draft = GoalDraft {
        owner: ctx.owner.clone(),
        schema_id: encoded.schema_id.clone(),
        schema_version: encoded.schema_version,
        title: encoded.title.clone(),
        text: encoded.text.clone(),
        payload: encoded.bytes.clone(),
        state,
        parent_goal_ids: Vec::new(),
        supersedes_goal_id: Some(proposal_id),
        authorship: GoalAuthorship::User,
        request_id: request_id("goal_accept", args.idempotency_key),
    };
    let goal_id = insert_goal_in_tx(&mut tx, &ctx, &draft, &encoded).await?;
    let edge_uuids = if state == GoalState::Rejected {
        Vec::new()
    } else {
        insert_motivated_by_edges(&mut tx, &ctx, goal_id, &evidence, "User").await?
    };
    tx.commit().await.map_err(map_storage)?;

    let handle = ctx.handles.assign_goal(GoalId::new(goal_id));
    Ok(AcceptOutput {
        handle: handle.as_str().to_string(),
        uuid: goal_id,
        supersedes: proposal_id.into_inner(),
        edge_uuids,
    })
}
