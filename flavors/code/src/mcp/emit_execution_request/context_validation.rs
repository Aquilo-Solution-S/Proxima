use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::QueryRequest;
use proxima_core::{EntityKind, GoalId, MemoryId, ToolCtx, ToolError};
use uuid::Uuid;

use super::super::sql::{map_storage, owner_columns};
use super::super::{code_store, engine};

/// Is this `repo_handle` a repository this owner has?
///
/// Answered against the pool before any transaction exists, which is what
/// turns a bad handle into an argument error instead of a write. It is NOT
/// the guard: an erase of this repository can commit between this answer
/// and the first row the tool writes. The guard is the `code-repo` scope
/// fence, which the Engine takes inside the write transaction and before
/// its handle/`t` locks because every payload these tools write declares
/// the scope — see [`crate::repos::fence`].
pub(super) async fn validate_repo(ctx: &ToolCtx, repo_id: Uuid) -> Result<(), ToolError> {
    let (owner_kind, owner_id) = owner_columns(&ctx.owner());
    let pool = code_store(ctx)?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1
             FROM proxima_code.repos
             WHERE owner_kind = $1
               AND owner_id = $2
               AND repo_id = $3
         )",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(repo_id)
    .fetch_one(pool.pool())
    .await
    .map_err(map_storage)?;
    if !exists {
        return Err(ToolError::InvalidInput(format!(
            "repo not found for owner: {repo_id}"
        )));
    }
    Ok(())
}

pub(super) async fn validate_goal_activated_fact(
    ctx: &ToolCtx,
    memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let engine = engine(ctx)?;
    let visible = proxima::flavor::authorized_memory_ids(
        &engine,
        ctx.authz(),
        ctx.owner(),
        &[memory_id.into_inner()],
        EntityKind::Fact,
        None,
        1,
    )
    .await?;
    if visible.is_empty() {
        return Err(ToolError::InvalidInput(format!(
            "goal_activated_memory is not visible: {}",
            memory_id.into_inner()
        )));
    }
    let planner = ctx
        .caller_self_perspective()
        .ok_or_else(|| ToolError::InvalidInput("caller_self_perspective is required".into()))?;
    let mut req = QueryRequest::for_owner(ctx.owner());
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_state = Some(GoalState::Active);
    req.assignment = Some(planner);
    req.limit = 1;
    let response = engine.query(ctx.authz(), &req).await?;
    response
        .goals
        .into_iter()
        .next()
        .map(|goal| goal.id.into_inner())
        .ok_or_else(|| ToolError::InvalidInput("no Active Goal assigned to caller".into()))
}

pub(super) async fn validate_active_goal_context(
    ctx: &ToolCtx,
    goal_id: Uuid,
    planner_root: MemoryId,
) -> Result<(), ToolError> {
    let engine = engine(ctx)?;
    let mut req = QueryRequest::for_owner(ctx.owner());
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_ids = vec![GoalId::new(goal_id)];
    req.limit = 1;
    let response = engine.query(ctx.authz(), &req).await?;
    let Some(goal) = response.goals.into_iter().next() else {
        return Err(ToolError::InvalidInput(
            "activated goal is not visible".into(),
        ));
    };
    if goal.state != GoalState::Active {
        return Err(ToolError::InvalidInput(
            "activated goal is not Active".into(),
        ));
    }

    if !goal_lineage_assigned_to(ctx, GoalId::new(goal_id), planner_root).await? {
        return Err(ToolError::InvalidInput(
            "activated goal is Active but not assigned to caller Root Perspective".into(),
        ));
    }
    Ok(())
}

async fn goal_lineage_assigned_to(
    ctx: &ToolCtx,
    start: GoalId,
    planner_root: MemoryId,
) -> Result<bool, ToolError> {
    let engine = engine(ctx)?;
    let mut req = QueryRequest::for_owner(ctx.owner());
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_ids = vec![start];
    req.limit = 1;
    let response = engine.query(ctx.authz(), &req).await?;
    Ok(response
        .goals
        .into_iter()
        .next()
        .is_some_and(|goal| goal.assignment == Some(planner_root)))
}

pub(super) async fn validate_plan_source_abstraction_in_owner(
    ctx: &ToolCtx,
    memory_id: MemoryId,
) -> Result<(), ToolError> {
    let engine = engine(ctx)?;
    let visible = proxima::flavor::authorized_memory_ids(
        &engine,
        ctx.authz(),
        ctx.owner(),
        &[memory_id.into_inner()],
        EntityKind::Abstraction,
        None,
        1,
    )
    .await?;
    if visible.is_empty() {
        return Err(ToolError::InvalidInput(format!(
            "plan_source_memory not visible: {}",
            memory_id.into_inner()
        )));
    }
    Ok(())
}

pub(super) async fn validate_evidence_in_owner(
    ctx: &ToolCtx,
    evidence: &[MemoryId],
) -> Result<(), ToolError> {
    let engine = engine(ctx)?;
    for memory_id in evidence {
        let visible = proxima::flavor::authorized_memory_ids(
            &engine,
            ctx.authz(),
            ctx.owner(),
            &[memory_id.into_inner()],
            EntityKind::Fact,
            None,
            1,
        )
        .await?;
        if visible.is_empty() {
            return Err(ToolError::InvalidInput(format!(
                "evidence not visible or not a Fact: {}",
                memory_id.into_inner()
            )));
        }
    }
    Ok(())
}
