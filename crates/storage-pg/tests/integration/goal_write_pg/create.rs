use proxima_core::verbs::goal_write::{GoalState, GoalWakeConfigWrite, GoalWakeTrigger};
use proxima_core::{
    CORE_DEPENDS_ON_RELATION, FlavorRegistry, GoalPayload, OwnerRef, SchemaId, SchemaVersion,
    UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use super::{
    TestCustomGoalPayload, create_db, create_goal, db_url, drop_db, fresh_draft, goal_topology,
    insert_self, owner_parts, transition_goal, wake_config,
};

#[tokio::test]
async fn goal_create_atom_writes_goal_side_effects_and_replays() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let self_id = insert_self(&pg, &owner).await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let draft = fresh_draft(&owner, "req-1".to_string());

        let outcome = create_goal(&pg, &registry, self_id, draft.clone()).await?;
        assert!(!outcome.idempotent_replay);
        assert!(outcome.lifecycle_memory_id.is_some());
        assert!(!outcome.edge_ids.is_empty());

        let payload: Vec<u8> =
            sqlx::query_scalar("SELECT payload FROM proxima_core.goals WHERE goal_id = $1")
                .bind(outcome.goal_id.into_inner())
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(payload, draft.payload);

        let replay = create_goal(&pg, &registry, self_id, draft.clone()).await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.goal_id, outcome.goal_id);
        assert_eq!(replay.lifecycle_memory_id, outcome.lifecycle_memory_id);
        assert_eq!(replay.edge_ids, outcome.edge_ids);

        let mut mutated = draft.clone();
        mutated.text = "Different text".to_string();
        let err = create_goal(&pg, &registry, self_id, mutated)
            .await
            .expect_err("same request id with different body conflicts");
        assert!(err.to_string().contains("idempotency_conflict"));

        let mut bad_schema = draft;
        bad_schema.schema_id = SchemaId::new("test/fact_blob".into());
        let err = create_goal(&pg, &registry, self_id, bad_schema)
            .await
            .expect_err("unknown Goal schema rejected");
        assert!(err.to_string().contains("unregistered GoalPayload schema"));

        let goals: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goals")
            .fetch_one(pg.pool_for_tests())
            .await?;
        assert_eq!(goals.0, 1);

        let activated: (i64,) =
            sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goal_activated_v1")
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(activated.0, 1);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal create atom test failed");
}

#[tokio::test]
async fn goal_create_atom_rejects_invalid_wake_config_before_goal_insert() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let self_id = insert_self(&pg, &owner).await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let mut draft = fresh_draft(&owner, "req-invalid-wake".to_string());
        draft.wake = Some(wake_config(
            &registry,
            GoalWakeTrigger::FactSchema {
                schema_id: SchemaId::new("test/missing-fact-v1".into()),
                schema_version: SchemaVersion::new(1),
            },
            &[],
        ));

        let err = create_goal(&pg, &registry, self_id, draft)
            .await
            .expect_err("storage rejects unregistered wake trigger Fact schema");
        assert!(
            err.to_string()
                .contains("unregistered wake trigger Fact schema")
        );

        let goals: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goals")
            .fetch_one(pg.pool_for_tests())
            .await?;
        assert_eq!(goals.0, 0);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal wake storage validation test failed");
}

#[tokio::test]
async fn goal_create_atom_rejects_deserialized_invalid_wake_tool_id() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let self_id = insert_self(&pg, &owner).await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let malformed_wake: GoalWakeConfigWrite = serde_json::from_value(serde_json::json!({
            "trigger": {
                "FactSchema": {
                    "schema_id": "core/agent-note-v1",
                    "schema_version": 1
                }
            },
            "tool_ids": ["not_registered_tool"],
            "prompt": "wake prompt",
            "hard_memory_ids": []
        }))?;
        let mut draft = fresh_draft(&owner, "req-invalid-tool".to_string());
        draft.wake = Some(malformed_wake);

        let err = create_goal(&pg, &registry, self_id, draft)
            .await
            .expect_err("storage rejects deserialized invalid wake tool id");
        assert!(err.to_string().contains("invalid wake tool id"));

        let goals: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goals")
            .fetch_one(pg.pool_for_tests())
            .await?;
        assert_eq!(goals.0, 0);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal wake tool-id validation test failed");
}

#[tokio::test]
async fn goal_create_atom_with_parent_writes_goal_parent() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let self_id = insert_self(&pg, &owner).await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let parent = create_goal(
            &pg,
            &registry,
            self_id,
            fresh_draft(&owner, "req-parent".to_string()),
        )
        .await?;

        let mut child = fresh_draft(&owner, "req-child".to_string());
        child.topology = goal_topology(self_id, vec![parent.goal_id], Vec::new());
        let child_outcome = create_goal(&pg, &registry, self_id, child).await?;
        assert!(!child_outcome.idempotent_replay);

        let parents: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint
               FROM proxima_core.edges
              WHERE relation = $1
                AND source_goal_id = $2",
        )
        .bind(CORE_DEPENDS_ON_RELATION)
        .bind(child_outcome.goal_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(parents.0, 1);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal parent atom test failed");
}

