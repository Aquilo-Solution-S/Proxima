//! citation_of_fact is memory ⋈ blob; no fabricated mapping columns.
#![allow(clippy::doc_markdown)]

use proxima_core::storage_ports::FactIngestPort;
use proxima_core::storage_ports::{CitationPort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{AccessKind, OwnerRef, SchemaId, SchemaVersion, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

fn draft() -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new("core/test-fact-v1".to_string()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        citation: None,
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

#[tokio::test]
async fn citation_of_fact_is_blob_id_and_schema_only() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let _seed = pg.ingest_fact_atomic(&permit, &draft(), None).await?;

        let hash = vec![9u8; 32];
        let blob_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
             VALUES ($1, 'core/bytes-v1', $2)
             RETURNING blob_id",
        )
        .bind(owner.stored_owner_id())
        .bind(&hash)
        .fetch_one(pool)
        .await?;
        let mut cited = draft();
        cited.blob_id = Some(blob_id);
        let written = pg.ingest_fact_atomic(&permit, &cited, None).await?;

        let readback = pg
            .citation_of_fact(&[owner], written.memory_id)
            .await?
            .expect("cited fact");
        assert_eq!(readback.cited_object_id, blob_id);
        assert_eq!(readback.citation_mapping_id, blob_id);
        assert_eq!(readback.cited_object_schema_id.as_str(), "core/bytes-v1");
        assert_eq!(readback.mapping_schema_id.as_str(), "core/bytes-v1");
        assert!(readback.page_span.is_none());
        assert!(readback.uploaded_blob.is_none());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("citation_of_fact blob projection failed");
}
