//! P6: derived replay compares origins and refs; ref mismatch is Conflict.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{
    AccessKind, DerivedEmbedding, EdgeEndpoint, EntityKind, InputContractId, MemoryOperatorKind,
    OperatorId, OwnerRef, SchemaId, SchemaVersion, StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::derive_append::{DerivedDraft, append_derived_with_edges_in_tx};
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use uuid::Uuid;

fn fact_draft() -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new("p6/fact".into()),
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

fn derived_draft(owner: OwnerRef, handle: Uuid) -> DerivedDraft<'static> {
    DerivedDraft {
        memory_id: handle,
        owner,
        kind: EntityKind::Abstraction,
        schema_id: SchemaId::new("p6/derived".into()),
        schema_version: SchemaVersion::new(1),
        text: "derived".into(),
        operator_kind: MemoryOperatorKind::FtoA,
        operator_id: OperatorId::new(Uuid::from_u128(1)),
        input_contract_id: InputContractId::new(Uuid::from_u128(2)),
        source_batch_id: None,
        model_id: "p6",
        prompt_version: "p6",
        authoring_perspective_id: None,
        supersedes: None,
        lexical_language: None,
        embedding: DerivedEmbedding::None,
    }
}

async fn append(
    pool: &sqlx::PgPool,
    permit: &OwnerWritePermit,
    draft: &DerivedDraft<'_>,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
) -> Result<proxima_storage_pg::verbs::derive_append::DerivedOutcome, StorageError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    let outcome = append_derived_with_edges_in_tx(
        &mut tx,
        permit,
        draft,
        origins,
        references,
        &[],
        |_, _| Box::pin(async { Ok(()) }),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    Ok(outcome)
}

async fn fresh_pg() -> (String, PgStorage) {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let pg = PgStorage::connect(&db_url(&db_name))
        .await
        .expect("connect");
    pg.run_migrations().await.expect("migrate");
    (db_name, pg)
}

#[tokio::test]
async fn same_origins_and_refs_replay() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Abstraction);
        let pool = pg.pool_for_tests();
        let origin = ingest_fact_atomic(pool, &permit, &fact_draft(), None).await?;
        let callee = ingest_fact_atomic(pool, &permit, &fact_draft(), None).await?;
        let handle = Uuid::now_v7();
        let draft = derived_draft(owner, handle);
        let origins = [EdgeEndpoint::memory(EntityKind::Fact, origin.memory_id)];
        let references = [EdgeEndpoint::memory(EntityKind::Fact, callee.memory_id)];
        let first = append(pool, &permit, &draft, &origins, &references).await?;
        assert!(!first.idempotent_replay);
        let replay = append(pool, &permit, &draft, &origins, &references).await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.memory_id, first.memory_id);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("same pins must replay");
}

#[tokio::test]
async fn same_origins_different_refs_is_conflict() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Abstraction);
        let pool = pg.pool_for_tests();
        let origin = ingest_fact_atomic(pool, &permit, &fact_draft(), None).await?;
        let callee_a = ingest_fact_atomic(pool, &permit, &fact_draft(), None).await?;
        let callee_b = ingest_fact_atomic(pool, &permit, &fact_draft(), None).await?;
        let handle = Uuid::now_v7();
        let draft = derived_draft(owner, handle);
        let origins = [EdgeEndpoint::memory(EntityKind::Fact, origin.memory_id)];
        let first = append(
            pool,
            &permit,
            &draft,
            &origins,
            &[EdgeEndpoint::memory(EntityKind::Fact, callee_a.memory_id)],
        )
        .await?;
        assert!(!first.idempotent_replay);
        let err = append(
            pool,
            &permit,
            &draft,
            &origins,
            &[EdgeEndpoint::memory(EntityKind::Fact, callee_b.memory_id)],
        )
        .await
        .expect_err("changed refs must not replay");
        match err {
            StorageError::Conflict(msg) => {
                assert!(msg.contains("refs"), "got {msg}");
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("ref mismatch must Conflict");
}

#[tokio::test]
async fn different_origins_append_new_t() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Abstraction);
        let pool = pg.pool_for_tests();
        let origin_a = ingest_fact_atomic(pool, &permit, &fact_draft(), None).await?;
        let origin_b = ingest_fact_atomic(pool, &permit, &fact_draft(), None).await?;
        let handle = Uuid::now_v7();
        let draft = derived_draft(owner, handle);
        let first = append(
            pool,
            &permit,
            &draft,
            &[EdgeEndpoint::memory(EntityKind::Fact, origin_a.memory_id)],
            &[],
        )
        .await?;
        let second = append(
            pool,
            &permit,
            &draft,
            &[EdgeEndpoint::memory(EntityKind::Fact, origin_b.memory_id)],
            &[],
        )
        .await?;
        assert!(!first.idempotent_replay);
        assert!(!second.idempotent_replay);
        assert_ne!(first.memory_id, second.memory_id);
        let head: Uuid =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
                .bind(handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head, second.memory_id.into_inner());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("origin change must append a new t");
}
