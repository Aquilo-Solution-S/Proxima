//! End-to-end MCP Goal tools against transient PG storage.

use std::{future::Future, pin::Pin, sync::Arc};

mod common;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::engine::Engine;
use proxima_core::goal::relations::CORE_MOTIVATED_BY_RELATION;
use proxima_core::mcp::core_tools::goal::{
    ChildGoalInput, GoalDecomposeArgs, GoalMarkAchievedArgs, GoalModifyArgs, GoalPayloadArgs,
    GoalSetArgs, GoalTransition, GoalTransitionArgs, GoalWriteOutput,
};
use proxima_core::mcp::core_tools::{
    GoalDecomposeTool, GoalMarkAchievedTool, GoalModifyTool, GoalSetTool, GoalTransitionTool,
};
use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolCtx, McpToolExtensions, OutputMode};
use proxima_core::{
    AuthPath, AuthzContext, EntityKind, FlavorRegistry, FlavorRegistryFrozen, GoalId, McpTool,
    McpToolError, MemoryId, MemoryOperatorKind, Owner, OwnerPrincipalKind, PersonalityInstanceId,
    PersonalityStatus, Principal,
};
use serde_json::json;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type BoxTestFuture<'a> = Pin<Box<dyn Future<Output = TestResult> + 'a>>;

