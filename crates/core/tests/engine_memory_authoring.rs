use proxima_core::storage_ports::*;
mod common;

use std::sync::Arc;

use common::{
    ConstantEmbedding, EmbedRefusal, RefusingEmbedding, drop_db, fresh_pg, owner_fixture,
};
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

    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let outcome = engine
        .author_derived_authorized(
            &authz,
            proxima_core::AuthorDerivedRequestInput {
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
                lexical_language: None,
                edges: &edges,
            },
        )
        .await?;

    assert!(!outcome.idempotent_replay);
    assert_eq!(outcome.edge_ids.len(), 1);
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
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let old = engine
        .author_derived_authorized(
            &authz,
            proxima_core::AuthorDerivedRequestInput {
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
                lexical_language: None,
                edges: &old_edges,
            },
        )
        .await?;
    assert_eq!(old.memory_id, old_memory_id);
    assert_eq!(old.edge_ids.len(), 1);

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
        .author_derived_authorized(
            &authz,
            proxima_core::AuthorDerivedRequestInput {
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
                lexical_language: None,
                edges: &new_edges,
            },
        )
        .await?;
    assert_eq!(new.memory_id, new_memory_id);
    assert_eq!(new.edge_ids.len(), 1);

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
                owner,
                start_memory_id: new_memory_id,
                direction: proxima_core::verbs::query::MemoryLineageDirection::Ancestors,
                depth: 2,
                limit: 10,
                after: None,
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
        let authz = AuthzContext::single_owner(&attacker, AuthPath::HostBearer);

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
                    lexical_language: None,
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

/// Long enough that [`RefusingEmbedding`] refuses it and short enough that
/// its length is obviously not the point — the fixture's threshold is.
const UNEMBEDDABLE_BODY: &str = "a derived body no local runner will take";

/// The threshold the fixtures refuse above. Well under
/// [`UNEMBEDDABLE_BODY`] and well over the engine's liveness probe, so
/// "refuses the real text" and "answers the probe" are independent.
const REFUSES_ABOVE_CHARS: usize = 8;

/// Author one Abstraction through the public verb with `client` wired as
/// the engine's embedder. Hands the engine back so a test can drain, and
/// the result unresolved so a test can assert on the failure.
async fn derive_once_with(
    pg: &proxima_storage_pg::PgStorage,
    owner: Owner,
    client: Arc<dyn proxima_core::llm::EmbeddingClient>,
    body: &str,
) -> (
    proxima_core::Engine,
    MemoryId,
    Result<proxima_core::AuthorDerivedAuthorizedOutcome, proxima_core::ProtocolError>,
) {
    let source = insert_source_abstraction(pg, &owner)
        .await
        .expect("source abstraction inserts");
    let engine = proxima_core::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports())
        .with_embed(client);
    let relation = engine
        .registry()
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .expect("core relation registered");
    let memory_id = MemoryId::new(Uuid::now_v7());
    let edges = [proxima_core::AuthorDerivedEdgeInput {
        relation,
        source_kind: EntityKind::Abstraction,
        source_memory_id: memory_id,
        target_kind: EntityKind::Abstraction,
        target_memory_id: source,
        authorship_kind: EdgeAuthorshipKind::OperatorAtoA,
        authorship_owner_memory_id: Some(source),
    }];
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let outcome = engine
        .author_derived_authorized(
            &authz,
            proxima_core::AuthorDerivedRequestInput {
                edges: &edges,
                text: body.to_string(),
                ..derived_authorized_request(memory_id, owner, EntityKind::Abstraction, body, None)
            },
        )
        .await;
    (engine, memory_id, outcome)
}

/// `(status, model_id)` of every embedding job standing against a memory.
async fn embedding_jobs_for(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: MemoryId,
) -> Result<Vec<(String, String, EntityKind)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT status::text, model_id, entity_kind
           FROM proxima_core.embedding_jobs
          WHERE entity_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_all(pg.pool_for_tests())
    .await
}

