//! Postgres acceptance coverage for authorized Fact reference persistence.
//!
//! The fixture intentionally owns its schema, sidecar, and registration. That
//! keeps this target independent from any product flavor while exercising the
//! same engine, write-session, and Postgres paths used by a composed host.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::engine::TypedFactIngest;
use proxima_core::flavor::{
    CounterRule, DbConstraint, EmbeddingRecipe, EraseRule, ExportRule, FlavorContract, ForgetRule,
    KeyShape, ProjectionDecl, Provenance, SchemaContract, SchemaRef, SearchProjectionDecl, Surface,
    TransferRule,
};
use proxima_core::storage_ports::{
    FactIngestPort, GoalWritePort, MemoryAuthoringPort, OwnerWritePermit,
};
use proxima_core::verbs::fact_ingest::{
    AuthorizedFactWrite, AuthorizedNodeLinks, FactWriteCommand, InlineCitationMappingDraft,
    InlineCitedObjectDraft,
};
use proxima_core::verbs::goal_write::{
    CreateGoalAtomicRequest, GoalAssignmentTarget, GoalAtomicContext, GoalAuthorship, GoalDraft,
    GoalState, GoalTopologyWrite, IdempotencyKey,
};
use proxima_core::verbs::query::{EdgeFilter, EdgeReadRequest, QueryRequest};
use proxima_core::{
    AccessKind, AgentDerivationV1, AuthPath, AuthorDerivedRequestInput, AuthzContext, EdgeEndpoint,
    EdgeKind, EdgeTargetProjection, EntityKind, EntityRef, FactPayload, FlavorRegistry, GoalId,
    InputContractId, MemoryId, MemoryOperatorKind, OperatorId, Owner, OwnerRef, PayloadKeyBuilder,
    PayloadReference, Relation, SchemaId, SchemaVersion, SidecarPayload, StorageError,
    UploadedBlobPayload, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::verbs::forget::MemoryColdStore;
use proxima_storage_pg::{PgSidecarRegistry, PgStorage, register_core_pg_sidecars};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const TEST_FLAVOR: &str = "test-refs";
const FACT_TABLE: &str = "test_refs.referenced_fact_v1";
const FACT_SCHEMA: &str = "test-refs/referenced-fact-v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReferencedFactV1 {
    logical_id: String,
    fact_id: Uuid,
    goal_id: Uuid,
    target_kind: String,
}

impl FactPayload for ReferencedFactV1 {
    const SCHEMA_ID: &'static str = FACT_SCHEMA;
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("logical_id", &self.logical_id);
        key.finish()
    }

    fn render(&self) -> String {
        self.logical_id.clone()
    }

    fn references(&self) -> Vec<PayloadReference> {
        vec![
            PayloadReference::memory(
                "fact_id",
                if self.target_kind == "abstraction" {
                    EntityKind::Abstraction
                } else {
                    EntityKind::Fact
                },
                MemoryId::new(self.fact_id),
            ),
            PayloadReference::goal("goal_id", GoalId::new(self.goal_id)),
        ]
    }

    fn sidecar_table() -> Option<&'static str> {
        Some(FACT_TABLE)
    }
}

const FACT_SURFACE: Surface = Surface {
    table: "test_refs.referenced_fact_v1",
    key: KeyShape::MemoryT { column: "t" },
    owner_column: None,
    transfer: TransferRule::StaysOnKey,
    erase: EraseRule::ByKey,
    export: ExportRule::Rows,
    forget: ForgetRule::DumpThenDelete,
    lexical_language_column: None,
    counter: CounterRule::Counted("sidecar_rows"),
    completeness: Some(DbConstraint {
        relation: FACT_TABLE,
        name: "referenced_fact_v1_t_fkey",
    }),
};

const FACT_CONTRACT_SCHEMA: SchemaContract = SchemaContract {
    id: SchemaRef::new(TEST_FLAVOR, "referenced-fact", 1),
    kind: proxima_core::verbs::schema::PayloadKind::Fact,
    sidecar_table: Some(FACT_TABLE),
    search: SearchProjectionDecl::None {
        why: "the acceptance payload is read by key, not searched",
    },
    embedding: EmbeddingRecipe::Never {
        why: "the acceptance payload carries identifiers, not searchable text",
    },
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[FACT_SURFACE],
    natural_key_columns: &[],
};

static TEST_REFS_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: TEST_FLAVOR,
    ordinal: 77,
    schemas: &[FACT_CONTRACT_SCHEMA],
    state_surfaces: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    projection: ProjectionDecl::None {
        why: "the acceptance payload is not a search surface",
    },
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};

proxima_core::proxima_flavor! {
    name = "test-refs",
    fact_schemas = [ReferencedFactV1],
    contract = &TEST_REFS_CONTRACT,
}

proxima_storage_pg::pg_sidecar! {
    payload: ReferencedFactV1,
    row: ReferencedFactPayloadRow,
    kinds: [Fact],
    table: "test_refs.referenced_fact_v1",
    key: t,
    fields: {
        logical_id => logical_id: (text),
        fact_id => fact_id: (uuid),
        goal_id => goal_id: (uuid),
        target_kind => target_kind: (text),
    },
}

fn owner() -> Owner {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
}

fn draft(logical_id: &str, fact_id: Uuid, goal_id: Uuid) -> FactWriteCommand {
    let payload = ReferencedFactV1 {
        logical_id: logical_id.to_owned(),
        fact_id,
        goal_id,
        target_kind: "fact".to_owned(),
    };
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand::from_payload("test/authorized-fact-refs", &payload, now)
}

fn payload(logical_id: &str, fact_id: Uuid, goal_id: Uuid) -> ReferencedFactV1 {
    ReferencedFactV1 {
        logical_id: logical_id.to_owned(),
        fact_id,
        goal_id,
        target_kind: "fact".to_owned(),
    }
}

