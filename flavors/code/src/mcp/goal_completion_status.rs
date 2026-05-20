use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::{MemoryId, relation::CORE_DERIVED_FROM_RELATION};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::mcp::sql::{map_storage, owner_principal};
use crate::mcp::workspace_review::load_workspace_review;
use crate::payloads::WorkspaceReviewVerdict;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeGoalCompletionStatusArgs {
    #[schemars(
        description = "`F...` memory handle for the proxima-code/workspace-review-v1 Fact used to derive Goal completion status."
    )]
    pub workspace_review_memory: String,
}

#[derive(Debug, Serialize)]
pub struct CodeGoalCompletionStatusOutput {
    pub status: String,
    pub workspace_review_memory: String,
    pub review_verdict: WorkspaceReviewVerdict,
    pub execution_request_memory: String,
    pub originating_goal: Option<GoalStatus>,
    pub child_close: Option<GoalCloseCommand>,
    pub parent: Option<ParentCompletionStatus>,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct GoalStatus {
    pub root_goal: String,
    pub current_goal: String,
    pub title: String,
    pub state: String,
}

#[derive(Debug, Serialize)]
pub struct GoalCloseCommand {
    pub goal: String,
    pub evidence: Vec<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
pub struct ParentCompletionStatus {
    pub parent_goal: GoalStatus,
    pub child_count: u32,
    pub achieved_child_count_after_this_close: u32,
    pub children: Vec<ChildCompletionStatus>,
    pub parent_ready_after_this_child_close: bool,
    pub parent_close: Option<GoalCloseCommand>,
}

#[derive(Debug, Serialize)]
pub struct ChildCompletionStatus {
    pub root_goal: String,
    pub current_goal: String,
    pub title: String,
    pub current_state: String,
    pub state_after_this_review_close: String,
    pub is_originating_goal: bool,
}

#[derive(Debug)]
pub struct CodeGoalCompletionStatusTool;

impl McpTool for CodeGoalCompletionStatusTool {
    const NAME: &'static str = "proxima-code/code_goal_completion_status";
    const DESCRIPTION: &'static str =
        "Read graph-derived Goal completion status for a workspace review.";

    type Args = CodeGoalCompletionStatusArgs;
    type Output = CodeGoalCompletionStatusOutput;

