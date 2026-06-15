//! End-to-end core Goal storage atoms against a transient PG database.

use crate::common::{create_db, db_url, drop_db};

use proxima_core::storage::Storage;
use proxima_core::verbs::goal_write::{
    CreateGoalAtomicRequest, GoalAtomicContext, GoalAuthorship, GoalDraft, GoalState,
    GoalWriteOutcome, IdempotencyKey, TransitionGoalAtomicRequest,
};
use proxima_core::{
    FlavorRegistry, MemoryId, OrgId, Owner, OwnerPrincipalKind, Principal, SchemaId, SchemaVersion,
    UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

fn owner_parts(owner: &Owner) -> (OwnerPrincipalKind, Uuid, Uuid) {
    let kind = OwnerPrincipalKind::of(&owner.principal);
    let principal_id = match owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

fn fresh_draft(owner: &Owner, request_id: String) -> GoalDraft {
    GoalDraft {
        principal: owner.principal.clone(),
        org_id: Some(owner.org_id),
        schema_id: SchemaId::new("core/simple-text-v1".into()),
        schema_version: SchemaVersion::new(1),
        title: "Test goal".to_string(),
        text: "Test goal text".to_string(),
        payload: b"{}".to_vec(),
        state: GoalState::Active,
        parent_goal_ids: vec![],
        supersedes_goal_id: None,
        authorship: GoalAuthorship::User,
        request_id,
    }
}

async fn insert_self(
    pg: &PgStorage,
    owner: &Owner,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_parts(owner);
    let memory_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id)
         VALUES ($1, $2, $3, $4, 'test/self', 1, $5,
                 'self', $6, 'test-model', 'v1', $7)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(proxima_core::EntityKind::Perspective)
    .bind(proxima_core::MemoryOperatorKind::AtoP)
    .bind(Uuid::nil())
    .execute(pg.pool())
    .await?;
    Ok(MemoryId::new(memory_id))
}

async fn create_goal(
    pg: &PgStorage,
    registry: &proxima_core::FlavorRegistryFrozen,
    self_id: MemoryId,
    draft: GoalDraft,
) -> Result<GoalWriteOutcome, proxima_core::StorageError> {
    pg.create_goal_atomic(&CreateGoalAtomicRequest {
        draft,
        context: GoalAtomicContext {
            registry,
            embedding_model_id: None,
            author_self_perspective_id: Some(self_id),
        },
        target_self_perspective_id: self_id,
        evidence: Vec::new(),
    })
    .await
}

#[tokio::test]
async fn goal_create_atom_writes_goal_side_effects_and_replays() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let self_id = insert_self(&pg, &owner).await?;
        let registry = FlavorRegistry::new().freeze();
        let draft = fresh_draft(&owner, "req-1".to_string());

        let outcome = create_goal(&pg, &registry, self_id, draft.clone()).await?;
        assert!(!outcome.idempotent_replay);
        assert!(outcome.lifecycle_memory_id.is_some());
        assert!(!outcome.edge_ids.is_empty());

        let payload: Vec<u8> =
            sqlx::query_scalar("SELECT payload FROM proxima_core.goals WHERE goal_id = $1")
                .bind(outcome.goal_id.into_inner())
                .fetch_one(pg.pool())
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
            .fetch_one(pg.pool())
            .await?;
        assert_eq!(goals.0, 1);

        let activated: (i64,) =
            sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goal_activated_v1")
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(activated.0, 1);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal create atom test failed");
}

#[tokio::test]
async fn goal_transition_atom_writes_superseding_goal() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let self_id = insert_self(&pg, &owner).await?;
        let registry = FlavorRegistry::new().freeze();
        let prior = create_goal(
            &pg,
            &registry,
            self_id,
            fresh_draft(&owner, "req-prior".to_string()),
        )
        .await?;

        let transitioned = pg
            .transition_goal_atomic(&TransitionGoalAtomicRequest {
                owner: owner.clone(),
                prior_goal_id: prior.goal_id,
                next_state: GoalState::Paused,
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("req-paused").expect("valid idempotency key"),
                context: GoalAtomicContext {
                    registry: &registry,
                    embedding_model_id: None,
                    author_self_perspective_id: Some(self_id),
                },
            })
            .await?;
        assert!(!transitioned.idempotent_replay);
        assert_ne!(transitioned.goal_id, prior.goal_id);

        let row: (Option<Uuid>, GoalState) =
            sqlx::query_as("SELECT supersedes, state FROM proxima_core.goals WHERE goal_id = $1")
                .bind(transitioned.goal_id.into_inner())
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(row, (Some(prior.goal_id.into_inner()), GoalState::Paused));

        let replay = pg
            .transition_goal_atomic(&TransitionGoalAtomicRequest {
                owner: owner.clone(),
                prior_goal_id: prior.goal_id,
                next_state: GoalState::Paused,
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("req-paused").expect("valid idempotency key"),
                context: GoalAtomicContext {
                    registry: &registry,
                    embedding_model_id: None,
                    author_self_perspective_id: Some(self_id),
                },
            })
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
async fn goal_create_atom_with_parent_writes_goal_parent() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let self_id = insert_self(&pg, &owner).await?;
        let registry = FlavorRegistry::new().freeze();
        let parent = create_goal(
            &pg,
            &registry,
            self_id,
            fresh_draft(&owner, "req-parent".to_string()),
        )
        .await?;

        let mut child = fresh_draft(&owner, "req-child".to_string());
        child.parent_goal_ids = vec![parent.goal_id];
        let child_outcome = create_goal(&pg, &registry, self_id, child).await?;
        assert!(!child_outcome.idempotent_replay);

        let parents: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM proxima_core.goal_parents WHERE goal_id = $1",
        )
        .bind(child_outcome.goal_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(parents.0, 1);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal parent atom test failed");
}

#[tokio::test]
async fn goal_create_atom_rejects_empty_or_non_object_payload() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let self_id = insert_self(&pg, &owner).await?;
        let registry = FlavorRegistry::new().freeze();

        let mut empty = fresh_draft(&owner, "req-empty".to_string());
        empty.payload = Vec::new();
        let err = create_goal(&pg, &registry, self_id, empty)
            .await
            .expect_err("empty payload rejected");
        assert!(err.to_string().contains("EOF"));

        let mut scalar = fresh_draft(&owner, "req-scalar".to_string());
        scalar.payload = b"123".to_vec();
        let err = create_goal(&pg, &registry, self_id, scalar)
            .await
            .expect_err("scalar payload rejected");
        assert!(err.to_string().contains("must be a JSON object"));

        let goals: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goals")
            .fetch_one(pg.pool())
            .await?;
        assert_eq!(goals.0, 0);

        let (owner_kind, owner_principal_id, owner_org_id) = owner_parts(&owner);
        let raw = sqlx::query(
            "INSERT INTO proxima_core.goals
                (goal_id, schema_id, schema_version,
                 owner_principal_kind, owner_principal_id, owner_org_id,
                 title, text, payload, state, authorship_kind, request_id)
             VALUES ($1, 'core/simple-text-v1', 1,
                     $2, $3, $4,
                     'raw', 'raw', $5, 'Active', 'User', 'req-raw')",
        )
        .bind(Uuid::now_v7())
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(Vec::<u8>::new())
        .execute(pg.pool())
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
