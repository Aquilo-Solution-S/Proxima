//! End-to-end core Goal storage atoms against a transient PG database.

use crate::common::{create_db, db_url, drop_db};

use proxima_core::storage::Storage;
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, GoalAtomicContext, GoalAuthorship, GoalDraft, GoalPayloadWrite,
    GoalState, GoalWriteOutcome, IdempotencyKey, ModifyGoalAtomicRequest,
    TransitionGoalAtomicRequest,
};
use proxima_core::{
    CORE_MOTIVATED_BY_RELATION, FlavorRegistry, FlavorRegistryFrozen, GoalPayload, MemoryId, Owner,
    OwnerRef, OwnerRefKind, PayloadKeyBuilder, SchemaId, SchemaVersion, StorageError, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct TestCustomGoalPayload {
    note: String,
}

impl GoalPayload for TestCustomGoalPayload {
    const SCHEMA_ID: &'static str = "test/custom-goal-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn goal_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("note", &self.note);
        key.finish()
    }
}

fn owner_parts(owner: &Owner) -> (OwnerRefKind, Uuid) {
    owner.columns()
}

fn fresh_draft(owner: &Owner, request_id: String) -> GoalDraft {
    GoalDraft {
        principal: *owner,
        schema_id: SchemaId::new("core/simple-text-v1".into()),
        schema_version: SchemaVersion::new(1),
        title: "Test goal".to_string(),
        text: "Test goal text".to_string(),
        payload: b"{}".to_vec(),
        sidecar_payload: None,
        state: GoalState::Active,
        parent_goal_ids: vec![],
        supersedes_goal_id: None,
        authorship: GoalAuthorship::User,
        request_id,
    }
}

fn replacement_payload(title: &str, text: &str, payload: &[u8]) -> GoalPayloadWrite {
    GoalPayloadWrite {
        schema_id: SchemaId::new("core/simple-text-v1".into()),
        schema_version: SchemaVersion::new(1),
        title: title.to_string(),
        text: text.to_string(),
        payload: payload.to_vec(),
        sidecar_payload: None,
    }
}

fn goal_context(registry: &FlavorRegistryFrozen, self_id: MemoryId) -> GoalAtomicContext<'_> {
    GoalAtomicContext {
        registry,
        embedding_model_id: None,
        author_self_perspective_id: Some(self_id),
    }
}

async fn insert_self(
    pg: &PgStorage,
    owner: &Owner,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    let memory_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id)
         VALUES ($1, 'test/self', 1, $2,
                 'self', $3, 'test-model', 'v1', $4)",
    )
    .bind(memory_id)
    .bind(proxima_core::EntityKind::Perspective)
    .bind(proxima_core::MemoryOperatorKind::AtoP)
    .bind(Uuid::nil())
    .execute(pg.pool())
    .await?;
    insert_home(pg, memory_id, owner, owner_kind, owner_principal_id).await?;
    Ok(MemoryId::new(memory_id))
}

async fn insert_evidence_abstraction(
    pg: &PgStorage,
    owner: &Owner,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let (owner_kind, owner_principal_id) = owner_parts(owner);
    let memory_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id)
         VALUES ($1, 'test/evidence-abstraction', 1, $2,
                 'evidence', $3, 'test-model', 'v1', $4)",
    )
    .bind(memory_id)
    .bind(proxima_core::EntityKind::Abstraction)
    .bind(proxima_core::MemoryOperatorKind::FtoA)
    .bind(Uuid::nil())
    .execute(pg.pool())
    .await?;
    insert_home(pg, memory_id, owner, owner_kind, owner_principal_id).await?;
    Ok(MemoryId::new(memory_id))
}