fn registry() -> proxima_core::FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    register(&mut registry).expect("fixture schema registration");
    registry.freeze_or_panic_for_tests()
}

fn sidecars(
    registry: &proxima_core::FlavorRegistryFrozen,
) -> proxima_storage_pg::PgSidecarRegistryFrozen {
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    sidecars.add_fact::<ReferencedFactV1>();
    sidecars
        .freeze_against(registry)
        .expect("fixture sidecar registration")
}

async fn bootstrap() -> (String, PgStorage, proxima_core::FlavorRegistryFrozen) {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(error) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {error}");
    }
    let registry = registry();
    let result = async {
        let pg = PgStorage::connect(&db_url(&db_name)).await?;
        pg.run_migrations().await?;
        sqlx::raw_sql(
            "CREATE SCHEMA test_refs;
             CREATE TABLE test_refs.referenced_fact_v1 (
                 t uuid PRIMARY KEY REFERENCES proxima_core.memory(t) ON DELETE CASCADE,
                 logical_id text NOT NULL,
                 fact_id uuid NOT NULL,
                 goal_id uuid NOT NULL,
                 target_kind text NOT NULL
             );
             INSERT INTO proxima_core.flavor_surface (table_name, flavor_id)
             VALUES ('test_refs.referenced_fact_v1', 'test-refs');
             CREATE TRIGGER referenced_fact_v1_declared_by_memory
             BEFORE INSERT ON test_refs.referenced_fact_v1
             FOR EACH ROW
             EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let pg = pg
            .with_sidecars(sidecars(&registry))
            .with_flavors(&registry);
        Ok::<_, Box<dyn std::error::Error>>((pg, registry))
    }
    .await;
    match result {
        Ok((pg, registry)) => (db_name, pg, registry),
        Err(error) => {
            let _ = drop_db(&db_name).await;
            panic!("fixture bootstrap failed: {error}");
        }
    }
}

async fn seed_fact(pg: &PgStorage, owner: Owner) -> Result<MemoryId, StorageError> {
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    Ok(pg
        .ingest_fact_atomic(
            &permit,
            &draft(
                &format!("seed-{}", Uuid::now_v7()),
                Uuid::now_v7(),
                Uuid::now_v7(),
            ),
            None,
        )
        .await?
        .memory_id)
}

