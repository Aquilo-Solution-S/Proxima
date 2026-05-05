//! End-to-end `Query` against a transient PG database.

use std::sync::Arc;

use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::engine::Engine;
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState};
use proxima_core::verbs::query::{EntityKind, MemoryStore, QueryRequest, SupersessionStatus};
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo, SchemaRegistry};
use proxima_core::{
    OrgId, Owner, Principal, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
};
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
            schema_id: SchemaId::new("test/fact_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::Fact,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            cbor_encoder: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/fact_blob_v2".into()),
            schema_version: SchemaVersion::new(2),
            kind: PayloadKind::Fact,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            cbor_encoder: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/cited_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::CitedObject,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            cbor_encoder: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/citation_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::CitationMapping,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            cbor_encoder: None,
        },
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
            schema_id: SchemaId::new("test/goal_blob_v2".into()),
            schema_version: SchemaVersion::new(2),
            kind: PayloadKind::Goal,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            cbor_encoder: None,
        },
    ]
}

fn fresh_draft(owner: Owner) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner,
        schema_id: SchemaId::new("test/fact_blob".into()),
        schema_version: SchemaVersion::new(1),
        payload: b"hello world".to_vec(),
        observed_at: now,
        occurred_at: now,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new("test/cited_blob".into()),
            schema_version: SchemaVersion::new(1),
            content_hash: [42u8; 32],
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new("test/citation_blob".into()),
            schema_version: SchemaVersion::new(1),
        },
    }
}

#[tokio::test]
async fn query_returns_stored_schema_version() {
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

        let mut draft = fresh_draft(owner.clone());
        draft.schema_id = SchemaId::new("test/fact_blob_v2".into());
        draft.schema_version = SchemaVersion::new(2);
        engine.event_ingest(&Credentials::None, draft).await?;

        let resp = engine
            .query(&Credentials::None, &QueryRequest::for_owner(owner))
            .await?;

        assert_eq!(resp.memories.len(), 1);
        assert_eq!(
            resp.memories[0].schema_id,
            SchemaId::new("test/fact_blob_v2".into())
        );
        assert_eq!(resp.memories[0].schema_version, SchemaVersion::new(2));

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_returns_stored_schema_version test failed");
}

#[tokio::test]
async fn query_returns_fact_rows() {
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

        // Ingest two distinct Facts.
        let draft1 = fresh_draft(owner.clone());
        let draft2 = {
            let mut d = fresh_draft(owner.clone());
            d.payload = b"another fact".to_vec();
            d.source_batch_id = SourceBatchId::new(Uuid::now_v7());
            d
        };

        let outcome1 = engine
            .event_ingest(&Credentials::None, draft1.clone())
            .await?;
        let outcome2 = engine
            .event_ingest(&Credentials::None, draft2.clone())
            .await?;

        // Query for all memories for this owner.
        let req = QueryRequest::for_owner(owner.clone());
        let resp = engine.query(&Credentials::None, &req).await?;

        assert_eq!(resp.memories.len(), 2);
        for m in &resp.memories {
            assert_eq!(m.kind, EntityKind::Fact);
            assert_eq!(m.schema_id, SchemaId::new("test/fact_blob".into()));
            assert_eq!(m.owner, owner);
        }

        // seq_high_water should be Some and equal to the greater of the two seqs.
        assert!(resp.seq_high_water.is_some());
        let expected_max = std::cmp::max(outcome1.change_event_seq, outcome2.change_event_seq);
        assert_eq!(resp.seq_high_water, Some(expected_max));

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_returns_fact_rows test failed");
}