#[tokio::test]
async fn goal_create_atom_rejects_empty_payload_bytes() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let self_id = insert_self(&pg, &owner).await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();

        let mut empty = fresh_draft(&owner, "req-empty".to_string());
        empty.payload = Vec::new();
        let err = create_goal(&pg, &registry, self_id, empty)
            .await
            .expect_err("empty payload rejected");
        assert!(err.to_string().contains("goals_payload_nonempty_chk"));

        let goals: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goals")
            .fetch_one(pg.pool_for_tests())
            .await?;
        assert_eq!(goals.0, 0);

        let (owner_kind, owner_id) = owner_parts(&owner);
        let raw = sqlx::query(
            "INSERT INTO proxima_core.goals
                (goal_id, owner_kind, owner_id, schema_id, schema_version,
                 title, text, payload, state, authorship_kind, request_id,
                 idempotency_key)
             VALUES ($1, $2, $3, 'core/simple-text-v1', 1,
                     'raw', 'raw', $4, 'Active', 'User', 'req-raw',
                     md5($2::text || ':' || $3::text || ':' || 'req-raw'))",
        )
        .bind(Uuid::now_v7())
        .bind(owner_kind)
        .bind(owner_id)
        .bind(Vec::<u8>::new())
        .execute(pg.pool_for_tests())
        .await;
        assert!(
            raw.expect_err("DB CHECK must reject a zero-byte payload")
                .to_string()
                .contains("goals_payload_nonempty_chk")
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal payload rejection test failed");
}

#[tokio::test]
async fn goal_create_atom_rejects_inactive_or_superseded_parent() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let self_id = insert_self(&pg, &owner).await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();

        let active_parent = create_goal(
            &pg,
            &registry,
            self_id,
            fresh_draft(&owner, "req-direct-inactive-parent".to_string()),
        )
        .await?;
        let paused_parent = transition_goal(
            &pg,
            &registry,
            self_id,
            owner,
            active_parent.goal_id,
            GoalState::Paused,
            "req-direct-inactive-parent-paused",
        )
        .await?;
        let mut inactive_child = fresh_draft(&owner, "req-direct-inactive-child".to_string());
        inactive_child.topology = goal_topology(self_id, vec![paused_parent.goal_id], Vec::new());
        let inactive_err = create_goal(&pg, &registry, self_id, inactive_child)
            .await
            .expect_err("inactive direct create parent rejected");
        assert!(
            inactive_err
                .to_string()
                .contains("parent_goal must be Active")
        );

        let superseded_parent = create_goal(
            &pg,
            &registry,
            self_id,
            fresh_draft(&owner, "req-direct-superseded-parent".to_string()),
        )
        .await?;
        let _newer_parent = transition_goal(
            &pg,
            &registry,
            self_id,
            owner,
            superseded_parent.goal_id,
            GoalState::Active,
            "req-direct-superseded-parent-newer",
        )
        .await?;
        let mut superseded_child = fresh_draft(&owner, "req-direct-superseded-child".to_string());
        superseded_child.topology =
            goal_topology(self_id, vec![superseded_parent.goal_id], Vec::new());
        let superseded_err = create_goal(&pg, &registry, self_id, superseded_child)
            .await
            .expect_err("superseded direct create parent rejected");
        assert!(
            superseded_err
                .to_string()
                .contains("parent_goal is not current head")
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("direct parent validation test failed");
}

#[tokio::test]
async fn goal_create_atom_with_registry_generic_payload() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let self_id = insert_self(&pg, &owner).await?;
        let mut registry = FlavorRegistry::new();
        registry.add_goal_schema_or_panic_for_tests::<TestCustomGoalPayload>();
        let registry = registry.freeze_or_panic_for_tests();
        let mut draft = fresh_draft(&owner, "req-custom-goal".to_string());
        draft.schema_id = SchemaId::new(TestCustomGoalPayload::SCHEMA_ID.to_string());
        draft.schema_version = SchemaVersion::new(TestCustomGoalPayload::SCHEMA_VERSION);
        draft.title = "Custom goal".to_string();
        draft.text = "Custom goal text".to_string();
        draft.payload = br#"{"note":"custom"}"#.to_vec();

        let outcome = create_goal(&pg, &registry, self_id, draft.clone()).await?;
        assert!(!outcome.idempotent_replay);

        let goals: (i64,) =
            sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goals WHERE schema_id = $1")
                .bind(TestCustomGoalPayload::SCHEMA_ID)
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(goals.0, 1);

        let activated: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM proxima_core.goal_activated_v1 WHERE goal_id = $1",
        )
        .bind(outcome.goal_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(activated.0, 1);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("registry-generic goal payload test failed");
}
