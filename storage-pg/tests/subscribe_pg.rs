//! End-to-end subscribe verb test against a transient PG database.

use std::sync::Arc;
use std::time::Duration;

use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::engine::Engine;
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo, SchemaRegistry};
use proxima_core::verbs::subscribe::SubscribeRequest;
use proxima_core::{
    ChangeEventKind, EntityKind, EntityRef, OrgId, Owner, Principal, SchemaId, SchemaVersion,
    SourceBatchId, SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection};
use tokio_stream::StreamExt;
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
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/cited_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::CitedObject,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/citation_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::CitationMapping,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/goal_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::Goal,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
        },
    ]
}

fn fresh_event_draft(owner: Owner) -> EventDraft {
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

fn fresh_goal_draft(owner: &Owner, request_id: String) -> GoalDraft {
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

fn build_engine(storage: Arc<dyn Storage>, owner: Owner, principal: Principal) -> Engine {
    let registry = SchemaRegistry::with_schemas(schemas_for_test());
    Engine::new(
        registry,
        MemoryStore::new(),
        Box::new(NoAuth::new(principal, owner)),
    )
    .with_storage(storage)
}

#[tokio::test]
async fn subscribe_fresh_no_since_live_ingest() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return;
    }
    let url = format!("postgres://postgres@localhost/{db_name}");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        pg.start_outbox().await?;

        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let engine = build_engine(storage.clone(), owner.clone(), Principal::User(user));

        // Subscribe with no since cursor.
        let req = SubscribeRequest {
            owner: owner.clone(),
            since: None,
        };
        let mut stream = engine.subscribe(&Credentials::None, req).await?;

        // Ingest a Fact.
        let draft = fresh_event_draft(owner.clone());
        let outcome = engine
            .event_ingest(&Credentials::None, draft.clone())
            .await?;

        // Pull one item from the stream with a 3s timeout.
        let ce = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await?
            .expect("expected ChangeEvent");

        // Assert it matches the ingested Fact.
        assert_eq!(ce.seq, outcome.change_event_seq);
        assert_eq!(ce.owner.principal, owner.principal);
        assert_eq!(ce.owner.org_id, owner.org_id);

        match &ce.kind {
            ChangeEventKind::EntityAppend {
                entity_kind,
                entity,
                schema_id,
                schema_version,
                supersedes,
            } => {
                assert_eq!(*entity_kind, EntityKind::Fact);
                assert_eq!(*entity, EntityRef::Memory(outcome.memory_id));
                assert_eq!(*schema_id, draft.schema_id);
                assert_eq!(*schema_version, draft.schema_version);
                assert!(supersedes.is_none());
            }
        }

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("subscribe_fresh_no_since_live_ingest test failed");
}

#[tokio::test]
async fn subscribe_resume_with_since_mid() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return;
    }
    let url = format!("postgres://postgres@localhost/{db_name}");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        pg.start_outbox().await?;

        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let engine = build_engine(storage.clone(), owner.clone(), Principal::User(user));

        // Ingest event A.
        let draft_a = fresh_event_draft(owner.clone());
        let outcome_a = engine
            .event_ingest(&Credentials::None, draft_a.clone())
            .await?;

        // Ingest event B (a Goal write).
        let draft_b = fresh_goal_draft(&owner, "req-1".to_string());
        let outcome_b = engine
            .write_goal(&Credentials::None, draft_b.clone())
            .await?;

        // Subscribe with since: Some(A.change_event_seq).
        let req = SubscribeRequest {
            owner: owner.clone(),
            since: Some(outcome_a.change_event_seq),
        };
        let mut stream = engine.subscribe(&Credentials::None, req).await?;

        // Pull one item with a 3s timeout. Assert it's the Goal — A is
        // gone (it's at-or-before the cursor).
        let ce = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await?
            .expect("expected ChangeEvent");

        assert_eq!(ce.seq, outcome_b.change_event_seq);
        match &ce.kind {
            ChangeEventKind::EntityAppend { entity_kind, .. } => {
                assert_eq!(*entity_kind, EntityKind::Goal);
            }
        }

        // Ingest event C (another Fact).
        let draft_c = fresh_event_draft(owner.clone());
        let outcome_c = engine
            .event_ingest(&Credentials::None, draft_c.clone())
            .await?;

        // Pull next item; assert C.
        let ce = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await?
            .expect("expected ChangeEvent");

        assert_eq!(ce.seq, outcome_c.change_event_seq);
        match &ce.kind {
            ChangeEventKind::EntityAppend { entity_kind, .. } => {
                assert_eq!(*entity_kind, EntityKind::Fact);
            }
        }

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("subscribe_resume_with_since_mid test failed");
}

#[tokio::test]
async fn subscribe_owner_isolation() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return;
    }
    let url = format!("postgres://postgres@localhost/{db_name}");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        pg.start_outbox().await?;

        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user1 = UserId::new(Uuid::now_v7());
        let owner1 = Owner {
            principal: Principal::User(user1),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let user2 = UserId::new(Uuid::now_v7());
        let owner2 = Owner {
            principal: Principal::User(user2),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        // Distinct engines per owner because NoAuth is scoped to one Owner.
        let engine1 = build_engine(storage.clone(), owner1.clone(), Principal::User(user1));
        let engine2 = build_engine(storage.clone(), owner2.clone(), Principal::User(user2));

        // Subscribe as Owner1 BEFORE any ingest, so backfill is empty and
        // the test isolates the live-side owner filter.
        let req = SubscribeRequest {
            owner: owner1.clone(),
            since: None,
        };
        let mut stream = engine1.subscribe(&Credentials::None, req).await?;

        // Owner1 ingest — should land on the stream.
        let draft1 = fresh_event_draft(owner1.clone());
        let outcome1 = engine1
            .event_ingest(&Credentials::None, draft1.clone())
            .await?;

        // Owner2 ingest — must NOT land on Owner1's stream (filter drops it).
        let draft2 = fresh_event_draft(owner2.clone());
        let _ = engine2
            .event_ingest(&Credentials::None, draft2.clone())
            .await?;

        // First pull: Owner1's event.
        let ce = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await?
            .expect("expected ChangeEvent");
        assert_eq!(ce.seq, outcome1.change_event_seq);
        assert_eq!(ce.owner.principal, owner1.principal);

        // Owner2's event was emitted before we read here; if it leaked
        // through, .next() would return it within the 1s window.
        let next_result = tokio::time::timeout(Duration::from_secs(1), stream.next()).await;
        assert!(
            next_result.is_err(),
            "expected timeout (Owner2's event filtered out); got {next_result:?}",
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("subscribe_owner_isolation test failed");
}