async fn seed_abstraction(
    pg: &PgStorage,
    owner: Owner,
    origin: MemoryId,
) -> Result<MemoryId, StorageError> {
    let mut draft = draft(
        &format!("abstraction-{}", Uuid::now_v7()),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    "abstraction".clone_into(&mut draft.kind);
    draft.source_id = None;
    draft.ingest_key = None;
    draft.receipt = None;
    draft.refs.clear();
    draft.derived_from = vec![EdgeEndpoint::memory(EntityKind::Fact, origin)];
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    Ok(pg
        .ingest_fact_atomic(&permit, &draft, None)
        .await?
        .memory_id)
}

async fn seed_perspective(pg: &PgStorage, owner: Owner) -> Result<MemoryId, StorageError> {
    let fact = seed_fact(pg, owner).await?;
    let abstraction = seed_abstraction(pg, owner, fact).await?;
    let mut draft = draft(
        &format!("perspective-{}", Uuid::now_v7()),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    "perspective".clone_into(&mut draft.kind);
    draft.source_id = None;
    draft.ingest_key = None;
    draft.receipt = None;
    draft.refs.clear();
    draft.derived_from = vec![EdgeEndpoint::memory(EntityKind::Abstraction, abstraction)];
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    Ok(pg
        .ingest_fact_atomic(&permit, &draft, None)
        .await?
        .memory_id)
}

async fn stored_refs(pg: &PgStorage, memory_id: MemoryId) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT refs FROM proxima_core.memory WHERE t = $1")
        .bind(memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await
}

async fn stored_goal_refs(pg: &PgStorage, memory_id: MemoryId) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT goal_refs FROM proxima_core.memory WHERE t = $1")
        .bind(memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await
}

fn direct_draft(logical_id: &str, raw_refs: Vec<Uuid>) -> FactWriteCommand {
    let mut draft = draft(logical_id, Uuid::now_v7(), Uuid::now_v7());
    draft.schema_id = SchemaId::new("core/upload-v1".into());
    draft.schema_version = SchemaVersion::new(1);
    draft.payload.clear();
    draft.refs = raw_refs;
    draft
}

fn engine(pg: &PgStorage, registry: &proxima_core::FlavorRegistryFrozen) -> proxima_core::Engine {
    proxima_core::Engine::new(registry.clone())
        .with_storage_ports(std::sync::Arc::new(pg.clone()).storage_ports())
}

fn inline_citation() -> (InlineCitedObjectDraft, InlineCitationMappingDraft) {
    let cited = UploadedBlobPayload {
        content_hash: [7; 32],
        bucket: "test".to_owned(),
        object_key: "authorized-fact-refs".to_owned(),
        sha256: [8; 32],
        byte_len: 16,
        mime: "text/plain".to_owned(),
        filename: "refs.txt".to_owned(),
        etag: None,
        uploaded_at: time::OffsetDateTime::now_utc(),
    };
    (
        InlineCitedObjectDraft {
            schema_id: SchemaId::new("core/uploaded-blob-v1".into()),
            schema_version: SchemaVersion::new(1),
            payload_bytes: serde_json::to_vec(&cited).expect("cited object JSON"),
        },
        InlineCitationMappingDraft {
            schema_id: SchemaId::new("core/uploaded-blob-whole-v1".into()),
            schema_version: SchemaVersion::new(1),
            payload_bytes: b"{}".to_vec(),
        },
    )
}

fn custom_draft(payload: &ReferencedFactV1) -> FactWriteCommand {
    FactWriteCommand::from_payload(
        "test/authorized-fact-refs",
        payload,
        time::OffsetDateTime::now_utc(),
    )
}

async fn create_goal(
    pg: &PgStorage,
    registry: &proxima_core::FlavorRegistryFrozen,
    owner: Owner,
    perspective: MemoryId,
) -> Result<GoalId, Box<dyn std::error::Error>> {
    let topology = GoalTopologyWrite::new(
        GoalAssignmentTarget::perspective(perspective),
        Vec::new(),
        Vec::new(),
    )?;
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
    Ok(pg
        .create_goal_atomic(
            &CreateGoalAtomicRequest {
                draft: GoalDraft {
                    owner,
                    schema_id: SchemaId::new("core/simple-text-v1".into()),
                    schema_version: SchemaVersion::new(1),
                    title: format!("reference target {}", Uuid::now_v7()),
                    text: "reference target".into(),
                    payload: Vec::new(),
                    sidecar_payload: None,
                    state: GoalState::Active,
                    topology,
                    wake: None,
                    supersedes_goal_id: None,
                    authorship: GoalAuthorship::User,
                    request_id: IdempotencyKey::new(format!(
                        "authorized-fact-refs-goal-{}",
                        Uuid::now_v7()
                    ))?
                    .into_string(),
                },
                context: GoalAtomicContext {
                    registry,
                    embedding_model_id: None,
                    author_self_perspective_id: None,
                },
                write_act_t: None,
            },
            &permit,
        )
        .await?
        .goal_id)
}

#[tokio::test]
async fn authorized_links_are_persisted_by_engine_uow() {
    let (db_name, pg, registry) = bootstrap().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner();
        let fact_id = seed_fact(&pg, owner).await?;
        let perspective = seed_perspective(&pg, owner).await?;
        let goal = create_goal(&pg, &registry, owner, perspective).await?;
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let engine = engine(&pg, &registry);
        let uow_payload = payload("uow-one", fact_id.into_inner(), goal.into_inner());
        let mut uow = engine.unit_of_work(&authz).await?;
        let outcome = uow.ingest_fact("test/uow", &uow_payload).await?;
        uow.commit().await?;
        assert_eq!(
            stored_refs(&pg, outcome.memory_id).await?,
            vec![fact_id.into_inner()]
        );
        assert_eq!(
            stored_goal_refs(&pg, outcome.memory_id).await?,
            vec![goal.into_inner()]
        );
        let sidecar_row: (String, Uuid, Uuid) = sqlx::query_as(
            "SELECT logical_id, fact_id, goal_id
               FROM test_refs.referenced_fact_v1
              WHERE t = $1",
        )
        .bind(outcome.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            sidecar_row,
            (
                "uow-one".to_owned(),
                fact_id.into_inner(),
                goal.into_inner()
            )
        );

        let pool_payload = payload("pool-typed", fact_id.into_inner(), goal.into_inner());
        let authorized = engine
            .authorize_fact_ingest(
                &authz,
                Relation::Editor,
                custom_draft(&pool_payload),
                &[SidecarPayload::fact(pool_payload.clone())],
            )
            .await?;
        let pool_typed = engine
            .ingest_fact_with_typed_sidecar(
                &authorized,
                &[SidecarPayload::fact(pool_payload)],
                None,
            )
            .await?;
        assert_eq!(
            stored_refs(&pg, pool_typed.memory_id).await?,
            vec![fact_id.into_inner()]
        );
        assert_eq!(
            stored_goal_refs(&pg, pool_typed.memory_id).await?,
            vec![goal.into_inner()]
        );

        let mut snapshot_req = QueryRequest::for_owner(owner);
        snapshot_req.memory_ids = vec![pool_typed.memory_id];
        snapshot_req.goal_ids = vec![goal];
        let snapshot = engine.query(&authz, &snapshot_req).await?;
        let snapshot_row = snapshot
            .memories
            .iter()
            .find(|row| row.id == pool_typed.memory_id)
            .expect("typed reference row is in the query window");
        assert_eq!(snapshot_row.goal_refs, vec![goal]);
        assert!(snapshot.edges.iter().any(|edge| {
            edge.source.memory_id() == Some(pool_typed.memory_id)
                && matches!(
                    edge.target,
                    EdgeTargetProjection::Visible {
                        target: EdgeEndpoint {
                            entity: EntityRef::Goal(id), ..
                        }
                    } if id == goal
                )
        }));

        let mut fact_only = QueryRequest::for_owner(owner);
        fact_only.entity_kind = Some(EntityKind::Fact);
        fact_only.memory_ids = vec![pool_typed.memory_id];
        let fact_snapshot = engine.query(&authz, &fact_only).await?;
        let fact_row = fact_snapshot
            .memories
            .iter()
            .find(|row| row.id == pool_typed.memory_id)
            .expect("typed reference Fact is in the filtered query window");
        assert!(fact_row.goal_refs.is_empty());
        assert!(fact_row.refs.contains(&MemoryId::new(goal.into_inner())));

        let outbound = engine
            .read_edges(
                &authz,
                &EdgeReadRequest {
                    owner,
                    filter: EdgeFilter {
                        kind: Some(EdgeKind::Reference),
                        source: Some(EntityRef::Memory(pool_typed.memory_id)),
                        target: None,
                    },
                    limit: 10,
                    cursor: None,
                },
            )
            .await?;
        assert_eq!(outbound.edges.len(), 2);
        assert!(outbound.edges.iter().any(|edge| matches!(
            edge.target,
            EdgeTargetProjection::Visible {
                target: EdgeEndpoint {
                    entity: EntityRef::Goal(id), ..
                }
            } if id == goal
        )));

        let inbound_goal = engine
            .read_edges(
                &authz,
                &EdgeReadRequest {
                    owner,
                    filter: EdgeFilter {
                        kind: Some(EdgeKind::Reference),
                        source: None,
                        target: Some(EntityRef::Goal(goal)),
                    },
                    limit: 10,
                    cursor: None,
                },
            )
            .await?;
        assert_eq!(inbound_goal.edges.len(), 2);
        assert!(inbound_goal.edges.iter().all(|edge| matches!(
            edge.target,
            EdgeTargetProjection::Visible {
                target: EdgeEndpoint {
                    entity: EntityRef::Goal(id), ..
                }
            } if id == goal
        )));
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("authorized UoW Fact refs failed");
}

#[tokio::test]
async fn inline_and_by_ref_citation_routes_keep_authorized_links() {
    let (db_name, pg, registry) = bootstrap().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner();
        let fact_id = seed_fact(&pg, owner).await?;
        let perspective = seed_perspective(&pg, owner).await?;
        let goal_id = create_goal(&pg, &registry, owner, perspective).await?;
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let engine = engine(&pg, &registry);

        let inline_payload = payload(
            "inline-citation",
            fact_id.into_inner(),
            goal_id.into_inner(),
        );
        let inline_draft = FactWriteCommand::from_payload(
            "test/authorized-citation",
            &inline_payload,
            time::OffsetDateTime::now_utc(),
        );
        let (cited_object, mapping) = inline_citation();
        let authorized = engine
            .authorize_fact_with_citation(
                &authz,
                Relation::Editor,
                inline_draft,
                cited_object,
                mapping,
                &[SidecarPayload::fact(inline_payload.clone())],
            )
            .await?;
        let inline = engine
            .ingest_fact_with_citation_and_typed_sidecar(
                &authorized,
                &[SidecarPayload::fact(inline_payload)],
                None,
            )
            .await?;
        assert_eq!(
            stored_refs(&pg, inline.memory_id).await?,
            vec![fact_id.into_inner()]
        );
        assert_eq!(
            stored_goal_refs(&pg, inline.memory_id).await?,
            vec![goal_id.into_inner()]
        );
        let cited_object_id = inline.cited_object_id.expect("inline citation blob");

        let by_ref_payload = payload(
            "by-ref-citation",
            fact_id.into_inner(),
            goal_id.into_inner(),
        );
        let by_ref_draft = FactWriteCommand::from_payload(
            "test/authorized-citation",
            &by_ref_payload,
            time::OffsetDateTime::now_utc(),
        );
        let (_, mapping) = inline_citation();
        let authorized = engine
            .authorize_fact_with_citation_by_ref(
                &authz,
                Relation::Editor,
                by_ref_draft,
                cited_object_id,
                mapping,
                &[SidecarPayload::fact(by_ref_payload.clone())],
            )
            .await?;
        let by_ref = engine
            .ingest_fact_with_citation_ref_and_typed_sidecar(
                &authorized,
                &[SidecarPayload::fact(by_ref_payload)],
                None,
            )
            .await?;
        assert_eq!(
            stored_refs(&pg, by_ref.memory_id).await?,
            vec![fact_id.into_inner()]
        );
        assert_eq!(
            stored_goal_refs(&pg, by_ref.memory_id).await?,
            vec![goal_id.into_inner()]
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("citation Fact routes lost authorized links");
}

#[tokio::test]
async fn typed_raw_refs_cannot_disagree_with_payload_declarations() {
    let (db_name, pg, registry) = bootstrap().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner();
        let fact_a = seed_fact(&pg, owner).await?;
        let fact_b = seed_fact(&pg, owner).await?;
        let goal = create_goal(&pg, &registry, owner, seed_perspective(&pg, owner).await?).await?;
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let engine = engine(&pg, &registry);
        let payload = payload("raw-disagreement", fact_a.into_inner(), goal.into_inner());
        let mut uow = engine.unit_of_work(&authz).await?;
        let error = uow
            .ingest_typed(
                TypedFactIngest::new("test/raw-disagreement", &payload).refs([fact_b.into_inner()]),
            )
            .await
            .expect_err("raw refs must not replace typed refs");
        assert_eq!(error.code, proxima_core::error::ErrorCode::InvalidArgument);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("typed/raw Fact ref disagreement was accepted");
}

#[tokio::test]
async fn storage_rejects_sidecars_whose_references_changed_after_authorization() {
    let (db_name, pg, registry) = bootstrap().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner();
        let first = seed_fact(&pg, owner).await?;
        let second = seed_fact(&pg, owner).await?;
        let goal = create_goal(&pg, &registry, owner, seed_perspective(&pg, owner).await?).await?;
        let admitted = payload("bound-sidecar", first.into_inner(), goal.into_inner());
        let substituted = payload("bound-sidecar", second.into_inner(), goal.into_inner());
        let admitted_sidecars = [SidecarPayload::fact(admitted.clone())];
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let engine = engine(&pg, &registry);
        let authorized = engine
            .authorize_fact_ingest(
                &authz,
                Relation::Editor,
                custom_draft(&admitted),
                &admitted_sidecars,
            )
            .await?;
        let error = pg
            .ingest_fact_with_typed_sidecar(&authorized, &[SidecarPayload::fact(substituted)], None)
            .await
            .expect_err("storage must recheck the authorized reference declaration");
        assert!(matches!(error, StorageError::ConstraintViolation(_)));

        let written: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM proxima_core.memory
                  WHERE owner_id = $1 AND source_id = $2 AND ingest_key = $3
             )",
        )
        .bind(owner.stored_owner_id())
        .bind(authorized.draft().source_id.as_deref())
        .bind(authorized.draft().ingest_key.as_deref())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(!written, "a changed declaration must not reach storage");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("storage accepted a substituted typed reference declaration");
}

#[tokio::test]
async fn target_kind_and_visibility_are_checked_before_fact_write() {
    let (db_name, pg, registry) = bootstrap().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let foreign_owner = owner();
        let owner = owner();
        let fact = seed_fact(&pg, owner).await?;
        let abstraction = seed_abstraction(&pg, owner, fact).await?;
        let foreign_fact = seed_fact(&pg, foreign_owner).await?;
        let goal = create_goal(&pg, &registry, owner, seed_perspective(&pg, owner).await?).await?;
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let engine = engine(&pg, &registry);

        let wrong_kind = payload("wrong-kind", fact.into_inner(), goal.into_inner());
        let mut wrong_kind = wrong_kind;
        wrong_kind.target_kind = "abstraction".to_owned();
        let error = engine
            .authorize_fact_ingest(
                &authz,
                Relation::Editor,
                custom_draft(&wrong_kind),
                &[SidecarPayload::fact(wrong_kind)],
            )
            .await
            .expect_err("stored Fact must not authorize as Abstraction");
        assert_eq!(error.code, proxima_core::error::ErrorCode::InvalidArgument);

        let declared_fact = payload(
            "declared-fact-stored-abstraction",
            abstraction.into_inner(),
            goal.into_inner(),
        );
        let error = engine
            .authorize_fact_ingest(
                &authz,
                Relation::Editor,
                custom_draft(&declared_fact),
                &[SidecarPayload::fact(declared_fact)],
            )
            .await
            .expect_err("a Fact endpoint must not target an Abstraction row");
        assert_eq!(error.code, proxima_core::error::ErrorCode::InvalidArgument);

        let memory_declared_as_goal = payload(
            "memory-declared-as-goal",
            fact.into_inner(),
            abstraction.into_inner(),
        );
        let error = engine
            .authorize_fact_ingest(
                &authz,
                Relation::Editor,
                custom_draft(&memory_declared_as_goal),
                &[SidecarPayload::fact(memory_declared_as_goal)],
            )
            .await
            .expect_err("a Goal endpoint must resolve to a stored Goal row");
        assert_eq!(error.code, proxima_core::error::ErrorCode::Forbidden);

        let unreadable = payload("unreadable", foreign_fact.into_inner(), goal.into_inner());
        let error = engine
            .authorize_fact_ingest(
                &authz,
                Relation::Editor,
                custom_draft(&unreadable),
                &[SidecarPayload::fact(unreadable)],
            )
            .await
            .expect_err("foreign Fact must not authorize");
        assert_eq!(error.code, proxima_core::error::ErrorCode::Forbidden);

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM proxima_core.memory WHERE owner_id = $1 AND schema_id = $2",
        )
        .bind(owner.stored_owner_id())
        .bind(FACT_SCHEMA)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            count, 5,
            "authorization failures must not append beyond the five owner seeds"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("Fact target authorization was not fail-closed");
}

