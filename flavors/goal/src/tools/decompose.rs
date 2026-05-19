#![allow(clippy::missing_errors_doc)]

use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState, SystemOrigin};
use proxima_core::{EdgeAuthorshipKind, EdgeId, GoalId, MemoryId, ToolId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util::{
    GoalPayloadInput, append_inspires_edge, emit_goal_activated_fact, emit_goal_proposed_fact,
    insert_goal_in_tx, insert_motivated_by_edges, map_storage, owner_columns,
    target_personality_root, validate_evidence_in_owner,
};

const MAX_CHILD_GOALS: usize = 50;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DecomposeArgs {
    #[schemars(
        description = "`G...` Goal handle for the current Active parent Goal. A visible `N...` goal_activated Fact memory is also accepted for compatibility."
    )]
    pub parent_goal: String,
    #[schemars(
        description = "Child Goals to create under `parent_goal`. Must contain at least one child and at most 50."
    )]
    pub children: Vec<ChildGoalInput>,
    #[schemars(
        description = "Optional `P...` Personality handle to assign active children to. Omit or null to use the caller Self when `activate_children` is true."
    )]
    pub target_personality: Option<String>,
    #[schemars(
        description = "Whether children should be created Active and assigned, instead of Proposed."
    )]
    pub activate_children: bool,
    #[schemars(
        description = "Stable idempotency key for this decomposition. Reuse only for exact replay."
    )]
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChildGoalInput {
    #[schemars(description = "Typed Goal payload for the child Goal.")]
    pub payload: GoalPayloadInput,
    #[serde(default)]
    #[schemars(
        description = "Optional memory evidence handles (`N...`) motivating this child Goal. Use `[]` unless explicit Fact or Abstraction evidence is required."
    )]
    pub evidence: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DecomposeOutput {
    pub parent_goal: String,
    pub children: Vec<DecomposedChildOutput>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Serialize)]
pub struct DecomposedChildOutput {
    pub handle: String,
    pub lifecycle_memory: Option<String>,
    pub evidence_edge_handles: Vec<String>,
    pub inspires_edge_handle: Option<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug)]
pub struct DecomposeTool;

impl McpTool for DecomposeTool {
    const NAME: &'static str = "proxima-goal/goal_decompose";
    const DESCRIPTION: &'static str =
        "Decompose a current Active Goal into child Goals backed by goal_parents.";

    type Args = DecomposeArgs;
    type Output = DecomposeOutput;

    fn call(
        ctx: McpToolCtx,
        args: DecomposeArgs,
    ) -> futures::future::BoxFuture<'static, Result<DecomposeOutput, McpToolError>> {
        Box::pin(async move { decompose_goal(ctx, args).await })
    }
}

