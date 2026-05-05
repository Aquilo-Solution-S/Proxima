//! End-to-end `GoalWrite` against a transient PG database.

use std::sync::Arc;

use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::engine::Engine;
use proxima_core::error::ErrorCode;
use proxima_core::storage::Storage;
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo, SchemaRegistry};
use proxima_core::{GoalId, OrgId, Owner, Principal, SchemaId, SchemaVersion, UserId};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://postgres@localhost/postgres";

async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

fn schemas_for_test() -> Vec<SchemaInfo> {
    vec![
        SchemaInfo {
            schema_id: SchemaId::new("test/goal_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::Goal,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            cbor_encoder: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/fact_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::Fact,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            cbor_encoder: None,
        },
    ]
}

fn fresh_draft(owner: &Owner, request_id: String) -> GoalDraft {
    GoalDraft {
        owner: owner.clone(),
        schema_id: SchemaId::new("test/goal_blob".into()),
        schema_version: SchemaVersion::new(1),
        text: "Test goal text".to_string(),
        state: GoalState::Active,
        parent_goal_ids: vec![],
        authorship: GoalAuthorship::User,
        request_id,
    }
}

fn draft_with_parent(owner: &Owner, request_id: String, parent: GoalId) -> GoalDraft {
    GoalDraft {
        owner: owner.clone(),
        schema_id: SchemaId::new("test/goal_blob".into()),
        schema_version: SchemaVersion::new(1),
        text: "Test goal with parent".to_string(),
        state: GoalState::Active,
        parent_goal_ids: vec![parent],
        authorship: GoalAuthorship::User,
        request_id,
    }
}

#[tokio::test]
async fn goal_write_writes_goal_and_change_event() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return;
    }
    let url = format!("postgres://postgres@localhost/{db_name}");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = SchemaRegistry::with_schemas(schemas_for_test());
        let engine = Engine::new(
            registry,
            MemoryStore::new(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        )
        .with_storage(storage);

        let draft = fresh_draft(&owner, "req-1".to_string());

        // Happy path: write_goal with User authorship.
        let outcome = engine.write_goal(&Credentials::None, draft.clone()).await?;
        assert!(!outcome.idempotent_replay);

        // Idempotent replay with same request_id and body.
        let replay = engine.write_goal(&Credentials::None, draft.clone()).await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.goal_id, outcome.goal_id);

        // Idempotency conflict: same request_id but different text.
        let mut mutated = draft.clone();
        mutated.text = "Different text".to_string();
        let err = engine
            .write_goal(&Credentials::None, mutated)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::IdempotencyConflict);

        // Schema rejection: use a Fact schema for a Goal write.
        let mut bad_schema = draft.clone();
        bad_schema.schema_id = SchemaId::new("test/fact_blob".into());
        let err = engine
            .write_goal(&Credentials::None, bad_schema)
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
async fn goal_supersede_writes_new_goal() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return;
    }
    let url = format!("postgres://postgres@localhost/{db_name}");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = SchemaRegistry::with_schemas(schemas_for_test());
        let engine = Engine::new(
            registry,
            MemoryStore::new(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        )
        .with_storage(storage);

        // Write initial goal.
        let draft = fresh_draft(&owner, "req-1".to_string());
        let outcome = engine.write_goal(&Credentials::None, draft.clone()).await?;
        let prior_goal_id = outcome.goal_id;

        // Supersede with Paused state.
        let supersede_draft = GoalDraft {
            owner: owner.clone(),
            schema_id: SchemaId::new("test/goal_blob".into()),
            schema_version: SchemaVersion::new(1),
            text: "Updated goal text".to_string(),
            state: GoalState::Paused,
            parent_goal_ids: vec![],
            authorship: GoalAuthorship::User,
            request_id: "req-2".to_string(),
        };

        let supersede_outcome = engine
            .supersede_goal(&Credentials::None, prior_goal_id, supersede_draft.clone())
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

        // Idempotent replay of supersede.
        let replay = engine
            .supersede_goal(&Credentials::None, prior_goal_id, supersede_draft.clone())
            .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.goal_id, supersede_outcome.goal_id);

        // Counts: 2 goals, 2 change_event rows.
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
    result.expect("goal_supersede_pg test failed");
}

#[tokio::test]
async fn goal_write_with_parent() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return;
    }
    let url = format!("postgres://postgres@localhost/{db_name}");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = SchemaRegistry::with_schemas(schemas_for_test());
        let engine = Engine::new(
            registry,
            MemoryStore::new(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        )
        .with_storage(storage);

        // Write parent goal.
        let parent_draft = fresh_draft(&owner, "req-parent".to_string());
        let parent_outcome = engine.write_goal(&Credentials::None, parent_draft).await?;
        let parent_id = parent_outcome.goal_id;

        // Write child goal with parent.
        let child_draft = draft_with_parent(&owner, "req-child".to_string(), parent_id);
        let child_outcome = engine.write_goal(&Credentials::None, child_draft).await?;
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
