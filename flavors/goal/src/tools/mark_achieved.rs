use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState, SystemOrigin};
use proxima_core::{
    EdgeAuthorshipKind, EdgeId, EntityKind, FactPayload, GoalId, MemoryId, OwnerPrincipalKind,
    ToolId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util::{
    EvidenceRef, append_lifecycle_derived_from_edges, emit_goal_achieved_fact, insert_goal_in_tx,
    insert_motivated_by_edges, load_goal_payload, map_storage, owner_columns, request_id,
};
use crate::payloads::GoalAchievedV1;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MarkAchievedArgs {
    #[schemars(
        description = "`G...` Goal handle for the current Active Goal head to mark Achieved."
    )]
    pub goal: String,
    #[schemars(
        description = "Required `F...` Fact or `A...` Abstraction memory evidence handles supporting achievement."
    )]
    pub evidence: Vec<String>,
    #[schemars(
        description = "Optional stable idempotency key. Omit or null to derive a fresh request id."
    )]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MarkAchievedOutput {
    pub status: MarkAchievedStatus,
    pub handle: Option<String>,
    pub supersedes: String,
    pub lifecycle_memory: Option<String>,
    pub evidence_edge_handles: Vec<String>,
    pub derived_edge_handles: Vec<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkAchievedStatus {
    Achieved,
    IdempotentReplay,
    Skipped,
}

#[derive(Debug)]
pub struct MarkAchievedTool;

impl McpTool for MarkAchievedTool {
    const NAME: &'static str = "proxima-goal/goal_mark_achieved";
    const DESCRIPTION: &'static str =
        "Mark the current Active Goal head as Achieved using supplied evidence.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[GoalAchievedV1::SCHEMA_ID];
    type Args = MarkAchievedArgs;
    type Output = MarkAchievedOutput;

    fn call(
        ctx: McpToolCtx,
        args: MarkAchievedArgs,
    ) -> futures::future::BoxFuture<'static, Result<MarkAchievedOutput, McpToolError>> {
        Box::pin(async move { mark_achieved(ctx, args).await })
    }
}

/// Mark a goal as achieved.
///
/// # Errors
///
/// Returns an error if goal resolution fails, evidence resolution fails, or storage operations fail.
pub async fn mark_achieved(
    ctx: McpToolCtx,
    args: MarkAchievedArgs,
) -> Result<MarkAchievedOutput, McpToolError> {
    let prior_goal_id = resolve_goal_ref(&ctx, &args.goal)?;
    let supersedes = ctx.format_goal(prior_goal_id);
    let request_id = request_id("goal_mark_achieved", args.idempotency_key);

    let mut tx = ctx.pool.begin().await.map_err(map_storage)?;
    let evidence = resolve_evidence_refs(&mut tx, &ctx, &args.evidence).await?;

    if let Some(existing) =
        existing_achieved_goal(&mut tx, &ctx, &request_id, prior_goal_id).await?
    {
        tx.commit().await.map_err(map_storage)?;
        return Ok(MarkAchievedOutput {
            status: MarkAchievedStatus::IdempotentReplay,
            handle: Some(ctx.format_goal(GoalId::new(existing))),
            supersedes,
            lifecycle_memory: None,
            evidence_edge_handles: Vec::new(),
            derived_edge_handles: Vec::new(),
            reason: None,
        });
    }

    let Some(state): Option<GoalState> = load_goal_state(&mut tx, &ctx, prior_goal_id).await?
    else {
        return Err(McpToolError::InvalidInput(format!(
            "goal not found for owner: {}",
            args.goal
        )));
    };
    if state != GoalState::Active {
        tx.commit().await.map_err(map_storage)?;
        return Ok(skipped_output(
            &supersedes,
            format!("goal head is not Active: {state:?}"),
        ));
    }
    if has_newer_goal(&mut tx, &ctx, prior_goal_id).await? {
        tx.commit().await.map_err(map_storage)?;
        return Ok(skipped_output(
            &supersedes,
            "goal is not the current lineage head",
        ));
    }

    let payload = load_goal_payload(&mut tx, prior_goal_id).await?;
    let encoded = payload.encode(&ctx.registry)?;
    let draft = GoalDraft {
        owner: ctx.owner.clone(),
        schema_id: encoded.schema_id.clone(),
        schema_version: encoded.schema_version,
        title: encoded.title.clone(),
        text: encoded.text.clone(),
        payload: encoded.bytes.clone(),
        state: GoalState::Achieved,
        parent_goal_ids: Vec::new(),
        supersedes_goal_id: Some(prior_goal_id),
        authorship: GoalAuthorship::System(SystemOrigin::Tool {
            tool_id: ToolId::new(MarkAchievedTool::NAME),
        }),
        request_id,
    };
    let achieved_id = insert_goal_in_tx(&mut tx, &ctx, &draft, &encoded).await?;
    let evidence_edge_ids = insert_motivated_by_edges(
        &mut tx,
        &ctx,
        achieved_id,
        &evidence,
        EdgeAuthorshipKind::Engine,
    )
    .await?;
    let lifecycle_memory = emit_goal_achieved_fact(
        &mut tx,
        &ctx,
        achieved_id,
        &encoded,
        time::OffsetDateTime::now_utc(),
        evidence.len(),
    )
    .await?;
    let derived_edge_ids =
        append_lifecycle_derived_from_edges(&mut tx, &ctx, lifecycle_memory, &evidence).await?;
    tx.commit().await.map_err(map_storage)?;

    Ok(MarkAchievedOutput {
        status: MarkAchievedStatus::Achieved,
        handle: Some(ctx.format_goal(GoalId::new(achieved_id))),
        supersedes,
        lifecycle_memory: Some(ctx.format_fact_memory(lifecycle_memory)),
        evidence_edge_handles: evidence_edge_ids
            .into_iter()
            .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id)))
            .collect(),
        derived_edge_handles: derived_edge_ids
            .into_iter()
            .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id)))
            .collect(),
        reason: None,
    })
}

