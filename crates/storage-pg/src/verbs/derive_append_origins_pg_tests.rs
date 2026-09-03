//! The in-tx origin proof reads the STORED kind, not the declared one: a
//! declared Fact on a Perspective row must fail.
//!
//! Crate-internal because the verb is: the derive helpers are `pub(crate)`
//! implementation detail of the write ports, so the test that pins their
//! refusals lives beside them rather than reaching in from outside.

// One test walks every refusal in turn against one seeded graph; splitting it
// would re-seed the same three memories four times to assert four messages.
#![expect(clippy::too_many_lines, reason = "one seeded graph, four refusals")]

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
) -> DerivedDraft<'static> {
    DerivedDraft {
        memory_id,
        owner,
        kind,
        schema_id: SchemaId::new("d2/derived".into()),
        schema_version: SchemaVersion::new(1),
        text: "derived".into(),
        operator_kind,
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

        let fact = pg.ingest_fact_atomic(&permit, &draft("fact"), None).await?;
        let mut abs = draft("abstraction");
        abs.derived_from = vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
        let abs = pg.ingest_fact_atomic(&permit, &abs, None).await?;
        let mut perspective = draft("perspective");
        perspective.derived_from =
            vec![EdgeEndpoint::memory(EntityKind::Abstraction, abs.memory_id)];
        let perspective = pg.ingest_fact_atomic(&permit, &perspective, None).await?;

        let spoof = derived_draft(
            owner,
            Uuid::now_v7(),
            MemoryOperatorKind::FtoA,
            EntityKind::Abstraction,
        );
        let err = append_with_edges(
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
        );
        let err = append_with_edges(
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
        );
        let err = append_with_edges(
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
                    "phase is on stored kind, got {msg}"
                );
            }
            other => panic!("expected ConstraintViolation, got {other:?}"),
        }

        let honest = derived_draft(
            owner,
            Uuid::now_v7(),
            MemoryOperatorKind::FtoA,
            EntityKind::Abstraction,
        );
        let ok = append_with_edges(
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
    result.expect("stored-kind origin proof failed");
}
