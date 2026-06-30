use std::fs;
use std::path::PathBuf;

#[test]
fn baseline_schema_contains_pr7_operator_proof_carriers() {
    let sql_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations/0001_init.sql");
    let sql = fs::read_to_string(sql_path).expect("baseline migration readable");

    assert!(
        sql.contains("input_contract_id uuid"),
        "PR7 proof carrier must persist opaque UUID input contract ids"
    );
    assert!(
        sql.contains("operator_id uuid"),
        "PR7 proof carrier must persist operator id on derived memories"
    );
    assert!(
        sql.contains("source_batch_id uuid"),
        "F→A exclusivity needs source batch on derived memories"
    );
    assert!(
        sql.contains("'AtoA'"),
        "memory_operator_kind must include Lean A→A"
    );
    assert!(
        !sql.contains("'ExternalAgent'::proxima_core.memory_operator_kind")
            && !sql.contains("'Wake'::proxima_core.memory_operator_kind"),
        "derived memory operator kind must not preserve stale ExternalAgent/Wake variants"
    );
    assert!(
        sql.contains("memories_ftoa_batch_exclusive_uidx"),
        "F→A uniqueness index must witness ftoa_batch_exclusive"
    );
}

use std::sync::Arc;

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::storage::{AuthorDerivedRequest, DerivedEdgeSpec};
use proxima_core::storage_ports::MemoryAuthoringPort;
use proxima_core::{
    AbstractionPayload, AgentDerivationV1, CORE_DERIVED_FROM_RELATION, EdgeAuthorshipKind,
    EntityKind, FlavorRegistry, InputContractId, MemoryId, MemoryOperatorKind, OperatorId, Owner,
    SchemaId, SchemaVersion, SidecarPayload, SourceBatchId, StorageError,
};
use uuid::Uuid;

