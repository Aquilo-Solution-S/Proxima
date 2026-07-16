use std::collections::HashSet;

use proxima_core::relation::CORE_INSPIRES_RELATION;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::{EdgeFilter, EdgeReadRequest, QueryRequest};
use proxima_core::{EntityKind, EntityRef, GoalActivatedV1, GoalId, MemoryId, ToolCtx, ToolError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::super::sql::{map_storage, owner_columns};
use super::super::{code_store, engine};

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
    _tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    memory_id: MemoryId,
) -> Result<Uuid, ToolError> {
    let pool = code_store(ctx)?;
    let engine = engine(ctx)?;
    let Some((_, payload)) = pool
        .authorized_fact_payloads::<GoalActivatedV1>(
            &engine,
            ctx.authz(),
            ctx.owner(),
            &[memory_id.into_inner()],
            1,
        )
        .await?
        .into_iter()
        .next()
    else {
        return Err(ToolError::InvalidInput(format!(
            "goal_activated_memory is not visible: {}",
            memory_id.into_inner()
        )));
    };
    Ok(payload.goal_id)
}

pub(super) async fn validate_active_goal_context(
    _tx: &mut Transaction<'_, Postgres>,
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
    let mut current = Some(start);
    let mut seen = HashSet::new();
    for _ in 0..100 {
        let Some(goal_id) = current else {
            return Ok(false);
        };
        if !seen.insert(goal_id) {
            return Ok(false);
        }

        let edges = engine
            .read_edges(
                ctx.authz(),
                &EdgeReadRequest {
                    owner: ctx.owner(),
                    edge_ids: Vec::new(),
                    filter: EdgeFilter {
                        relation: Some(CORE_INSPIRES_RELATION.to_string()),
                        source: Some(EntityRef::Goal(goal_id)),
                        target: Some(EntityRef::Memory(planner_root)),
                    },
                    limit: 1,
                    cursor: None,
                    include_payloads: false,
                },
            )
            .await?;
        if !edges.edges.is_empty() {
            return Ok(true);
        }

        let mut req = QueryRequest::for_owner(ctx.owner());
        req.entity_kind = Some(EntityKind::Goal);
        req.goal_ids = vec![goal_id];
        req.limit = 1;
        req.include_payloads = false;
        current = engine
            .query(ctx.authz(), &req)
            .await?
            .goals
            .into_iter()
            .next()
            .and_then(|goal| goal.supersedes);
    }
    Ok(false)
}

pub(super) async fn validate_plan_source_abstraction_in_owner(
    _tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    memory_id: MemoryId,
) -> Result<(), ToolError> {
    let pool = code_store(ctx)?;
    let engine = engine(ctx)?;
    let visible = pool
        .authorized_memory_ids(
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
    _tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    evidence: &[MemoryId],
) -> Result<(), ToolError> {
    let pool = code_store(ctx)?;
    let engine = engine(ctx)?;
    for memory_id in evidence {
        let visible = pool
            .authorized_memory_ids(
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
