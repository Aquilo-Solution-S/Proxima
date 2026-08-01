//! The index is a consequence of node writes, and nothing else writes it.
//!
//! These tests drive the real authoring paths — derive, interpret, Fact
//! ingest — and then look at `proxima_core.edges` to check that the rows it
//! holds are exactly the rows the nodes imply. Nothing here calls an edge
//! verb, because there is none.

use std::sync::Arc;

use proxima_core::storage_ports::*;
use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
use proxima_core::{
    AbstractionPayload, AgentDerivationV1, AuthPath, AuthzContext, EdgeEndpoint, EntityKind,
    FlavorRegistry, InputContractId, InterpretationSubjectKind, InterpretationV1, MemoryId,
    MemoryOperatorKind, OperatorId, Owner, OwnerRef, PerspectivePayload, Relation, SchemaId,
    SchemaVersion, SidecarPayload, SourceBatchId, SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use crate::common::{count_memory_edges, drop_db, fresh_pg, owner_write_permit, seed_memory};

fn engine_for(pg: &PgStorage) -> proxima_core::Engine {
    proxima_core::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports())
}

fn fresh_owner() -> Owner {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
}

fn derivation_payload(body: &str, sources: &[MemoryId]) -> AgentDerivationV1 {
    AgentDerivationV1 {
        title: "derived".into(),
        body: body.into(),
        tags: Vec::new(),
        idempotency_key: None,
        source_memory_ids: sources.iter().map(|id| id.into_inner()).collect(),
        model_id: "test-model".into(),
        client_name: "test".into(),
        client_version: "1".into(),
    }
}

/// Author one Abstraction from one Abstraction input, through the engine.
async fn derive_from(
    engine: &proxima_core::Engine,
    owner: Owner,
    memory_id: MemoryId,
    input: MemoryId,
) -> Result<proxima_core::AuthorDerivedAuthorizedOutcome, proxima_core::ProtocolError> {
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let derived_from = [EdgeEndpoint::memory(EntityKind::Abstraction, input)];
    engine
        .author_derived_authorized(
            &authz,
            proxima_core::AuthorDerivedRequestInput {
                memory_id,
                owner,
                kind: EntityKind::Abstraction,
                text: "derived".into(),
                schema_id: SchemaId::new(
                    <AgentDerivationV1 as AbstractionPayload>::SCHEMA_ID.into(),
                ),
                schema_version: SchemaVersion::new(
                    <AgentDerivationV1 as AbstractionPayload>::SCHEMA_VERSION,
                ),
                operator_kind: MemoryOperatorKind::AtoA,
                operator_id: OperatorId::new(Uuid::new_v5(
                    &Uuid::NAMESPACE_OID,
                    b"edge-index-operator",
                )),
                input_contract_id: InputContractId::new(Uuid::new_v5(
                    &Uuid::NAMESPACE_OID,
                    b"edge-index-contract",
                )),
                source_batch_id: None,
                model_id: "test-model",
                prompt_version: "edge-index-pg",
                sidecar_payload: SidecarPayload::abstraction(derivation_payload(
                    "derived",
                    &[input],
                )),
                authoring_perspective_id: None,
                derived_from: &derived_from,
                supersedes: None,
                lexical_language: None,
            },
        )
        .await
}

/// Author one interpretation Perspective over the given subjects.
async fn interpret(
    engine: &proxima_core::Engine,
    owner: Owner,
    memory_id: MemoryId,
    payload: InterpretationV1,
) -> Result<proxima_core::AuthorDerivedAuthorizedOutcome, proxima_core::ProtocolError> {
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    engine
        .author_derived_authorized(
            &authz,
            proxima_core::AuthorDerivedRequestInput {
                memory_id,
                owner,
                kind: EntityKind::Perspective,
                text: payload.claim.clone(),
                schema_id: SchemaId::new(
                    <InterpretationV1 as PerspectivePayload>::SCHEMA_ID.into(),
                ),
                schema_version: SchemaVersion::new(
                    <InterpretationV1 as PerspectivePayload>::SCHEMA_VERSION,
                ),
                operator_kind: MemoryOperatorKind::AtoP,
                operator_id: OperatorId::new(Uuid::now_v7()),
                input_contract_id: InputContractId::new(Uuid::now_v7()),
                source_batch_id: None,
                model_id: "test-model",
                prompt_version: "edge-index-pg",
                sidecar_payload: SidecarPayload::perspective(payload),
                authoring_perspective_id: None,
                // An interpretation consumes nothing; it grounds through the
                // references its payload carries.
                derived_from: &[],
                supersedes: None,
                lexical_language: None,
            },
        )
        .await
}

