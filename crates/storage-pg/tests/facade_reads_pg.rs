//! Receipt lookup and per-job embedding diagnostics against Postgres.

use proxima_core::storage_ports::{EmbeddingJobPort, FactIngestPort, OwnerWritePermit};
use proxima_core::test_fixtures::owner_scope_for_tests;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{AccessKind, EmbeddingJobState, MemoryId, OwnerId, StorageError, UserId};
use proxima_pg_testkit::drop_db;
use proxima_storage_pg::ingest_receipt;
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::{PgStorage, begin_owner_transaction};
use uuid::Uuid;

fn sourced(source: &str, key: &str) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: proxima_core::SchemaId::new("core/test-fact-v1".to_string()),
        schema_version: proxima_core::SchemaVersion::new(1),
        handle: None,
        source_id: Some(source.to_owned()),
        ingest_key: Some(key.to_owned()),
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
        request: None,
    }
}

#[tokio::test]
async fn the_receipt_read_uses_the_caller_transaction_under_row_security() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(error) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {error}");
    }
    let url = proxima_pg_testkit::db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let admitted = pg
            .ingest_fact_atomic(&permit, &sourced("src/webhook", "delivery-1"), None)
            .await?;
        let scope = owner_scope_for_tests(&[owner]);
        let runtime = pg.clone_pool_for_backend();
        let mut tx = begin_owner_transaction(&runtime, &scope).await?;
        let found = ingest_receipt(&mut tx, &owner, "src/webhook", "delivery-1").await?;
        let found = found.expect("receipt");
        assert_eq!(found.memory_id, admitted.memory_id);
        assert_eq!(found.handle, admitted.handle);
        assert_eq!(found.schema_id.as_str(), "core/test-fact-v1");
        let shares: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_locks
              WHERE pid = pg_backend_pid() AND granted
                AND locktype = 'relation' AND mode = 'RowShareLock'",
        )
        .fetch_one(tx.as_mut())
        .await?;
        assert!(shares >= 1, "the admission stays share-locked");
        let absent = ingest_receipt(&mut tx, &owner, "", "delivery-1").await;
        assert!(matches!(
            absent,
            Err(StorageError::ConstraintViolation(message)) if message.contains("source_id")
        ));
        tx.rollback().await?;

        let other = OwnerId::from(UserId::new(Uuid::now_v7()));
        let mut hidden_tx =
            begin_owner_transaction(&runtime, &owner_scope_for_tests(&[other])).await?;
        let hidden = ingest_receipt(&mut hidden_tx, &owner, "src/webhook", "delivery-1").await?;
        assert!(hidden.is_none());
        hidden_tx.rollback().await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("receipt lookup");
}

#[tokio::test]
async fn embedding_job_diagnostics_report_each_job_and_its_claim_age() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(error) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {error}");
    }
    let url = proxima_pg_testkit::db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let other = OwnerId::from(UserId::new(Uuid::now_v7()));
        let admin = pg.pool_for_tests();
        let mut setup = admin.begin().await?;
        proxima_storage_pg::access::owner_columns::ensure_owner_row(setup.as_mut(), &owner).await?;
        proxima_storage_pg::access::owner_columns::ensure_owner_row(setup.as_mut(), &other).await?;
        let unit = Uuid::now_v7();
        let elsewhere = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.embedding_jobs
                (entity_id, model_id, owner_id, status, claimed_at, claim_token, dim, last_error)
             VALUES
                ($1, 'diag', $2, 'processing', timestamptz '2020-01-01 00:00:00+00', $3, 384, NULL),
                ($1, 'wait', $2, 'pending', NULL, NULL, 768, NULL),
                ($1, 'dead', $2, 'failed', NULL, NULL, 1024, 'provider refused'),
                ($1, 'stuck', $2, 'failed_permanent', NULL, NULL, 1536, NULL),
                ($4, 'diag', $5, 'processing', timestamptz '2020-01-01 00:00:00+00', $6, 384, NULL)",
        )
        .bind(unit)
        .bind(owner.into_inner())
        .bind(Uuid::now_v7())
        .bind(elsewhere)
        .bind(other.into_inner())
        .bind(Uuid::now_v7())
        .execute(&mut *setup)
        .await?;
        setup.commit().await?;

        let scope = owner_scope_for_tests(&[owner]);
        let rows = pg
            .embedding_job_diagnostics(Some(&scope), &owner, &[MemoryId::new(unit)])
            .await?;
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert!(rows.iter().all(|row| row.entity_id == MemoryId::new(unit)));
        let processing = rows
            .iter()
            .find(|row| row.model_id == "diag")
            .expect("processing job");
        let EmbeddingJobState::Processing { claim_age } = processing.state else {
            panic!("processing job carries a claim age, got {:?}", processing.state);
        };
        assert!(claim_age.whole_seconds() > 86_400, "{claim_age:?}");
        assert!(matches!(
            rows.iter().find(|row| row.model_id == "wait").expect("pending").state,
            EmbeddingJobState::Pending
        ));
        assert!(matches!(
            &rows.iter().find(|row| row.model_id == "dead").expect("failed").state,
            EmbeddingJobState::Failed { last_error: Some(error) } if error == "provider refused"
        ));
        assert!(matches!(
            rows.iter().find(|row| row.model_id == "stuck").expect("permanent").state,
            EmbeddingJobState::FailedPermanent { last_error: None }
        ));

        let not_named = pg
            .embedding_job_diagnostics(Some(&scope), &owner, &[MemoryId::new(elsewhere)])
            .await?;
        assert!(not_named.is_empty(), "another owner's job is not this unit");
        let none = pg
            .embedding_job_diagnostics(Some(&scope), &owner, &[])
            .await?;
        assert!(none.is_empty());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("job diagnostics");
}
