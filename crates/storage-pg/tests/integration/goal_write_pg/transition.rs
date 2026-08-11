use proxima_core::storage_ports::GoalWritePort;
use proxima_core::verbs::goal_write::{
    GoalAtomicContext, GoalAuthorship, GoalEvidenceRef, GoalState, IdempotencyKey,
    ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use proxima_core::{FlavorRegistry, OwnerRef, UserId};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use super::{
    create_db, create_goal, db_url, drop_db, fresh_draft, goal_context, goal_permit,
    insert_evidence_abstraction, insert_self, operator_authorship, replacement_payload,
    transition_goal,
};

#[tokio::test]
async fn goal_transition_atom_writes_superseding_goal() {
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
            fresh_draft(&owner, "req-prior".to_string()),
        )
        .await?;
        let permit = goal_permit(&owner).await?;

        let transitioned = pg
            .transition_goal_atomic(
                &TransitionGoalAtomicRequest {
                    owner,
                    prior_goal_id: prior.goal_id,
                    next_state: GoalState::Paused,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("req-paused").expect("valid idempotency key"),
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: None,
                        author_self_perspective_id: Some(self_id),
                    },
                },
                &permit,
            )
            .await?;
        assert!(!transitioned.idempotent_replay);
        assert_ne!(transitioned.goal_id, prior.goal_id);

        let row: (Option<Uuid>, GoalState) =
            sqlx::query_as("SELECT supersedes, state FROM proxima_core.goals WHERE goal_id = $1")
                .bind(transitioned.goal_id.into_inner())
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(row, (Some(prior.goal_id.into_inner()), GoalState::Paused));

        let replay = pg
            .transition_goal_atomic(
                &TransitionGoalAtomicRequest {
                    owner,
                    prior_goal_id: prior.goal_id,
                    next_state: GoalState::Paused,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("req-paused").expect("valid idempotency key"),
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: None,
                        author_self_perspective_id: Some(self_id),
                    },
                },
                &permit,
            )
            .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.goal_id, transitioned.goal_id);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal transition atom test failed");
}

#[tokio::test]
async fn goal_transition_rejects_operator_authorship_without_evidence() {
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
            fresh_draft(&owner, "req-operator-transition-prior".to_string()),
        )
        .await?;
        let permit = goal_permit(&owner).await?;

        let err = pg
            .transition_goal_atomic(
                &TransitionGoalAtomicRequest {
                    owner,
                    prior_goal_id: prior.goal_id,
                    next_state: GoalState::Paused,
                    authorship: operator_authorship(),
                    request_id: IdempotencyKey::new("req-operator-transition")
                        .expect("valid idempotency key"),
                    context: goal_context(&registry, self_id),
                },
                &permit,
            )
            .await
            .expect_err("operator transition has no evidence carrier");
        assert!(
            err.to_string().contains(
                "operator-authored Goal transition requires explicit Abstraction evidence"
            ),
            "unexpected {err:?}"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("operator transition rejection test failed");
}