async fn embedding_row_count(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: MemoryId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM proxima_core.embeddings WHERE entity_id = $1")
        .bind(memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await
}

/// The defect, at its narrowest: the derive phase of a corpus ingest died
/// on one over-long section unit, discarding 326 written units and twenty
/// minutes of GPU already spent upstream, because the derived write embeds
/// synchronously and had no rescue. Facts have survived this since ingest
/// existed — they enqueue a job and let the drain (which bisects
/// over-limit input) deal with it. Nothing tested this path with a failing
/// embedder at all, which is how the asymmetry shipped.
#[tokio::test]
async fn a_permanently_refused_derived_text_is_still_written()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let owner = owner_fixture();

    let (engine, memory_id, outcome) = derive_once_with(
        &pg,
        owner,
        Arc::new(RefusingEmbedding::provider_up(
            "test-embed",
            REFUSES_ABOVE_CHARS,
            EmbedRefusal::Permanent,
        )),
        UNEMBEDDABLE_BODY,
    )
    .await;

    let outcome = outcome.expect("a text one provider call refuses is not a failed write");
    assert_eq!(outcome.memory_id, memory_id);
    assert!(
        outcome.embedding_deferred,
        "a memory that landed with no vector must say so, not only in a log line",
    );
    assert_eq!(memory_count(&pg, memory_id).await?, 1);
    assert_eq!(embedding_row_count(&pg, memory_id).await?, 0);
    assert_eq!(
        embedding_jobs_for(&pg, memory_id).await?,
        vec![(
            "pending".to_string(),
            "test-embed".to_string(),
            EntityKind::Abstraction,
        )],
        "the vector must be owed by a job row, in the same transaction as the memory",
    );

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// The error production actually saw is *not* the clean one. A local
/// runner that an over-long input kills answers `400 {"error": "… EOF"}`,
/// which is classified transient because nothing looked at the input — so
/// keying the rescue on `EmbedPermanent` alone would have left the
/// reported failure unfixed. The provider answering a liveness probe right
/// afterwards is what attributes the refusal to the text.
#[tokio::test]
async fn a_transient_refusal_from_a_live_provider_defers_instead_of_failing()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let owner = owner_fixture();

    let (engine, memory_id, outcome) = derive_once_with(
        &pg,
        owner,
        Arc::new(RefusingEmbedding::provider_up(
            "test-embed",
            REFUSES_ABOVE_CHARS,
            EmbedRefusal::Transient,
        )),
        UNEMBEDDABLE_BODY,
    )
    .await;

    assert!(
        outcome
            .expect("a live provider refusing one text is not a failed write")
            .embedding_deferred,
    );
    assert_eq!(memory_count(&pg, memory_id).await?, 1);
    assert_eq!(embedding_jobs_for(&pg, memory_id).await?.len(), 1);

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Deferring is only worth anything if something collects the debt. The
/// enqueued job must be claimable by the drain that owns the bisecting
/// rescue, and a drain against a working client must leave the memory with
/// the vector the write could not produce.
#[tokio::test]
async fn a_deferred_derived_embedding_is_drained_into_a_vector()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let owner = owner_fixture();

    let (engine, memory_id, outcome) = derive_once_with(
        &pg,
        owner,
        Arc::new(RefusingEmbedding::provider_up(
            "test-embed",
            REFUSES_ABOVE_CHARS,
            EmbedRefusal::Permanent,
        )),
        UNEMBEDDABLE_BODY,
    )
    .await;
    assert!(outcome?.embedding_deferred);

    // Same model id, or the claim query would not see the job: the drain
    // is scoped to the currently active embedding model.
    engine
        .set_embed_client(Some(Arc::new(ConstantEmbedding::prefixed(
            "test-embed",
            &[12.0, 1.0, 2.0],
        ))))
        .await;
    let drained = engine.drain_embedding_jobs(16).await?;
    assert_eq!(drained.processed, 1);
    assert_eq!(drained.failed, 0);

    assert_embedding_row(pg.pool_for_tests(), memory_id.into_inner()).await?;
    assert!(
        embedding_jobs_for(&pg, memory_id).await?.is_empty(),
        "a completed job is deleted, not left claimable forever",
    );

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// The other side of the classification, and the reason it cannot simply
/// be "defer on any error". A provider that is merely down says nothing
/// about the text, and quietly writing every derived memory of a long run
/// with no vector would trade one loud failure for a silent corpus of
/// half-written memories. The probe fails here too, so the write fails —
/// exactly as it did before this change.
#[tokio::test]
async fn a_provider_that_is_down_still_fails_the_derived_write()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let owner = owner_fixture();

    let (engine, memory_id, outcome) = derive_once_with(
        &pg,
        owner,
        Arc::new(RefusingEmbedding::provider_down(
            "test-embed",
            EmbedRefusal::Transient,
        )),
        UNEMBEDDABLE_BODY,
    )
    .await;

    let err = outcome.expect_err("an unavailable provider must not mint unembedded memories");
    assert_eq!(err.code, ErrorCode::Internal);
    assert!(
        err.message.contains("embed derived memory text"),
        "unexpected message: {}",
        err.message,
    );
    assert_eq!(memory_count(&pg, memory_id).await?, 0);
    assert!(embedding_jobs_for(&pg, memory_id).await?.is_empty());

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
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
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let err = engine
        .author_derived_authorized(
            &authz,
            proxima_core::AuthorDerivedRequestInput {
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
                lexical_language: None,
                edges: &[],
            },
        )
        .await
        .expect_err("operator derivation without declared input edges is invalid");

    assert!(
        err.to_string()
            .contains("operator invocation inputs must be nonempty"),
        "unexpected error: {err}"
    );
}

