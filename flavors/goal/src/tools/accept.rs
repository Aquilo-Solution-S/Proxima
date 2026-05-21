#![allow(clippy::missing_errors_doc)]

use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState};
use proxima_core::{EdgeAuthorshipKind, EdgeId, FactPayload, GoalId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util::{
    GoalPayloadInput, append_inspires_edge, emit_goal_activated_fact, insert_goal_in_tx,
    insert_motivated_by_edges, load_goal_payload, map_storage, outgoing_motivated_by_evidence,
    request_id, target_personality_root, validate_evidence_in_owner,
};
use crate::payloads::GoalActivatedV1;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AcceptArgs {
    #[schemars(description = "`G...` Goal handle for the Proposed Goal to accept.")]
    pub proposal: String,
    #[schemars(
        description = "Optional replacement typed Goal payload. Omit or null to keep the proposal payload."
    )]
    pub payload: Option<GoalPayloadInput>,
    #[schemars(
        description = "Optional replacement evidence memory handles (`F...` Fact or `A...` Abstraction). Omit or null to copy proposal evidence; use `[]` to clear evidence."
    )]
    pub evidence: Option<Vec<String>>,
    #[schemars(
        description = "Optional `I...` Personality handle to assign the accepted Active Goal to. Omit or null for no new assignment."
    )]
    pub target_personality: Option<String>,
    #[schemars(
        description = "Optional stable idempotency key. Omit or null to derive a fresh request id."
    )]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AcceptOutput {
    pub handle: String,
    pub supersedes: String,
    pub edge_handles: Vec<String>,
    pub inspires_edge_handle: Option<String>,
}

#[derive(Debug)]
pub struct AcceptTool;

impl McpTool for AcceptTool {
    const NAME: &'static str = "proxima-goal/goal_accept";
    const DESCRIPTION: &'static str =
        "Accept a Proposed Goal, optionally overriding payload or evidence.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[GoalActivatedV1::SCHEMA_ID];
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
    let proposal_id = ctx.resolve_goal(&args.proposal)?;
    let supersedes = ctx.format_goal(proposal_id);

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
        insert_motivated_by_edges(&mut tx, &ctx, goal_id, &evidence, EdgeAuthorshipKind::User)
            .await?
    };
    let inspires_edge_id = if state == GoalState::Active {
        match args.target_personality.as_deref() {
            Some(handle) => {
                let target_root = target_personality_root(&mut tx, &ctx, handle).await?;
                Some(
                    append_inspires_edge(
                        &mut tx,
                        &ctx,
                        goal_id,
                        target_root,
                        EdgeAuthorshipKind::User,
                    )
                    .await?,
                )
            }
            None => None,
        }
    } else {
        None
    };
    if state == GoalState::Active {
        emit_goal_activated_fact(
            &mut tx,
            &ctx,
            goal_id,
            &encoded,
            time::OffsetDateTime::now_utc(),
            evidence.len(),
        )
        .await?;
    }
    tx.commit().await.map_err(map_storage)?;

    let edge_handles = edge_uuids
        .into_iter()
        .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id)))
        .collect();
    Ok(AcceptOutput {
        handle: ctx.format_goal(GoalId::new(goal_id)),
        supersedes,
        edge_handles,
        inspires_edge_handle: inspires_edge_id.map(|edge_id| ctx.format_edge(EdgeId::new(edge_id))),
    })
}