#[tokio::test]
async fn goal_transition_atom_abandon_and_resume() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let self_id = insert_self(&pg, &owner).await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();

        let abandoned_prior = create_goal(
            &pg,
            &registry,
            self_id,
            fresh_draft(&owner, "req-abandon-prior".to_string()),
        )
        .await?;
        let abandoned = transition_goal(
            &pg,
            &registry,
            self_id,
            owner,
            abandoned_prior.goal_id,
            GoalState::Abandoned,
            "req-abandon",
        )
        .await?;
        let abandoned_row: (Option<Uuid>, GoalState) =
            sqlx::query_as("SELECT supersedes, state FROM proxima_core.goals WHERE goal_id = $1")
                .bind(abandoned.goal_id.into_inner())
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(
            abandoned_row,
            (
                Some(abandoned_prior.goal_id.into_inner()),
                GoalState::Abandoned
            )
        );
        let abandoned_facts: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM proxima_core.goal_abandoned_v1 WHERE goal_id = $1",
        )
        .bind(abandoned.goal_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(abandoned_facts.0, 1);

        let pause_prior = create_goal(
            &pg,
            &registry,
            self_id,
            fresh_draft(&owner, "req-resume-prior".to_string()),
        )
        .await?;
        let paused = transition_goal(
            &pg,
            &registry,
            self_id,
            owner,
            pause_prior.goal_id,
            GoalState::Paused,
            "req-pause-for-resume",
        )
        .await?;
        let resumed = transition_goal(
            &pg,
            &registry,
            self_id,
            owner,
            paused.goal_id,
            GoalState::Active,
            "req-resume",
        )
        .await?;
        let resumed_state: (GoalState,) =
            sqlx::query_as("SELECT state FROM proxima_core.goals WHERE goal_id = $1")
                .bind(resumed.goal_id.into_inner())
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(resumed_state.0, GoalState::Active);

        let activations: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM proxima_core.goal_activated_v1
             WHERE goal_id = ANY($1)",
        )
        .bind(vec![
            pause_prior.goal_id.into_inner(),
            resumed.goal_id.into_inner(),
        ])
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(activations.0, 2);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal abandon/resume transition test failed");
}

#[tokio::test]
async fn goal_transition_replay_conflicts_on_a_row_resting_on_other_evidence() {
    // The idempotency key is `md5(owner_kind:owner_id:request_id)` — one
    // namespace for every goal write — and the body comparison behind a
    // replay reads the content columns, not `evidence_memory_ids`. So a key
    // reused across two verbs can hand a transition someone else's row: a
    // content-preserving modify writes an Active successor resting on
    // evidence, and a resume of the same prior head under the same key
    // matches it on every column the body check reads.
    //
    // Evidence is part of what a write claimed, so that is a conflict, not a
    // replay. This is the transition-side twin of
    // `goal_modify_operator_replay_conflicts_on_changed_evidence`.
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
            fresh_draft(&owner, "req-shared-key-prior".to_string()),
        )
        .await?;
        let permit = goal_permit(&owner).await?;

        // Same title/text/payload as the prior head, so the successor is
        // indistinguishable from a resume on every content column.
        let modified = pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner,
                    prior_goal_id: prior.goal_id,
                    replacement: replacement_payload("Test goal", "Test goal text", b"{}"),
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("req-shared-key")
                        .expect("valid idempotency key"),
                    context: goal_context(&registry, self_id),
                    evidence: Some(vec![GoalEvidenceRef::new(evidence_id)]),
                    wake: None,
                },
                &permit,
            )
            .await?;
        assert!(!modified.idempotent_replay);

        let err = transition_goal(
            &pg,
            &registry,
            self_id,
            owner,
            prior.goal_id,
            GoalState::Active,
            "req-shared-key",
        )
        .await
        .expect_err("a row resting on evidence the transition never claimed is a conflict");
        assert!(
            err.to_string()
                .contains("idempotency_conflict:req-shared-key"),
            "unexpected {err:?}"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("transition replay evidence conflict test failed");
}

#[tokio::test]
async fn goal_transition_atom_rejects_stale_head() {
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
            fresh_draft(&owner, "req-stale-prior".to_string()),
        )
        .await?;
        let paused = transition_goal(
            &pg,
            &registry,
            self_id,
            owner,
            prior.goal_id,
            GoalState::Paused,
            "req-stale-pause",
        )
        .await?;
        assert_ne!(paused.goal_id, prior.goal_id);

        let err = transition_goal(
            &pg,
            &registry,
            self_id,
            owner,
            prior.goal_id,
            GoalState::Abandoned,
            "req-stale-abandon",
        )
        .await
        .expect_err("stale prior head rejected");
        assert!(err.to_string().contains("stale goal head"));

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("stale goal head test failed");
}
