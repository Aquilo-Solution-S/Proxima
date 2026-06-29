//! Receiptless Fact ingest behavior.

use crate::common::{create_db, db_url, drop_db};
use std::sync::Arc;

use proxima_core::engine::Engine;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{EntityKind, OwnerRef, SchemaId, SchemaVersion, UserId};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::fact_embeddings::{list_facts_missing_embedding, load_fact_text};
use proxima_storage_pg::verbs::hard_delete::{
    HardDeleteSet, HardDeleteSidecars, execute_hard_delete,
};
use uuid::Uuid;

fn schemas_for_test() -> Vec<SchemaInfo> {
    vec![SchemaInfo {
        schema_id: SchemaId::new("test/receiptless_fact".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::Fact,
        filter_keys: vec![],
        sidecar_table: None,
        natural_key_columns: vec![],
        tombstone: None,
        has_typed_ingress: false,
        cited_object_schema: None,
    }]
}

fn receiptless_command() -> FactWriteCommand {
    FactWriteCommand {
        author_personality_instance_id: None,
        schema_id: SchemaId::new("test/receiptless_fact".into()),
        schema_version: SchemaVersion::new(1),
        payload: format!("receiptless {}", Uuid::now_v7()).into_bytes(),
        rendered_text: Some("receiptless fact".to_string()),
        receipt: None,
        citation: None,
    }
}

#[tokio::test]
async fn receiptless_fact_ingest_creates_fresh_queryable_facts() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();

        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);
        let authz =
            proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System);

        let command = receiptless_command();
        let first = engine.fact_ingest(&authz, command.clone()).await?;
        let second = engine.fact_ingest(&authz, command).await?;
        assert_ne!(first.memory_id, second.memory_id);
        assert_eq!(first.receipt_id, None);
        assert_eq!(second.receipt_id, None);
        assert!(!first.idempotent_replay);
        assert!(!second.idempotent_replay);

        let count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
             FROM proxima_core.memories
             WHERE owner_kind = 'personal'
               AND owner_id = $1
               AND kind IS NULL
               AND receipt_id IS NULL",
        )
        .bind(owner.stable_key_uuid())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(count, 2);

        let receipt_rows: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.fact_receipts")
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(receipt_rows, 0);

        assert_eq!(
            load_fact_text(pg.pool(), &owner, first.memory_id).await?,
            Some("receiptless fact".to_string())
        );
        let missing = list_facts_missing_embedding(pg.pool(), &owner, "test-embed", 10).await?;
        assert!(missing.contains(&first.memory_id));
        assert!(missing.contains(&second.memory_id));

        let mut tx = pg.pool().begin().await?;
        let hard_delete_counts = execute_hard_delete(
            &mut tx,
            &HardDeleteSet {
                memories: vec![(EntityKind::Fact, first.memory_id.into_inner())],
                edge_ids: vec![],
                fact_entity_ids: vec![],
                receipt_ids: vec![],
            },
            &HardDeleteSidecars {
                memory_keyed: &[],
                edge_keyed: &[],
                citation_mapping_keyed: &[],
            },
        )
        .await?;
        tx.commit().await?;
        assert_eq!(hard_delete_counts.memories, 1);
        assert_eq!(hard_delete_counts.receipts, 0);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("receiptless fact ingest test failed");
}
