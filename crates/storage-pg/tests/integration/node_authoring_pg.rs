//! End-to-end node authoring against live Postgres.
//!
//! Derivation lands origin rows; a revision lands a lineage pointer and no
//! edge; authorship is a column; goal topology is columns plus reference
//! rows; an upload citation lands on the Fact — each emits change events.

use std::sync::Arc;

use proxima_core::storage_ports::*;
use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
use proxima_core::verbs::goal_write::{GoalCreateRequest, GoalState, IdempotencyKey};
use proxima_core::{
    AbstractionPayload, AgentDerivationV1, AuthPath, AuthzContext, ChangeEventKind, EdgeEndpoint,
    EdgeKind, EntityKind, EntityRef, FlavorRegistry, GoalAssignmentTarget, GoalEvidenceRef,
    InputContractId, MemoryId, MemoryOperatorKind, OperatorId, Owner, OwnerRef, SchemaId,
    SchemaVersion, SidecarPayload, SimpleTextGoalV1, SourceBatchId, SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use crate::common::{drop_db, fresh_pg, owner_write_permit, seed_memory};

fn engine_for(pg: &PgStorage) -> proxima_core::Engine {
    proxima_core::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports())
}

fn fresh_owner() -> Owner {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
}

fn derivation(body: &str, sources: &[MemoryId]) -> AgentDerivationV1 {
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

struct DeriveInput<'a> {
    owner: Owner,
    memory_id: MemoryId,
    input: MemoryId,
    supersedes: Option<MemoryId>,
    authoring_perspective_id: Option<MemoryId>,
    body: &'a str,
}

async fn author_abstraction(
    engine: &proxima_core::Engine,
    args: DeriveInput<'_>,
) -> Result<proxima_core::AuthorDerivedAuthorizedOutcome, proxima_core::ProtocolError> {
    let authz = AuthzContext::single_owner(&args.owner, AuthPath::HostBearer);
    let derived_from = [EdgeEndpoint::memory(EntityKind::Abstraction, args.input)];
    engine
        .author_derived_authorized(
            &authz,
            proxima_core::AuthorDerivedRequestInput {
                memory_id: args.memory_id,
                owner: args.owner,
                kind: EntityKind::Abstraction,
                text: args.body.into(),
                schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
                schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
                operator_kind: MemoryOperatorKind::AtoA,
                operator_id: OperatorId::new(Uuid::now_v7()),
                input_contract_id: InputContractId::new(Uuid::now_v7()),
                source_batch_id: None,
                model_id: "test-model",
                prompt_version: "node-authoring-pg",
                sidecar_payload: SidecarPayload::abstraction(derivation(args.body, &[args.input])),
                authoring_perspective_id: args.authoring_perspective_id,
                derived_from: &derived_from,
                supersedes: args.supersedes,
                lexical_language: None,
            },
        )
        .await
}