/// Operator-provenance fixtures.
///
/// Cases 1 ("missing output-to-input provenance edge") and 2 ("edge source
/// not equal to output memory") collapse onto the
/// SAME runtime check in this data model: `AuthorDerivedRequestInput`'s
/// `edges` array supplies both the manifest's `inputs` list AND its
/// `output_edges` list from the same element (`OperatorInvocationManifest`
/// in `crates/core/src/operator_proofs.rs`), so an edge whose
/// `source_memory_id` is not the output memory is, by construction, an
/// input with no provenance edge back to the output —
/// `OperatorProofError::MissingProvenanceEdge` fires for both framings. The
/// two tests below exercise it at different edge-list multiplicities (one
/// bad edge in isolation; one bad edge alongside a correctly-sourced one)
/// so the check is pinned as regression coverage from both angles rather
/// than testing the identical single-edge shape twice.
#[tokio::test]
async fn author_derived_authorized_rejects_operator_edge_sourced_from_wrong_memory()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = owner_fixture();
        let real_input = insert_source_abstraction(&pg, &owner).await?;
        let unrelated_memory =
            insert_source_memory(&pg, &owner, EntityKind::Abstraction, "unrelated").await?;
        let engine = proxima_core::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let relation = engine
            .registry()
            .resolve_relation(CORE_DERIVED_FROM_RELATION)
            .expect("core relation registered");
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let memory_id = MemoryId::new(Uuid::now_v7());

        // The edge's declared source is `unrelated_memory`, not `memory_id`
        // (the actual output) — the input it claims to provenance
        // (`real_input`) therefore has no edge pointing back to the real
        // output.
        let edges = [proxima_core::AuthorDerivedEdgeInput {
            relation,
            source_kind: EntityKind::Abstraction,
            source_memory_id: unrelated_memory,
            target_kind: EntityKind::Abstraction,
            target_memory_id: real_input,
            authorship_kind: EdgeAuthorshipKind::OperatorAtoA,
            authorship_owner_memory_id: Some(real_input),
        }];

        let err = engine
            .author_derived_authorized(
                &authz,
                proxima_core::AuthorDerivedRequestInput {
                    memory_id,
                    owner,
                    kind: EntityKind::Abstraction,
                    text: "derived".into(),
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
                        source_memory_ids: vec![real_input.into_inner()],
                        model_id: "test-model".into(),
                        client_name: "test".into(),
                        client_version: "1".into(),
                    }),
                    supersedes: None,
                    lexical_language: None,
                    edges: &edges,
                },
            )
            .await
            .expect_err("edge sourced from a memory other than the output is invalid");

        assert_eq!(
            err.code,
            ErrorCode::InvalidArgument,
            "unexpected error: {err}"
        );
        assert!(
            err.to_string().contains("missing provenance edge")
                || err
                    .to_string()
                    .contains("operator provenance edge source must be the output memory"),
            "unexpected error: {err}"
        );
        assert_eq!(memory_count(&pg, memory_id).await?, 0);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn author_derived_authorized_rejects_operator_input_missing_provenance_edge_to_output()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = owner_fixture();
        let input_a = insert_source_abstraction(&pg, &owner).await?;
        let input_b =
            insert_source_memory(&pg, &owner, EntityKind::Abstraction, "second input").await?;
        let engine = proxima_core::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let relation = engine
            .registry()
            .resolve_relation(CORE_DERIVED_FROM_RELATION)
            .expect("core relation registered");
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let memory_id = MemoryId::new(Uuid::now_v7());

        // input_a is correctly provenanced (source == output memory_id).
        // input_b's edge is sourced from input_a instead of the output, so
        // input_b never gets a provenance edge back to the real output —
        // a second, unrelated declared input silently loses its proof.
        let edges = [
            proxima_core::AuthorDerivedEdgeInput {
                relation,
                source_kind: EntityKind::Abstraction,
                source_memory_id: memory_id,
                target_kind: EntityKind::Abstraction,
                target_memory_id: input_a,
                authorship_kind: EdgeAuthorshipKind::OperatorAtoA,
                authorship_owner_memory_id: Some(input_a),
            },
            proxima_core::AuthorDerivedEdgeInput {
                relation,
                source_kind: EntityKind::Abstraction,
                source_memory_id: input_a,
                target_kind: EntityKind::Abstraction,
                target_memory_id: input_b,
                authorship_kind: EdgeAuthorshipKind::OperatorAtoA,
                authorship_owner_memory_id: Some(input_b),
            },
        ];

        let err = engine
            .author_derived_authorized(
                &authz,
                proxima_core::AuthorDerivedRequestInput {
                    memory_id,
                    owner,
                    kind: EntityKind::Abstraction,
                    text: "derived".into(),
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
                        source_memory_ids: vec![input_a.into_inner(), input_b.into_inner()],
                        model_id: "test-model".into(),
                        client_name: "test".into(),
                        client_version: "1".into(),
                    }),
                    supersedes: None,
                    lexical_language: None,
                    edges: &edges,
                },
            )
            .await
            .expect_err("a declared input with no provenance edge to the output is invalid");

        assert_eq!(
            err.code,
            ErrorCode::InvalidArgument,
            "unexpected error: {err}"
        );
        assert!(
            err.to_string().contains("missing provenance edge")
                || err
                    .to_string()
                    .contains("operator provenance edge source must be the output memory"),
            "unexpected error: {err}"
        );
        assert_eq!(memory_count(&pg, memory_id).await?, 0);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