    fn call(
        ctx: McpToolCtx,
        args: CodeGoalCompletionStatusArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeGoalCompletionStatusOutput, McpToolError>>
    {
        Box::pin(async move { goal_completion_status(ctx, args).await })
    }
}

pub async fn goal_completion_status(
    ctx: McpToolCtx,
    args: CodeGoalCompletionStatusArgs,
) -> Result<CodeGoalCompletionStatusOutput, McpToolError> {
    let review_memory = ctx.resolve_fact_memory(&args.workspace_review_memory)?;
    let review_handle = ctx.format_fact_memory(review_memory);
    let mut tx = ctx.pool.begin().await.map_err(map_storage)?;
    let review = load_workspace_review(&mut tx, &ctx, review_memory).await?;
    tx.commit().await.map_err(map_storage)?;

    let request_memory = review.execution_request_memory_id;
    let originating_goal = load_originating_goal(&ctx, request_memory).await?;
    let Some(originating_goal) = originating_goal else {
        return Ok(CodeGoalCompletionStatusOutput {
            status: "skipped".into(),
            workspace_review_memory: review_handle,
            review_verdict: review.payload.verdict,
            execution_request_memory: ctx.format_fact_memory(request_memory),
            originating_goal: None,
            child_close: None,
            parent: None,
            reason: Some("execution request has no originating active Goal".into()),
        });
    };

    let approved = review.payload.verdict == WorkspaceReviewVerdict::Approved;
    let child_close = if approved && originating_goal.state == "Active" {
        Some(close_command(
            &ctx,
            review_memory,
            &originating_goal.current_goal,
            "child",
        ))
    } else {
        None
    };
    let parent =
        load_parent_completion_status(&ctx, review_memory, &originating_goal, approved).await?;
    let status = if child_close.is_some()
        || parent
            .as_ref()
            .and_then(|parent| parent.parent_close.as_ref())
            .is_some()
    {
        "ready"
    } else {
        "skipped"
    };
    let reason = if approved {
        None
    } else {
        Some("workspace review is not approved".into())
    };

    Ok(CodeGoalCompletionStatusOutput {
        status: status.into(),
        workspace_review_memory: review_handle,
        review_verdict: review.payload.verdict,
        execution_request_memory: ctx.format_fact_memory(request_memory),
        originating_goal: Some(originating_goal),
        child_close,
        parent,
        reason,
    })
}

async fn load_originating_goal(
    ctx: &McpToolCtx,
    request_memory: MemoryId,
) -> Result<Option<GoalStatus>, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let row = sqlx::query(
        "WITH RECURSIVE ancestry(memory_id, depth, path) AS (
             SELECT e.target_memory_id, 1, ARRAY[$4::uuid, e.target_memory_id]
               FROM proxima_core.edges e
              WHERE e.owner_principal_kind = $1
                AND e.owner_principal_id = $2
                AND e.relation = $3
                AND e.source_kind = 'Fact'
                AND e.source_memory_id = $4
                AND e.target_kind = 'Fact'
                AND e.target_memory_id IS NOT NULL
             UNION ALL
             SELECT e.target_memory_id, a.depth + 1, a.path || e.target_memory_id
               FROM ancestry a
               JOIN proxima_core.edges e
                 ON e.owner_principal_kind = $1
                AND e.owner_principal_id = $2
                AND e.relation = $3
                AND e.source_kind = 'Fact'
                AND e.source_memory_id = a.memory_id
                AND e.target_kind = 'Fact'
                AND e.target_memory_id IS NOT NULL
              WHERE NOT e.target_memory_id = ANY(a.path)
         ),
         activated AS (
             SELECT g.goal_id
               FROM ancestry a
               JOIN proxima_core.memories m
                 ON m.memory_id = a.memory_id
                AND m.owner_principal_kind = $1
                AND m.owner_principal_id = $2
                AND m.schema_id = 'proxima-goal/goal-activated-v1'
               JOIN proxima_goal.goal_activated_v1 g
                 ON g.memory_id = a.memory_id
              ORDER BY a.depth, a.memory_id DESC
              LIMIT 1
         ),
         goal_lineage(goal_id, depth, path) AS (
             SELECT goal_id, 0, ARRAY[goal_id]
               FROM activated
             UNION ALL
             SELECT child.goal_id, gl.depth + 1, gl.path || child.goal_id
               FROM goal_lineage gl
               JOIN proxima_core.goals child
                 ON child.supersedes = gl.goal_id
                AND child.owner_principal_kind = $1
                AND child.owner_principal_id = $2
              WHERE NOT child.goal_id = ANY(gl.path)
         )
         SELECT a.goal_id AS root_goal_id,
                gh.goal_id AS current_goal_id,
                gh.title,
                gh.state::text AS state
           FROM activated a
           JOIN goal_lineage gl ON true
           JOIN proxima_core.goals gh ON gh.goal_id = gl.goal_id
          ORDER BY gl.depth DESC, gh.created_at DESC
          LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(request_memory.into_inner())
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_storage)?;

    row.map(|row| goal_status_from_row(ctx, row)).transpose()
}

async fn load_parent_completion_status(
    ctx: &McpToolCtx,
    review_memory: MemoryId,
    originating_goal: &GoalStatus,
    review_approved: bool,
) -> Result<Option<ParentCompletionStatus>, McpToolError> {
    let root_goal_id = ctx.resolve_goal(&originating_goal.root_goal)?;
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let parent_root: Option<Uuid> = sqlx::query_scalar(
        "SELECT parent_goal_id
           FROM proxima_core.goal_parents
          WHERE goal_id = $1
          ORDER BY parent_goal_id
          LIMIT 1",
    )
    .bind(root_goal_id.into_inner())
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_storage)?;
    let Some(parent_root) = parent_root else {
        return Ok(None);
    };

    let parent_goal = load_current_goal(ctx, parent_root).await?;
    let child_rows = sqlx::query(
        "WITH RECURSIVE child_roots AS (
             SELECT gp.goal_id AS root_goal_id
               FROM proxima_core.goal_parents gp
               JOIN proxima_core.goals g ON g.goal_id = gp.goal_id
              WHERE gp.parent_goal_id = $3
                AND g.owner_principal_kind = $1
                AND g.owner_principal_id = $2
         ),
         lineage(root_goal_id, goal_id, depth, path) AS (
             SELECT root_goal_id, root_goal_id, 0, ARRAY[root_goal_id]
               FROM child_roots
             UNION ALL
             SELECT l.root_goal_id, child.goal_id, l.depth + 1, l.path || child.goal_id
               FROM lineage l
               JOIN proxima_core.goals child
                 ON child.supersedes = l.goal_id
                AND child.owner_principal_kind = $1
                AND child.owner_principal_id = $2
              WHERE NOT child.goal_id = ANY(l.path)
         ),
         heads AS (
             SELECT DISTINCT ON (l.root_goal_id)
                    l.root_goal_id,
                    g.goal_id AS current_goal_id,
                    g.title,
                    g.state::text AS state,
                    l.depth,
                    g.created_at
               FROM lineage l
               JOIN proxima_core.goals g ON g.goal_id = l.goal_id
              ORDER BY l.root_goal_id, l.depth DESC, g.created_at DESC
         )
         SELECT root_goal_id, current_goal_id, title, state
           FROM heads
          ORDER BY title, root_goal_id",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(parent_root)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_storage)?;