/// The derive path end to end: the row lands, its sidecar lands, its origin
/// row lands, and both the entity append and the edge append are announced.
#[tokio::test]
async fn a_derivation_lands_its_row_its_sidecar_and_its_origin()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = fresh_owner();
        let engine = engine_for(&pg);
        let input = seed_memory(&pg, &owner, EntityKind::Abstraction, "input").await?;
        let memory_id = MemoryId::new(Uuid::now_v7());

        let outcome = author_abstraction(
            &engine,
            DeriveInput {
                owner,
                memory_id,
                input,
                supersedes: None,
                authoring_perspective_id: None,
                body: "the derived view",
            },
        )
        .await?;
        assert!(!outcome.idempotent_replay);
        assert_eq!(outcome.edge_count, 1);

        let sidecar_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.agent_derivation_v1 WHERE memory_id = $1",
        )
        .bind(memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(sidecar_rows, 1);

        let entity_events: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.change_event
              WHERE kind = 'EntityAppend' AND entity_memory_id = $1",
        )
        .bind(memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(entity_events, 1);

        let edge_events: Vec<(String, Uuid)> = sqlx::query_as(
            "SELECT edge_kind::text, edge_target_id FROM proxima_core.change_event
              WHERE kind = 'EdgeAppend' AND edge_source_id = $1",
        )
        .bind(memory_id.into_inner())
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert_eq!(
            edge_events,
            vec![("origin".to_string(), input.into_inner())]
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// Supersession is the same thing persisting through revision: a pointer on
/// each row, written in the successor's transaction, and NO edge.
#[tokio::test]
async fn a_revision_moves_a_pointer_and_writes_no_edge() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = fresh_owner();
        let engine = engine_for(&pg);
        let input = seed_memory(&pg, &owner, EntityKind::Abstraction, "input").await?;

        let prior_id = MemoryId::new(Uuid::now_v7());
        author_abstraction(
            &engine,
            DeriveInput {
                owner,
                memory_id: prior_id,
                input,
                supersedes: None,
                authoring_perspective_id: None,
                body: "first take",
            },
        )
        .await?;

        let revision_id = MemoryId::new(Uuid::now_v7());
        author_abstraction(
            &engine,
            DeriveInput {
                owner,
                memory_id: revision_id,
                input,
                supersedes: Some(prior_id),
                authoring_perspective_id: None,
                body: "second take",
            },
        )
        .await?;

        let (supersedes, superseded_by): (Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "SELECT r.supersedes, p.superseded_by
               FROM proxima_core.memories r
               JOIN proxima_core.memories p ON p.memory_id = $1
              WHERE r.memory_id = $2",
        )
        .bind(prior_id.into_inner())
        .bind(revision_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(supersedes, Some(prior_id.into_inner()));
        assert_eq!(
            superseded_by,
            Some(revision_id.into_inner()),
            "the prior row learns it is no longer the head"
        );

        let between: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.edges
              WHERE (source_id = $1 AND target_id = $2)
                 OR (source_id = $2 AND target_id = $1)",
        )
        .bind(revision_id.into_inner())
        .bind(prior_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            between, 0,
            "supersession is not a connection between things"
        );

        // A second successor for the same prior head is a conflict, not a
        // silent overwrite.
        let third = MemoryId::new(Uuid::now_v7());
        let err = author_abstraction(
            &engine,
            DeriveInput {
                owner,
                memory_id: third,
                input,
                supersedes: Some(prior_id),
                authoring_perspective_id: None,
                body: "third take",
            },
        )
        .await
        .expect_err("the prior row is no longer the head");
        assert!(
            err.to_string().contains("not the current head"),
            "unexpected {err:?}"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// Authorship of a memory is node metadata, known at write time: a column,
/// not an edge with a mask on it.
#[tokio::test]
async fn authorship_lands_as_a_column() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = fresh_owner();
        let engine = engine_for(&pg);
        let author = seed_memory(&pg, &owner, EntityKind::Perspective, "the self").await?;
        let input = seed_memory(&pg, &owner, EntityKind::Abstraction, "input").await?;
        let memory_id = MemoryId::new(Uuid::now_v7());

        author_abstraction(
            &engine,
            DeriveInput {
                owner,
                memory_id,
                input,
                supersedes: None,
                authoring_perspective_id: Some(author),
                body: "authored",
            },
        )
        .await?;

        let stored: Option<Uuid> = sqlx::query_scalar(
            "SELECT authoring_perspective_id FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(stored, Some(author.into_inner()));

        let authorship_edges: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.edges
              WHERE source_id = $1 AND target_id = $2",
        )
        .bind(author.into_inner())
        .bind(memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(authorship_edges, 0, "authorship is answered by the node");
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// Goal topology: the Goal row is the home of the statement, and the index
/// rows are derived from it. One reference per declared entry, all sourced at
/// the Goal, all one kind.
#[tokio::test]
async fn a_goal_write_lands_its_topology_as_columns_and_references()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = fresh_owner();
        let engine = engine_for(&pg);
        let assignment = seed_memory(&pg, &owner, EntityKind::Perspective, "the self").await?;
        let evidence = seed_memory(&pg, &owner, EntityKind::Fact, "why").await?;

        let request = GoalCreateRequest::product(
            owner,
            GoalAssignmentTarget::perspective(assignment),
            IdempotencyKey::new("node-authoring-goal-1")?,
            "Ship the lane",
            "Replace the edge layer.",
            SimpleTextGoalV1 {},
        )
        .with_evidence(vec![GoalEvidenceRef::new(evidence)]);

        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let outcome = engine.create_goal(&authz, request).await?;
        assert!(!outcome.idempotent_replay);
        assert_eq!(outcome.edge_count, 2, "one assignment, one evidence");

        let (stored_assignment, stored_evidence): (Option<Uuid>, Vec<Uuid>) = sqlx::query_as(
            "SELECT assignment_perspective_id, evidence_memory_ids
               FROM proxima_core.goals WHERE goal_id = $1",
        )
        .bind(outcome.goal_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(stored_assignment, Some(assignment.into_inner()));
        assert_eq!(stored_evidence, vec![evidence.into_inner()]);

        let mut rows: Vec<(String, Uuid)> = sqlx::query_as(
            "SELECT kind::text, target_id FROM proxima_core.edges
              WHERE source_kind = 'Goal' AND source_id = $1",
        )
        .bind(outcome.goal_id.into_inner())
        .fetch_all(pg.pool_for_tests())
        .await?;
        rows.sort_by_key(|(_, target)| *target);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|(kind, _)| kind == "reference"));

        // The Goal's own lifecycle Fact is a Fact like any other, and the
        // Goal's state is a column, not an edge.
        let lifecycle: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.goal_activated_v1 WHERE goal_id = $1",
        )
        .bind(outcome.goal_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(lifecycle, 1);
        let state: GoalState =
            sqlx::query_scalar("SELECT state FROM proxima_core.goals WHERE goal_id = $1")
                .bind(outcome.goal_id.into_inner())
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(state, GoalState::Active);
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// A Goal write replayed on the same idempotency key re-asserts the same
/// rows and reports the same count.
#[tokio::test]
async fn a_replayed_goal_write_asserts_the_same_rows() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = fresh_owner();
        let engine = engine_for(&pg);
        let assignment = seed_memory(&pg, &owner, EntityKind::Perspective, "the self").await?;
        let make_request =
            || -> Result<GoalCreateRequest<SimpleTextGoalV1>, Box<dyn std::error::Error>> {
                Ok(GoalCreateRequest::product(
                    owner,
                    GoalAssignmentTarget::perspective(assignment),
                    IdempotencyKey::new("node-authoring-goal-replay")?,
                    "Ship the lane",
                    "Replace the edge layer.",
                    SimpleTextGoalV1 {},
                ))
            };

        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let first = engine.create_goal(&authz, make_request()?).await?;
        let replay = engine.create_goal(&authz, make_request()?).await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.goal_id, first.goal_id);
        assert_eq!(replay.edge_count, first.edge_count);

        let rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.edges
              WHERE source_kind = 'Goal' AND source_id = $1",
        )
        .bind(first.goal_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(rows, 1, "a replay adds no second row");
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// An upload is a Fact, and a Fact may carry an outside proof. The citation
/// lands on the Fact row; a re-ingest of the same observation finds the same
/// cited object rather than minting a second one.
#[tokio::test]
async fn an_upload_is_a_fact_that_carries_its_citation() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = fresh_owner();
        let permit = owner_write_permit(&owner, proxima_core::AccessKind::Fact).await?;
        let now = time::OffsetDateTime::now_utc();
        let draft = FactWriteCommand {
            schema_id: SchemaId::new("test/upload-v1".into()),
            schema_version: SchemaVersion::new(1),
            payload: b"upload-key".to_vec(),
            rendered_text: Some("handbook.pdf".into()),
            lexical_language: None,
            receipt: Some(FactReceiptDraft {
                source_id: SourceId::new("test/uploads"),
                source_batch_id: SourceBatchId::new(Uuid::now_v7()),
                observed_at: now,
                occurred_at: now,
            }),
            citation: Some(proxima_core::verbs::fact_ingest::Citation {
                object: proxima_core::verbs::fact_ingest::CitedObjectHint {
                    schema_id: SchemaId::new("test/uploaded-blob-v1".into()),
                    schema_version: SchemaVersion::new(1),
                    content_hash: [7u8; 32],
                },
                mapping: proxima_core::verbs::fact_ingest::CitationMappingHint {
                    schema_id: SchemaId::new("test/uploaded-blob-whole-v1".into()),
                    schema_version: SchemaVersion::new(1),
                },
            }),
            derived_from: Vec::new(),
        };

        let first = pg.ingest_fact_atomic(&permit, &draft, None).await?;
        let cited = first.cited_object_id.expect("the Fact cites its artefact");

        let mapping: Option<Uuid> = sqlx::query_scalar(
            "SELECT citation_mapping_id FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(first.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(mapping.is_some(), "the citation is a column on the Fact");

        let replay = pg.ingest_fact_atomic(&permit, &draft, None).await?;
        assert!(replay.idempotent_replay);
        assert_eq!(
            replay.cited_object_id,
            Some(cited),
            "a content-addressed upload gives the second caller the first caller's object"
        );

        let objects: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cited_objects")
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(objects, 1);
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// The change log is what a consumer reads back, and an edge event now
/// carries the whole edge — both endpoints with their kinds, and the kind of
/// the edge — because there is no id left to dereference.
#[tokio::test]
async fn change_events_carry_the_whole_edge() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = fresh_owner();
        let engine = engine_for(&pg);
        let input = seed_memory(&pg, &owner, EntityKind::Abstraction, "input").await?;
        let memory_id = MemoryId::new(Uuid::now_v7());
        author_abstraction(
            &engine,
            DeriveInput {
                owner,
                memory_id,
                input,
                supersedes: None,
                authoring_perspective_id: None,
                body: "derived",
            },
        )
        .await?;

        let events = pg
            .list_change_events_after(&[owner], Uuid::nil(), 100)
            .await?;
        let edge_event = events
            .iter()
            .find_map(|wake| match &wake.event.kind {
                ChangeEventKind::EdgeAppend {
                    source,
                    target,
                    kind,
                } => Some((*source, *target, *kind)),
                _ => None,
            })
            .expect("an edge append event");
        assert_eq!(edge_event.0.entity, EntityRef::Memory(memory_id));
        assert_eq!(edge_event.0.kind, EntityKind::Abstraction);
        assert_eq!(
            edge_event.1.endpoint().map(|endpoint| endpoint.entity),
            Some(EntityRef::Memory(input))
        );
        assert_eq!(
            edge_event.1.endpoint().map(|endpoint| endpoint.kind),
            Some(EntityKind::Abstraction)
        );
        assert_eq!(edge_event.2, EdgeKind::Origin);

        assert!(
            events.iter().any(|wake| matches!(
                &wake.event.kind,
                ChangeEventKind::EntityAppend { entity, .. }
                    if *entity == EntityRef::Memory(memory_id)
            )),
            "the node's own append is announced too"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}