#[tokio::test]
async fn malformed_fact_source_and_endpoints_are_rejected_before_write() {
    let (db_name, pg, registry) = bootstrap().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner();
        let target = seed_fact(&pg, owner).await?;
        let goal = create_goal(&pg, &registry, owner, seed_perspective(&pg, owner).await?).await?;
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let engine = engine(&pg, &registry);

        let mut non_fact = direct_draft("non-fact-source", Vec::new());
        non_fact.kind = "abstraction".to_owned();
        let error = engine
            .authorize_fact_ingest(&authz, Relation::Editor, non_fact, &[])
            .await
            .expect_err("Fact authorization must reject non-Fact source");
        assert_eq!(error.code, proxima_core::error::ErrorCode::InvalidArgument);

        let payload = payload("malformed-endpoint", target.into_inner(), goal.into_inner());
        let mut malformed = custom_draft(&payload);
        malformed.derived_from = vec![EdgeEndpoint {
            kind: EntityKind::Goal,
            entity: EntityRef::Memory(target),
        }];
        let error = engine
            .authorize_fact_ingest(
                &authz,
                Relation::Editor,
                malformed,
                &[SidecarPayload::fact(payload)],
            )
            .await
            .expect_err("malformed Goal/Memory endpoint must be rejected");
        assert_eq!(error.code, proxima_core::error::ErrorCode::InvalidArgument);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("malformed Fact authorization was accepted");
}

