//! `CloseBatch` integration tests against a transient PG database.
//!
//! Covers: open-then-close, idempotent re-close, cross-owner reject,
//! `NotFound` for unknown batches.

use crate::common::{create_db, db_url, drop_db};
use proxima_core::engine::Engine;
use proxima_core::error::ErrorCode;
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, EventDraft,
};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{
    OrgId, Owner, Principal, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use std::sync::Arc;
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
            protocol_ingress: None,
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
            protocol_ingress: None,
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
            protocol_ingress: None,
            cited_object_schema: None,
        },
    ]
}

fn fresh_draft(owner: Owner, source_batch_id: SourceBatchId) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/source"),
        source_batch_id,
        principal: owner.principal,
        org_id: Some(owner.org_id),
        author_personality_instance_id: None,
        schema_id: SchemaId::new("test/fact_blob".into()),
        schema_version: SchemaVersion::new(1),
        payload: format!("payload-{}", Uuid::now_v7()).into_bytes(),
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

#[tokio::test]
async fn close_batch_idempotent_and_owner_scoped() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

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

        let engine_a = Engine::new(FlavorRegistryFrozen::with_schemas(schemas_for_test()))
            .with_storage(storage.clone());
        let engine_b = Engine::new(FlavorRegistryFrozen::with_schemas(schemas_for_test()))
            .with_storage(storage);

        // Open a batch by ingesting one event under owner A.
        let batch_id = SourceBatchId::new(Uuid::now_v7());
        let draft = fresh_draft(owner_a.clone(), batch_id);
        engine_a
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner_a, proxima_core::AuthPath::System),
                draft,
            )
            .await?;

        // Initial close.
        let outcome = engine_a
            .close_batch(
                &proxima_core::AuthzContext::single_owner(&owner_a, proxima_core::AuthPath::System),
                owner_a.principal.clone(),
                batch_id,
            )
            .await?;
        assert!(!outcome.already_closed);
        let first_closed_at = outcome.closed_at;

        // Re-close — idempotent, same closed_at, already_closed=true.
        let replay = engine_a
            .close_batch(
                &proxima_core::AuthzContext::single_owner(&owner_a, proxima_core::AuthPath::System),
                owner_a.principal.clone(),
                batch_id,
            )
            .await?;
        assert!(replay.already_closed);
        assert_eq!(replay.closed_at, first_closed_at);

        // Cross-owner: B sees the batch as NotFound (information leak guard).
        let cross = engine_b
            .close_batch(
                &proxima_core::AuthzContext::single_owner(&owner_b, proxima_core::AuthPath::System),
                owner_b.principal.clone(),
                batch_id,
            )
            .await
            .unwrap_err();
        assert_eq!(cross.code, ErrorCode::NotFound);

        // Unknown batch under correct owner: NotFound.
        let nope = SourceBatchId::new(Uuid::now_v7());
        let missing = engine_a
            .close_batch(
                &proxima_core::AuthzContext::single_owner(&owner_a, proxima_core::AuthPath::System),
                owner_a.principal.clone(),
                nope,
            )
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
