//! End-to-end `EventIngest` against a transient PG database.

mod common;

use common::{create_db, db_url, drop_db};
use std::sync::Arc;

use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::engine::Engine;
use proxima_core::error::ErrorCode;
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{
    OrgId, Owner, Principal, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

fn schemas_for_test() -> Vec<SchemaInfo> {
    vec![
        SchemaInfo {
            schema_id: SchemaId::new("test/fact_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::Fact,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            tombstone: None,
            cbor_encoder: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/cited_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::CitedObject,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            tombstone: None,
            cbor_encoder: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/citation_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::CitationMapping,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            tombstone: None,
            cbor_encoder: None,
        },
    ]
}

fn fresh_draft(owner: Owner) -> EventDraft {
    // Event identity is blake3(source_id, owner, payload) — distinct
    // events need distinct payloads, not just distinct batch ids.
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner,
        schema_id: SchemaId::new("test/fact_blob".into()),
        schema_version: SchemaVersion::new(1),
        payload: format!("hello world {}", Uuid::now_v7()).into_bytes(),
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
async fn event_ingest_writes_fact_and_change_event() {
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
        let engine = Engine::new(
            registry,
            MemoryStore::new(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        )
        .with_storage(storage);

        let draft = fresh_draft(owner.clone());

        let outcome = engine
            .event_ingest(&Credentials::None, draft.clone())
            .await?;
        assert!(!outcome.idempotent_replay);

        let replay = engine
            .event_ingest(&Credentials::None, draft.clone())
            .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.memory_id, outcome.memory_id);

        // Schema check.
        let mut bad = draft.clone();
        bad.schema_id = SchemaId::new("test/unregistered".into());
        let err = engine
            .event_ingest(&Credentials::None, bad)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::UnknownSchema);

        // Counts.
        let memories: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.memories")
            .fetch_one(pg.pool())
            .await?;
        assert_eq!(memories.0, 1);

        let change: (i64,) =
            sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.change_event")
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(change.0, 1);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("event_ingest_pg test failed");
}

#[tokio::test]
async fn list_change_events_for_replay_respects_bounds_and_owner() {
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
        let other_owner = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(
            registry,
            MemoryStore::new(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        )
        .with_storage(storage);

        let first = engine
            .event_ingest(&Credentials::None, fresh_draft(owner.clone()))
            .await?;
        let second = engine
            .event_ingest(&Credentials::None, fresh_draft(owner.clone()))
            .await?;
        pg.ingest_event_atomic(&fresh_draft(other_owner.clone()))
            .await?;

        let rows = pg
            .list_change_events_for_replay(
                &owner,
                first.change_event_seq,
                Some(second.change_event_seq),
                10,
            )
            .await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event.seq, second.change_event_seq);
        assert_eq!(rows[0].event.owner, owner);

        let rows = pg
            .list_change_events_for_replay(&owner, Uuid::nil(), None, 1)
            .await?;
        assert_eq!(rows.len(), 1, "limit applies to replay scan");
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("replay scan bounds failed");
}
