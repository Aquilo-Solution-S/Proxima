//! End-to-end `GoalWrite` against a transient PG database.

use crate::common::{create_db, db_url, drop_db};
use std::sync::Arc;

use proxima_core::engine::Engine;
use proxima_core::error::ErrorCode;
use proxima_core::storage::Storage;
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{GoalId, OrgId, Owner, Principal, SchemaId, SchemaVersion, UserId};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

fn schemas_for_test() -> Vec<SchemaInfo> {
    vec![
        SchemaInfo {
            schema_id: SchemaId::new("test/goal_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::Goal,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            tombstone: None,
            json_encoder: None,
            sidecar_inserter: None,
            cited_object_schema: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/fact_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::Fact,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            tombstone: None,
            json_encoder: None,
            sidecar_inserter: None,
            cited_object_schema: None,
        },
    ]
}

fn fresh_draft(owner: &Owner, request_id: String) -> GoalDraft {
    GoalDraft {
        principal: owner.principal.clone(),
        org_id: Some(owner.org_id),
        schema_id: SchemaId::new("test/goal_blob".into()),
        schema_version: SchemaVersion::new(1),
        title: "Test goal".to_string(),
        text: "Test goal text".to_string(),
        payload: br#"{"goal":"fresh"}"#.to_vec(),
        state: GoalState::Active,
        parent_goal_ids: vec![],
        supersedes_goal_id: None,
        authorship: GoalAuthorship::User,
        request_id,
    }
}

fn draft_with_parent(owner: &Owner, request_id: String, parent: GoalId) -> GoalDraft {
    GoalDraft {
        principal: owner.principal.clone(),
        org_id: Some(owner.org_id),
        schema_id: SchemaId::new("test/goal_blob".into()),
        schema_version: SchemaVersion::new(1),
        title: "Test goal".to_string(),
        text: "Test goal with parent".to_string(),
        payload: br#"{"goal":"child"}"#.to_vec(),
        state: GoalState::Active,
        parent_goal_ids: vec![parent],
        supersedes_goal_id: None,
        authorship: GoalAuthorship::User,
        request_id,
    }
}

#[tokio::test]
async fn goal_write_writes_goal_and_change_event() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry, MemoryStore::new()).with_storage(storage);

        let draft = fresh_draft(&owner, "req-1".to_string());

        // Happy path: write_goal with User authorship.
        let outcome = engine
            .write_goal(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                draft.clone(),
            )
            .await?;
        assert!(!outcome.idempotent_replay);

        let payload: Vec<u8> =
            sqlx::query_scalar("SELECT payload FROM proxima_core.goals WHERE goal_id = $1")
                .bind(outcome.goal_id.into_inner())
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(payload, draft.payload);

        // Idempotent replay with same request_id and body.
        let replay = engine
            .write_goal(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                draft.clone(),
            )
            .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.goal_id, outcome.goal_id);

        // Idempotency conflict: same request_id but different text.
        let mut mutated = draft.clone();
        mutated.text = "Different text".to_string();
        let err = engine
            .write_goal(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                mutated,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::IdempotencyConflict);

        // Idempotency conflict: same request_id but different payload.
        let mut mutated_payload = draft.clone();
        mutated_payload.payload = br#"{"goal":"different"}"#.to_vec();
        let err = engine
            .write_goal(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                mutated_payload,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::IdempotencyConflict);

        // Schema rejection: use a Fact schema for a Goal write.
        let mut bad_schema = draft.clone();
        bad_schema.schema_id = SchemaId::new("test/fact_blob".into());
        let err = engine
            .write_goal(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                bad_schema,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::UnknownSchema);

        // Counts: 1 goal, 1 change_event.
        let goals: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goals")
            .fetch_one(pg.pool())
            .await?;
        assert_eq!(goals.0, 1);

        let change: (i64,) =
            sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.change_event")
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(change.0, 1);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal_write_pg test failed");
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "linear supersession storage fixture")]
async fn goal_supersede_writes_new_goal() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry, MemoryStore::new()).with_storage(storage);

        // Write initial goal.
        let draft = fresh_draft(&owner, "req-1".to_string());
        let outcome = engine
            .write_goal(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                draft.clone(),
            )
            .await?;
        let prior_goal_id = outcome.goal_id;

        // Supersede with Paused state.
        let supersede_draft = GoalDraft {
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
            schema_id: SchemaId::new("test/goal_blob".into()),
            schema_version: SchemaVersion::new(1),
            title: "Test goal".to_string(),
            text: "Updated goal text".to_string(),
            payload: br#"{"goal":"updated"}"#.to_vec(),
            state: GoalState::Paused,
            parent_goal_ids: vec![],
            supersedes_goal_id: None,
            authorship: GoalAuthorship::User,
            request_id: "req-2".to_string(),
        };

        let supersede_outcome = engine
            .supersede_goal(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                prior_goal_id,
                supersede_draft.clone(),
            )
            .await?;
        assert!(!supersede_outcome.idempotent_replay);
        assert_ne!(supersede_outcome.goal_id, prior_goal_id);

        // Verify the new goal has supersedes pointing to prior.
        let supersedes: Option<Uuid> =
            sqlx::query_scalar("SELECT supersedes FROM proxima_core.goals WHERE goal_id = $1")
                .bind(supersede_outcome.goal_id.into_inner())
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(supersedes, Some(prior_goal_id.into_inner()));

        let payload: Vec<u8> =
            sqlx::query_scalar("SELECT payload FROM proxima_core.goals WHERE goal_id = $1")
                .bind(supersede_outcome.goal_id.into_inner())
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(payload, supersede_draft.payload);

        // Idempotent replay of supersede.
        let replay = engine
            .supersede_goal(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                prior_goal_id,
                supersede_draft.clone(),
            )
            .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.goal_id, supersede_outcome.goal_id);

        let direct_prior = engine
            .write_goal(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                fresh_draft(&owner, "req-direct-prior".to_string()),
            )
            .await?;
        let mut direct_draft = fresh_draft(&owner, "req-direct-supersede".to_string());
        direct_draft.text = "Direct supersede".to_string();
        direct_draft.supersedes_goal_id = Some(direct_prior.goal_id);
        let direct = engine
            .write_goal(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                direct_draft.clone(),
            )
            .await?;
        let direct_supersedes: Option<Uuid> =
            sqlx::query_scalar("SELECT supersedes FROM proxima_core.goals WHERE goal_id = $1")
                .bind(direct.goal_id.into_inner())
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(direct_supersedes, Some(direct_prior.goal_id.into_inner()));

        // Counts: explicit supersede path + direct draft.supersedes_goal_id path.
        let goals: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goals")
            .fetch_one(pg.pool())
            .await?;
        assert_eq!(goals.0, 4);

        let change: (i64,) =
            sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.change_event")
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(change.0, 4);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal_supersede_pg test failed");
}