#[tokio::test]
async fn goal_set_tool_creates_active_goal() -> TestResult {
    with_harness(|harness| {
        Box::pin(async move {
            let first = harness
                .call::<GoalSetTool>(goal_set_args(
                    "Tool-created goal",
                    "Create this goal through the MCP tool.",
                    "goal-set-tool-active",
                ))
                .await?;
            assert_goal_write(&first.handle, first.lifecycle_memory.as_deref());
            assert!(!first.idempotent_replay);

            let goal_id = harness.goal_id(&first.handle)?;
            let (state, authorship_kind, authorship_origin, operator_kind, model_id, prompt, pid) =
                goal_set_authorship_row(harness.pg.pool(), goal_id).await?;
            assert_eq!(state, "Active");
            assert_eq!(authorship_kind, "System");
            assert_eq!(authorship_origin.as_deref(), Some("Operator"));
            assert_eq!(operator_kind.as_deref(), Some("AtoGoal"));
            assert_eq!(model_id.as_deref(), Some("codex-test"));
            assert_eq!(prompt.as_deref(), Some("goal_set"));
            assert_eq!(pid, Some(harness.personality_id.into_inner()));
            assert_eq!(count_goals(harness.pg.pool()).await?, 1);
            assert_eq!(count_goal_activated(harness.pg.pool()).await?, 1);

            let replay = harness
                .call::<GoalSetTool>(goal_set_args(
                    "Tool-created goal",
                    "Create this goal through the MCP tool.",
                    "goal-set-tool-active",
                ))
                .await?;
            assert!(replay.idempotent_replay);
            assert_eq!(replay.handle, first.handle);
            assert_eq!(count_goals(harness.pg.pool()).await?, 1);
            assert_eq!(count_goal_activated(harness.pg.pool()).await?, 1);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn goal_lifecycle_tool_chain() -> TestResult {
    with_harness(|harness| {
        Box::pin(async move {
            let evidence = harness
                .seed_evidence_memory("evidence for achievement")
                .await?;
            let active = harness
                .call::<GoalSetTool>(goal_set_args(
                    "Lifecycle chain goal",
                    "Pause, resume, then achieve this goal.",
                    "goal-lifecycle-set",
                ))
                .await?;
            let paused = harness
                .call::<GoalTransitionTool>(GoalTransitionArgs {
                    goal: active.handle.clone(),
                    transition: GoalTransition::Pause,
                    idempotency_key: Some("goal-lifecycle-pause".into()),
                })
                .await?;
            let resumed = harness
                .call::<GoalTransitionTool>(GoalTransitionArgs {
                    goal: paused.handle.clone(),
                    transition: GoalTransition::Resume,
                    idempotency_key: Some("goal-lifecycle-resume".into()),
                })
                .await?;
            let achieved = harness
                .call::<GoalMarkAchievedTool>(GoalMarkAchievedArgs {
                    goal: resumed.handle.clone(),
                    evidence: vec![evidence.handle.clone()],
                    idempotency_key: Some("goal-lifecycle-achieved".into()),
                })
                .await?;

            let active_id = harness.goal_id(&active.handle)?;
            let paused_id = harness.goal_id(&paused.handle)?;
            let resumed_id = harness.goal_id(&resumed.handle)?;
            let achieved_id = harness.goal_id(&achieved.handle)?;

            assert_goal_state_supersedes(harness.pg.pool(), active_id, "Active", None).await?;
            assert_goal_state_supersedes(harness.pg.pool(), paused_id, "Paused", Some(active_id))
                .await?;
            assert_goal_state_supersedes(harness.pg.pool(), resumed_id, "Active", Some(paused_id))
                .await?;
            assert_goal_state_supersedes(
                harness.pg.pool(),
                achieved_id,
                "Achieved",
                Some(resumed_id),
            )
            .await?;
            assert_eq!(superseding_count(harness.pg.pool(), achieved_id).await?, 0);
            assert_eq!(
                count_goal_achieved_for(harness.pg.pool(), achieved_id).await?,
                1
            );
            assert_eq!(
                count_motivated_by_edge(harness.pg.pool(), achieved_id, evidence.id).await?,
                1,
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn goal_full_lifecycle_accepts_structured_body_payload() -> TestResult {
    with_harness(|harness| {
        Box::pin(async move {
            let evidence = harness
                .seed_evidence_memory("structured body lifecycle evidence")
                .await?;
            let active = harness
                .call::<GoalSetTool>(task_goal_set_args(
                    "Structured lifecycle goal",
                    "Exercise every goal tool with a structured body object.",
                    "High",
                    "structured-lifecycle-set",
                ))
                .await?;
            assert_fresh_goal_write(&active);

            let active_id = harness.goal_id(&active.handle)?;
            let decomposed = harness
                .call::<GoalDecomposeTool>(GoalDecomposeArgs {
                    parent_goal: active.handle.clone(),
                    children: vec![task_child_goal(
                        "Structured lifecycle child",
                        "Child goal with a structured body object.",
                        "Low",
                    )],
                    target_personality: None,
                    idempotency_key: "structured-lifecycle-decompose".into(),
                })
                .await?;
            assert_eq!(decomposed.parent_goal, active.handle);
            assert!(!decomposed.idempotent_replay);
            assert_eq!(decomposed.children.len(), 1);
            let child = decomposed.children.first().expect("one child output");
            assert_fresh_goal_write(child);
            let child_id = harness.goal_id(&child.handle)?;
            assert_eq!(
                count_parent_link(harness.pg.pool(), child_id, active_id).await?,
                1,
            );

            let paused = harness
                .call::<GoalTransitionTool>(GoalTransitionArgs {
                    goal: active.handle.clone(),
                    transition: GoalTransition::Pause,
                    idempotency_key: Some("structured-lifecycle-pause".into()),
                })
                .await?;
            assert_fresh_goal_write(&paused);

            let resumed = harness
                .call::<GoalTransitionTool>(GoalTransitionArgs {
                    goal: paused.handle.clone(),
                    transition: GoalTransition::Resume,
                    idempotency_key: Some("structured-lifecycle-resume".into()),
                })
                .await?;
            assert_fresh_goal_write(&resumed);

            let modified = harness
                .call::<GoalModifyTool>(GoalModifyArgs {
                    goal: resumed.handle.clone(),
                    payload: task_payload(
                        "Structured lifecycle goal modified",
                        "Modified structured body object.",
                        "Medium",
                    ),
                    evidence: Some(vec![evidence.handle.clone()]),
                    idempotency_key: Some("structured-lifecycle-modify".into()),
                })
                .await?;
            assert_fresh_goal_write(&modified);

            let achieved = harness
                .call::<GoalMarkAchievedTool>(GoalMarkAchievedArgs {
                    goal: modified.handle.clone(),
                    evidence: vec![evidence.handle.clone()],
                    idempotency_key: Some("structured-lifecycle-achieved".into()),
                })
                .await?;
            assert_fresh_goal_write(&achieved);

            let paused_id = harness.goal_id(&paused.handle)?;
            let resumed_id = harness.goal_id(&resumed.handle)?;
            let modified_id = harness.goal_id(&modified.handle)?;
            let achieved_id = harness.goal_id(&achieved.handle)?;
            assert_structured_lifecycle_chain(
                harness.pg.pool(),
                active_id,
                paused_id,
                resumed_id,
                modified_id,
                achieved_id,
            )
            .await?;
            assert_eq!(
                task_goal_priority(harness.pg.pool(), achieved_id).await?,
                Some("Medium".to_string())
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn goal_transition_tool_rejects_raw_uuid() -> TestResult {
    with_harness(|harness| {
        Box::pin(async move {
            let err = harness
                .call::<GoalTransitionTool>(GoalTransitionArgs {
                    goal: Uuid::now_v7().to_string(),
                    transition: GoalTransition::Pause,
                    idempotency_key: Some("goal-transition-raw-uuid".into()),
                })
                .await
                .expect_err("raw UUID must not resolve in handle mode");
            match err {
                McpToolError::Resolve(_) => {}
                other => panic!("expected handle resolution error, got {other:?}"),
            }
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn goal_decompose_tool_writes_children() -> TestResult {
    with_harness(|harness| {
        Box::pin(async move {
            let parent = harness
                .call::<GoalSetTool>(goal_set_args(
                    "Parent goal",
                    "Split this parent into children.",
                    "goal-decompose-parent",
                ))
                .await?;
            let parent_id = harness.goal_id(&parent.handle)?;
            let decomposed = harness
                .call::<GoalDecomposeTool>(GoalDecomposeArgs {
                    parent_goal: parent.handle.clone(),
                    children: vec![
                        child_goal("Child one", "First child goal."),
                        child_goal("Child two", "Second child goal."),
                    ],
                    target_personality: None,
                    idempotency_key: "goal-decompose-children".into(),
                })
                .await?;

            assert_eq!(decomposed.parent_goal, parent.handle);
            assert!(!decomposed.idempotent_replay);
            assert_eq!(decomposed.children.len(), 2);
            for child in &decomposed.children {
                assert_goal_write(&child.handle, child.lifecycle_memory.as_deref());
                assert!(!child.idempotent_replay);
                let child_id = harness.goal_id(&child.handle)?;
                assert_eq!(
                    count_parent_link(harness.pg.pool(), child_id, parent_id).await?,
                    1,
                );
            }
            assert_eq!(
                count_children_for_parent(harness.pg.pool(), parent_id).await?,
                2,
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn goal_modify_tool_supersedes_head() -> TestResult {
    with_harness(|harness| {
        Box::pin(async move {
            let prior = harness
                .call::<GoalSetTool>(goal_set_args(
                    "Original title",
                    "Original goal text.",
                    "goal-modify-prior",
                ))
                .await?;
            let modified = harness
                .call::<GoalModifyTool>(GoalModifyArgs {
                    goal: prior.handle.clone(),
                    payload: simple_payload("Modified title", "Modified goal text."),
                    evidence: Some(Vec::new()),
                    idempotency_key: Some("goal-modify-replacement".into()),
                })
                .await?;

            let prior_id = harness.goal_id(&prior.handle)?;
            let modified_id = harness.goal_id(&modified.handle)?;
            assert_ne!(modified_id, prior_id);
            let (state, supersedes, title, text) =
                goal_content_row(harness.pg.pool(), modified_id).await?;
            assert_eq!(state, "Active");
            assert_eq!(supersedes, Some(prior_id.into_inner()));
            assert_eq!(title, "Modified title");
            assert_eq!(text, "Modified goal text.");
            Ok(())
        })
    })
    .await
}

async fn with_harness<F>(test: F) -> TestResult
where
    F: for<'a> FnOnce(&'a ToolHarness) -> BoxTestFuture<'a>,
{
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let harness = ToolHarness::new(pg).await?;
        test(&harness).await
    }
    .await;
    drop_db(&db_name).await?;
    result
}

struct ToolHarness {
    pg: proxima_storage_pg::PgStorage,
    owner: Owner,
    handles: Arc<HandleTable>,
    registry: Arc<FlavorRegistryFrozen>,
    author: McpAuthorContext,
    engine: Arc<Engine>,
    personality_id: PersonalityInstanceId,
}

impl ToolHarness {
    async fn new(pg: proxima_storage_pg::PgStorage) -> Result<Self, Box<dyn std::error::Error>> {
        let owner = nil_owner();
        let registry = Arc::new(FlavorRegistry::new().freeze());
        let handles = Arc::new(HandleTable::new());
        let (personality_id, root_memory_id) = seed_personality_self(&pg, &owner).await?;
        let author = author_ctx()
            .with_personality(personality_id)
            .with_self_perspective(root_memory_id);
        let engine = engine_for_registry(&registry, &pg);
        Ok(Self {
            pg,
            owner,
            handles,
            registry,
            author,
            engine,
            personality_id,
        })
    }

    async fn call<T: McpTool>(&self, args: T::Args) -> Result<T::Output, McpToolError> {
        T::call(self.ctx(), args).await
    }

    async fn seed_evidence_memory(
        &self,
        text: &str,
    ) -> Result<EvidenceHandle, Box<dyn std::error::Error>> {
        let id = seed_fact_memory(&self.pg, &self.owner, text).await?;
        let handle = self.handles.assign_fact_memory(id).as_str().to_string();
        Ok(EvidenceHandle { id, handle })
    }

    fn goal_id(&self, handle: &str) -> Result<GoalId, proxima_core::mcp::ResolveError> {
        self.handles.resolve_goal(handle)
    }

    fn ctx(&self) -> McpToolCtx {
        McpToolCtx {
            owner: self.owner.clone(),
            authz: AuthzContext::single_owner(&self.owner, AuthPath::System),
            handles: Some(self.handles.clone()),
            mode: OutputMode::Handles,
            registry: self.registry.clone(),
            author: self.author.clone(),
            caller_self_perspective: self.author.caller_self_perspective,
            master_token_id: None,
            extensions: McpToolExtensions::with(self.pg.pool().clone()),
            engine: Some(self.engine.clone()),
        }
    }
}

struct EvidenceHandle {
    id: MemoryId,
    handle: String,
}

fn goal_set_args(title: &str, text: &str, idempotency_key: &str) -> GoalSetArgs {
    GoalSetArgs {
        payload: simple_payload(title, text),
        evidence: Vec::new(),
        target_personality: None,
        idempotency_key: Some(idempotency_key.into()),
    }
}

fn task_goal_set_args(
    title: &str,
    text: &str,
    priority: &str,
    idempotency_key: &str,
) -> GoalSetArgs {
    GoalSetArgs {
        payload: task_payload(title, text, priority),
        evidence: Vec::new(),
        target_personality: None,
        idempotency_key: Some(idempotency_key.into()),
    }
}

fn child_goal(title: &str, text: &str) -> ChildGoalInput {
    ChildGoalInput {
        payload: simple_payload(title, text),
        evidence: Vec::new(),
    }
}

fn task_child_goal(title: &str, text: &str, priority: &str) -> ChildGoalInput {
    ChildGoalInput {
        payload: task_payload(title, text, priority),
        evidence: Vec::new(),
    }
}

fn simple_payload(title: &str, text: &str) -> GoalPayloadArgs {
    GoalPayloadArgs {
        schema_id: "core/simple-text-v1".into(),
        schema_version: Some(1),
        title: title.into(),
        text: text.into(),
        body: json!({}),
    }
}

fn task_payload(title: &str, text: &str, priority: &str) -> GoalPayloadArgs {
    GoalPayloadArgs {
        schema_id: "core/task-v1".into(),
        schema_version: Some(1),
        title: title.into(),
        text: text.into(),
        body: json!({
            "due_at": null,
            "priority": priority,
        }),
    }
}

async fn seed_personality_self(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
) -> Result<(PersonalityInstanceId, MemoryId), Box<dyn std::error::Error>> {
    let personality_id = PersonalityInstanceId::new(Uuid::now_v7());
    let root_memory_id = MemoryId::new(Uuid::now_v7());
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, 'test/self-perspective-v1', 1, $4,
                 'goal tool self perspective', $5, 'codex-test', 'self-v1', $6, 0)",
    )
    .bind(root_memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(EntityKind::Perspective)
    .bind(MemoryOperatorKind::AtoP)
    .bind(personality_id.into_inner())
    .execute(pg.pool())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.personality
            (owner_principal_kind, owner_principal_id,
             personality_instance_id, current_root_perspective_memory_id, status)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(personality_id.into_inner())
    .bind(root_memory_id.into_inner())
    .bind(PersonalityStatus::Active)
    .execute(pg.pool())
    .await?;
    Ok((personality_id, root_memory_id))
}

async fn seed_fact_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let memory_id = MemoryId::new(Uuid::now_v7());
    let source_batch_id = Uuid::now_v7();
    let event_id = Uuid::now_v7().as_bytes().to_vec();
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    let now = time::OffsetDateTime::now_utc();
    sqlx::query(
        "INSERT INTO proxima_core.source_batches
            (id, source_id, owner_principal_kind, owner_principal_id)
         VALUES ($1, 'test/goal-tools', $2, $3)",
    )
    .bind(source_batch_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .execute(pg.pool())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.events
            (event_id, source_id, source_batch_id, owner_principal_kind,
             owner_principal_id, schema_id, schema_version,
             observed_at, occurred_at)
         VALUES ($1, 'test/goal-tools', $2, $3, $4,
                 'test/evidence-v1', 1, $5, $5)",
    )
    .bind(&event_id)
    .bind(source_batch_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(now)
    .execute(pg.pool())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id,
             schema_id, schema_version, event_id, personality_instance_id)
         VALUES ($1, $2, $3, 'test/evidence-v1', 1, $4, $5)",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(event_id)
    .bind(Uuid::nil())
    .execute(pg.pool())
    .await?;
    let _ = text;
    Ok(memory_id)
}

fn assert_goal_write(handle: &str, lifecycle_memory: Option<&str>) {
    assert!(
        handle.starts_with('G') && handle.len() > 1,
        "goal output must carry a goal handle, got {handle}"
    );
    let lifecycle = lifecycle_memory.expect("goal output lifecycle memory");
    assert!(
        lifecycle.starts_with('F') && lifecycle.len() > 1,
        "lifecycle output must carry a fact handle, got {lifecycle}"
    );
}

fn assert_fresh_goal_write(output: &GoalWriteOutput) {
    assert_goal_write(&output.handle, output.lifecycle_memory.as_deref());
    assert!(!output.idempotent_replay);
}

async fn count_goals(pool: &sqlx::PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goals")
        .fetch_one(pool)
        .await
}

async fn count_goal_activated(pool: &sqlx::PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goal_activated_v1")
        .fetch_one(pool)
        .await
}

async fn count_goal_achieved_for(pool: &sqlx::PgPool, goal_id: GoalId) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.goal_achieved_v1
          WHERE goal_id = $1",
    )
    .bind(goal_id.into_inner())
    .fetch_one(pool)
    .await
}

async fn task_goal_priority(
    pool: &sqlx::PgPool,
    goal_id: GoalId,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT priority::text
           FROM proxima_core.task_goal_v1
          WHERE goal_id = $1",
    )
    .bind(goal_id.into_inner())
    .fetch_one(pool)
    .await
}

async fn count_motivated_by_edge(
    pool: &sqlx::PgPool,
    goal_id: GoalId,
    memory_id: MemoryId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.edges
          WHERE relation = $1
            AND source_goal_id = $2
            AND target_memory_id = $3",
    )
    .bind(CORE_MOTIVATED_BY_RELATION)
    .bind(goal_id.into_inner())
    .bind(memory_id.into_inner())
    .fetch_one(pool)
    .await
}

async fn count_parent_link(
    pool: &sqlx::PgPool,
    child_id: GoalId,
    parent_id: GoalId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.goal_parents
          WHERE goal_id = $1
            AND parent_goal_id = $2",
    )
    .bind(child_id.into_inner())
    .bind(parent_id.into_inner())
    .fetch_one(pool)
    .await
}

async fn count_children_for_parent(
    pool: &sqlx::PgPool,
    parent_id: GoalId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.goal_parents
          WHERE parent_goal_id = $1",
    )
    .bind(parent_id.into_inner())
    .fetch_one(pool)
    .await
}

async fn superseding_count(pool: &sqlx::PgPool, goal_id: GoalId) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.goals
          WHERE supersedes = $1",
    )
    .bind(goal_id.into_inner())
    .fetch_one(pool)
    .await
}

async fn goal_set_authorship_row(
    pool: &sqlx::PgPool,
    goal_id: GoalId,
) -> Result<
    (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<Uuid>,
    ),
    sqlx::Error,
> {
    sqlx::query_as(
        "SELECT state::text, authorship_kind::text, authorship_origin::text,
                operator_kind::text, model_id, prompt_version, personality_instance_id
           FROM proxima_core.goals
          WHERE goal_id = $1",
    )
    .bind(goal_id.into_inner())
    .fetch_one(pool)
    .await
}

async fn goal_content_row(
    pool: &sqlx::PgPool,
    goal_id: GoalId,
) -> Result<(String, Option<Uuid>, String, String), sqlx::Error> {
    sqlx::query_as(
        "SELECT state::text, supersedes, title, text
           FROM proxima_core.goals
          WHERE goal_id = $1",
    )
    .bind(goal_id.into_inner())
    .fetch_one(pool)
    .await
}

async fn assert_goal_state_supersedes(
    pool: &sqlx::PgPool,
    goal_id: GoalId,
    expected_state: &str,
    expected_supersedes: Option<GoalId>,
) -> Result<(), sqlx::Error> {
    let (state, supersedes): (String, Option<Uuid>) = sqlx::query_as(
        "SELECT state::text, supersedes
           FROM proxima_core.goals
          WHERE goal_id = $1",
    )
    .bind(goal_id.into_inner())
    .fetch_one(pool)
    .await?;
    assert_eq!(state, expected_state);
    assert_eq!(supersedes, expected_supersedes.map(GoalId::into_inner));
    Ok(())
}

async fn assert_structured_lifecycle_chain(
    pool: &sqlx::PgPool,
    active_id: GoalId,
    paused_id: GoalId,
    resumed_id: GoalId,
    modified_id: GoalId,
    achieved_id: GoalId,
) -> Result<(), sqlx::Error> {
    assert_goal_state_supersedes(pool, active_id, "Active", None).await?;
    assert_goal_state_supersedes(pool, paused_id, "Paused", Some(active_id)).await?;
    assert_goal_state_supersedes(pool, resumed_id, "Active", Some(paused_id)).await?;
    assert_goal_state_supersedes(pool, modified_id, "Active", Some(resumed_id)).await?;
    assert_goal_state_supersedes(pool, achieved_id, "Achieved", Some(modified_id)).await?;
    Ok(())
}

fn nil_owner() -> Owner {
    owner_fixture()
}

fn owner_parts(owner: &Owner) -> (OwnerPrincipalKind, Uuid) {
    let kind = OwnerPrincipalKind::of(owner);
    let principal_id = match owner {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    (kind, principal_id)
}

fn author_ctx() -> McpAuthorContext {
    McpAuthorContext {
        model_id: "codex-test".into(),
        client_name: "codex".into(),
        client_version: "1".into(),
        personality_instance_id: None,
        caller_self_perspective: None,
    }
}

trait AuthorCtxExt {
    fn with_self_perspective(self, memory_id: MemoryId) -> Self;
    fn with_personality(self, personality: PersonalityInstanceId) -> Self;
}

impl AuthorCtxExt for McpAuthorContext {
    fn with_self_perspective(mut self, memory_id: MemoryId) -> Self {
        self.caller_self_perspective = Some(memory_id);
        self
    }

    fn with_personality(mut self, personality: PersonalityInstanceId) -> Self {
        self.personality_instance_id = Some(personality);
        self
    }
}

fn engine_for_registry(
    registry: &Arc<FlavorRegistryFrozen>,
    pg: &proxima_storage_pg::PgStorage,
) -> Arc<Engine> {
    Arc::new(Engine::new((**registry).clone()).with_storage(pg.clone().into_handle()))
}
