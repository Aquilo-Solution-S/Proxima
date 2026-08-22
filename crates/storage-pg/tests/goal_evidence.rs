//! Evidence kind ∈ {F, A} is enforced in-tx against TOCTOU, not by an engine
//! pre-tx walk.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::storage_ports::{GoalWritePort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::goal_write::{
    CreateGoalAtomicRequest, GoalAssignmentTarget, GoalAtomicContext, GoalAuthorship, GoalDraft,
    GoalEvidenceRef, GoalState, GoalTopologyWrite, IdempotencyKey,
};
use proxima_core::{
    AccessKind, EdgeEndpoint, EntityKind, FlavorRegistry, OwnerRef, SchemaId, SchemaVersion,
    StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use uuid::Uuid;

fn draft(kind: &str) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(format!("d7/{kind}")),
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

#[tokio::test]
async fn perspective_evidence_is_rejected_in_tx() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let pool = pg.pool_for_tests();
        let fact = ingest_fact_atomic(pool, &permit, &draft("fact"), None).await?;
        let mut abs = draft("abstraction");
        abs.derived_from = vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
        let abs = ingest_fact_atomic(pool, &permit, &abs, None).await?;
        let mut perspective = draft("perspective");
        perspective.derived_from =
            vec![EdgeEndpoint::memory(EntityKind::Abstraction, abs.memory_id)];
        let perspective = ingest_fact_atomic(pool, &permit, &perspective, None).await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let topology = GoalTopologyWrite::new(
            GoalAssignmentTarget::perspective(perspective.memory_id),
            Vec::new(),
            vec![GoalEvidenceRef::new(perspective.memory_id)],
        )?;
        let err = pg
            .create_goal_atomic(
                &CreateGoalAtomicRequest {
                    draft: GoalDraft {
                        owner,
                        schema_id: SchemaId::new("d7/goal".into()),
                        schema_version: SchemaVersion::new(1),
                        title: "d7".into(),
                        text: "d7".into(),
                        payload: Vec::new(),
                        sidecar_payload: None,
                        state: GoalState::Active,
                        topology,
                        wake: None,
                        supersedes_goal_id: None,
                        authorship: GoalAuthorship::User,
                        request_id: IdempotencyKey::new("d7-evidence")?.into_string(),
                    },
                    context: GoalAtomicContext {
                        registry: &registry,
                        embedding_model_id: None,
                        author_self_perspective_id: None,
                    },
                    write_act_t: None,
                },
                &permit,
            )
            .await
            .expect_err("Perspective evidence must fail in-tx");
        match err {
            StorageError::ConstraintViolation(msg) => {
                assert!(
                    msg.contains("Fact or Abstraction"),
                    "storage kind TOCTOU, got {msg}"
                );
            }
            other => panic!("expected ConstraintViolation, got {other:?}"),
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("D7 evidence kind TOCTOU failed");
}
