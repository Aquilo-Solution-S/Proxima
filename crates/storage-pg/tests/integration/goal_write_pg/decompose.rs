use proxima_core::verbs::goal_write::{ChildGoalDraft, GoalEvidenceRef, GoalState, IdempotencyKey};
use proxima_core::{CORE_DEPENDS_ON_RELATION, FlavorRegistry, OwnerRef, UserId};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use super::{
    create_db, create_goal, db_url, decompose_goal, drop_db, fresh_draft, goal_topology,
    insert_evidence_abstraction, insert_self, replacement_payload, transition_goal,
};

#[tokio::test]
async fn goal_decompose_atom_writes_children_and_parents() {
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
        let parent = create_goal(
            &pg,
            &registry,
            self_id,
            fresh_draft(&owner, "req-decompose-parent".to_string()),
        )
        .await?;
        let children = vec![
            ChildGoalDraft {
                payload: replacement_payload("Child one", "Child one text", b"{}"),
                evidence: evidence.clone(),
                wake: None,
                request_id: IdempotencyKey::new("req-decompose-child-1")
                    .expect("valid idempotency key"),
            },
            ChildGoalDraft {
                payload: replacement_payload("Child two", "Child two text", b"{}"),
                evidence: evidence.clone(),
                wake: None,
                request_id: IdempotencyKey::new("req-decompose-child-2")
                    .expect("valid idempotency key"),
            },
        ];

        let outcome =
            decompose_goal(&pg, &registry, self_id, owner, parent.goal_id, children).await?;
        assert!(!outcome.idempotent_replay);
        assert_eq!(outcome.children.len(), 2);

        for child in &outcome.children {
            let parents: (i64,) = sqlx::query_as(
                "SELECT count(*)::bigint
                   FROM proxima_core.edges
                  WHERE relation = $1
                    AND source_goal_id = $2
                    AND target_goal_id = $3",
            )
            .bind(CORE_DEPENDS_ON_RELATION)
            .bind(child.outcome.goal_id.into_inner())
            .bind(parent.goal_id.into_inner())
            .fetch_one(pg.pool_for_tests())
            .await?;
            assert_eq!(parents.0, 1);
        }

        let replay_children = vec![
            ChildGoalDraft {
                payload: replacement_payload("Child one", "Child one text", b"{}"),
                evidence: evidence.clone(),
                wake: None,
                request_id: IdempotencyKey::new("req-decompose-child-1")
                    .expect("valid idempotency key"),
            },
            ChildGoalDraft {
                payload: replacement_payload("Child two", "Child two text", b"{}"),
                evidence,
                wake: None,
                request_id: IdempotencyKey::new("req-decompose-child-2")
                    .expect("valid idempotency key"),
            },
        ];
        let replay = decompose_goal(
            &pg,
            &registry,
            self_id,
            owner,
            parent.goal_id,
            replay_children,
        )
        .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.children.len(), 2);
        assert_eq!(
            replay.children[0].outcome.goal_id,
            outcome.children[0].outcome.goal_id
        );
        assert_eq!(
            replay.children[1].outcome.goal_id,
            outcome.children[1].outcome.goal_id
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal decompose atom test failed");
}

#[tokio::test]
async fn goal_decompose_atom_rejects_cross_owner_parent() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner_a = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let owner_b = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let self_a = insert_self(&pg, &owner_a).await?;
        let self_b = insert_self(&pg, &owner_b).await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let parent = create_goal(
            &pg,
            &registry,
            self_a,
            fresh_draft(&owner_a, "req-cross-owner-parent".to_string()),
        )
        .await?;

        let decompose_err = decompose_goal(
            &pg,
            &registry,
            self_b,
            owner_b,
            parent.goal_id,
            vec![ChildGoalDraft {
                payload: replacement_payload("Cross child", "Cross child text", b"{}"),
                evidence: vec![],
                wake: None,
                request_id: IdempotencyKey::new("req-cross-owner-child")
                    .expect("valid idempotency key"),
            }],
        )
        .await
        .expect_err("cross-owner decompose parent rejected before child insert");
        assert!(matches!(
            decompose_err,
            proxima_core::StorageError::NotFound
        ));

        let mut cross_parent_child =
            fresh_draft(&owner_b, "req-cross-owner-create-child".to_string());
        cross_parent_child.topology = goal_topology(self_b, vec![parent.goal_id], Vec::new());
        let err = create_goal(&pg, &registry, self_b, cross_parent_child)
            .await
            .expect_err("cross-owner parent edge rejected");
        assert!(matches!(err, proxima_core::StorageError::NotFound));

        let active_parent = create_goal(
            &pg,
            &registry,
            self_b,
            fresh_draft(&owner_b, "req-inactive-parent".to_string()),
        )
        .await?;
        let paused_parent = transition_goal(
            &pg,
            &registry,
            self_b,
            owner_b,
            active_parent.goal_id,
            GoalState::Paused,
            "req-inactive-parent-paused",
        )
        .await?;
        let inactive_err = decompose_goal(
            &pg,
            &registry,
            self_b,
            owner_b,
            paused_parent.goal_id,
            vec![ChildGoalDraft {
                payload: replacement_payload("Inactive child", "Inactive child text", b"{}"),
                evidence: vec![],
                wake: None,
                request_id: IdempotencyKey::new("req-inactive-child")
                    .expect("valid idempotency key"),
            }],
        )
        .await
        .expect_err("inactive parent rejected");
        assert!(
            inactive_err
                .to_string()
                .contains("parent_goal must be Active")
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("cross-owner parent test failed");
}