#[tokio::test]
async fn uow_rejects_session_visible_target_kind_mismatch() {
    let (db_name, pg, registry) = bootstrap().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner();
        let anchor = seed_fact(&pg, owner).await?;
        let goal = create_goal(&pg, &registry, owner, seed_perspective(&pg, owner).await?).await?;
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let engine = engine(&pg, &registry);
        let mut uow = engine.unit_of_work(&authz).await?;
        let origin = EdgeEndpoint::memory(EntityKind::Fact, anchor);
        let origins = [origin];
        let derived = uow
            .author_derived(AuthorDerivedRequestInput {
                memory_id: MemoryId::new(Uuid::now_v7()),
                owner,
                kind: EntityKind::Abstraction,
                text: "session-visible abstraction".to_owned(),
                schema_id: SchemaId::new(
                    <AgentDerivationV1 as proxima_core::AbstractionPayload>::SCHEMA_ID.to_owned(),
                ),
                schema_version: SchemaVersion::new(
                    <AgentDerivationV1 as proxima_core::AbstractionPayload>::SCHEMA_VERSION,
                ),
                operator_kind: MemoryOperatorKind::FtoA,
                operator_id: OperatorId::new(Uuid::now_v7()),
                input_contract_id: InputContractId::new(Uuid::now_v7()),
                model_id: "test",
                sidecar_payload: SidecarPayload::abstraction(AgentDerivationV1 {
                    title: "session-visible abstraction".to_owned(),
                    body: "session-visible abstraction".to_owned(),
                    tags: Vec::new(),
                    idempotency_key: None,
                    source_memory_ids: vec![anchor.into_inner()],
                    model_id: "test".to_owned(),
                    client_name: "test".to_owned(),
                    client_version: "1".to_owned(),
                }),
                derived_from: &origins,
                extra_refs: &[],
                supersedes: None,
                lexical_language: Some(
                    proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT,
                ),
            })
            .await?;
        let wrong = payload(
            "session-wrong-kind",
            derived.memory_id.into_inner(),
            goal.into_inner(),
        );
        let error = uow
            .ingest_fact("test/session", &wrong)
            .await
            .expect_err("session-visible Abstraction must not authorize as a Fact target");
        assert_eq!(error.code, proxima_core::error::ErrorCode::InvalidArgument);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("UoW session-visible kind mismatch was accepted");
}