pub async fn decompose_goal(
    ctx: McpToolCtx,
    args: DecomposeArgs,
) -> Result<DecomposeOutput, McpToolError> {
    let mut tx = ctx.pool.begin().await.map_err(map_storage)?;
    let parent_goal_id = resolve_parent_goal_ref(&mut tx, &ctx, &args.parent_goal).await?;
    if args.children.is_empty() {
        return Err(McpToolError::InvalidInput(
            "children must contain at least one child goal".into(),
        ));
    }
    if args.children.len() > MAX_CHILD_GOALS {
        return Err(McpToolError::InvalidInput(format!(
            "children must contain at most {MAX_CHILD_GOALS} child goals"
        )));
    }
    let idempotency_key = args.idempotency_key.trim();
    if idempotency_key.is_empty() || idempotency_key.chars().count() > 180 {
        return Err(McpToolError::InvalidInput(
            "idempotency_key must be 1..=180 chars".into(),
        ));
    }

    validate_current_active_parent(&mut tx, &ctx, parent_goal_id).await?;
    let target_root = if args.activate_children {
        match args.target_personality.as_deref() {
            Some(handle) => Some(target_personality_root(&mut tx, &ctx, handle).await?),
            None => Some(ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput(
                    "activate_children requires target_personality or caller_self_perspective"
                        .into(),
                )
            })?),
        }
    } else {
        None
    };

    let mut children = Vec::with_capacity(args.children.len());
    for (idx, child) in args.children.into_iter().enumerate() {
        let request_id = format!("goal_decompose:{idempotency_key}:{idx}");
        if let Some(existing) =
            existing_child_by_request_id(&mut tx, &ctx, &request_id, parent_goal_id).await?
        {
            children.push(existing);
            continue;
        }

        let encoded = child.payload.encode(&ctx.registry)?;
        let evidence = validate_evidence_in_owner(&mut tx, &ctx, &child.evidence).await?;
        let draft = GoalDraft {
            owner: ctx.owner.clone(),
            schema_id: encoded.schema_id.clone(),
            schema_version: encoded.schema_version,
            title: encoded.title.clone(),
            text: encoded.text.clone(),
            payload: encoded.bytes.clone(),
            state: if args.activate_children {
                GoalState::Active
            } else {
                GoalState::Proposed
            },
            parent_goal_ids: vec![parent_goal_id],
            supersedes_goal_id: None,
            authorship: GoalAuthorship::System(SystemOrigin::Tool {
                tool_id: ToolId::new(DecomposeTool::NAME),
            }),
            request_id,
        };
        let goal_id = insert_goal_in_tx(&mut tx, &ctx, &draft, &encoded).await?;
        let lifecycle_memory = if args.activate_children {
            Some(
                emit_goal_activated_fact(
                    &mut tx,
                    &ctx,
                    goal_id,
                    &encoded,
                    time::OffsetDateTime::now_utc(),
                    evidence.len(),
                )
                .await?,
            )
        } else {
            Some(emit_goal_proposed_fact(&mut tx, &ctx, goal_id, &encoded).await?)
        };
        let evidence_edge_ids = insert_motivated_by_edges(
            &mut tx,
            &ctx,
            goal_id,
            &evidence,
            EdgeAuthorshipKind::ExternalAgent,
        )
        .await?;
        let inspires_edge_id = match target_root {
            Some(root) => Some(
                append_inspires_edge(
                    &mut tx,
                    &ctx,
                    goal_id,
                    root,
                    EdgeAuthorshipKind::ExternalAgent,
                )
                .await?,
            ),
            None => None,
        };
        children.push(DecomposedChildOutput {
            handle: ctx.format_goal(GoalId::new(goal_id)),
            lifecycle_memory: lifecycle_memory.map(|id| ctx.format_memory(id)),
            evidence_edge_handles: evidence_edge_ids
                .into_iter()
                .map(|id| ctx.format_edge(EdgeId::new(id)))
                .collect(),
            inspires_edge_handle: inspires_edge_id.map(|id| ctx.format_edge(EdgeId::new(id))),
            idempotent_replay: false,
        });
    }

    tx.commit().await.map_err(map_storage)?;
    let idempotent_replay = children.iter().all(|child| child.idempotent_replay);
    Ok(DecomposeOutput {
        parent_goal: ctx.format_goal(parent_goal_id),
        children,
        idempotent_replay,
    })
}

async fn resolve_parent_goal_ref(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    value: &str,
) -> Result<GoalId, McpToolError> {
    match ctx.resolve_goal(value) {
        Ok(goal_id) => return Ok(goal_id),
        Err(goal_err) => {
            let memory_id = ctx.resolve_memory(value).map_err(|_| goal_err)?;
            let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&ctx.owner);
            let row: Option<(uuid::Uuid,)> = sqlx::query_as(
                "SELECT g.goal_id
                   FROM proxima_core.memories m
                   JOIN proxima_goal.goal_activated_v1 g USING (memory_id)
                  WHERE m.memory_id = $1
                    AND m.owner_principal_kind = $2
                    AND m.owner_principal_id = $3
                    AND m.owner_org_id = $4
                    AND COALESCE(m.kind, 'Fact'::proxima_core.entity_kind) = 'Fact'
                    AND m.schema_id = 'proxima-goal/goal-activated-v1'",
            )
            .bind(memory_id.into_inner())
            .bind(owner_kind)
            .bind(owner_principal_id)
            .bind(owner_org_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_storage)?;
            row.map(|(goal_id,)| GoalId::new(goal_id)).ok_or_else(|| {
                McpToolError::InvalidInput(format!(
                    "parent_goal must be a Goal or visible goal_activated memory: {value}"
                ))
            })
        }
    }
}

