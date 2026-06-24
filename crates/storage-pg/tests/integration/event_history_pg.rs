//! End-to-end `EventHistory` verb test against a transient PG database.

use crate::common::{create_db, db_url, drop_db};
use std::sync::Arc;

use proxima_core::engine::Engine;
use proxima_core::storage::Storage;
use proxima_core::verbs::event_history::EventHistoryRequest;
use proxima_core::verbs::event_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, EventDraft,
};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{Owner, Principal, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId};
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
            has_typed_ingress: false,
            cited_object_schema: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/cited_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::CitedObject,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            tombstone: None,
            has_typed_ingress: false,
            cited_object_schema: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/citation_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::CitationMapping,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            tombstone: None,
            has_typed_ingress: false,
            cited_object_schema: None,
        },
    ]
}

fn fresh_event_draft(owner: Owner, payload: Vec<u8>) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: owner,
        author_personality_instance_id: None,
        schema_id: SchemaId::new("test/fact_blob".into()),
        schema_version: SchemaVersion::new(1),
        payload,
        rendered_text: None,
        observed_at: now,
        occurred_at: now,
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new("test/cited_blob".into()),
                schema_version: SchemaVersion::new(1),
                content_hash: [42u8; 32],
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new("test/citation_blob".into()),
                schema_version: SchemaVersion::new(1),
            },
        }),
    }
}

fn build_engine(storage: Arc<dyn Storage>, _owner: Owner, _principal: Principal) -> Engine {
    let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
    Engine::new(registry).with_storage(storage)
}

#[tokio::test]
async fn event_history_returns_owner_scoped_newest_first() {
    let db_name = format!("proxima_test_eh_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        let storage: Arc<dyn Storage> = Arc::new(pg.clone());
        let user1 = UserId::new(Uuid::now_v7());
        let user2 = UserId::new(Uuid::now_v7());
        let owner1 = Principal::User(user1);
        let owner2 = Principal::User(user2);
        let engine1 = build_engine(storage.clone(), owner1.clone(), Principal::User(user1));
        let engine2 = build_engine(storage, owner2.clone(), Principal::User(user2));
        let authz1 =
            proxima_core::AuthzContext::single_owner(&owner1, proxima_core::AuthPath::System);
        let authz2 =
            proxima_core::AuthzContext::single_owner(&owner2, proxima_core::AuthPath::System);

        for body in [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()] {
            engine1
                .event_ingest(&authz1, fresh_event_draft(owner1.clone(), body))
                .await?;
        }
        engine2
            .event_ingest(&authz2, fresh_event_draft(owner2.clone(), b"z".to_vec()))
            .await?;

        let resp1 = engine1
            .event_history(
                &authz1,
                &EventHistoryRequest {
                    principal: owner1.clone(),
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
                &authz2,
                &EventHistoryRequest {
                    principal: owner2.clone(),
                    limit: 100,
                    before: None,
                },
            )
            .await?;
        assert_eq!(resp2.events.len(), 1, "owner2 is isolated from owner1");

        let page1 = engine1
            .event_history(
                &authz1,
                &EventHistoryRequest {
                    principal: owner1.clone(),
                    limit: 2,
                    before: None,
                },
            )
            .await?;
        assert_eq!(page1.events.len(), 2);

        let page2 = engine1
            .event_history(
                &authz1,
                &EventHistoryRequest {
                    principal: owner1,
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
