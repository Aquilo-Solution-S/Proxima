use crate::common::{drop_db, fresh_pg, seed_memory};

use proxima_core::storage_ports::GoalWakeCandidatePort;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::{
    EntityKind, GoalId, GoalWakeCandidateRequest, MemoryId, Owner, OwnerRef, OwnerRefKind,
    SchemaId, SchemaVersion, ToolScope, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

const TEST_FACT_SCHEMA: &str = "test/edge-access-fact-v1";

fn owner_parts(owner: &Owner) -> (OwnerRefKind, Option<Uuid>) {
    owner.columns()
}

async fn insert_goal(
    pg: &PgStorage,
    owner: &Owner,
    state: GoalState,
    supersedes: Option<GoalId>,
) -> Result<GoalId, sqlx::Error> {
    let (owner_kind, owner_id) = owner_parts(owner);
    let goal_id = Uuid::now_v7();
    let request_id = format!("wake-candidate:{}", Uuid::now_v7());
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, owner_kind, owner_id, schema_id, schema_version,
             title, text, payload, state, supersedes, authorship_kind,
             request_id, idempotency_key)
         VALUES ($1, $2, $3, 'core/simple-text-v1', 1,
                 'wake goal', 'wake goal', convert_to('{}', 'UTF8'), $4, $5,
                 'User', $6, md5($2::text || ':' || $3::text || ':' || $6))",
    )
    .bind(goal_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(state)
    .bind(supersedes.map(GoalId::into_inner))
    .bind(request_id)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(GoalId::new(goal_id))
}

async fn arm_goal_for_schema(
    pg: &PgStorage,
    goal_id: GoalId,
    tool_ids: &[&str],
    hard_memory_ids: &[MemoryId],
) -> Result<(), sqlx::Error> {
    let tools = tool_ids
        .iter()
        .map(|tool| (*tool).to_string())
        .collect::<Vec<_>>();
    let hard = hard_memory_ids
        .iter()
        .map(|memory_id| memory_id.into_inner())
        .collect::<Vec<_>>();
    sqlx::query(
        "INSERT INTO proxima_core.goal_wake_config
            (goal_id, trigger_kind, trigger_schema_id, trigger_schema_version,
             trigger_memory_id, tool_ids, prompt, hard_memory_ids)
         VALUES ($1, 'fact_schema', $2, 1, NULL, $3, 'plan only', $4)",
    )
    .bind(goal_id.into_inner())
    .bind(TEST_FACT_SCHEMA)
    .bind(tools)
    .bind(hard)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

async fn tombstone_memory(pg: &PgStorage, memory_id: MemoryId) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE proxima_core.memories SET tombstoned_at = now() WHERE memory_id = $1")
        .bind(memory_id.into_inner())
        .execute(pg.pool_for_tests())
        .await?;
    Ok(())
}

async fn wake_candidates(
    pg: &PgStorage,
    actor_read_owners: &[OwnerRef],
    actor_write_owners: &[OwnerRef],
    trigger_owner: OwnerRef,
    trigger_fact_id: MemoryId,
    actor_tool_scope: &ToolScope,
    deployment_tool_scope: &ToolScope,
) -> Result<Vec<proxima_core::GoalWakeCandidate>, proxima_core::StorageError> {
    let trigger_schema_id = SchemaId::new(TEST_FACT_SCHEMA.into());
    pg.list_goal_wake_candidates(&GoalWakeCandidateRequest {
        actor_read_owners,
        actor_write_owners,
        trigger_owner,
        trigger_fact_id,
        trigger_schema_id: &trigger_schema_id,
        trigger_schema_version: SchemaVersion::new(1),
        actor_tool_scope,
        deployment_tool_scope,
        limit: 50,
    })
    .await
}

