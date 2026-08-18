//! D2: in-tx origin proof uses stored kind. Declared Fact on a
//! Perspective row must fail — old code discarded the SELECT kind.
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

fn draft(kind: &str) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(format!("d2/{kind}")),
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
        kind: kind.into(),
    }
}

fn derived_draft(
    owner: OwnerRef,
    memory_id: Uuid,
    operator_kind: MemoryOperatorKind,
    kind: EntityKind,
    model_id: &str,
) -> DerivedDraft<'_> {
    DerivedDraft {
        memory_id,
        owner,
        kind,
        schema_id: SchemaId::new("d2/derived".into()),
        schema_version: SchemaVersion::new(1),
        text: "derived".into(),
        operator_kind,
        operator_id: OperatorId::new(Uuid::now_v7()),
        input_contract_id: InputContractId::new(Uuid::now_v7()),
        source_batch_id: None,
        model_id,
        prompt_version: "d2",
        authoring_perspective_id: None,
        supersedes: None,
        lexical_language: None,
        embedding: DerivedEmbedding::None,
    }
}

async fn append_ftoa(
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

fn assert_kind_mismatch(err: StorageError) {
    match err {
        StorageError::ConstraintViolation(msg) => {
            assert!(
                msg.contains("must match the stored row"),
                "in-tx must use stored kind, got {msg}"
            );
        }
        other => panic!("expected ConstraintViolation, got {other:?}"),
    }
}

#[tokio::test]
async fn declared_fact_on_perspective_origin_is_rejected() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Perspective);
        let pool = pg.pool_for_tests();

        let fact = ingest_fact_atomic(pool, &permit, &draft("fact"), None).await?;
        let mut abs = draft("abstraction");
        abs.derived_from = vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
        let abs = ingest_fact_atomic(pool, &permit, &abs, None).await?;
        let mut perspective = draft("perspective");
        perspective.derived_from =
            vec![EdgeEndpoint::memory(EntityKind::Abstraction, abs.memory_id)];
        let perspective = ingest_fact_atomic(pool, &permit, &perspective, None).await?;

        let spoof = derived_draft(
            owner,
            Uuid::now_v7(),
            MemoryOperatorKind::FtoA,
            EntityKind::Abstraction,
            "d2-model",
        );
        let err = append_ftoa(
            pool,
            &permit,
            &spoof,
            &[EdgeEndpoint::memory(
                EntityKind::Fact,
                perspective.memory_id,
            )],
            &[],
        )
        .await
        .expect_err("declared Fact on a Perspective origin must fail");
        assert_kind_mismatch(err);

        let spoofed_ref = derived_draft(
            owner,
            Uuid::now_v7(),
            MemoryOperatorKind::FtoA,
            EntityKind::Abstraction,
            "d2-model",
        );
        let err = append_ftoa(
            pool,
            &permit,
            &spoofed_ref,
            &[EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)],
            &[EdgeEndpoint::memory(
                EntityKind::Fact,
                perspective.memory_id,
            )],
        )
        .await
        .expect_err("declared Fact on a Perspective reference must fail");
        assert_kind_mismatch(err);

        let honest_wrong_phase = derived_draft(
            owner,
            Uuid::now_v7(),
            MemoryOperatorKind::FtoA,
            EntityKind::Abstraction,
            "d2-model",
        );
        let err = append_ftoa(
            pool,
            &permit,
            &honest_wrong_phase,
            &[EdgeEndpoint::memory(
                EntityKind::Perspective,
                perspective.memory_id,
            )],
            &[],
        )
        .await
        .expect_err("honest Perspective origin must fail F→A phase");
        match err {
            StorageError::ConstraintViolation(msg) => {
                assert!(
                    msg.contains("does not match operator phase"),
                    "D6: phase is on stored kind, got {msg}"
                );
            }
            other => panic!("expected ConstraintViolation, got {other:?}"),
        }

        let honest = derived_draft(
            owner,
            Uuid::now_v7(),
            MemoryOperatorKind::FtoA,
            EntityKind::Abstraction,
            "d2-model",
        );
        let ok = append_ftoa(
            pool,
            &permit,
            &honest,
            &[EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)],
            &[],
        )
        .await
        .expect("declared Fact on a Fact row must pass");
        assert!(!ok.idempotent_replay);
        assert_ne!(ok.memory_id, fact.memory_id);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("D2 stored-kind origin proof failed");
}