async fn insert_home(
    pg: &PgStorage,
    entity_id: Uuid,
    _owner: &Owner,
    owner_kind: OwnerRefKind,
    owner_principal_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(proxima_storage_pg::access::owner_ref_compat::sql(
        "INSERT INTO __PROXIMA_ENTITY_OWNER__
            (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
         VALUES ($1, $2, $3, true, $4)
         ON CONFLICT DO NOTHING",
    ))
    .bind(entity_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(Uuid::nil())
    .execute(pg.pool())
    .await
    .map(|_| ())
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

async fn achieve_goal(
    pg: &PgStorage,
    registry: &FlavorRegistryFrozen,
    self_id: MemoryId,
    owner: Owner,
    prior_goal_id: proxima_core::GoalId,
    request_id: &str,
    evidence: Vec<proxima_core::verbs::goal_write::GoalEvidenceRef>,
) -> Result<GoalWriteOutcome, proxima_core::StorageError> {
    pg.achieve_goal_atomic(&AchieveGoalAtomicRequest {
        owner,
        prior_goal_id,
        authorship: GoalAuthorship::User,
        request_id: IdempotencyKey::new(request_id).expect("valid idempotency key"),
        context: goal_context(registry, self_id),
        evidence,
    })
    .await
}

async fn transition_goal(
    pg: &PgStorage,
    registry: &FlavorRegistryFrozen,
    self_id: MemoryId,
    owner: Owner,
    prior_goal_id: proxima_core::GoalId,
    next_state: GoalState,
    request_id: &str,
) -> Result<GoalWriteOutcome, proxima_core::StorageError> {
    pg.transition_goal_atomic(&TransitionGoalAtomicRequest {
        owner,
        prior_goal_id,
        next_state,
        authorship: GoalAuthorship::User,
        request_id: IdempotencyKey::new(request_id).expect("valid idempotency key"),
        context: goal_context(registry, self_id),
    })
    .await
}

async fn decompose_goal(
    pg: &PgStorage,
    registry: &FlavorRegistryFrozen,
    self_id: MemoryId,
    owner: Owner,
    parent_goal_id: proxima_core::GoalId,
    children: Vec<ChildGoalDraft>,
) -> Result<DecomposeGoalOutcome, proxima_core::StorageError> {
    pg.decompose_goal_atomic(&DecomposeGoalAtomicRequest {
        owner,
        parent_goal_id,
        authorship: GoalAuthorship::User,
        context: goal_context(registry, self_id),
        target_self_perspective_id: self_id,
        children,
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
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
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
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
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
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
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
async fn goal_create_atom_rejects_empty_payload_bytes() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let self_id = insert_self(&pg, &owner).await?;
        let registry = FlavorRegistry::new().freeze();

        let mut empty = fresh_draft(&owner, "req-empty".to_string());
        empty.payload = Vec::new();
        let err = create_goal(&pg, &registry, self_id, empty)
            .await
            .expect_err("empty payload rejected");
        assert!(err.to_string().contains("goals_payload_nonempty_chk"));

        let goals: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goals")
            .fetch_one(pg.pool())
            .await?;
        assert_eq!(goals.0, 0);

        let (owner_kind, owner_principal_id) = owner_parts(&owner);
        let raw = sqlx::query(
            "INSERT INTO proxima_core.goals
                (goal_id, schema_id, schema_version,
                 title, text, payload, state, authorship_kind, request_id,
                 idempotency_key)
             VALUES ($1, 'core/simple-text-v1', 1,
                     'raw', 'raw', $2, 'Active', 'User', 'req-raw',
                     md5($3::text || ':' || $4::text || ':' || 'req-raw'))",
        )
        .bind(Uuid::now_v7())
        .bind(Vec::<u8>::new())
        .bind(owner_kind)
        .bind(owner_principal_id)
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
        let evidence = vec![proxima_core::verbs::goal_write::GoalEvidenceRef {
            memory_id: evidence_id,
        }];
        let registry = FlavorRegistry::new().freeze();
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
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(row, (Some(prior.goal_id.into_inner()), GoalState::Achieved));

        let achieved: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM proxima_core.goal_achieved_v1 WHERE goal_id = $1",
        )
        .bind(outcome.goal_id.into_inner())
        .fetch_one(pg.pool())
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
        assert_eq!(replay.edge_ids, outcome.edge_ids);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal achieve atom test failed");
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
        let registry = FlavorRegistry::new().freeze();
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
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(active.0, 1);

        let achieved: (i64,) =
            sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goal_achieved_v1")
                .fetch_one(pg.pool())
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
        let evidence_id = insert_evidence_abstraction(&pg, &owner).await?;
        sqlx::query(proxima_storage_pg::access::owner_ref_compat::sql(
            "DELETE FROM __PROXIMA_ENTITY_OWNER__ WHERE entity_id = $1",
        ))
        .bind(evidence_id.into_inner())
        .execute(pg.pool())
        .await?;

        let registry = FlavorRegistry::new().freeze();
        let prior = create_goal(
            &pg,
            &registry,
            self_id,
            fresh_draft(&owner, "req-no-owner-evidence-prior".to_string()),
        )
        .await?;
        let evidence = vec![proxima_core::verbs::goal_write::GoalEvidenceRef {
            memory_id: evidence_id,
        }];

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
        .expect_err("no-home evidence must be rejected before edge append");
        assert!(matches!(err, StorageError::ConstraintViolation(_)));
        assert!(
            err.to_string()
                .contains("evidence crosses Owner boundary or does not exist")
        );

        let motivated_edges: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint
               FROM proxima_core.edges
              WHERE relation = $1
                AND target_memory_id = $2",
        )
        .bind(CORE_MOTIVATED_BY_RELATION)
        .bind(evidence_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            motivated_edges.0, 0,
            "rejected no-home evidence must not receive a motivated-by edge"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("no-home evidence rejection test failed");
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
        let registry = FlavorRegistry::new().freeze();

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
                .fetch_one(pg.pool())
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
        .fetch_one(pg.pool())
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
                .fetch_one(pg.pool())
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
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(activations.0, 2);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal abandon/resume transition test failed");
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
        let registry = FlavorRegistry::new().freeze();
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
        let evidence = vec![proxima_core::verbs::goal_write::GoalEvidenceRef {
            memory_id: evidence_id,
        }];
        let registry = FlavorRegistry::new().freeze();
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
                request_id: IdempotencyKey::new("req-decompose-child-1")
                    .expect("valid idempotency key"),
            },
            ChildGoalDraft {
                payload: replacement_payload("Child two", "Child two text", b"{}"),
                evidence: evidence.clone(),
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
                "SELECT count(*)::bigint FROM proxima_core.goal_parents
                 WHERE goal_id = $1 AND parent_goal_id = $2",
            )
            .bind(child.outcome.goal_id.into_inner())
            .bind(parent.goal_id.into_inner())
            .fetch_one(pg.pool())
            .await?;
            assert_eq!(parents.0, 1);
        }

        let replay_children = vec![
            ChildGoalDraft {
                payload: replacement_payload("Child one", "Child one text", b"{}"),
                evidence: evidence.clone(),
                request_id: IdempotencyKey::new("req-decompose-child-1")
                    .expect("valid idempotency key"),
            },
            ChildGoalDraft {
                payload: replacement_payload("Child two", "Child two text", b"{}"),
                evidence,
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
        let registry = FlavorRegistry::new().freeze();
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
        cross_parent_child.parent_goal_ids = vec![parent.goal_id];
        let err = create_goal(&pg, &registry, self_b, cross_parent_child)
            .await
            .expect_err("cross-owner parent edge rejected");
        assert!(err.to_string().contains("crosses Owner boundary"));

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
        let evidence = vec![proxima_core::verbs::goal_write::GoalEvidenceRef {
            memory_id: evidence_id,
        }];
        let registry = FlavorRegistry::new().freeze();
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

        let outcome = pg
            .modify_goal_atomic(&ModifyGoalAtomicRequest {
                owner,
                prior_goal_id: prior.goal_id,
                replacement: replacement.clone(),
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("req-modify").expect("valid idempotency key"),
                context: goal_context(&registry, self_id),
                evidence: Some(evidence.clone()),
            })
            .await?;
        assert!(!outcome.idempotent_replay);
        assert_ne!(outcome.goal_id, prior.goal_id);

        let row: (Option<Uuid>, GoalState, String, String, Vec<u8>) = sqlx::query_as(
            "SELECT supersedes, state, title, text, payload
             FROM proxima_core.goals WHERE goal_id = $1",
        )
        .bind(outcome.goal_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(row.0, Some(prior.goal_id.into_inner()));
        assert_eq!(row.1, GoalState::Active);
        assert_eq!(row.2, replacement.title);
        assert_eq!(row.3, replacement.text);
        assert_eq!(row.4, replacement.payload);

        let replay = pg
            .modify_goal_atomic(&ModifyGoalAtomicRequest {
                owner,
                prior_goal_id: prior.goal_id,
                replacement,
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("req-modify").expect("valid idempotency key"),
                context: goal_context(&registry, self_id),
                evidence: Some(evidence),
            })
            .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.goal_id, outcome.goal_id);
        assert_eq!(replay.lifecycle_memory_id, outcome.lifecycle_memory_id);
        let mut replay_edge_ids = replay.edge_ids;
        let mut outcome_edge_ids = outcome.edge_ids;
        replay_edge_ids.sort_unstable();
        outcome_edge_ids.sort_unstable();
        assert_eq!(replay_edge_ids, outcome_edge_ids);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal modify atom test failed");
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
        registry.add_goal_schema::<TestCustomGoalPayload>();
        let registry = registry.freeze();
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
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(goals.0, 1);

        let activated: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM proxima_core.goal_activated_v1 WHERE goal_id = $1",
        )
        .bind(outcome.goal_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(activated.0, 1);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("registry-generic goal payload test failed");
}