#[tokio::test]
async fn sql_accepts_goal_refs_but_rejects_goals_in_origins_or_refs() {
    let (db_name, pg, registry) = bootstrap().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner();
        let perspective = seed_perspective(&pg, owner).await?;
        let goal = create_goal(&pg, &registry, owner, perspective).await?;
        let pool = pg.pool_for_tests();

        let reference_handle = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/upload-v1', $2, $1)",
        )
        .bind(reference_handle)
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory
                (handle, t, kind, owner_id, schema_id, origins, refs, goal_refs,
                 sidecar_tables)
             VALUES ($1, $1, 'fact', $2, 'core/upload-v1', '{}', '{}', $3, '{}')",
        )
        .bind(reference_handle)
        .bind(owner.stored_owner_id())
        .bind(vec![goal.into_inner()])
        .execute(pool)
        .await?;

        // `refs` is the Memory spine now, so it rejects a Goal t exactly as
        // `origins` always did. Before the split this same insert succeeded.
        let memory_ref_handle = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/upload-v1', $2, $1)",
        )
        .bind(memory_ref_handle)
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;
        let error = sqlx::query(
            "INSERT INTO proxima_core.memory
                (handle, t, kind, owner_id, schema_id, origins, refs, goal_refs,
                 sidecar_tables)
             VALUES ($1, $1, 'fact', $2, 'core/upload-v1', '{}', $3, '{}', '{}')",
        )
        .bind(memory_ref_handle)
        .bind(owner.stored_owner_id())
        .bind(vec![goal.into_inner()])
        .execute(pool)
        .await
        .expect_err("Goal t must not be accepted in refs");
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .map(|code| code.to_string()),
            Some("23503".to_owned())
        );

        // And the mirror image: `goal_refs` is the Goal spine, so a Memory
        // t is not a Goal reference either. Neither column is a place to
        // put an id of the other kind.
        let goal_ref_handle = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/upload-v1', $2, $1)",
        )
        .bind(goal_ref_handle)
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;
        let error = sqlx::query(
            "INSERT INTO proxima_core.memory
                (handle, t, kind, owner_id, schema_id, origins, refs, goal_refs,
                 sidecar_tables)
             VALUES ($1, $1, 'fact', $2, 'core/upload-v1', '{}', '{}', $3, '{}')",
        )
        .bind(goal_ref_handle)
        .bind(owner.stored_owner_id())
        .bind(vec![perspective.into_inner()])
        .execute(pool)
        .await
        .expect_err("a Memory t must not be accepted in goal_refs");
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .map(|code| code.to_string()),
            Some("23503".to_owned())
        );

        let origin_handle = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/upload-v1', $2, $1)",
        )
        .bind(origin_handle)
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;
        let error = sqlx::query(
            "INSERT INTO proxima_core.memory
                (handle, t, kind, owner_id, schema_id, origins, refs, sidecar_tables)
             VALUES ($1, $1, 'fact', $2, 'core/upload-v1', $3, '{}', '{}')",
        )
        .bind(origin_handle)
        .bind(owner.stored_owner_id())
        .bind(vec![goal.into_inner()])
        .execute(pool)
        .await
        .expect_err("Goal t must not be accepted in origins");
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .map(|code| code.to_string()),
            Some("23503".to_owned())
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("Goal reference column SQL distinction failed");
}

#[tokio::test]
async fn goal_erase_serializes_against_reference_admission() {
    let (db_name, pg, registry) = bootstrap().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner();
        let goal = create_goal(&pg, &registry, owner, seed_perspective(&pg, owner).await?).await?;
        let pool = pg.pool_for_tests();
        let mut erase_tx = pool.begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(
                 hashtextextended('proxima-forget:' || $1::text, 0)
             )",
        )
        .bind(goal.into_inner())
        .execute(erase_tx.as_mut())
        .await?;

        let waiter_pool = pool.clone();
        let owner_id = owner.stored_owner_id();
        let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            let mut tx = waiter_pool.begin().await?;
            let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(tx.as_mut())
                .await?;
            let handle = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
                 VALUES ($1, 'fact', 'core/upload-v1', $2, $1)",
            )
            .bind(handle)
            .bind(owner_id)
            .execute(tx.as_mut())
            .await?;
            pid_tx
                .send(pid)
                .map_err(|_| sqlx::Error::Protocol("lock-wait observer was dropped".to_owned()))?;
            sqlx::query(
                "INSERT INTO proxima_core.memory
                    (handle, t, kind, owner_id, schema_id, origins, refs, goal_refs,
                     sidecar_tables)
                 VALUES ($1, $1, 'fact', $2, 'core/upload-v1', '{}', '{}', $3, '{}')",
            )
            .bind(handle)
            .bind(owner_id)
            .bind(vec![goal.into_inner()])
            .execute(tx.as_mut())
            .await?;
            tx.commit().await?;
            Ok::<(), sqlx::Error>(())
        });
        let pid = pid_rx.await?;
        let mut waiting = false;
        for _ in 0..100 {
            waiting = sqlx::query_scalar(
                "SELECT EXISTS (
                     SELECT 1 FROM pg_locks
                      WHERE pid = $1 AND NOT granted
                 )",
            )
            .bind(pid)
            .fetch_one(pool)
            .await?;
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            waiting,
            "reference admission must wait on the shared lifecycle lock"
        );

        sqlx::query("DELETE FROM proxima_core.goal WHERE t = $1")
            .bind(goal.into_inner())
            .execute(erase_tx.as_mut())
            .await?;
        erase_tx.commit().await?;
        let error = waiter
            .await
            .expect("reference admission task panicked")
            .expect_err("a Goal erased before admission must not be referenced");
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .map(|code| code.to_string()),
            Some("23503".to_owned())
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("Goal erase/reference serialization contract failed");
}