async fn validate_current_active_parent(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    goal_id: GoalId,
) -> Result<(), McpToolError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&ctx.owner);
    let row: Option<(GoalState,)> = sqlx::query_as(
        "SELECT state
           FROM proxima_core.goals
          WHERE goal_id = $1
            AND owner_principal_kind = $2
            AND owner_principal_id = $3
            AND owner_org_id = $4",
    )
    .bind(goal_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_storage)?;
    let Some((state,)) = row else {
        return Err(McpToolError::InvalidInput(format!(
            "parent_goal not found for owner: {}",
            ctx.format_goal(goal_id)
        )));
    };
    if state != GoalState::Active {
        return Err(McpToolError::InvalidInput(format!(
            "parent_goal must be Active, got {state:?}"
        )));
    }
    let newer_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM proxima_core.goals WHERE supersedes = $1)",
    )
    .bind(goal_id.into_inner())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_storage)?;
    if newer_exists {
        return Err(McpToolError::InvalidInput(
            "parent_goal is not the current lineage head".into(),
        ));
    }
    Ok(())
}

async fn existing_child_by_request_id(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    request_id: &str,
    parent_goal_id: GoalId,
) -> Result<Option<DecomposedChildOutput>, McpToolError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&ctx.owner);
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT goal_id
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
    let Some((goal_id,)) = row else {
        return Ok(None);
    };
    let parent_matches: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.goal_parents
              WHERE goal_id = $1
                AND parent_goal_id = $2
         )",
    )
    .bind(goal_id)
    .bind(parent_goal_id.into_inner())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_storage)?;
    if !parent_matches {
        return Err(McpToolError::InvalidInput(format!(
            "idempotency conflict for {request_id}"
        )));
    }

    let lifecycle_memory = latest_child_lifecycle_memory(tx, goal_id).await?;
    let evidence_edge_handles =
        edge_handles_for_goal_relation(tx, ctx, goal_id, crate::relations::MOTIVATED_BY_RELATION)
            .await?;
    let inspires_edge_handle = edge_handles_for_goal_relation(
        tx,
        ctx,
        goal_id,
        proxima_core::relation::CORE_INSPIRES_RELATION,
    )
    .await?
    .into_iter()
    .next();

    Ok(Some(DecomposedChildOutput {
        handle: ctx.format_goal(GoalId::new(goal_id)),
        lifecycle_memory: lifecycle_memory.map(|id| ctx.format_memory(id)),
        evidence_edge_handles,
        inspires_edge_handle,
        idempotent_replay: true,
    }))
}

async fn latest_child_lifecycle_memory(
    tx: &mut sqlx::PgConnection,
    goal_id: uuid::Uuid,
) -> Result<Option<MemoryId>, McpToolError> {
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT memory_id
           FROM proxima_goal.goal_activated_v1
          WHERE goal_id = $1
          UNION ALL
         SELECT memory_id
           FROM proxima_goal.goal_proposed_v1
          WHERE goal_id = $1
          LIMIT 1",
    )
    .bind(goal_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_storage)?;
    Ok(row.map(|(id,)| MemoryId::new(id)))
}

async fn edge_handles_for_goal_relation(
    tx: &mut sqlx::PgConnection,
    ctx: &McpToolCtx,
    goal_id: uuid::Uuid,
    relation: &str,
) -> Result<Vec<String>, McpToolError> {
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT edge_id
           FROM proxima_core.edges
          WHERE source_goal_id = $1
            AND relation = $2
          ORDER BY created_at ASC",
    )
    .bind(goal_id)
    .bind(relation)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_storage)?;
    Ok(rows
        .into_iter()
        .map(|(id,)| ctx.format_edge(EdgeId::new(id)))
        .collect())
}
