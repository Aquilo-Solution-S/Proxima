//! Derived replay compares origins and refs; a ref mismatch is Conflict.
//!
//! Crate-internal for the same reason as the origin-proof tests beside it.

use super::{DerivedDraft, DerivedOutcome};
use crate::PgStorage;
use proxima_core::storage_ports::FactIngestPort;
use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{
    AccessKind, DerivedEmbedding, EdgeEndpoint, EntityKind, MemoryOperatorKind, OwnerRef, SchemaId,
    SchemaVersion, StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
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
        supersedes: None,
        lexical_language: None,
        embedding: DerivedEmbedding::None,
    }
}

/// The four steps a derived write is: the origin proof, the reference-kind
/// proof, the append, and the declared-index assertion.
///
/// [`crate::ports::memory`] and [`crate::ports::write_session`] run exactly
/// these (with a sketch write between the last two). These tests are about
/// the four proofs, none of which reads the sidecar payload, so they compose
/// them directly rather than through a payload registration that would only
/// stand between the assertion and what it asserts.
async fn append_with_edges(
    pool: &sqlx::PgPool,
    permit: &OwnerWritePermit,
    draft: &DerivedDraft<'_>,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
) -> Result<DerivedOutcome, StorageError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    super::validate_derived_origins_in_tx(&mut tx, draft, origins).await?;
    super::validate_derived_reference_kinds_in_tx(&mut tx, references).await?;
    let outcome = super::append_derived_with_content_payloads_in_tx(
        &mut tx,
        permit,
        draft,
        super::DerivedAdmissionInput {
            origins,
            references,
            sidecar_tables: &[],
            scopes: &[],
            content: super::ContentResolution {
                content_id: None,
                payloads: None,
            },
        },
        |_, _| Box::pin(async { Ok(()) }),
    )
    .await?;
    super::assert_derived_index_rows(&mut tx, draft, &outcome, origins, references).await?;
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
        let origin = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let callee = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let handle = Uuid::now_v7();
        let draft = derived_draft(owner, handle);
        let origins = [EdgeEndpoint::memory(EntityKind::Fact, origin.memory_id)];
        let references = [EdgeEndpoint::memory(EntityKind::Fact, callee.memory_id)];
        let first = append_with_edges(pool, &permit, &draft, &origins, &references).await?;
        assert!(!first.idempotent_replay);
        let replay = append_with_edges(pool, &permit, &draft, &origins, &references).await?;
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
        let origin = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let callee_a = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let callee_b = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let handle = Uuid::now_v7();
        let draft = derived_draft(owner, handle);
        let origins = [EdgeEndpoint::memory(EntityKind::Fact, origin.memory_id)];
        let first = append_with_edges(
            pool,
            &permit,
            &draft,
            &origins,
            &[EdgeEndpoint::memory(EntityKind::Fact, callee_a.memory_id)],
        )
        .await?;
        assert!(!first.idempotent_replay);
        let err = append_with_edges(
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
        let origin_a = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let origin_b = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let handle = Uuid::now_v7();
        let draft = derived_draft(owner, handle);
        let first = append_with_edges(
            pool,
            &permit,
            &draft,
            &[EdgeEndpoint::memory(EntityKind::Fact, origin_a.memory_id)],
            &[],
        )
        .await?;
        let second = append_with_edges(
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
