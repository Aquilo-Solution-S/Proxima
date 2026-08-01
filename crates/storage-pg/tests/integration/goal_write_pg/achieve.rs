use proxima_core::storage_ports::GoalWritePort;
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, GoalEvidenceRef, GoalState, IdempotencyKey,
};
use proxima_core::{FlavorRegistry, MemoryId, OwnerRef, StorageError, UserId};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use super::{
    achieve_goal, create_db, create_goal, db_url, drop_db, fresh_draft, goal_context, goal_permit,
    insert_evidence_abstraction, insert_self, operator_authorship,
};

#[tokio::test]
async fn goal_achieve_atom_writes_achieved_and_fact() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let self_id = insert_self(&pg, &owner).await?;
        let evidence_id = insert_evidence_abstraction(&pg, &owner).await?;
        let evidence = vec![GoalEvidenceRef::new(evidence_id)];
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let prior = create_goal(
            &pg,
            &registry,
            self_id,
            fresh_draft(&owner, "req-achieve-prior".to_string()),
        )
        .await?;

        let outcome = achieve_goal(
            &pg,
            &registry,
            self_id,
            owner,
            prior.goal_id,
            "req-achieve",
            evidence.clone(),
        )
        .await?;
        assert!(!outcome.idempotent_replay);
        assert_ne!(outcome.goal_id, prior.goal_id);
        assert!(outcome.lifecycle_memory_id.is_some());

        let row: (Option<Uuid>, GoalState) =
            sqlx::query_as("SELECT supersedes, state FROM proxima_core.goals WHERE goal_id = $1")
                .bind(outcome.goal_id.into_inner())
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(row, (Some(prior.goal_id.into_inner()), GoalState::Achieved));

        let achieved: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM proxima_core.goal_achieved_v1 WHERE goal_id = $1",
        )
        .bind(outcome.goal_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(achieved.0, 1);

        let replay = achieve_goal(
            &pg,
            &registry,
            self_id,
            owner,
            prior.goal_id,
            "req-achieve",
            evidence,
        )
        .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.goal_id, outcome.goal_id);
        assert_eq!(replay.lifecycle_memory_id, outcome.lifecycle_memory_id);
        assert_eq!(replay.edge_count, outcome.edge_count);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal achieve atom test failed");
}

#[tokio::test]
async fn goal_achieve_operator_authorship_writes_atogoal_evidence_edges() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let self_id = insert_self(&pg, &owner).await?;
        let evidence_id = insert_evidence_abstraction(&pg, &owner).await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let prior = create_goal(
            &pg,
            &registry,
            self_id,
            fresh_draft(&owner, "req-operator-achieve-prior".to_string()),
        )
        .await?;
        let permit = goal_permit(&owner).await?;

        let outcome = pg
            .achieve_goal_atomic(
                &AchieveGoalAtomicRequest {
                    owner,
                    prior_goal_id: prior.goal_id,
                    authorship: operator_authorship(),
                    request_id: IdempotencyKey::new("req-operator-achieve")
                        .expect("valid idempotency key"),
                    context: goal_context(&registry, self_id),
                    evidence: vec![GoalEvidenceRef::new(evidence_id)],
                },
                &permit,
            )
            .await?;

        // The evidence lives on the Goal row; the index row is its
        // consequence, and its kind follows from being a declared pointer.
        let row: (String,) = sqlx::query_as(
            "SELECT e.kind::text
               FROM proxima_core.edges e
               JOIN proxima_core.goals g ON g.goal_id = e.source_id
              WHERE e.source_id = $1
                AND e.target_id = $2
                AND $2 = ANY(g.evidence_memory_ids)",
        )
        .bind(outcome.goal_id.into_inner())
        .bind(evidence_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(row.0, "reference");

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("operator achieve evidence edge test failed");
}

#[tokio::test]
async fn goal_achieve_operator_replay_conflicts_on_changed_evidence() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let self_id = insert_self(&pg, &owner).await?;
        let evidence_a = insert_evidence_abstraction(&pg, &owner).await?;
        let evidence_b = insert_evidence_abstraction(&pg, &owner).await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let prior = create_goal(
            &pg,
            &registry,
            self_id,
            fresh_draft(&owner, "req-operator-achieve-replay-prior".to_string()),
        )
        .await?;
        let authorship = operator_authorship();
        let permit = goal_permit(&owner).await?;

        pg.achieve_goal_atomic(
            &AchieveGoalAtomicRequest {
                owner,
                prior_goal_id: prior.goal_id,
                authorship: authorship.clone(),
                request_id: IdempotencyKey::new("req-operator-achieve-replay")
                    .expect("valid idempotency key"),
                context: goal_context(&registry, self_id),
                evidence: vec![GoalEvidenceRef::new(evidence_a)],
            },
            &permit,
        )
        .await?;

        let err = pg
            .achieve_goal_atomic(
                &AchieveGoalAtomicRequest {
                    owner,
                    prior_goal_id: prior.goal_id,
                    authorship,
                    request_id: IdempotencyKey::new("req-operator-achieve-replay")
                        .expect("valid idempotency key"),
                    context: goal_context(&registry, self_id),
                    evidence: vec![GoalEvidenceRef::new(evidence_b)],
                },
                &permit,
            )
            .await
            .expect_err("same operator achieve request with changed evidence conflicts");
        assert!(
            err.to_string()
                .contains("idempotency_conflict:req-operator-achieve-replay"),
            "unexpected {err:?}"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("operator achieve replay evidence conflict test failed");
}

#[tokio::test]
async fn goal_achieve_atom_rejects_empty_evidence() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let self_id = insert_self(&pg, &owner).await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let prior = create_goal(
            &pg,
            &registry,
            self_id,
            fresh_draft(&owner, "req-empty-achieve-prior".to_string()),
        )
        .await?;

        let err = achieve_goal(
            &pg,
            &registry,
            self_id,
            owner,
            prior.goal_id,
            "req-empty-achieve",
            vec![],
        )
        .await
        .expect_err("empty achievement evidence rejected");
        assert!(
            err.to_string()
                .contains("achievement evidence must be nonempty")
        );

        let active: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM proxima_core.goals
             WHERE goal_id = $1 AND state = 'Active' AND supersedes IS NULL",
        )
        .bind(prior.goal_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(active.0, 1);

        let achieved: (i64,) =
            sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goal_achieved_v1")
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(achieved.0, 0);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("empty achievement evidence test failed");
}

#[tokio::test]
async fn goal_achieve_atom_rejects_evidence_without_home_owner() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let self_id = insert_self(&pg, &owner).await?;
        let evidence_id = MemoryId::new(Uuid::now_v7());

        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let prior = create_goal(
            &pg,
            &registry,
            self_id,
            fresh_draft(&owner, "req-no-owner-evidence-prior".to_string()),
        )
        .await?;
        let evidence = vec![GoalEvidenceRef::new(evidence_id)];

        let err = achieve_goal(
            &pg,
            &registry,
            self_id,
            owner,
            prior.goal_id,
            "req-no-owner-evidence",
            evidence,
        )
        .await
        .expect_err("missing evidence must be rejected before edge append");
        assert!(matches!(err, StorageError::ConstraintViolation(_)));
        assert!(err.to_string().contains("evidence does not exist"));

        let motivated_edges: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint
               FROM proxima_core.edges
              WHERE source_kind = 'Goal'::proxima_core.edge_endpoint_kind
                AND target_id = $1",
        )
        .bind(evidence_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            motivated_edges.0, 0,
            "rejected missing evidence must not receive a motivated-by edge"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("no-home evidence rejection test failed");
}
