use proxima_core::mcp::{EntityRef, McpTool, McpToolCtx, McpToolError};
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState, SystemOrigin};
use proxima_core::{EdgeId, GoalId, MemoryId, ToolId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util::{
    EvidenceRef, append_lifecycle_derived_from_edges, emit_goal_achieved_fact, insert_goal_in_tx,
    insert_motivated_by_edges, load_goal_payload, map_storage, owner_columns, request_id,
};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MarkAchievedArgs {
    pub goal: String,
    pub evidence: Vec<String>,
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
    let supersedes_handle = ctx.handles.as_ref().unwrap().assign_goal(prior_goal_id);
    let request_id = request_id("goal_mark_achieved", args.idempotency_key);

    let mut tx = ctx.pool.begin().await.map_err(map_storage)?;
    let evidence = resolve_evidence_refs(&mut tx, &ctx, &args.evidence).await?;

    if let Some(existing) =
        existing_achieved_goal(&mut tx, &ctx, &request_id, prior_goal_id).await?
    {
        tx.commit().await.map_err(map_storage)?;
        let handle = ctx.handles.as_ref().unwrap().assign_goal(GoalId::new(existing));
        return Ok(MarkAchievedOutput {
            status: MarkAchievedStatus::IdempotentReplay,
            handle: Some(handle.as_str().to_string()),
            supersedes: supersedes_handle.as_str().to_string(),
            lifecycle_memory: None,
            evidence_edge_handles: Vec::new(),
            derived_edge_handles: Vec::new(),
            reason: None,
        });
    }

    let Some(state) = load_goal_state(&mut tx, &ctx, prior_goal_id).await? else {
        return Err(McpToolError::UnknownHandle(args.goal));
    };
    if state != "Active" {
        tx.commit().await.map_err(map_storage)?;
        return Ok(skipped_output(
            supersedes_handle.as_str(),
            format!("goal head is not Active: {state}"),
        ));
    }
    if has_newer_goal(&mut tx, &ctx, prior_goal_id).await? {
        tx.commit().await.map_err(map_storage)?;
        return Ok(skipped_output(
            supersedes_handle.as_str(),
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
    let evidence_edge_ids =
        insert_motivated_by_edges(&mut tx, &ctx, achieved_id, &evidence, "Engine").await?;
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

    let handle = ctx.handles.as_ref().unwrap().assign_goal(GoalId::new(achieved_id));
    Ok(MarkAchievedOutput {
        status: MarkAchievedStatus::Achieved,
        handle: Some(handle.as_str().to_string()),
        supersedes: supersedes_handle.as_str().to_string(),
        lifecycle_memory: Some(lifecycle_memory.into_inner().to_string()),
        evidence_edge_handles: evidence_edge_ids
            .into_iter()
            .map(|edge_id| {
                ctx.handles.as_ref().unwrap()
                    .assign_edge(EdgeId::new(edge_id))
                    .as_str()
                    .to_string()
            })
            .collect(),
        derived_edge_handles: derived_edge_ids
            .into_iter()
            .map(|edge_id| {
                ctx.handles.as_ref().unwrap()
                    .assign_edge(EdgeId::new(edge_id))
                    .as_str()
                    .to_string()
            })
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
    if let Some(entity) = ctx.handles.as_ref().unwrap().resolve(value) {
        return match entity {
            EntityRef::Goal(id) => Ok(id),
            EntityRef::Memory(_)
            | EntityRef::Edge(_)
            | EntityRef::FlavorObject { .. }
            | EntityRef::Personality(_)
            | EntityRef::WakeEntry(_) => Err(McpToolError::InvalidInput(
                "goal must resolve to a Goal handle".into(),
            )),
        };
    }
    uuid::Uuid::parse_str(value)
        .map(GoalId::new)
        .map_err(|_| McpToolError::UnknownHandle(value.to_string()))
}

async fn resolve_evidence_refs(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    evidence: &[String],
) -> Result<Vec<EvidenceRef>, McpToolError> {
    let mut out = Vec::with_capacity(evidence.len());
    for value in evidence {
        let memory_id = match ctx.handles.as_ref().unwrap().resolve(value) {
            Some(EntityRef::Memory(memory_id)) => memory_id,
            Some(_) => {
                return Err(McpToolError::InvalidInput(format!(
                    "evidence {value} must resolve to a Memory handle"
                )));
            }
            None => uuid::Uuid::parse_str(value)
                .map(MemoryId::new)
                .map_err(|_| McpToolError::UnknownHandle(value.clone()))?,
        };
        out.push(load_evidence_ref(tx, ctx, memory_id, value).await?);
    }
    Ok(out)
}

async fn load_evidence_ref(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    memory_id: MemoryId,
    original_ref: &str,
) -> Result<EvidenceRef, McpToolError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(&ctx.owner);
    let row: Option<(String, String, uuid::Uuid)> = sqlx::query_as(
        "SELECT COALESCE(kind, 'Fact') AS kind, owner_principal_kind, owner_principal_id
         FROM proxima_core.memories
         WHERE memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_storage)?;
    let Some((kind, row_owner_kind, row_owner_principal_id)) = row else {
        return Err(McpToolError::UnknownHandle(original_ref.to_string()));
    };
    if row_owner_kind != owner_kind || row_owner_principal_id != owner_principal_id {
        return Err(McpToolError::LayeringViolation(format!(
            "evidence {original_ref} crosses Owner boundary"
        )));
    }
    let target_kind = match kind.as_str() {
        "Fact" => "Fact",
        "Abstraction" => "Abstraction",
        _ => {
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
    let row: Option<(uuid::Uuid, String, Option<uuid::Uuid>)> = sqlx::query_as(
        "SELECT goal_id, state, supersedes
         FROM proxima_core.goals
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND request_id = $4",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(request_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_storage)?;
    match row {
        Some((goal_id, state, supersedes))
            if state == "Achieved" && supersedes == Some(prior_goal_id.into_inner()) =>
        {
            Ok(Some(goal_id))
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
) -> Result<Option<String>, McpToolError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(&ctx.owner);
    sqlx::query_scalar(
        "SELECT state
         FROM proxima_core.goals
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND goal_id = $3",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(goal_id.into_inner())
    .fetch_optional(tx)
    .await
    .map_err(map_storage)
}

async fn has_newer_goal(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    goal_id: GoalId,
) -> Result<bool, McpToolError> {
    let (owner_kind, owner_principal_id, _owner_org_id) = owner_columns(&ctx.owner);
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
             FROM proxima_core.goals newer
             WHERE newer.owner_principal_kind = $1
               AND newer.owner_principal_id = $2
               AND newer.supersedes = $3
         )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(goal_id.into_inner())
    .fetch_one(tx)
    .await
    .map_err(map_storage)
}