#[tokio::test]
async fn query_owner_scope_ignores_org_id() {
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
        let stored_owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let requested_owner = Owner {
            principal: stored_owner.principal.clone(),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = SchemaRegistry::with_schemas(schemas_for_test());
        let engine = Engine::new(
            registry,
            MemoryStore::new(),
            Box::new(NoAuth::new(Principal::User(user), stored_owner.clone())),
        )
        .with_storage(storage);

        let draft = fresh_draft(stored_owner.clone());
        let outcome = engine
            .event_ingest(&Credentials::None, draft.clone())
            .await?;

        let resp = engine
            .query(
                &Credentials::None,
                &QueryRequest::for_owner(requested_owner),
            )
            .await?;

        assert_eq!(resp.memories.len(), 1);
        assert_eq!(resp.memories[0].owner, stored_owner);
        assert_eq!(resp.seq_high_water, Some(outcome.change_event_seq));

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_owner_scope_ignores_org_id test failed");
}

#[tokio::test]
async fn query_filter_abstraction_returns_empty() {
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

        // Ingest a Fact.
        let draft = fresh_draft(owner.clone());
        engine.event_ingest(&Credentials::None, draft).await?;

        // Query with entity_kind = Abstraction filter.
        let req = QueryRequest {
            owner: owner.clone(),
            entity_kind: Some(EntityKind::Abstraction),
            schema_id: None,
            supersession: SupersessionStatus::HeadsOnly,
            limit: 100,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: None,
        };
        let resp = engine.query(&Credentials::None, &req).await?;

        assert!(resp.memories.is_empty());

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_filter_abstraction_returns_empty test failed");
}

#[tokio::test]
async fn query_goals_filter_by_schema_id() {
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

        // Write a goal under "test/goal_blob" v1.
        let goal_v1 = GoalDraft {
            owner: owner.clone(),
            schema_id: SchemaId::new("test/goal_blob".into()),
            schema_version: SchemaVersion::new(1),
            text: "v1 goal".to_string(),
            state: GoalState::Active,
            parent_goal_ids: vec![],
            authorship: GoalAuthorship::User,
            request_id: "req-v1".to_string(),
        };
        engine.write_goal(&Credentials::None, goal_v1).await?;

        // Filtering by a Fact schema_id must return zero goals.
        let req_fact_filter = QueryRequest {
            owner: owner.clone(),
            entity_kind: Some(EntityKind::Goal),
            schema_id: Some(SchemaId::new("test/fact_blob".into())),
            supersession: SupersessionStatus::HeadsOnly,
            limit: 100,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: None,
        };
        let resp = engine.query(&Credentials::None, &req_fact_filter).await?;
        assert!(
            resp.goals.is_empty(),
            "expected zero goals when filtering by Fact schema, got {}",
            resp.goals.len()
        );

        // Filtering by the matching goal schema_id returns the goal.
        let req_goal_filter = QueryRequest {
            owner: owner.clone(),
            entity_kind: Some(EntityKind::Goal),
            schema_id: Some(SchemaId::new("test/goal_blob".into())),
            supersession: SupersessionStatus::HeadsOnly,
            limit: 100,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: None,
        };
        let resp = engine.query(&Credentials::None, &req_goal_filter).await?;
        assert_eq!(resp.goals.len(), 1);

        // Filtering by a non-existent schema_id returns zero goals.
        let req_unknown = QueryRequest {
            owner,
            entity_kind: None,
            schema_id: Some(SchemaId::new("test/never_registered".into())),
            supersession: SupersessionStatus::HeadsOnly,
            limit: 100,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: None,
        };
        let resp = engine.query(&Credentials::None, &req_unknown).await?;
        assert!(resp.goals.is_empty());

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_goals_filter_by_schema_id test failed");
}

#[tokio::test]
async fn query_returns_stored_goal_schema_version() {
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

        // Write a goal under schema_version=2.
        let goal_v2 = GoalDraft {
            owner: owner.clone(),
            schema_id: SchemaId::new("test/goal_blob_v2".into()),
            schema_version: SchemaVersion::new(2),
            text: "v2 goal".to_string(),
            state: GoalState::Active,
            parent_goal_ids: vec![],
            authorship: GoalAuthorship::User,
            request_id: "req-v2".to_string(),
        };
        engine.write_goal(&Credentials::None, goal_v2).await?;

        let resp = engine
            .query(&Credentials::None, &QueryRequest::for_owner(owner))
            .await?;
        assert_eq!(resp.goals.len(), 1);
        assert_eq!(
            resp.goals[0].schema_id,
            SchemaId::new("test/goal_blob_v2".into())
        );
        assert_eq!(resp.goals[0].schema_version, SchemaVersion::new(2));

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_returns_stored_goal_schema_version test failed");
}

#[tokio::test]
async fn query_filter_nonexistent_schema_returns_empty() {
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

        // Ingest a Fact.
        let draft = fresh_draft(owner.clone());
        engine.event_ingest(&Credentials::None, draft).await?;

        // Query with non-existent schema_id filter.
        let req = QueryRequest {
            owner,
            entity_kind: None,
            schema_id: Some(SchemaId::new("test/non_existent".into())),
            supersession: SupersessionStatus::HeadsOnly,
            limit: 100,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: None,
        };
        let resp = engine.query(&Credentials::None, &req).await?;

        assert!(resp.memories.is_empty());

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_filter_nonexistent_schema_returns_empty test failed");
}