/// Publicly-observable half of the "F→A without source batch" fixture (see
/// `author_derived_rejects_operator_ftoa_missing_source_batch` in
/// `crates/core/src/engine/memory_authoring.rs` for the internal check this
/// complements). `author_derived_authorized` never reaches that internal
/// check with `source_batch_id: None` for F→A: `effective_operator_source_batch_id`
/// recomputes the batch from declared F→A input edges first, and with zero
/// edges it fails even earlier ("F→A operator invocation requires source
/// inputs"). Either way, a caller cannot get an F→A derivation through the
/// public API without a legitimate closed source batch.
#[tokio::test]
async fn author_derived_authorized_rejects_operator_ftoa_without_source_batch_via_public_api()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = owner_fixture();
        let engine = proxima_core::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let memory_id = MemoryId::new(Uuid::now_v7());

        let err = engine
            .author_derived_authorized(
                &authz,
                proxima_core::AuthorDerivedRequestInput {
                    memory_id,
                    owner,
                    kind: EntityKind::Abstraction,
                    text: "derived".into(),
                    schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
                    schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
                    operator_kind: MemoryOperatorKind::FtoA,
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
                    lexical_language: None,
                    edges: &[],
                },
            )
            .await
            .expect_err("F→A invocation without any source batch is invalid before storage");

        assert_eq!(
            err.code,
            ErrorCode::InvalidArgument,
            "unexpected error: {err}"
        );
        assert_eq!(memory_count(&pg, memory_id).await?, 0);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn ingest_fact_with_sidecar_writes_fact_and_note_sidecar()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let owner = owner_fixture();
    let registry = FlavorRegistry::new();
    let engine = proxima_core::Engine::new(registry.freeze_or_panic_for_tests());
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
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
        .ingest_fact_with_typed_sidecar(
            &authorized,
            std::slice::from_ref(&sidecar_payload),
            None,
            None,
        )
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
        lexical_language: None,
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