#[tokio::test]
async fn crossed_memory_pins_lock_in_global_t_order() {
    let (db_name, pg, _registry) = bootstrap().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner();
        let first = seed_fact(&pg, owner).await?.into_inner();
        let second = seed_fact(&pg, owner).await?.into_inner();
        let (smaller, larger) = if first < second {
            (first, second)
        } else {
            (second, first)
        };
        let pool = pg.pool_for_tests();
        let mut blocker = pool.begin().await?;
        sqlx::query("SELECT 1 FROM proxima_core.memory WHERE t = $1 FOR UPDATE")
            .bind(smaller)
            .execute(blocker.as_mut())
            .await?;

        let waiter_pool = pool.clone();
        let owner_id = owner.stored_owner_id();
        let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            let mut tx = waiter_pool.begin().await?;
            let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(tx.as_mut())
                .await?;
            let handle = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
                 VALUES ($1, 'fact', 'core/upload-v1', $2, $1)",
            )
            .bind(handle)
            .bind(owner_id)
            .execute(tx.as_mut())
            .await?;
            pid_tx
                .send(pid)
                .map_err(|_| sqlx::Error::Protocol("lock-wait observer was dropped".to_owned()))?;
            let inserted = sqlx::query(
                "INSERT INTO proxima_core.memory
                    (handle, t, kind, owner_id, schema_id, origins, refs, sidecar_tables)
                 VALUES ($1, $1, 'fact', $2, 'core/upload-v1', $3, $4, '{}')",
            )
            .bind(handle)
            .bind(owner_id)
            // Deliberately reverse declaration order. The trigger must still
            // attempt the globally smaller target before locking `larger`.
            .bind(vec![larger])
            .bind(vec![smaller])
            .execute(tx.as_mut())
            .await;
            let _ = tx.rollback().await;
            inserted
        });
        let pid = pid_rx.await?;
        let mut waiting = false;
        for _ in 0..100 {
            waiting = sqlx::query_scalar(
                "SELECT EXISTS (
                     SELECT 1 FROM pg_locks
                      WHERE pid = $1 AND NOT granted
                 )",
            )
            .bind(pid)
            .fetch_one(pool)
            .await?;
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(waiting, "crossed declaration must wait on the smaller t");

        let mut probe = pool.begin().await?;
        sqlx::query("SELECT 1 FROM proxima_core.memory WHERE t = $1 FOR UPDATE NOWAIT")
            .bind(larger)
            .execute(probe.as_mut())
            .await
            .expect("the waiter must not lock the larger t before the blocked smaller t");
        probe.rollback().await?;

        blocker.rollback().await?;
        waiter
            .await
            .expect("crossed-lock waiter panicked")
            .expect_err("the test-only Fact origin remains invalid after the lock probe");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("Memory pin locks did not follow global t order");
}

#[tokio::test]
async fn storage_persists_authorized_links_not_draft_refs() {
    let (db_name, pg, registry) = bootstrap().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner();
        let fact_a = seed_fact(&pg, owner).await?;
        let fact_b = seed_fact(&pg, owner).await?;
        let goal = create_goal(&pg, &registry, owner, seed_perspective(&pg, owner).await?).await?;
        // The boundary regression uses real, readable Memory and Goal targets
        // so the write proves both endpoint classes come from the carrier.
        let draft = direct_draft("direct-links-real", vec![fact_b.into_inner()]);
        let links = AuthorizedNodeLinks::new_for_tests(
            Vec::new(),
            vec![
                EdgeEndpoint::memory(EntityKind::Fact, fact_a),
                EdgeEndpoint::goal(goal),
            ],
        );
        let authorized = AuthorizedFactWrite::new_with_links_for_tests(
            OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
            draft,
            None,
            Vec::new(),
            links,
        );
        let outcome = pg.ingest_authorized_fact_atomic(&authorized, None).await?;
        assert_eq!(
            stored_refs(&pg, outcome.memory_id).await?,
            vec![fact_a.into_inner()]
        );
        assert_eq!(
            stored_goal_refs(&pg, outcome.memory_id).await?,
            vec![goal.into_inner()]
        );

        // The engine keeps the storage conflict's caller-fixable class when
        // routing the same replay through the public FactIngest verb.
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let engine = engine(&pg, &registry);
        engine
            .fact_ingest(
                &authz,
                direct_draft("engine-replay-links", vec![fact_a.into_inner()]),
            )
            .await?;
        let error = engine
            .fact_ingest(
                &authz,
                direct_draft("engine-replay-links", vec![fact_b.into_inner()]),
            )
            .await
            .expect_err("Engine must map changed-ref replay to InvalidArgument");
        assert_eq!(error.code, proxima_core::error::ErrorCode::InvalidArgument);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("authorized carrier persistence regression failed");
}