    let mut children = Vec::with_capacity(child_rows.len());
    let mut achieved_after_close = 0_u32;
    for row in child_rows {
        let root_goal: Uuid = row.try_get("root_goal_id").map_err(map_storage)?;
        let current_goal: Uuid = row.try_get("current_goal_id").map_err(map_storage)?;
        let title: String = row.try_get("title").map_err(map_storage)?;
        let current_state: String = row.try_get("state").map_err(map_storage)?;
        let is_originating_goal = root_goal == root_goal_id.into_inner();
        let state_after_this_review_close =
            if is_originating_goal && review_approved && current_state == "Active" {
                "Achieved".to_string()
            } else {
                current_state.clone()
            };
        if state_after_this_review_close == "Achieved" {
            achieved_after_close += 1;
        }
        children.push(ChildCompletionStatus {
            root_goal: ctx.format_goal(proxima_core::GoalId::new(root_goal)),
            current_goal: ctx.format_goal(proxima_core::GoalId::new(current_goal)),
            title,
            current_state,
            state_after_this_review_close,
            is_originating_goal,
        });
    }
    let child_count = u32::try_from(children.len()).unwrap_or(u32::MAX);
    let parent_ready_after_this_child_close = child_count > 0
        && achieved_after_close == child_count
        && parent_goal.state == "Active"
        && review_approved;
    let parent_close = parent_ready_after_this_child_close
        .then(|| close_command(ctx, review_memory, &parent_goal.current_goal, "parent"));

    Ok(Some(ParentCompletionStatus {
        parent_goal,
        child_count,
        achieved_child_count_after_this_close: achieved_after_close,
        children,
        parent_ready_after_this_child_close,
        parent_close,
    }))
}

async fn load_current_goal(ctx: &McpToolCtx, root_goal: Uuid) -> Result<GoalStatus, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let row = sqlx::query(
        "WITH RECURSIVE lineage(goal_id, depth, path) AS (
             SELECT $3::uuid, 0, ARRAY[$3::uuid]
             UNION ALL
             SELECT child.goal_id, l.depth + 1, l.path || child.goal_id
               FROM lineage l
               JOIN proxima_core.goals child
                 ON child.supersedes = l.goal_id
                AND child.owner_principal_kind = $1
                AND child.owner_principal_id = $2
              WHERE NOT child.goal_id = ANY(l.path)
         )
         SELECT $3::uuid AS root_goal_id,
                g.goal_id AS current_goal_id,
                g.title,
                g.state::text AS state
           FROM lineage l
           JOIN proxima_core.goals g ON g.goal_id = l.goal_id
          ORDER BY l.depth DESC, g.created_at DESC
          LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(root_goal)
    .fetch_one(&ctx.pool)
    .await
    .map_err(map_storage)?;
    goal_status_from_row(ctx, row)
}

fn goal_status_from_row(
    ctx: &McpToolCtx,
    row: sqlx::postgres::PgRow,
) -> Result<GoalStatus, McpToolError> {
    let root_goal: Uuid = row.try_get("root_goal_id").map_err(map_storage)?;
    let current_goal: Uuid = row.try_get("current_goal_id").map_err(map_storage)?;
    let title: String = row.try_get("title").map_err(map_storage)?;
    let state: String = row.try_get("state").map_err(map_storage)?;
    Ok(GoalStatus {
        root_goal: ctx.format_goal(proxima_core::GoalId::new(root_goal)),
        current_goal: ctx.format_goal(proxima_core::GoalId::new(current_goal)),
        title,
        state,
    })
}

fn close_command(
    ctx: &McpToolCtx,
    review_memory: MemoryId,
    goal: &str,
    kind: &str,
) -> GoalCloseCommand {
    GoalCloseCommand {
        goal: goal.to_string(),
        evidence: vec![ctx.format_fact_memory(review_memory)],
        idempotency_key: format!("goal-completion-{kind}-{}", review_memory.into_inner()),
    }
}