fn skipped_output(supersedes: &str, reason: impl Into<String>) -> MarkAchievedOutput {
    MarkAchievedOutput {
        status: MarkAchievedStatus::Skipped,
        handle: None,
        supersedes: supersedes.to_string(),
        lifecycle_memory: None,
        evidence_edge_handles: Vec::new(),
        derived_edge_handles: Vec::new(),
        reason: Some(reason.into()),
    }
}

fn resolve_goal_ref(ctx: &McpToolCtx, value: &str) -> Result<GoalId, McpToolError> {
    match ctx.resolve_goal(value) {
        Ok(goal_id) => Ok(goal_id),
        Err(resolve_err) => value
            .parse::<uuid::Uuid>()
            .map(GoalId::new)
            .map_err(|_| resolve_err),
    }
}

async fn resolve_evidence_refs(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    evidence: &[String],
) -> Result<Vec<EvidenceRef>, McpToolError> {
    let mut out = Vec::with_capacity(evidence.len());
    for value in evidence {
        let memory_id = resolve_memory_ref(ctx, value)?;
        out.push(load_evidence_ref(tx, ctx, memory_id, value).await?);
    }
    Ok(out)
}

fn resolve_memory_ref(ctx: &McpToolCtx, value: &str) -> Result<MemoryId, McpToolError> {
    match ctx.resolve_memory(value) {
        Ok(memory_id) => Ok(memory_id),
        Err(resolve_err) => value
            .parse::<uuid::Uuid>()
            .map(MemoryId::new)
            .map_err(|_| resolve_err),
    }
}

async fn load_evidence_ref(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    memory_id: MemoryId,
    original_ref: &str,
) -> Result<EvidenceRef, McpToolError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(&ctx.owner);
    let row = sqlx::query!(
        r#"SELECT kind AS "kind: EntityKind",
                  owner_principal_kind AS "owner_principal_kind: OwnerPrincipalKind",
                  owner_principal_id
             FROM proxima_core.memories
             WHERE memory_id = $1"#,
        memory_id.into_inner(),
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_storage)?;
    let Some(row) = row else {
        return Err(McpToolError::InvalidInput(format!(
            "evidence not found for owner: {original_ref}"
        )));
    };
    if row.owner_principal_kind != owner_kind || row.owner_principal_id != owner_principal_id {
        return Err(McpToolError::LayeringViolation(format!(
            "evidence {original_ref} crosses Owner boundary"
        )));
    }
    let target_kind = match row.kind {
        Some(EntityKind::Abstraction) => EntityKind::Abstraction,
        // NULL kind on memories indicates a Fact (variant check enforces invariant).
        None => EntityKind::Fact,
        Some(_) => {
            return Err(McpToolError::LayeringViolation(format!(
                "evidence {original_ref} must be Fact or Abstraction"
            )));
        }
    };
    Ok(EvidenceRef {
        handle: original_ref.to_string(),
        target_kind,
        target_memory_id: Some(memory_id.into_inner()),
        target_goal_id: None,
    })
}

async fn existing_achieved_goal(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    request_id: &str,
    prior_goal_id: GoalId,
) -> Result<Option<uuid::Uuid>, McpToolError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&ctx.owner);
    let row = sqlx::query!(
        r#"SELECT goal_id,
                  state AS "state: GoalState",
                  supersedes
             FROM proxima_core.goals
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND owner_org_id = $3
               AND request_id = $4"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        request_id,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_storage)?;
    match row {
        Some(row)
            if row.state == GoalState::Achieved
                && row.supersedes == Some(prior_goal_id.into_inner()) =>
        {
            Ok(Some(row.goal_id))
        }
        Some(_) => Err(McpToolError::InvalidInput(format!(
            "idempotency conflict for {request_id}"
        ))),
        None => Ok(None),
    }
}

async fn load_goal_state(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    goal_id: GoalId,
) -> Result<Option<GoalState>, McpToolError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(&ctx.owner);
    sqlx::query_scalar!(
        r#"SELECT state AS "state: GoalState"
             FROM proxima_core.goals
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND goal_id = $3"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        goal_id.into_inner(),
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_storage)
}

async fn has_newer_goal(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    goal_id: GoalId,
) -> Result<bool, McpToolError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(&ctx.owner);
    sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1
             FROM proxima_core.goals newer
             WHERE newer.owner_principal_kind = $1
               AND newer.owner_principal_id = $2
               AND newer.supersedes = $3
         ) AS "exists!""#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        goal_id.into_inner(),
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(map_storage)
}