#[tokio::test]
async fn receipt_replay_requires_identical_authorized_refs() {
    let (db_name, pg, registry) = bootstrap().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner();
        let fact_a = seed_fact(&pg, owner).await?;
        let fact_b = seed_fact(&pg, owner).await?;
        let goal = create_goal(&pg, &registry, owner, seed_perspective(&pg, owner).await?).await?;
        let first_draft = direct_draft("replay-links", vec![fact_a.into_inner()]);
        let first_links = AuthorizedNodeLinks::new_for_tests(
            Vec::new(),
            vec![
                EdgeEndpoint::memory(EntityKind::Fact, fact_a),
                EdgeEndpoint::goal(goal),
            ],
        );
        let first = AuthorizedFactWrite::new_with_links_for_tests(
            OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
            first_draft.clone(),
            None,
            Vec::new(),
            first_links.clone(),
        );
        let outcome = pg.ingest_authorized_fact_atomic(&first, None).await?;
        assert!(!outcome.idempotent_replay);

        let same = AuthorizedFactWrite::new_with_links_for_tests(
            OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
            first_draft.clone(),
            None,
            Vec::new(),
            first_links,
        );
        let replay = pg.ingest_authorized_fact_atomic(&same, None).await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.memory_id, outcome.memory_id);

        let changed_draft = direct_draft("replay-links", vec![fact_b.into_inner()]);
        let changed = AuthorizedFactWrite::new_with_links_for_tests(
            OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
            changed_draft,
            None,
            Vec::new(),
            AuthorizedNodeLinks::new_for_tests(
                Vec::new(),
                vec![
                    EdgeEndpoint::memory(EntityKind::Fact, fact_b),
                    EdgeEndpoint::goal(goal),
                ],
            ),
        );
        let error = pg
            .ingest_authorized_fact_atomic(&changed, None)
            .await
            .expect_err("changed refs must conflict");
        assert!(matches!(error, StorageError::Conflict(message) if message.contains("refs")));
        assert_eq!(
            stored_refs(&pg, outcome.memory_id).await?,
            vec![fact_a.into_inner()]
        );
        assert_eq!(
            stored_goal_refs(&pg, outcome.memory_id).await?,
            vec![goal.into_inner()]
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("receipt reference replay contract failed");
}

#[tokio::test]
async fn cooled_replay_requires_known_identical_refs() {
    let (db_name, pg, registry) = bootstrap().await;
    let cold = std::sync::Arc::new(MemoryColdStore::default());
    let pg = pg.with_cold(cold);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner();
        let fact_a = seed_fact(&pg, owner).await?;
        let fact_b = seed_fact(&pg, owner).await?;
        let goal = create_goal(&pg, &registry, owner, seed_perspective(&pg, owner).await?).await?;
        let first_draft = direct_draft("cooled-links", vec![fact_a.into_inner()]);
        let first = AuthorizedFactWrite::new_with_links_for_tests(
            OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
            first_draft.clone(),
            None,
            Vec::new(),
            AuthorizedNodeLinks::new_for_tests(
                Vec::new(),
                vec![
                    EdgeEndpoint::memory(EntityKind::Fact, fact_a),
                    EdgeEndpoint::goal(goal),
                ],
            ),
        );
        let outcome = pg.ingest_authorized_fact_atomic(&first, None).await?;
        pg.forget_memory(
            &OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
            outcome.memory_id,
        )
        .await?;

        let replay = AuthorizedFactWrite::new_with_links_for_tests(
            OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
            first_draft.clone(),
            None,
            Vec::new(),
            AuthorizedNodeLinks::new_for_tests(
                Vec::new(),
                vec![
                    EdgeEndpoint::memory(EntityKind::Fact, fact_a),
                    EdgeEndpoint::goal(goal),
                ],
            ),
        );
        assert!(
            pg.ingest_authorized_fact_atomic(&replay, None)
                .await?
                .idempotent_replay
        );

        let changed = AuthorizedFactWrite::new_with_links_for_tests(
            OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
            direct_draft("cooled-links", vec![fact_b.into_inner()]),
            None,
            Vec::new(),
            AuthorizedNodeLinks::new_for_tests(
                Vec::new(),
                vec![
                    EdgeEndpoint::memory(EntityKind::Fact, fact_b),
                    EdgeEndpoint::goal(goal),
                ],
            ),
        );
        let error = pg
            .ingest_authorized_fact_atomic(&changed, None)
            .await
            .expect_err("cooled replay with changed refs must conflict");
        assert!(matches!(error, StorageError::Conflict(message) if message.contains("refs")));

        // Fixture-only simulation of a pre-0003 cooled row whose nullable
        // declaration was never recorded. Production updates remain sealed;
        // the trigger is disabled only inside this transaction and is
        // restored before commit (rollback restores it automatically).
        let mut legacy = pg.pool_for_tests().begin().await?;
        sqlx::query("ALTER TABLE proxima_core.cooled DISABLE TRIGGER cooled_append_only")
            .execute(&mut *legacy)
            .await?;
        sqlx::query("UPDATE proxima_core.cooled SET refs = NULL WHERE t = $1")
            .bind(outcome.memory_id.into_inner())
            .execute(&mut *legacy)
            .await?;
        sqlx::query("ALTER TABLE proxima_core.cooled ENABLE TRIGGER cooled_append_only")
            .execute(&mut *legacy)
            .await?;
        legacy.commit().await?;
        let unknown = AuthorizedFactWrite::new_with_links_for_tests(
            OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
            first_draft,
            None,
            Vec::new(),
            AuthorizedNodeLinks::new_for_tests(
                Vec::new(),
                vec![
                    EdgeEndpoint::memory(EntityKind::Fact, fact_a),
                    EdgeEndpoint::goal(goal),
                ],
            ),
        );
        let error = pg
            .ingest_authorized_fact_atomic(&unknown, None)
            .await
            .expect_err("legacy cooled refs must not be fabricated");
        assert!(
            matches!(error, StorageError::Conflict(message) if message.contains("unavailable"))
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("cooled reference replay contract failed");
}
