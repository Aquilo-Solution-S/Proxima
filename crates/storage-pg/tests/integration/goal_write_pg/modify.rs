use proxima_core::storage_ports::GoalWritePort;
use proxima_core::verbs::goal_write::{
    GoalAuthorship, GoalEvidenceRef, GoalState, IdempotencyKey, ModifyGoalAtomicRequest,
};
use proxima_core::{FlavorRegistry, OwnerRef, UserId};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use super::{
    create_db, create_goal, db_url, drop_db, fresh_draft, goal_context, goal_permit,
    insert_evidence_abstraction, insert_self, operator_authorship, replacement_payload,
};

#[tokio::test]
async fn goal_modify_atom_writes_replacement() {
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
            fresh_draft(&owner, "req-modify-prior".to_string()),
        )
        .await?;
        let replacement = replacement_payload(
            "Modified goal",
            "Modified goal text",
            br#"{"changed":true}"#,
        );
        let permit = goal_permit(&owner).await?;

        let outcome = pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner,
                    prior_goal_id: prior.goal_id,
                    replacement: replacement.clone(),
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("req-modify").expect("valid idempotency key"),
                    context: goal_context(&registry, self_id),
                    evidence: Some(evidence.clone()),
                    wake: None,
                },
                &permit,
            )
            .await?;
        assert!(!outcome.idempotent_replay);
        assert_ne!(outcome.goal_id, prior.goal_id);

        let row: (Option<Uuid>, GoalState, String, String, Vec<u8>) = sqlx::query_as(
            "SELECT supersedes, state, title, text, payload
             FROM proxima_core.goals WHERE goal_id = $1",
        )
        .bind(outcome.goal_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(row.0, Some(prior.goal_id.into_inner()));
        assert_eq!(row.1, GoalState::Active);
        assert_eq!(row.2, replacement.title);
        assert_eq!(row.3, replacement.text);
        assert_eq!(row.4, replacement.payload);

        let replay = pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner,
                    prior_goal_id: prior.goal_id,
                    replacement,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new("req-modify").expect("valid idempotency key"),
                    context: goal_context(&registry, self_id),
                    evidence: Some(evidence),
                    wake: None,
                },
                &permit,
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
    result.expect("goal modify atom test failed");
}

#[tokio::test]
async fn goal_modify_operator_authorship_writes_atogoal_evidence_edges() {
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
            fresh_draft(&owner, "req-operator-modify-prior".to_string()),
        )
        .await?;
        let permit = goal_permit(&owner).await?;

        let outcome = pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner,
                    prior_goal_id: prior.goal_id,
                    replacement: replacement_payload(
                        "Operator modified",
                        "Operator modified text",
                        br#"{"operator":true}"#,
                    ),
                    authorship: operator_authorship(),
                    request_id: IdempotencyKey::new("req-operator-modify")
                        .expect("valid idempotency key"),
                    context: goal_context(&registry, self_id),
                    evidence: Some(vec![GoalEvidenceRef::new(evidence_id)]),
                    wake: None,
                },
                &permit,
            )
            .await?;

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
    result.expect("operator modify evidence edge test failed");
}

#[tokio::test]
async fn goal_modify_operator_replay_conflicts_on_changed_evidence() {
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
            fresh_draft(&owner, "req-operator-modify-replay-prior".to_string()),
        )
        .await?;
        let authorship = operator_authorship();
        let replacement = replacement_payload(
            "Operator modified replay",
            "Operator modified replay text",
            br#"{"operator":true}"#,
        );
        let permit = goal_permit(&owner).await?;

        pg.modify_goal_atomic(
            &ModifyGoalAtomicRequest {
                owner,
                prior_goal_id: prior.goal_id,
                replacement: replacement.clone(),
                authorship: authorship.clone(),
                request_id: IdempotencyKey::new("req-operator-modify-replay")
                    .expect("valid idempotency key"),
                context: goal_context(&registry, self_id),
                evidence: Some(vec![GoalEvidenceRef::new(evidence_a)]),
                wake: None,
            },
            &permit,
        )
        .await?;

        let err = pg
            .modify_goal_atomic(
                &ModifyGoalAtomicRequest {
                    owner,
                    prior_goal_id: prior.goal_id,
                    replacement,
                    authorship,
                    request_id: IdempotencyKey::new("req-operator-modify-replay")
                        .expect("valid idempotency key"),
                    context: goal_context(&registry, self_id),
                    evidence: Some(vec![GoalEvidenceRef::new(evidence_b)]),
                    wake: None,
                },
                &permit,
            )
            .await
            .expect_err("same operator modify request with changed evidence conflicts");
        assert!(
            err.to_string()
                .contains("idempotency_conflict:req-operator-modify-replay"),
            "unexpected {err:?}"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("operator modify replay evidence conflict test failed");
}