#[tokio::test]
async fn wake_actor_goal_write_not_required_for_admission() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let goal_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let trigger = seed_memory(&pg, &goal_owner, EntityKind::Fact, "trigger").await?;
        let goal = insert_goal(&pg, &goal_owner, GoalState::Active, None).await?;
        arm_goal_for_schema(&pg, goal, &["core_search_memories"], &[]).await?;

        let actor_scope = ToolScope::Palette(vec!["core_search_memories".to_string()]);
        let deployment_scope = ToolScope::All;
        let candidates = wake_candidates(
            &pg,
            &[goal_owner],
            &[],
            goal_owner,
            trigger,
            &actor_scope,
            &deployment_scope,
        )
        .await?;

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].goal_id, goal);
        assert!(candidates[0].actor_write_owners.is_empty());
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn wake_actor_must_have_goal_owner_grant() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let goal_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let trigger_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let trigger = seed_memory(&pg, &trigger_owner, EntityKind::Fact, "trigger").await?;
        let goal = insert_goal(&pg, &goal_owner, GoalState::Active, None).await?;
        arm_goal_for_schema(&pg, goal, &["core_search_memories"], &[]).await?;

        let actor_scope = ToolScope::All;
        let candidates = wake_candidates(
            &pg,
            &[trigger_owner],
            &[trigger_owner],
            trigger_owner,
            trigger,
            &actor_scope,
            &actor_scope,
        )
        .await?;

        assert!(candidates.is_empty());
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn w3_wake_trigger_actual_owner_read() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let goal_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let trigger_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let trigger =
            seed_memory(&pg, &trigger_owner, EntityKind::Fact, "cross-owner trigger").await?;
        let goal = insert_goal(&pg, &goal_owner, GoalState::Active, None).await?;
        arm_goal_for_schema(&pg, goal, &["core_search_memories"], &[]).await?;

        let actor_scope = ToolScope::All;
        let without_trigger_owner = wake_candidates(
            &pg,
            &[goal_owner],
            &[goal_owner],
            trigger_owner,
            trigger,
            &actor_scope,
            &actor_scope,
        )
        .await?;
        assert!(without_trigger_owner.is_empty());

        let with_trigger_owner = wake_candidates(
            &pg,
            &[goal_owner, trigger_owner],
            &[goal_owner],
            trigger_owner,
            trigger,
            &actor_scope,
            &actor_scope,
        )
        .await?;
        assert_eq!(with_trigger_owner.len(), 1);

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn w3_wake_trigger_and_context_must_be_readable() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let goal_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let hard_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let trigger = seed_memory(&pg, &goal_owner, EntityKind::Fact, "trigger").await?;
        let hard_memory = seed_memory(&pg, &hard_owner, EntityKind::Fact, "hard memory").await?;
        let goal = insert_goal(&pg, &goal_owner, GoalState::Active, None).await?;
        arm_goal_for_schema(&pg, goal, &["core_search_memories"], &[hard_memory]).await?;

        let actor_scope = ToolScope::All;
        let unreadable_context = wake_candidates(
            &pg,
            &[goal_owner],
            &[goal_owner],
            goal_owner,
            trigger,
            &actor_scope,
            &actor_scope,
        )
        .await?;
        assert!(unreadable_context.is_empty());

        let readable_context = wake_candidates(
            &pg,
            &[goal_owner, hard_owner],
            &[goal_owner],
            goal_owner,
            trigger,
            &actor_scope,
            &actor_scope,
        )
        .await?;
        assert_eq!(readable_context.len(), 1);
        assert_eq!(readable_context[0].hard_memory_ids, vec![hard_memory]);

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn tombstoned_wake_trigger_fact_is_not_admitted() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let trigger = seed_memory(&pg, &owner, EntityKind::Fact, "trigger").await?;
        let goal = insert_goal(&pg, &owner, GoalState::Active, None).await?;
        arm_goal_for_schema(&pg, goal, &["core_search_memories"], &[]).await?;
        tombstone_memory(&pg, trigger).await?;

        let actor_scope = ToolScope::All;
        let candidates = wake_candidates(
            &pg,
            &[owner],
            &[owner],
            owner,
            trigger,
            &actor_scope,
            &actor_scope,
        )
        .await?;
        assert!(candidates.is_empty());

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn tombstoned_wake_hard_memory_is_not_admitted() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let trigger = seed_memory(&pg, &owner, EntityKind::Fact, "trigger").await?;
        let hard_memory = seed_memory(&pg, &owner, EntityKind::Fact, "hard memory").await?;
        let goal = insert_goal(&pg, &owner, GoalState::Active, None).await?;
        arm_goal_for_schema(&pg, goal, &["core_search_memories"], &[hard_memory]).await?;
        tombstone_memory(&pg, hard_memory).await?;

        let actor_scope = ToolScope::All;
        let candidates = wake_candidates(
            &pg,
            &[owner],
            &[owner],
            owner,
            trigger,
            &actor_scope,
            &actor_scope,
        )
        .await?;
        assert!(candidates.is_empty());

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn active_goal_only() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let trigger = seed_memory(&pg, &owner, EntityKind::Fact, "trigger").await?;
        let passive = insert_goal(&pg, &owner, GoalState::Active, None).await?;
        let prior = insert_goal(&pg, &owner, GoalState::Active, None).await?;
        let paused = insert_goal(&pg, &owner, GoalState::Paused, Some(prior)).await?;
        arm_goal_for_schema(&pg, prior, &["core_search_memories"], &[]).await?;
        arm_goal_for_schema(&pg, paused, &["core_search_memories"], &[]).await?;

        let actor_scope = ToolScope::All;
        let candidates = wake_candidates(
            &pg,
            &[owner],
            &[owner],
            owner,
            trigger,
            &actor_scope,
            &actor_scope,
        )
        .await?;

        assert!(candidates.is_empty());
        let config_rows: (i64,) =
            sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goal_wake_config")
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(config_rows.0, 2);
        assert_ne!(passive, paused);

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn w6_wake_toolset_cannot_bypass_actor_or_deployment_scope()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let trigger = seed_memory(&pg, &owner, EntityKind::Fact, "trigger").await?;
        let goal = insert_goal(&pg, &owner, GoalState::Active, None).await?;
        arm_goal_for_schema(&pg, goal, &["core_goal:set"], &[]).await?;

        let leaf_scope = ToolScope::Palette(vec!["core_goal:set".to_string()]);
        let grouped_only = ToolScope::Palette(vec!["core_goal".to_string()]);
        let denied = ToolScope::Palette(vec!["core_search_memories".to_string()]);

        let admitted = wake_candidates(
            &pg,
            &[owner],
            &[owner],
            owner,
            trigger,
            &leaf_scope,
            &leaf_scope,
        )
        .await?;
        assert_eq!(admitted.len(), 1);

        let actor_group_only = wake_candidates(
            &pg,
            &[owner],
            &[owner],
            owner,
            trigger,
            &grouped_only,
            &leaf_scope,
        )
        .await?;
        assert!(actor_group_only.is_empty());

        let deployment_denied = wake_candidates(
            &pg,
            &[owner],
            &[owner],
            owner,
            trigger,
            &leaf_scope,
            &denied,
        )
        .await?;
        assert!(deployment_denied.is_empty());

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn w1_wake_no_executor_outputs() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let trigger = seed_memory(&pg, &owner, EntityKind::Fact, "trigger").await?;
        let goal = insert_goal(&pg, &owner, GoalState::Active, None).await?;
        arm_goal_for_schema(&pg, goal, &["core_search_memories"], &[]).await?;

        let before: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT count(*)::bigint FROM proxima_core.memories),
                (SELECT count(*)::bigint FROM proxima_core.goals),
                (SELECT count(*)::bigint FROM proxima_core.edges),
                (SELECT count(*)::bigint FROM proxima_core.goal_wake_config)",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;

        let actor_scope = ToolScope::All;
        let candidates = wake_candidates(
            &pg,
            &[owner],
            &[],
            owner,
            trigger,
            &actor_scope,
            &actor_scope,
        )
        .await?;
        assert_eq!(candidates.len(), 1);

        let after: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT count(*)::bigint FROM proxima_core.memories),
                (SELECT count(*)::bigint FROM proxima_core.goals),
                (SELECT count(*)::bigint FROM proxima_core.edges),
                (SELECT count(*)::bigint FROM proxima_core.goal_wake_config)",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(after, before);

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn no_generic_executor_or_tool_tables() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        for table in [
            "tools".to_string(),
            format!("tool_{}", "invocations"),
            "runtime_tool_manifest".to_string(),
            "runtime_plugins".to_string(),
            "wake_invocations".to_string(),
        ] {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                     SELECT 1
                       FROM information_schema.tables
                      WHERE table_schema = 'proxima_core'
                        AND table_name = $1
                 )",
            )
            .bind(table.as_str())
            .fetch_one(pg.pool_for_tests())
            .await?;
            assert!(
                !exists,
                "PR6 must not create proxima_core.{table} or a generic executor table"
            );
        }

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}
