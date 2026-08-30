//! Replay must not mint a second wake_config or close-fact write-act.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::Arc;

use proxima_core::storage_ports::FactIngestPort;
use proxima_core::storage_ports::{GoalWritePort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, CreateGoalAtomicRequest, GoalAssignmentTarget, GoalAtomicContext,
    GoalAuthorship, GoalDraft, GoalEvidenceRef, GoalState, GoalTopologyWrite, GoalWakeConfigWrite,
    GoalWakeToolId, GoalWakeTrigger, IdempotencyKey,
};
use proxima_core::{
    AccessKind, EdgeEndpoint, EntityKind, FlavorRegistry, GoalPayload, OwnerRef, SchemaId,
    SchemaVersion, SimpleTextGoalV1, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::goal_timeseries::WRITE_ACT_SCHEMA;
use uuid::Uuid;

fn memory_draft(kind: &str) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(format!("p5/{kind}")),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        citation: None,
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: kind.into(),
    }
}

async fn ingest_grounded_perspective(
    pg: &PgStorage,
    permit: &OwnerWritePermit,
) -> Result<proxima_core::verbs::fact_ingest::FactIngestOutcome, proxima_core::StorageError> {
    let fact = pg
        .ingest_fact_atomic(permit, &memory_draft("fact"), None)
        .await?;
    let mut abs = memory_draft("abstraction");
    abs.derived_from = vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
    let abs = pg.ingest_fact_atomic(permit, &abs, None).await?;
    let mut perspective = memory_draft("perspective");
    perspective.derived_from = vec![EdgeEndpoint::memory(EntityKind::Abstraction, abs.memory_id)];
    pg.ingest_fact_atomic(permit, &perspective, None).await
}

async fn fresh_pg() -> (String, PgStorage) {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let pg = PgStorage::connect(&db_url(&db_name))
        .await
        .expect("connect");
    pg.run_migrations().await.expect("migrate");
    (db_name, pg)
}

#[tokio::test]
async fn create_wake_replay_does_not_insert_second_wake_config() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let pool = pg.pool_for_tests();
        let planner = ingest_grounded_perspective(&pg, &permit).await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let trigger = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let wake = GoalWakeConfigWrite::new(
            GoalWakeTrigger::FactMemory {
                memory_id: trigger.memory_id,
            },
            vec![GoalWakeToolId::parse("core_remember", &registry)?],
            "wake on fact",
            &[],
        )?;
        let request_id = IdempotencyKey::new("p5-create-wake")?.into_string();
        let draft = GoalDraft {
            owner,
            schema_id: SchemaId::new(SimpleTextGoalV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(SimpleTextGoalV1::SCHEMA_VERSION),
            title: "armed".into(),
            text: "armed".into(),
            payload: Vec::new(),
            sidecar_payload: None,
            state: GoalState::Active,
            topology: GoalTopologyWrite::new(
                GoalAssignmentTarget::perspective(planner.memory_id),
                Vec::new(),
                Vec::new(),
            )?,
            wake: Some(wake),
            supersedes_goal_id: None,
            authorship: GoalAuthorship::User,
            request_id,
        };
        let context = GoalAtomicContext {
            registry: &registry,
            embedding_model_id: None,
            author_self_perspective_id: None,
        };
        let first = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: draft.clone(),
                    context,
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        assert!(!first.idempotent_replay);
        let wakes_after_first: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM proxima_core.wake_config WHERE owner_id = $1",
        )
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(wakes_after_first, 1);
        let stored_wake: Option<Uuid> =
            sqlx::query_scalar("SELECT wake_id FROM proxima_core.goal WHERE t = $1")
                .bind(first.goal_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert!(stored_wake.is_some());

        let replay = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft,
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: None,
                        author_self_perspective_id: None,
                    },
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.goal_id, first.goal_id);
        let wakes_after_replay: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM proxima_core.wake_config WHERE owner_id = $1",
        )
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(wakes_after_replay, 1, "replay must not mint a second wake");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("create wake replay must not orphan wake_config");
}

