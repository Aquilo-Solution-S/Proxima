use proxima_core::storage_ports::*;
mod common;

use std::sync::Arc;

use common::{ConstantEmbedding, drop_db, fresh_pg, owner_fixture};
use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{
    AbstractionPayload, AgentDerivationV1, AgentNoteV1, AuthPath, AuthorshipKindMask, AuthzContext,
    CORE_DERIVED_FROM_RELATION, EdgeAuthorshipKind, EntityKind, EntityKindMask, ErrorCode,
    FlavorRegistry, InputContractId, MemoryId, MemoryOperatorKind, OperatorId, Owner, OwnerRef,
    Relation, RelationClass, RelationDescriptor, SchemaId, SchemaVersion, SidecarPayload,
    SourceBatchId, UserId,
};
use uuid::Uuid;

fn padded_embedding(prefix: [f32; 3]) -> Vec<f32> {
    let mut embedding = vec![0.0; EMBEDDING_DIM];
    embedding[..prefix.len()].copy_from_slice(&prefix);
    embedding
}

fn vector_literal(vec: &[f32]) -> String {
    let mut out = String::with_capacity(vec.len().saturating_mul(8).saturating_add(2));
    out.push('[');
    for (idx, value) in vec.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&value.to_string());
    }
    out.push(']');
    out
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn engine_author_derived_writes_memory_edge_and_embedding()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let owner = owner_fixture();
    let source_abstraction = insert_source_abstraction(&pg, &owner).await?;
    let mut registry = FlavorRegistry::new();
    registry.add_relation_or_panic_for_tests(RelationDescriptor::substrate(
        "test/derived-from-abstraction",
        RelationClass::Provenance,
        proxima_core::EndpointBinding::Pin,
        proxima_core::EndpointBinding::Pin,
        EntityKindMask::abstraction(),
        EntityKindMask::abstraction(),
        AuthorshipKindMask::operator_a_to_a(),
    ));
    let engine = proxima_core::Engine::new(registry.freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports())
        .with_embed(Arc::new(ConstantEmbedding::prefixed(
            "test-embed",
            &[12.0, 1.0, 2.0],
        )));
    let relation = engine
        .registry()
        .resolve_relation("test/derived-from-abstraction")
        .expect("test relation registered");
    let sidecar_payload = SidecarPayload::abstraction(AgentDerivationV1 {
        title: "Derived".into(),
        body: "derived body".into(),
        tags: vec!["memory".into()],
        idempotency_key: Some("derive-1".into()),
        source_memory_ids: vec![source_abstraction.into_inner()],
        model_id: "agent-model".into(),
        client_name: "test-client".into(),
        client_version: "1".into(),
    });
    let derived_memory_id = MemoryId::new(Uuid::now_v7());
    let edges = [proxima_core::AuthorDerivedEdgeInput {
        relation,
        source_kind: EntityKind::Abstraction,
        source_memory_id: derived_memory_id,
        target_kind: EntityKind::Abstraction,
        target_memory_id: source_abstraction,
        authorship_kind: EdgeAuthorshipKind::OperatorAtoA,
        authorship_owner_memory_id: Some(source_abstraction),
    }];

    let outcome = engine
        .author_derived(proxima_core::AuthorDerivedRequestInput {
            memory_id: derived_memory_id,
            owner,
            kind: EntityKind::Abstraction,
            text: "derived body".into(),
            schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
            operator_kind: MemoryOperatorKind::AtoA,
            operator_id: OperatorId::new(Uuid::now_v7()),
            input_contract_id: InputContractId::new(Uuid::now_v7()),
            source_batch_id: None,
            model_id: "agent-model",
            prompt_version: "test-prompt",
            sidecar_payload,
            supersedes: None,
            edges: &edges,
        })
        .await?;

    assert!(!outcome.idempotent_replay);
    assert_eq!(outcome.edge_count, 1);
    let memory_id = outcome.memory_id.into_inner();
    let memory_row: (EntityKind, String) = sqlx::query_as(
        "SELECT kind, text
           FROM proxima_core.memories
          WHERE memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(memory_row.0, EntityKind::Abstraction);
    assert_eq!(memory_row.1, "derived body");

    let sidecar_title: String = sqlx::query_scalar(
        "SELECT title FROM proxima_core.agent_derivation_v1 WHERE memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(sidecar_title, "Derived");

    let edge_row: (String, Uuid, Uuid, Option<Uuid>) = sqlx::query_as(
        "SELECT relation, source_memory_id, target_memory_id, authorship_owner_memory_id
           FROM proxima_core.edges
          WHERE source_memory_id = $1 AND target_memory_id = $2",
    )
    .bind(memory_id)
    .bind(source_abstraction.into_inner())
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(edge_row.0, "test/derived-from-abstraction");
    assert_eq!(edge_row.1, memory_id);
    assert_eq!(edge_row.2, source_abstraction.into_inner());
    assert_eq!(edge_row.3, Some(source_abstraction.into_inner()));

    assert_embedding_row(pg.pool_for_tests(), memory_id).await?;

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn engine_author_derived_supersedes_in_same_transaction()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let owner = owner_fixture();
    let source_abstraction = insert_source_abstraction(&pg, &owner).await?;
    let registry = FlavorRegistry::new();
    let engine = proxima_core::Engine::new(registry.freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports())
        .with_embed(Arc::new(ConstantEmbedding::prefixed(
            "test-embed",
            &[22.0, 1.0, 2.0],
        )));
    let old_memory_id = MemoryId::new(Uuid::now_v7());
    let new_memory_id = MemoryId::new(Uuid::now_v7());

    let old_sidecar = SidecarPayload::abstraction(AgentDerivationV1 {
        title: "Old assertion".into(),
        body: "old assertion body".into(),
        tags: vec!["assertion".into()],
        idempotency_key: Some("assertion-old".into()),
        source_memory_ids: vec![source_abstraction.into_inner()],
        model_id: "agent-model".into(),
        client_name: "test-client".into(),
        client_version: "1".into(),
    });
    let relation = engine
        .registry()
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .expect("core relation registered");
    let old_edges = [proxima_core::AuthorDerivedEdgeInput {
        relation,
        source_kind: EntityKind::Abstraction,
        source_memory_id: old_memory_id,
        target_kind: EntityKind::Abstraction,
        target_memory_id: source_abstraction,
        authorship_kind: EdgeAuthorshipKind::OperatorAtoA,
        authorship_owner_memory_id: Some(source_abstraction),
    }];
    let old = engine
        .author_derived(proxima_core::AuthorDerivedRequestInput {
            memory_id: old_memory_id,
            owner,
            kind: EntityKind::Abstraction,
            text: "old assertion body".into(),
            schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
            operator_kind: MemoryOperatorKind::AtoA,
            operator_id: OperatorId::new(Uuid::now_v7()),
            input_contract_id: InputContractId::new(Uuid::now_v7()),
            source_batch_id: None,
            model_id: "agent-model",
            prompt_version: "test-prompt",
            sidecar_payload: old_sidecar,
            supersedes: None,
            edges: &old_edges,
        })
        .await?;
    assert_eq!(old.memory_id, old_memory_id);
    assert_eq!(old.edge_count, 1);

    let new_sidecar = SidecarPayload::abstraction(AgentDerivationV1 {
        title: "New assertion".into(),
        body: "new assertion body".into(),
        tags: vec!["assertion".into()],
        idempotency_key: Some("assertion-new".into()),
        source_memory_ids: vec![source_abstraction.into_inner()],
        model_id: "agent-model".into(),
        client_name: "test-client".into(),
        client_version: "1".into(),
    });
    let new_edges = [proxima_core::AuthorDerivedEdgeInput {
        relation,
        source_kind: EntityKind::Abstraction,
        source_memory_id: new_memory_id,
        target_kind: EntityKind::Abstraction,
        target_memory_id: source_abstraction,
        authorship_kind: EdgeAuthorshipKind::OperatorAtoA,
        authorship_owner_memory_id: Some(source_abstraction),
    }];
    let new = engine
        .author_derived(proxima_core::AuthorDerivedRequestInput {
            memory_id: new_memory_id,
            owner,
            kind: EntityKind::Abstraction,
            text: "new assertion body".into(),
            schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
            operator_kind: MemoryOperatorKind::AtoA,
            operator_id: OperatorId::new(Uuid::now_v7()),
            input_contract_id: InputContractId::new(Uuid::now_v7()),
            source_batch_id: None,
            model_id: "agent-model",
            prompt_version: "test-prompt",
            sidecar_payload: new_sidecar,
            supersedes: Some(old_memory_id),
            edges: &new_edges,
        })
        .await?;
    assert_eq!(new.memory_id, new_memory_id);
    assert_eq!(new.edge_count, 2);

    let stored_supersedes: Option<Uuid> =
        sqlx::query_scalar("SELECT supersedes FROM proxima_core.memories WHERE memory_id = $1")
            .bind(new_memory_id.into_inner())
            .fetch_one(pg.pool_for_tests())
            .await?;
    assert_eq!(stored_supersedes, Some(old_memory_id.into_inner()));

    let supersedes_edge_count: i64 = sqlx::query_scalar(
        "SELECT count(*)
           FROM proxima_core.edges
          WHERE relation = $1
            AND source_memory_id = $2
            AND target_memory_id = $3
            AND relation_class = 'Supersession'",
    )
    .bind(proxima_core::CORE_SUPERSEDES_RELATION)
    .bind(new_memory_id.into_inner())
    .bind(old_memory_id.into_inner())
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(supersedes_edge_count, 1);

    let (owner_kind, owner_id) = owner.columns();
    let head_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT m.memory_id
           FROM proxima_core.memories m
          WHERE m.owner_kind = $1
            AND m.owner_id = $2
            AND m.schema_id = $3
            AND m.kind = 'Abstraction'
            AND m.tombstoned_at IS NULL
            AND NOT EXISTS (
                 SELECT 1 FROM proxima_core.memories newer
                  WHERE newer.supersedes = m.memory_id
                    AND newer.tombstoned_at IS NULL
            )",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(AgentDerivationV1::SCHEMA_ID)
    .fetch_all(pg.pool_for_tests())
    .await?;
    assert!(head_ids.contains(&new_memory_id.into_inner()));
    assert!(!head_ids.contains(&old_memory_id.into_inner()));

    let lineage = pg
        .walk_memory_lineage(
            std::slice::from_ref(&owner),
            &proxima_core::verbs::query::MemoryLineageRequest {
                principal: owner,
                start_memory_id: new_memory_id,
                direction: proxima_core::verbs::query::MemoryLineageDirection::Ancestors,
                depth: 2,
                limit: 10,
            },
        )
        .await?;
    assert!(
        lineage
            .nodes
            .iter()
            .any(|node| node.memory_id == old_memory_id)
    );
    assert!(lineage.edges.iter().any(|edge| {
        edge.relation == proxima_core::CORE_SUPERSEDES_RELATION
            && edge.source_memory_id == new_memory_id
            && matches!(
                edge.target,
                proxima_core::EdgeTargetProjection::Visible {
                    target: proxima_core::EntityRef::Memory(target_memory_id),
                } if target_memory_id == old_memory_id
            )
    }));

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn author_derived_authorized_enforces_intra_owner_same_kind_supersedes()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let attacker = owner_fixture();
        let victim = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let victim_prior = insert_source_memory(
            &pg,
            &victim,
            EntityKind::Abstraction,
            "victim private abstraction",
        )
        .await?;
        let attacker_prior = insert_source_memory(
            &pg,
            &attacker,
            EntityKind::Abstraction,
            "attacker abstraction",
        )
        .await?;
        let attacker_perspective = insert_source_memory(
            &pg,
            &attacker,
            EntityKind::Perspective,
            "attacker perspective",
        )
        .await?;

        let engine = proxima_core::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let authz = AuthzContext::single_owner(&attacker, AuthPath::System);

        let foreign_new_id = MemoryId::new(Uuid::now_v7());
        let foreign_err = engine
            .author_derived_authorized(
                &authz,
                derived_authorized_request(
                    foreign_new_id,
                    attacker,
                    EntityKind::Abstraction,
                    "foreign successor",
                    Some(victim_prior),
                ),
            )
            .await
            .expect_err("foreign supersedes target must be forbidden");
        assert_eq!(foreign_err.code, ErrorCode::Forbidden);
        assert_eq!(
            foreign_err.message,
            "supersedes target is not an owned entity of the same owner"
        );
        assert_eq!(memory_count(&pg, foreign_new_id).await?, 0);
        assert_eq!(supersedes_pointer_count(&pg, victim_prior).await?, 0);
        assert_eq!(
            supersedes_edge_count(&pg, foreign_new_id, victim_prior).await?,
            0
        );

        let same_owner_new_id = MemoryId::new(Uuid::now_v7());
        let relation = engine
            .registry()
            .resolve_relation(CORE_DERIVED_FROM_RELATION)
            .expect("core relation registered");
        let same_owner_edges = [proxima_core::AuthorDerivedEdgeInput {
            relation,
            source_kind: EntityKind::Abstraction,
            source_memory_id: same_owner_new_id,
            target_kind: EntityKind::Abstraction,
            target_memory_id: attacker_prior,
            authorship_kind: EdgeAuthorshipKind::OperatorAtoA,
            authorship_owner_memory_id: Some(attacker_prior),
        }];
        let same_owner = engine
            .author_derived_authorized(
                &authz,
                proxima_core::AuthorDerivedRequestInput {
                    memory_id: same_owner_new_id,
                    owner: attacker,
                    kind: EntityKind::Abstraction,
                    text: "same-owner successor".to_string(),
                    schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
                    schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
                    operator_kind: MemoryOperatorKind::AtoA,
                    operator_id: OperatorId::new(Uuid::now_v7()),
                    input_contract_id: InputContractId::new(Uuid::now_v7()),
                    source_batch_id: None,
                    model_id: "agent-model",
                    prompt_version: "test-prompt",
                    sidecar_payload: SidecarPayload::abstraction(AgentDerivationV1 {
                        title: "same-owner successor".into(),
                        body: "same-owner successor".into(),
                        tags: Vec::new(),
                        idempotency_key: None,
                        source_memory_ids: vec![attacker_prior.into_inner()],
                        model_id: "agent-model".into(),
                        client_name: "test-client".into(),
                        client_version: "1".into(),
                    }),
                    supersedes: Some(attacker_prior),
                    edges: &same_owner_edges,
                },
            )
            .await?;
        assert_eq!(same_owner.memory_id, same_owner_new_id);
        assert_eq!(
            stored_supersedes(&pg, same_owner_new_id).await?,
            Some(attacker_prior.into_inner())
        );
        assert_eq!(
            supersedes_edge_count(&pg, same_owner_new_id, attacker_prior).await?,
            1
        );

        let wrong_kind_new_id = MemoryId::new(Uuid::now_v7());
        let wrong_kind_err = engine
            .author_derived_authorized(
                &authz,
                derived_authorized_request(
                    wrong_kind_new_id,
                    attacker,
                    EntityKind::Abstraction,
                    "wrong-kind successor",
                    Some(attacker_perspective),
                ),
            )
            .await
            .expect_err("same-owner wrong-kind supersedes target must be rejected");
        assert_eq!(wrong_kind_err.code, ErrorCode::InvalidArgument);
        assert_eq!(
            wrong_kind_err.message,
            "invalid argument supersedes: must supersede a memory of the same kind"
        );
        assert_eq!(memory_count(&pg, wrong_kind_new_id).await?, 0);
        assert_eq!(
            supersedes_pointer_count(&pg, attacker_perspective).await?,
            0
        );
        assert_eq!(
            supersedes_edge_count(&pg, wrong_kind_new_id, attacker_perspective).await?,
            0
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

async fn assert_embedding_row(
    pool: &sqlx::PgPool,
    memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let expected_embedding = padded_embedding([12.0, 1.0, 2.0]);
    let embedding_row: (String, i32, f64) = sqlx::query_as(
        "SELECT model_id, vector_dims(vec), 1 - (vec <=> $2::vector)
           FROM proxima_core.embeddings
          WHERE entity_kind = 'Abstraction' AND entity_id = $1",
    )
    .bind(memory_id)
    .bind(vector_literal(&expected_embedding))
    .fetch_one(pool)
    .await?;
    assert_eq!(embedding_row.0, "test-embed");
    assert_eq!(
        embedding_row.1,
        i32::try_from(EMBEDDING_DIM).expect("embedding dim fits i32")
    );
    assert!(
        (embedding_row.2 - 1.0).abs() <= 1.0e-6,
        "expected identical vector cosine, got {}",
        embedding_row.2
    );
    Ok(())
}

#[tokio::test]
async fn author_derived_rejects_empty_operator_inputs() {
    let engine = proxima_core::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    let owner = owner_fixture();
    let err = engine
        .author_derived(proxima_core::AuthorDerivedRequestInput {
            memory_id: MemoryId::new(Uuid::now_v7()),
            owner,
            kind: EntityKind::Abstraction,
            text: "body".into(),
            schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
            operator_kind: MemoryOperatorKind::AtoA,
            operator_id: OperatorId::new(Uuid::now_v7()),
            input_contract_id: InputContractId::new(Uuid::now_v7()),
            source_batch_id: None,
            model_id: "test-model",
            prompt_version: "test",
            sidecar_payload: SidecarPayload::abstraction(AgentDerivationV1 {
                title: "Derived".into(),
                body: "Body".into(),
                tags: Vec::new(),
                idempotency_key: None,
                source_memory_ids: Vec::new(),
                model_id: "test-model".into(),
                client_name: "test".into(),
                client_version: "1".into(),
            }),
            supersedes: None,
            edges: &[],
        })
        .await
        .expect_err("operator derivation without declared input edges is invalid");

    assert!(
        err.to_string()
            .contains("operator invocation inputs must be nonempty"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn ingest_fact_with_sidecar_writes_fact_and_note_sidecar()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let owner = owner_fixture();
    let registry = FlavorRegistry::new();
    let engine = proxima_core::Engine::new(registry.freeze_or_panic_for_tests());
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let note = AgentNoteV1 {
        note_id: Uuid::now_v7(),
        title: "Note title".into(),
        body: "Note body".into(),
        tags: vec!["tag".into()],
        idempotency_key: Some("note-1".into()),
    };
    let sidecar_payload = SidecarPayload::fact(note.clone());
    let draft = FactWriteCommand::from_payload(
        "test/source",
        SourceBatchId::new(Uuid::now_v7()),
        &note,
        time::OffsetDateTime::now_utc(),
    );
    let authorized = engine
        .authorize_fact_ingest(&authz, Relation::Ingest, draft)
        .await?;
    let outcome = pg
        .ingest_fact_with_typed_sidecar(&authorized, &sidecar_payload, None)
        .await?;

    let memory_row: (Option<EntityKind>, String) =
        sqlx::query_as("SELECT kind, text FROM proxima_core.memories WHERE memory_id = $1")
            .bind(outcome.memory_id.into_inner())
            .fetch_one(pg.pool_for_tests())
            .await?;
    assert_eq!(memory_row.0, None);
    assert_eq!(memory_row.1, "Note title\n\nNote body");

    let sidecar_row: (Uuid, String, String) = sqlx::query_as(
        "SELECT note_id, title, body
           FROM proxima_core.agent_note_v1
          WHERE memory_id = $1",
    )
    .bind(outcome.memory_id.into_inner())
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(sidecar_row.0, note.note_id);
    assert_eq!(sidecar_row.1, "Note title");
    assert_eq!(sidecar_row.2, "Note body");

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

async fn insert_source_abstraction(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
) -> Result<MemoryId, sqlx::Error> {
    insert_source_memory(pg, owner, EntityKind::Abstraction, "source abstraction").await
}

async fn insert_source_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    kind: EntityKind,
    text: &str,
) -> Result<MemoryId, sqlx::Error> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, $4, 1, $5,
                 $6,
                 CASE WHEN $5 = 'Perspective'::proxima_core.entity_kind
                      THEN 'AtoP'::proxima_core.memory_operator_kind
                      ELSE 'AtoA'::proxima_core.memory_operator_kind END,
                 '00000000-0000-0000-0000-000000000201'::uuid,
                 '00000000-0000-0000-0000-000000000202'::uuid,
                 NULL, 'source-model', 'source-prompt')",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(AgentDerivationV1::SCHEMA_ID)
    .bind(kind)
    .bind(text)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(MemoryId::new(memory_id))
}

fn derived_authorized_request(
    memory_id: MemoryId,
    owner: Owner,
    kind: EntityKind,
    title: &str,
    supersedes: Option<MemoryId>,
) -> proxima_core::AuthorDerivedRequestInput<'static> {
    proxima_core::AuthorDerivedRequestInput {
        memory_id,
        owner,
        kind,
        text: title.to_string(),
        schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
        operator_kind: MemoryOperatorKind::AtoA,
        operator_id: OperatorId::new(Uuid::now_v7()),
        input_contract_id: InputContractId::new(Uuid::now_v7()),
        source_batch_id: None,
        model_id: "agent-model",
        prompt_version: "test-prompt",
        sidecar_payload: derivation_sidecar(kind, title),
        supersedes,
        edges: &[],
    }
}

fn derivation_sidecar(kind: EntityKind, title: &str) -> SidecarPayload {
    let payload = AgentDerivationV1 {
        title: title.into(),
        body: title.into(),
        tags: Vec::new(),
        idempotency_key: None,
        source_memory_ids: Vec::new(),
        model_id: "agent-model".into(),
        client_name: "test-client".into(),
        client_version: "1".into(),
    };
    match kind {
        EntityKind::Abstraction => SidecarPayload::abstraction(payload),
        EntityKind::Perspective => SidecarPayload::perspective(payload),
        other => panic!("unexpected derived kind in test: {other:?}"),
    }
}

async fn memory_count(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: MemoryId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM proxima_core.memories WHERE memory_id = $1")
        .bind(memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await
}

async fn stored_supersedes(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: MemoryId,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT supersedes FROM proxima_core.memories WHERE memory_id = $1")
        .bind(memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await
}

async fn supersedes_pointer_count(
    pg: &proxima_storage_pg::PgStorage,
    prior: MemoryId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM proxima_core.memories WHERE supersedes = $1")
        .bind(prior.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await
}

async fn supersedes_edge_count(
    pg: &proxima_storage_pg::PgStorage,
    source: MemoryId,
    target: MemoryId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)
           FROM proxima_core.edges
          WHERE relation = $1
            AND source_memory_id = $2
            AND target_memory_id = $3
            AND relation_class = 'Supersession'",
    )
    .bind(proxima_core::CORE_SUPERSEDES_RELATION)
    .bind(source.into_inner())
    .bind(target.into_inner())
    .fetch_one(pg.pool_for_tests())
    .await
}