/// E5. Replaying a write re-asserts the same primary key; the second run adds
/// nothing and says so with the same count.
#[tokio::test]
async fn a_replayed_derivation_asserts_the_same_rows() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = fresh_owner();
        let engine = engine_for(&pg);
        let input = seed_memory(&pg, &owner, EntityKind::Abstraction, "input").await?;
        let memory_id = MemoryId::new(Uuid::now_v7());

        let first = derive_from(&engine, owner, memory_id, input).await?;
        assert!(!first.idempotent_replay);
        assert_eq!(first.edge_count, 1);
        assert_eq!(count_memory_edges(&pg, memory_id, input).await?, 1);

        let replay = derive_from(&engine, owner, memory_id, input).await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.edge_count, 1, "a replay reports what it re-asserted");
        assert_eq!(
            count_memory_edges(&pg, memory_id, input).await?,
            1,
            "structural idempotency: the key is the row, so there is no second one"
        );

        // Nor a second change event: nothing changed, so nothing is announced.
        let events: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.change_event
              WHERE kind = 'EdgeAppend' AND edge_source_id = $1",
        )
        .bind(memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(events, 1);
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// A derivation declaration produces `origin`. The caller named a target and
/// never a kind — there is no parameter for one anywhere on the path.
#[tokio::test]
async fn a_derivation_declaration_produces_origin() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = fresh_owner();
        let engine = engine_for(&pg);
        let input = seed_memory(&pg, &owner, EntityKind::Abstraction, "input").await?;
        let memory_id = MemoryId::new(Uuid::now_v7());
        derive_from(&engine, owner, memory_id, input).await?;

        let kind: String = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.edges WHERE source_id = $1 AND target_id = $2",
        )
        .bind(memory_id.into_inner())
        .bind(input.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(kind, "origin");
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// An interpretation's rows are `reference`, and they are re-derivable from
/// the payload alone — E7 in its smallest form.
#[tokio::test]
async fn an_interpretation_writes_references_derivable_from_its_payload()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = fresh_owner();
        let engine = engine_for(&pg);
        let subject_a = seed_memory(&pg, &owner, EntityKind::Fact, "the deploy").await?;
        let subject_b = seed_memory(&pg, &owner, EntityKind::Abstraction, "the outage").await?;
        let memory_id = MemoryId::new(Uuid::now_v7());

        let payload = InterpretationV1 {
            claim: "the outage followed the deploy".into(),
            confidence: 80,
            subject_memory_ids: vec![subject_a.into_inner(), subject_b.into_inner()],
            subject_kinds: vec![
                InterpretationSubjectKind::Fact,
                InterpretationSubjectKind::Abstraction,
            ],
            model_id: "test-model".into(),
            client_name: "test".into(),
            client_version: "1".into(),
        };
        let outcome = interpret(&engine, owner, memory_id, payload.clone()).await?;
        assert_eq!(outcome.edge_count, 2);

        let rows: Vec<(String, Uuid)> = sqlx::query_as(
            "SELECT kind::text, target_id FROM proxima_core.edges
              WHERE source_id = $1 ORDER BY target_id",
        )
        .bind(memory_id.into_inner())
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|(kind, _)| kind == "reference"));

        let mut declared: Vec<Uuid> = payload
            .references()
            .into_iter()
            .filter_map(|reference| reference.target.memory_id().map(MemoryId::into_inner))
            .collect();
        let mut stored: Vec<Uuid> = rows.into_iter().map(|(_, target)| target).collect();
        stored.sort_unstable();
        declared.sort_unstable();
        assert_eq!(stored, declared);

        // The claim landed in its own sidecar, where the reason and the
        // confidence that used to ride on an edge now live.
        let stored_claim: String = sqlx::query_scalar(
            "SELECT claim FROM proxima_core.interpretation_v1 WHERE memory_id = $1",
        )
        .bind(memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(stored_claim, "the outage followed the deploy");
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// A Fact declaring what it was read from lands its origin row inside its own
/// ingest transaction — and a receipt replay re-asserts the same key rather
/// than minting a second row. That is what #156's id scheme and its partial
/// unique index were approximating.
#[tokio::test]
async fn a_fact_declares_what_it_was_made_from() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = fresh_owner();
        let upload = seed_memory(&pg, &owner, EntityKind::Fact, "the uploaded pdf").await?;
        let permit = owner_write_permit(&owner, proxima_core::AccessKind::Fact).await?;
        let now = time::OffsetDateTime::now_utc();
        let draft = FactWriteCommand {
            schema_id: SchemaId::new("test/ocr-reading-v1".into()),
            schema_version: SchemaVersion::new(1),
            payload: b"ocr-reading".to_vec(),
            rendered_text: Some("the text on page one".into()),
            lexical_language: None,
            receipt: Some(FactReceiptDraft {
                source_id: SourceId::new("test/ocr"),
                source_batch_id: SourceBatchId::new(Uuid::now_v7()),
                observed_at: now,
                occurred_at: now,
            }),
            citation: None,
            derived_from: vec![EdgeEndpoint::memory(EntityKind::Fact, upload)],
        };

        let first = pg.ingest_fact_atomic(&permit, &draft, None).await?;
        assert!(!first.idempotent_replay);
        assert_eq!(count_memory_edges(&pg, first.memory_id, upload).await?, 1);

        let replay = pg.ingest_fact_atomic(&permit, &draft, None).await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.memory_id, first.memory_id);
        assert_eq!(
            count_memory_edges(&pg, first.memory_id, upload).await?,
            1,
            "a receipt replay re-asserts the same key and writes no second row"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// Nothing on the write path accepts a caller-chosen kind, and the enum
/// admits nothing beyond the two. This is the test that says so out loud, so
/// a third kind has to delete it first.
#[tokio::test]
async fn no_write_path_accepts_a_caller_chosen_kind() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = fresh_owner();
        let engine = engine_for(&pg);
        let input = seed_memory(&pg, &owner, EntityKind::Abstraction, "input").await?;
        let memory_id = MemoryId::new(Uuid::now_v7());
        derive_from(&engine, owner, memory_id, input).await?;

        let kinds: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT kind::text FROM proxima_core.edges")
                .fetch_all(pg.pool_for_tests())
                .await?;
        assert!(
            kinds
                .iter()
                .all(|kind| kind == "origin" || kind == "reference"),
            "the vocabulary is closed at two: {kinds:?}"
        );

        let labels: Vec<String> = sqlx::query_scalar(
            "SELECT enumlabel::text FROM pg_enum e
               JOIN pg_type t ON t.oid = e.enumtypid
               JOIN pg_namespace n ON n.oid = t.typnamespace
              WHERE n.nspname = 'proxima_core' AND t.typname = 'edge_kind'
              ORDER BY e.enumsortorder",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert_eq!(labels, vec!["origin".to_string(), "reference".to_string()]);
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// Dropping the index and re-deriving it from node content yields the same
/// set. This is the master invariant; the rest are its corollaries.
#[tokio::test]
async fn the_index_is_rebuildable_from_node_content() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = fresh_owner();
        let engine = engine_for(&pg);
        let subject = seed_memory(&pg, &owner, EntityKind::Fact, "subject").await?;
        let memory_id = MemoryId::new(Uuid::now_v7());
        interpret(
            &engine,
            owner,
            memory_id,
            InterpretationV1 {
                claim: "a claim".into(),
                confidence: 50,
                subject_memory_ids: vec![subject.into_inner()],
                subject_kinds: vec![InterpretationSubjectKind::Fact],
                model_id: "test-model".into(),
                client_name: "test".into(),
                client_version: "1".into(),
            },
        )
        .await?;

        let before: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
            "SELECT source_id, target_id, kind::text FROM proxima_core.edges
              ORDER BY source_id, target_id, kind",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert_eq!(before.len(), 1);

        // Re-derive: drop every reference row and rebuild it from the
        // interpretation sidecar, which is where the statement lives.
        sqlx::query("DELETE FROM proxima_core.edges WHERE kind = 'reference'")
            .execute(pg.pool_for_tests())
            .await?;
        sqlx::query(
            "INSERT INTO proxima_core.edges
                (source_kind, source_id, target_kind, target_id, kind, owner_kind, owner_id)
             SELECT 'Perspective', i.memory_id,
                    subject.kind::proxima_core.edge_endpoint_kind, subject.memory_id,
                    'reference', m.owner_kind, m.owner_id
               FROM proxima_core.interpretation_v1 i
               JOIN proxima_core.memories m ON m.memory_id = i.memory_id
               CROSS JOIN LATERAL unnest(i.subject_memory_ids, i.subject_kinds)
                   AS subject(memory_id, kind)",
        )
        .execute(pg.pool_for_tests())
        .await?;

        let after: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
            "SELECT source_id, target_id, kind::text FROM proxima_core.edges
              ORDER BY source_id, target_id, kind",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert_eq!(after, before, "the rebuilt set is the stored set");
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// Fact ingest through the gated path carries the links the engine resolved
/// and read-checked, and writes them in the Fact's own transaction.
#[tokio::test]
async fn a_gated_fact_write_carries_its_declared_links() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = fresh_owner();
        let engine = engine_for(&pg);
        let referenced = seed_memory(&pg, &owner, EntityKind::Fact, "the thing pointed at").await?;
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let now = time::OffsetDateTime::now_utc();
        let draft = FactWriteCommand {
            schema_id: SchemaId::new("core/agent-note-v1".into()),
            schema_version: SchemaVersion::new(1),
            payload: b"note".to_vec(),
            rendered_text: Some("a note".into()),
            lexical_language: None,
            receipt: Some(FactReceiptDraft {
                source_id: SourceId::new("test/notes"),
                source_batch_id: SourceBatchId::new(Uuid::now_v7()),
                observed_at: now,
                occurred_at: now,
            }),
            citation: None,
            derived_from: vec![EdgeEndpoint::memory(EntityKind::Fact, referenced)],
        };
        let sidecar = SidecarPayload::fact(proxima_core::AgentNoteV1 {
            note_id: Uuid::now_v7(),
            title: "a note".into(),
            body: "about that other fact".into(),
            tags: Vec::new(),
            idempotency_key: None,
        });
        let authorized = engine
            .authorize_fact_ingest(
                &authz,
                Relation::Ingest,
                draft,
                std::slice::from_ref(&sidecar),
            )
            .await?;
        let outcome = pg
            .ingest_fact_with_typed_sidecar(&authorized, std::slice::from_ref(&sidecar), None)
            .await?;

        assert_eq!(
            count_memory_edges(&pg, outcome.memory_id, referenced).await?,
            1,
            "the declared origin landed with the Fact"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}
