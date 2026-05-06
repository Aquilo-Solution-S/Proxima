//! End-to-end `EventHistory` verb test against a transient PG database.

mod common;

use common::{create_db, db_url, drop_db};
use std::sync::Arc;

use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::engine::Engine;
use proxima_core::storage::Storage;
use proxima_core::verbs::event_history::EventHistoryRequest;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo, SchemaRegistry};
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

fn fresh_event_draft(owner: Owner, payload: Vec<u8>) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner,
        schema_id: SchemaId::new("test/fact_blob".into()),
        schema_version: SchemaVersion::new(1),
        payload,
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
async fn event_history_returns_owner_scoped_newest_first() {
    let db_name = format!("proxima_test_eh_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return;
    }
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        pg.start_outbox().await?;

        let storage: Arc<dyn Storage> = Arc::new(pg.clone());
        let user1 = UserId::new(Uuid::now_v7());
        let user2 = UserId::new(Uuid::now_v7());
        let owner1 = Owner {
            principal: Principal::User(user1),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let owner2 = Owner {
            principal: Principal::User(user2),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let engine1 = build_engine(storage.clone(), owner1.clone(), Principal::User(user1));
        let engine2 = build_engine(storage, owner2.clone(), Principal::User(user2));

        for body in [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()] {
            engine1
                .event_ingest(&Credentials::None, fresh_event_draft(owner1.clone(), body))
                .await?;
        }
        engine2
            .event_ingest(
                &Credentials::None,
                fresh_event_draft(owner2.clone(), b"z".to_vec()),
            )
            .await?;

        let resp1 = engine1
            .event_history(
                &Credentials::None,
                &EventHistoryRequest {
                    owner: owner1.clone(),
                    limit: 100,
                    before: None,
                },
            )
            .await?;
        assert_eq!(resp1.events.len(), 3, "owner1 should see its events only");
        assert!(
            resp1.events[0].seq > resp1.events[1].seq && resp1.events[1].seq > resp1.events[2].seq,
            "events must be newest-first",
        );
        assert_eq!(
            resp1.seq_high_water,
            Some(resp1.events[0].seq),
            "seq_high_water is the newest event seq for that owner",
        );

        let resp2 = engine2
            .event_history(
                &Credentials::None,
                &EventHistoryRequest {
                    owner: owner2.clone(),
                    limit: 100,
                    before: None,
                },
            )
            .await?;
        assert_eq!(resp2.events.len(), 1, "owner2 is isolated from owner1");

        let page1 = engine1
            .event_history(
                &Credentials::None,
                &EventHistoryRequest {
                    owner: owner1.clone(),
                    limit: 2,
                    before: None,
                },
            )
            .await?;
        assert_eq!(page1.events.len(), 2);

        let page2 = engine1
            .event_history(
                &Credentials::None,
                &EventHistoryRequest {
                    owner: owner1,
                    limit: 2,
                    before: Some(page1.events[1].seq),
                },
            )
            .await?;
        assert_eq!(page2.events.len(), 1, "oldest event remains");
        assert!(page2.events[0].seq < page1.events[1].seq);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("event_history_returns_owner_scoped_newest_first failed");
}
