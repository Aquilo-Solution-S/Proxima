//! `CloseBatch` integration tests against a transient PG database.
//!
//! Covers: open-then-close, idempotent re-close, cross-owner reject,
//! `NotFound` for unknown batches.

use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::engine::Engine;
use proxima_core::error::ErrorCode;
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo, SchemaRegistry};
use proxima_core::{
    OrgId, Owner, Principal, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection};
use std::sync::Arc;
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
    ]
}

fn fresh_draft(owner: Owner, source_batch_id: SourceBatchId) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/source"),
        source_batch_id,
        owner,
        schema_id: SchemaId::new("test/fact_blob".into()),
        schema_version: SchemaVersion::new(1),
        payload: format!("payload-{}", Uuid::now_v7()).into_bytes(),
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
async fn close_batch_idempotent_and_owner_scoped() {
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

        let user_a = UserId::new(Uuid::now_v7());
        let owner_a = Owner {
            principal: Principal::User(user_a),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let user_b = UserId::new(Uuid::now_v7());
        let owner_b = Owner {
            principal: Principal::User(user_b),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let engine_a = Engine::new(
            SchemaRegistry::with_schemas(schemas_for_test()),
            MemoryStore::new(),
            Box::new(NoAuth::new(Principal::User(user_a), owner_a.clone())),
        )
        .with_storage(storage.clone());
        let engine_b = Engine::new(
            SchemaRegistry::with_schemas(schemas_for_test()),
            MemoryStore::new(),
            Box::new(NoAuth::new(Principal::User(user_b), owner_b.clone())),
        )
        .with_storage(storage);

        // Open a batch by ingesting one event under owner A.
        let batch_id = SourceBatchId::new(Uuid::now_v7());
        let draft = fresh_draft(owner_a.clone(), batch_id);
        engine_a.event_ingest(&Credentials::None, draft).await?;

        // Initial close.
        let outcome = engine_a
            .close_batch(&Credentials::None, owner_a.clone(), batch_id)
            .await?;
        assert!(!outcome.already_closed);
        let first_closed_at = outcome.closed_at;

        // Re-close — idempotent, same closed_at, already_closed=true.
        let replay = engine_a
            .close_batch(&Credentials::None, owner_a.clone(), batch_id)
            .await?;
        assert!(replay.already_closed);
        assert_eq!(replay.closed_at, first_closed_at);

        // Cross-owner: B sees the batch as NotFound (information leak guard).
        let cross = engine_b
            .close_batch(&Credentials::None, owner_b.clone(), batch_id)
            .await
            .unwrap_err();
        assert_eq!(cross.code, ErrorCode::NotFound);

        // Unknown batch under correct owner: NotFound.
        let nope = SourceBatchId::new(Uuid::now_v7());
        let missing = engine_a
            .close_batch(&Credentials::None, owner_a.clone(), nope)
            .await
            .unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);

        // SQL probe: closed_at is not NULL on the row.
        let (closed_at,): (Option<time::OffsetDateTime>,) =
            sqlx::query_as("SELECT closed_at FROM proxima_core.source_batches WHERE id = $1")
                .bind(batch_id.into_inner())
                .fetch_one(pg.pool())
                .await?;
        assert!(closed_at.is_some());

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("close_batch_pg test failed");
}