#[tokio::test]
async fn goal_write_with_parent() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry, MemoryStore::new()).with_storage(storage);

        // Write parent goal.
        let parent_draft = fresh_draft(&owner, "req-parent".to_string());
        let parent_outcome = engine
            .write_goal(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                parent_draft,
            )
            .await?;
        let parent_id = parent_outcome.goal_id;

        // Write child goal with parent.
        let child_draft = draft_with_parent(&owner, "req-child".to_string(), parent_id);
        let child_outcome = engine
            .write_goal(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                child_draft,
            )
            .await?;
        assert!(!child_outcome.idempotent_replay);

        // Verify goal_parents row exists.
        let parents: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM proxima_core.goal_parents WHERE goal_id = $1",
        )
        .bind(child_outcome.goal_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(parents.0, 1);

        // Counts: 2 goals, 2 change_event rows, 1 goal_parents.
        let goals: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goals")
            .fetch_one(pg.pool())
            .await?;
        assert_eq!(goals.0, 2);

        let change: (i64,) =
            sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.change_event")
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(change.0, 2);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal_write_with_parent_pg test failed");
}

/// Both layers reject a goal payload that is empty or not a JSON object:
/// the engine `write_goal` guard (symmetric with `EventIngest`) and the
/// `goals_payload_nonempty_chk` DB constraint as the last line of defense.
#[tokio::test]
async fn goal_write_rejects_empty_or_non_object_payload() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry, MemoryStore::new()).with_storage(storage);
        let authz =
            proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System);

        // Engine guard: a zero-byte payload is rejected as InvalidArgument.
        let mut empty = fresh_draft(&owner, "req-empty".to_string());
        empty.payload = Vec::new();
        let err = engine.write_goal(&authz, empty).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);

        // Engine guard: valid JSON that is not an object is rejected too.
        let mut scalar = fresh_draft(&owner, "req-scalar".to_string());
        scalar.payload = b"123".to_vec();
        let err = engine.write_goal(&authz, scalar).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);

        // Nothing reached storage.
        let goals: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goals")
            .fetch_one(pg.pool())
            .await?;
        assert_eq!(goals.0, 0);

        // Last line of defense: the storage verb bypasses the engine guard,
        // but goals_payload_nonempty_chk still rejects a zero-byte payload.
        let mut raw = fresh_draft(&owner, "req-raw".to_string());
        raw.payload = Vec::new();
        assert!(
            pg.write_goal_atomic(&raw).await.is_err(),
            "DB CHECK must reject a zero-byte payload"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal_write empty-payload rejection test failed");
}