#[tokio::test]
async fn replay_rejects_changed_operator_edge_set() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = owner_fixture();
        let source_a = insert_source_abstraction(&pg, &owner, "replay-edge-a").await?;
        let source_b = insert_source_abstraction(&pg, &owner, "replay-edge-b").await?;
        let engine = proxima_core::Engine::new(FlavorRegistry::new().freeze())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let memory_id = MemoryId::new(Uuid::now_v7());
        let operator_id = OperatorId::new(Uuid::now_v7());
        let input_contract_id = InputContractId::new(Uuid::now_v7());

        let first = author_test_abstraction(
            &engine,
            owner,
            memory_id,
            source_a,
            MemoryOperatorKind::AtoA,
            operator_id,
            input_contract_id,
            None,
        )
        .await?;
        assert!(!first.idempotent_replay);

        let err = author_test_abstraction(
            &engine,
            owner,
            memory_id,
            source_b,
            MemoryOperatorKind::AtoA,
            operator_id,
            input_contract_id,
            None,
        )
        .await
        .expect_err("same memory replay with changed proof edge set is a conflict");
        assert!(
            matches!(err, StorageError::Conflict(_)),
            "unexpected {err:?}"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn replay_rejects_omitted_operator_edge_set() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = owner_fixture();
        let source_a = insert_source_abstraction(&pg, &owner, "replay-omit-a").await?;
        let source_b = insert_source_abstraction(&pg, &owner, "replay-omit-b").await?;
        let engine = proxima_core::Engine::new(FlavorRegistry::new().freeze())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let memory_id = MemoryId::new(Uuid::now_v7());
        let operator_id = OperatorId::new(Uuid::now_v7());
        let input_contract_id = InputContractId::new(Uuid::now_v7());

        author_test_abstraction_multi(
            &engine,
            owner,
            memory_id,
            &[source_a, source_b],
            MemoryOperatorKind::AtoA,
            operator_id,
            input_contract_id,
            None,
        )
        .await?;

        let err = author_test_abstraction_multi(
            &engine,
            owner,
            memory_id,
            &[source_a],
            MemoryOperatorKind::AtoA,
            operator_id,
            input_contract_id,
            None,
        )
        .await
        .expect_err("same memory replay omitting a prior proof edge is a conflict");
        assert!(
            matches!(err, StorageError::Conflict(_)),
            "unexpected {err:?}"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn replay_rejects_persisted_wrong_operator_authorship_edge()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = owner_fixture();
        let source_a = insert_source_abstraction(&pg, &owner, "replay-wrong-auth-a").await?;
        let fact_batch = SourceBatchId::new(Uuid::now_v7());
        let fact = insert_fact(&pg, &owner, fact_batch, true, "replay-wrong-auth-fact").await?;
        let engine = proxima_core::Engine::new(FlavorRegistry::new().freeze())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let memory_id = MemoryId::new(Uuid::now_v7());
        let operator_id = OperatorId::new(Uuid::now_v7());
        let input_contract_id = InputContractId::new(Uuid::now_v7());

        author_test_abstraction(
            &engine,
            owner,
            memory_id,
            source_a,
            MemoryOperatorKind::AtoA,
            operator_id,
            input_contract_id,
            None,
        )
        .await?;

        let (owner_kind, owner_id) = owner.columns();
        sqlx::query(
            "INSERT INTO proxima_core.edges
                (edge_id, relation, relation_class, source_kind, source_memory_id, target_kind,
                 target_memory_id, authorship_kind, owner_kind, owner_id)
             VALUES ($1, $2, 'Provenance', 'Abstraction', $3, 'Fact', $4,
                     'OperatorFtoA', $5, $6)",
        )
        .bind(Uuid::now_v7())
        .bind(CORE_DERIVED_FROM_RELATION)
        .bind(memory_id.into_inner())
        .bind(fact.into_inner())
        .bind(owner_kind)
        .bind(owner_id)
        .execute(pg.pool())
        .await?;

        let err = author_test_abstraction(
            &engine,
            owner,
            memory_id,
            source_a,
            MemoryOperatorKind::AtoA,
            operator_id,
            input_contract_id,
            None,
        )
        .await
        .expect_err("same memory replay must detect persisted wrong-authorship proof edge");
        assert!(
            matches!(err, StorageError::Conflict(_)),
            "unexpected {err:?}"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn author_derived_rejects_extra_same_output_wrong_operator_authorship()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = owner_fixture();
        let source_a = insert_source_abstraction(&pg, &owner, "wrong-auth-proof-a").await?;
        let fact_batch = SourceBatchId::new(Uuid::now_v7());
        let fact = insert_fact(&pg, &owner, fact_batch, true, "wrong-auth-extra-fact").await?;
        let engine = proxima_core::Engine::new(FlavorRegistry::new().freeze())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let relation = engine
            .registry()
            .resolve_relation(CORE_DERIVED_FROM_RELATION)
            .expect("core derived-from relation registered");
        let memory_id = MemoryId::new(Uuid::now_v7());
        let operator_id = OperatorId::new(Uuid::now_v7());
        let input_contract_id = InputContractId::new(Uuid::now_v7());
        let sidecar_payload = SidecarPayload::abstraction(AgentDerivationV1 {
            title: "derived".into(),
            body: "derived".into(),
            tags: Vec::new(),
            idempotency_key: None,
            source_memory_ids: vec![source_a.into_inner()],
            model_id: "test-model".into(),
            client_name: "test".into(),
            client_version: "1".into(),
        });
        let edges = vec![
            DerivedEdgeSpec {
                owner: &owner,
                relation,
                source_kind: EntityKind::Abstraction,
                source_memory_id: memory_id,
                target_kind: EntityKind::Abstraction,
                target_memory_id: source_a,
                authorship_kind: EdgeAuthorshipKind::OperatorAtoA,
                authorship_owner_memory_id: Some(source_a),
                sidecar_payload: None,
            },
            DerivedEdgeSpec {
                owner: &owner,
                relation,
                source_kind: EntityKind::Abstraction,
                source_memory_id: memory_id,
                target_kind: EntityKind::Fact,
                target_memory_id: fact,
                authorship_kind: EdgeAuthorshipKind::OperatorFtoA,
                authorship_owner_memory_id: None,
                sidecar_payload: None,
            },
        ];
        let req = AuthorDerivedRequest {
            memory_id,
            owner,
            kind: EntityKind::Abstraction,
            text: "derived".into(),
            schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
            operator_kind: MemoryOperatorKind::AtoA,
            operator_id,
            input_contract_id,
            source_batch_id: None,
            model_id: "test-model",
            prompt_version: "operator-proofs-pg",
            sidecar_payload,
            supersedes: None,
            embedding: None,
            embedding_model_id: None,
            edges: &edges,
        };

        let err = pg
            .author_derived(&req)
            .await
            .expect_err("same-output wrong operator authorship must be rejected");
        assert!(
            err.to_string()
                .contains("authorship kind does not match operator phase"),
            "unexpected {err:?}"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn ftoa_requires_closed_matching_batch_and_conflicts_on_distinct_output()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = owner_fixture();
        let open_batch = SourceBatchId::new(Uuid::now_v7());
        let open_fact = insert_fact(&pg, &owner, open_batch, false, "open-ftoa").await?;
        let engine = proxima_core::Engine::new(FlavorRegistry::new().freeze())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let operator_id = OperatorId::new(Uuid::now_v7());
        let input_contract_id = InputContractId::new(Uuid::now_v7());
        let open_err = author_test_abstraction(
            &engine,
            owner,
            MemoryId::new(Uuid::now_v7()),
            open_fact,
            MemoryOperatorKind::FtoA,
            operator_id,
            input_contract_id,
            Some(open_batch),
        )
        .await
        .expect_err("open F→A source batch is rejected");
        assert!(open_err.to_string().contains("source batch must be closed"));

        let closed_batch = SourceBatchId::new(Uuid::now_v7());
        let fact = insert_fact(&pg, &owner, closed_batch, true, "closed-ftoa").await?;
        let first = author_test_abstraction(
            &engine,
            owner,
            MemoryId::new(Uuid::now_v7()),
            fact,
            MemoryOperatorKind::FtoA,
            operator_id,
            input_contract_id,
            Some(closed_batch),
        )
        .await?;
        assert!(!first.idempotent_replay);

        let conflict = author_test_abstraction(
            &engine,
            owner,
            MemoryId::new(Uuid::now_v7()),
            fact,
            MemoryOperatorKind::FtoA,
            operator_id,
            input_contract_id,
            Some(closed_batch),
        )
        .await
        .expect_err("second distinct F→A output for same batch/contract conflicts");
        assert!(
            matches!(conflict, StorageError::Conflict(_)),
            "unexpected {conflict:?}"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[cfg(feature = "test-fixtures")]
#[tokio::test]
async fn graph_fixture_flags_invalid_atogoal_fact_target() -> Result<(), Box<dyn std::error::Error>>
{
    use proxima_storage_pg::test_fixtures::operator_proofs::{
        GraphValidityViolation, collect_memory_graph_violations,
    };

    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = owner_fixture();
        let batch = SourceBatchId::new(Uuid::now_v7());
        let fact = insert_fact(&pg, &owner, batch, true, "bad-atogoal-target").await?;
        let goal_id = proxima_core::GoalId::new(Uuid::now_v7());
        let request_id = Uuid::now_v7();
        // Insert just enough rows for the fixture scan; this deliberately bypasses
        // the storage goal writer because production correctly rejects Fact evidence.
        let (owner_kind, owner_id) = owner.columns();
        sqlx::query(
            "INSERT INTO proxima_core.goals
                (goal_id, owner_kind, owner_id, state, schema_id, schema_version, title, text,
                 payload, request_id, idempotency_key, authorship_kind, authorship_origin,
                 operator_kind, authorship_operator_id, input_contract_id, model_id, prompt_version)
             VALUES ($1, $2, $3, 'Active', 'test/goal', 1, 'goal', 'goal', '{}'::bytea,
                     $4, 'bad-atogoal', 'System', 'Operator', 'AtoGoal', $5, $6, 'test', 'test')",
        )
        .bind(goal_id.into_inner())
        .bind(owner_kind)
        .bind(owner_id)
        .bind(request_id)
        .bind(OperatorId::new(Uuid::now_v7()).into_inner())
        .bind(InputContractId::new(Uuid::now_v7()).into_inner())
        .execute(pg.pool())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.edges
                (edge_id, relation, relation_class, source_kind, source_goal_id, target_kind,
                 target_memory_id, authorship_kind, owner_kind, owner_id)
             VALUES ($1, $2, 'Structural', 'Goal', $3, 'Fact', $4, 'OperatorAtoGoal', $5, $6)",
        )
        .bind(Uuid::now_v7())
        .bind(proxima_core::CORE_MOTIVATED_BY_RELATION)
        .bind(goal_id.into_inner())
        .bind(fact.into_inner())
        .bind(owner_kind)
        .bind(owner_id)
        .execute(pg.pool())
        .await?;
        let violations = collect_memory_graph_violations(pg.pool()).await?;
        assert!(
            violations
                .iter()
                .any(|v| matches!(v, GraphValidityViolation::InvalidOperatorEdgeShape { .. }))
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[allow(clippy::too_many_arguments)] // test helper mirrors operator proof request dimensions
async fn author_test_abstraction(
    engine: &proxima_core::Engine,
    owner: Owner,
    memory_id: MemoryId,
    input: MemoryId,
    operator_kind: MemoryOperatorKind,
    operator_id: OperatorId,
    input_contract_id: InputContractId,
    source_batch_id: Option<SourceBatchId>,
) -> Result<proxima_core::AuthorDerivedOutcome, StorageError> {
    author_test_abstraction_multi(
        engine,
        owner,
        memory_id,
        &[input],
        operator_kind,
        operator_id,
        input_contract_id,
        source_batch_id,
    )
    .await
}

#[allow(clippy::too_many_arguments)] // test helper mirrors operator proof request dimensions
async fn author_test_abstraction_multi(
    engine: &proxima_core::Engine,
    owner: Owner,
    memory_id: MemoryId,
    inputs: &[MemoryId],
    operator_kind: MemoryOperatorKind,
    operator_id: OperatorId,
    input_contract_id: InputContractId,
    source_batch_id: Option<SourceBatchId>,
) -> Result<proxima_core::AuthorDerivedOutcome, StorageError> {
    let relation = engine
        .registry()
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .expect("core derived-from relation registered");
    let target_kind = operator_kind.phase().input_kind();
    let edges = inputs
        .iter()
        .copied()
        .map(|input| proxima_core::AuthorDerivedEdgeInput {
            relation,
            source_kind: EntityKind::Abstraction,
            source_memory_id: memory_id,
            target_kind,
            target_memory_id: input,
            authorship_kind: operator_kind.edge_authorship(),
            authorship_owner_memory_id: Some(input),
        })
        .collect::<Vec<_>>();
    engine
        .author_derived(proxima_core::AuthorDerivedRequestInput {
            memory_id,
            owner,
            kind: EntityKind::Abstraction,
            text: "derived".into(),
            schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
            operator_kind,
            operator_id,
            input_contract_id,
            source_batch_id,
            model_id: "test-model",
            prompt_version: "operator-proofs-pg",
            sidecar_payload: SidecarPayload::abstraction(AgentDerivationV1 {
                title: "derived".into(),
                body: "derived".into(),
                tags: Vec::new(),
                idempotency_key: None,
                source_memory_ids: inputs.iter().map(|input| input.into_inner()).collect(),
                model_id: "test-model".into(),
                client_name: "test".into(),
                client_version: "1".into(),
            }),
            supersedes: None,
            edges: &edges,
        })
        .await
}

async fn insert_source_abstraction(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    label: &str,
) -> Result<MemoryId, sqlx::Error> {
    let memory_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, label.as_bytes());
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id,
             prompt_version)
         VALUES ($1, $2, $3, 'test/source-abstraction', 1, 'Abstraction', $4,
                 'AtoA', '00000000-0000-0000-0000-000000000581'::uuid,
                 '00000000-0000-0000-0000-000000000582'::uuid, NULL, 'test', 'test')",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(label)
    .execute(pg.pool())
    .await?;
    Ok(MemoryId::new(memory_id))
}

async fn insert_fact(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    source_batch_id: SourceBatchId,
    closed: bool,
    label: &str,
) -> Result<MemoryId, sqlx::Error> {
    let memory_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, label.as_bytes());
    let receipt_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("receipt:{label}").as_bytes())
        .as_bytes()
        .to_vec();
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.source_batches (id, source_id, owner_kind, owner_id, closed_at)
         VALUES ($1, 'test/operator-proofs', $2, $3,
                 CASE WHEN $4 THEN now() ELSE NULL END)",
    )
    .bind(source_batch_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(closed)
    .execute(pg.pool())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.fact_receipts
            (receipt_id, source, source_batch_id, owner_kind, owner_id, schema_id,
             schema_version, observed_at, occurred_at)
         VALUES ($1, 'test/operator-proofs', $2, $3, $4, 'test/fact', 1, now(), now())",
    )
    .bind(&receipt_id)
    .bind(source_batch_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, receipt_id)
         VALUES ($1, $2, $3, 'test/fact', 1, $4)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(receipt_id)
    .execute(pg.pool())
    .await?;
    Ok(MemoryId::new(memory_id))
}