#[tokio::test]
async fn achieve_replay_does_not_insert_second_close_fact() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let pool = pg.pool_for_tests();
        let planner = ingest_grounded_perspective(&pg, &permit).await?;
        let evidence = pg
            .ingest_fact_atomic(&permit, &memory_draft("fact"), None)
            .await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let created = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: GoalDraft {
                        owner,
                        schema_id: SchemaId::new(SimpleTextGoalV1::SCHEMA_ID.into()),
                        schema_version: SchemaVersion::new(SimpleTextGoalV1::SCHEMA_VERSION),
                        title: "close me".into(),
                        text: "close me".into(),
                        payload: Vec::new(),
                        sidecar_payload: None,
                        state: GoalState::Active,
                        topology: GoalTopologyWrite::new(
                            GoalAssignmentTarget::perspective(planner.memory_id),
                            Vec::new(),
                            Vec::new(),
                        )?,
                        wake: None,
                        supersedes_goal_id: None,
                        authorship: GoalAuthorship::User,
                        request_id: IdempotencyKey::new("p5-create")?.into_string(),
                    },
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: None,
                        author_self_perspective_id: None,
                    },
                    write_act_t: None,
                },
                &permit,
            )
            .await?;
        assert!(!created.idempotent_replay);

        let first = pg
            .achieve_goal_atomic(
                &AchieveGoalAtomicRequest {
                    owner,
                    prior_goal_id: created.goal_id,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("p5-achieve")?,
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: None,
                        author_self_perspective_id: None,
                    },
                    evidence: vec![GoalEvidenceRef::new(evidence.memory_id)],
                },
                &permit,
            )
            .await?;
        assert!(!first.idempotent_replay);
        let acts_after_first: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM proxima_core.memory
              WHERE owner_id = $1 AND schema_id = $2",
        )
        .bind(owner.stored_owner_id())
        .bind(WRITE_ACT_SCHEMA)
        .fetch_one(pool)
        .await?;
        assert_eq!(acts_after_first, 1);
        let close_fact: Option<Uuid> =
            sqlx::query_scalar("SELECT close_fact_t FROM proxima_core.goal WHERE t = $1")
                .bind(first.goal_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert!(close_fact.is_some());

        let replay = pg
            .achieve_goal_atomic(
                &AchieveGoalAtomicRequest {
                    owner,
                    prior_goal_id: created.goal_id,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("p5-achieve")?,
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: None,
                        author_self_perspective_id: None,
                    },
                    evidence: vec![GoalEvidenceRef::new(evidence.memory_id)],
                },
                &permit,
            )
            .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.goal_id, first.goal_id);
        let acts_after_replay: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM proxima_core.memory
              WHERE owner_id = $1 AND schema_id = $2",
        )
        .bind(owner.stored_owner_id())
        .bind(WRITE_ACT_SCHEMA)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            acts_after_replay, 1,
            "replay must not mint a second close-fact write-act"
        );
        let close_after: Option<Uuid> =
            sqlx::query_scalar("SELECT close_fact_t FROM proxima_core.goal WHERE t = $1")
                .bind(first.goal_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(close_after, close_fact);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("achieve replay must not orphan close_fact");
}

#[tokio::test]
async fn concurrent_terminal_create_mints_one_close_fact() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let planner = {
            let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
            ingest_grounded_perspective(&pg, &permit).await?
        };
        let draft = GoalDraft {
            owner,
            schema_id: SchemaId::new(SimpleTextGoalV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(SimpleTextGoalV1::SCHEMA_VERSION),
            title: "concurrent close".into(),
            text: "concurrent close".into(),
            payload: Vec::new(),
            sidecar_payload: None,
            state: GoalState::Abandoned,
            topology: GoalTopologyWrite::new(
                GoalAssignmentTarget::perspective(planner.memory_id),
                Vec::new(),
                Vec::new(),
            )?,
            wake: None,
            supersedes_goal_id: None,
            authorship: GoalAuthorship::User,
            request_id: IdempotencyKey::new("concurrent-terminal-create")?.into_string(),
        };
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let run = |pg: PgStorage, draft: GoalDraft, barrier: Arc<tokio::sync::Barrier>| {
            tokio::spawn(async move {
                let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
                let permit = OwnerWritePermit::new_for_tests(draft.owner, AccessKind::Goal);
                barrier.wait().await;
                pg.create_goal_atomic(
                    &CreateGoalAtomicRequest {
                        draft,
                        context: GoalAtomicContext {
                            registry: &registry,
                            embedding_model_id: None,
                            author_self_perspective_id: None,
                        },
                        write_act_t: None,
                    },
                    &permit,
                )
                .await
            })
        };
        let first = run(pg.clone(), draft.clone(), barrier.clone());
        let second = run(pg.clone(), draft, barrier);
        let first = first.await?;
        let second = second.await?;
        assert!(
            first.is_ok() || second.is_ok(),
            "at least one create must win"
        );

        let pool = pg.pool_for_tests();
        let goals: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.goal
             WHERE owner_id = $1 AND request_id = 'concurrent-terminal-create'",
        )
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        let close_facts: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory
             WHERE owner_id = $1 AND schema_id = $2",
        )
        .bind(owner.stored_owner_id())
        .bind(WRITE_ACT_SCHEMA)
        .fetch_one(pool)
        .await?;
        assert_eq!(goals, 1, "the request-id race leaves one Goal");
        assert_eq!(
            close_facts, 1,
            "the losing terminal transaction rolls back its close Fact"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("concurrent terminal create must not orphan close Fact");
}
