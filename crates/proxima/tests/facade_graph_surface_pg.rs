use std::collections::BTreeSet;

use proxima::flavor::{
    AbstractionPayload, AuthorshipKindMask, EntityKindMask, FactPayload, FlavorBundle,
    FlavorRegistry, InputContractId, MemoryId, OperatorId, PayloadKeyBuilder, PgMemoryPayload,
    PgMemoryPayloadFuture, PgMemorySidecar, PgSidecarFuture, PgSidecarReadCtx, PgSidecarRegistry,
    RelationClass, RelationDescriptor, SchemaId, SchemaVersion, SidecarPayload,
};
use proxima::{
    AppInfo, AuthPath, AuthzContext, EdgeExistsRequest, EdgeFilter, EdgeReadRequest, FlavorApp,
    MemoryLineageDirection, MemoryLineageRequest, Proxima, StorageError, ToolScope, company_owner,
};
use proxima_core::{
    AuthorDerivedEdgeInput, AuthorDerivedRequestInput, EdgeAuthorshipKind, EdgeTargetProjection,
    EndpointBinding, EntityKind, EntityRef, MemoryOperatorKind, Role, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::query::fact_entity_id_for;
use uuid::Uuid;

const DERIVED_FROM_FACT_RELATION: &str = "facade-test/derived-from-fact";
const FACT_ENTITY_EDGE_RELATION: &str = "facade-test/fact-entity-edge";

#[test]
fn facade_does_not_export_raw_edge_append_surface() {
    let facade = include_str!("../src/lib.rs");
    assert!(
        !facade.contains("pub use proxima_storage_pg::verbs::edge_append"),
        "facade must not re-export raw storage edge append APIs"
    );
    for forbidden in ["append_edge", "append_edge_in_tx", "EdgeDraft"] {
        assert!(
            !facade.contains(forbidden),
            "facade contains forbidden raw edge append surface {forbidden}"
        );
    }
}

#[allow(unused_imports)]
mod facade_imports_compile {
    use proxima::flavor::{
        AuthorshipKindMask, EndpointBinding, EntityKindMask, FlavorRegistryFrozen,
        PayloadKeyBuilder, RelationClass, RelationDescriptor, Tool, ToolCtx, ToolError,
    };
    use proxima::{
        EdgeExistsRequest, EdgeExistsResponse, EdgeFilter, EdgeReadRequest, EdgeReadResponse,
        EdgeRow, FactCitationReadback, MemoryLineageDirection, MemoryLineageEdge,
        MemoryLineageNode, MemoryLineageRequest, MemoryLineageResponse, MemoryRow, QueryRequest,
        QueryResponse, SupersessionStatus, TombstoneFilter, build_instructions, how_to_markdown,
    };

    #[cfg(feature = "openai-compat-embed")]
    use proxima::{
        MISTRAL_EMBED_BASE_URL, MISTRAL_EMBED_MODEL, OpenAiCompatConfig,
        OpenAiCompatEmbeddingClient,
    };
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FacadeFact {
    note_id: Uuid,
    title: String,
    body: String,
}

impl FactPayload for FacadeFact {
    const SCHEMA_ID: &'static str = "facade-test/fact-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_uuid("note_id", self.note_id);
        key.finish()
    }

    fn render(&self) -> String {
        let title = &self.title;
        let body = &self.body;
        format!("{title}\n\n{body}")
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("public.facade_surface_fact_v1")
    }

    fn natural_key_columns() -> &'static [&'static str] {
        &["note_id"]
    }
}

impl PgMemorySidecar for FacadeFact {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.facade_surface_fact_v1
                    (memory_id, note_id, title, body)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (memory_id) DO NOTHING",
            )
            .bind(memory_id.into_inner())
            .bind(self.note_id)
            .bind(&self.title)
            .bind(&self.body)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for FacadeFact {
    fn load_memory_payload(
        ctx: PgSidecarReadCtx<'_>,
        memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(Uuid, String, String)> = ctx
                .fetch_optional_by_memory_id(
                    "SELECT note_id, title, body
                       FROM public.facade_surface_fact_v1
                      WHERE memory_id = $1",
                    memory_id,
                )
                .await?;
            Ok(row.map(|(note_id, title, body)| {
                SidecarPayload::fact(FacadeFact {
                    note_id,
                    title,
                    body,
                })
            }))
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FacadeAbstraction {
    title: String,
    body: String,
    source_count: i32,
}

impl AbstractionPayload for FacadeAbstraction {
    const SCHEMA_ID: &'static str = "facade-test/abstraction-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "public.facade_surface_abstraction_v1"
    }
}

impl PgMemorySidecar for FacadeAbstraction {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.facade_surface_abstraction_v1
                    (memory_id, title, body, source_count)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (memory_id) DO NOTHING",
            )
            .bind(memory_id.into_inner())
            .bind(&self.title)
            .bind(&self.body)
            .bind(self.source_count)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for FacadeAbstraction {
    fn load_memory_payload(
        ctx: PgSidecarReadCtx<'_>,
        memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(String, String, i32)> = ctx
                .fetch_optional_by_memory_id(
                    "SELECT title, body, source_count
                       FROM public.facade_surface_abstraction_v1
                      WHERE memory_id = $1",
                    memory_id,
                )
                .await?;
            Ok(row.map(|(title, body, source_count)| {
                SidecarPayload::abstraction(FacadeAbstraction {
                    title,
                    body,
                    source_count,
                })
            }))
        })
    }
}

struct FacadeSurfaceApp;

impl FlavorBundle for FacadeSurfaceApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError> {
        registry.try_add_fact_schema::<FacadeFact>()?;
        registry.try_add_abstraction_schema::<FacadeAbstraction>()?;
        registry.try_add_relation(RelationDescriptor::substrate(
            DERIVED_FROM_FACT_RELATION,
            RelationClass::Provenance,
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::abstraction(),
            EntityKindMask::fact(),
            AuthorshipKindMask::operator_f_to_a(),
        ))?;
        registry.try_add_relation(RelationDescriptor::substrate(
            FACT_ENTITY_EDGE_RELATION,
            RelationClass::Provenance,
            EndpointBinding::Pin,
            EndpointBinding::FollowHead,
            EntityKindMask::abstraction(),
            EntityKindMask::fact(),
            AuthorshipKindMask::operator_a_to_a(),
        ))?;
        Ok(())
    }

    fn register_pg_sidecars(registry: &mut PgSidecarRegistry) {
        registry.add_fact::<FacadeFact>();
        registry.add_abstraction::<FacadeAbstraction>();
    }

    fn migrators() -> Vec<proxima::NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for FacadeSurfaceApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "facade-surface-test",
            title: "Facade Surface Test",
            version: "1",
        }
    }
}

#[test]
fn facade_flavor_authoring_symbols_are_reachable() {
    let mut key = PayloadKeyBuilder::new("facade-test/symbol", 1);
    key.field_str("kind", "proof");
    assert!(!key.finish().is_empty());

    let descriptor = RelationDescriptor::substrate(
        "facade-test/symbol-edge",
        RelationClass::Provenance,
        EndpointBinding::Pin,
        EndpointBinding::Pin,
        EntityKindMask::abstraction(),
        EntityKindMask::fact(),
        AuthorshipKindMask::operator_a_to_a(),
    );
    assert_eq!(descriptor.class, RelationClass::Provenance);

    let advertised_tools = BTreeSet::new();
    let advertised_resources = BTreeSet::new();
    assert!(proxima::build_instructions(&advertised_tools, &advertised_resources).is_empty());
    assert!(proxima::how_to_markdown(&advertised_tools, &advertised_resources).contains("Proxima"));
    let _ctx_size = std::mem::size_of::<Option<proxima::flavor::ToolCtx>>();
    let _ = proxima::flavor::ToolError::InvalidInput("bad input".to_string());

    #[cfg(feature = "openai-compat-embed")]
    {
        let config = proxima::OpenAiCompatConfig::new(
            proxima::MISTRAL_EMBED_BASE_URL,
            Some("token".to_string()),
        );
        assert_eq!(config.base_url, proxima::MISTRAL_EMBED_BASE_URL);
        assert_eq!(proxima::MISTRAL_EMBED_MODEL, "mistral-embed");
        let _client_size = std::mem::size_of::<proxima::OpenAiCompatEmbeddingClient>();
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn facade_engine_reads_lineage_edges_and_derives_without_embedding_client()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = unique_db_name("proxima_facade_surface");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<FacadeSurfaceApp>::app()
            .database_url(db_url)
            .owner(owner)
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        create_sidecar_tables(built.pool_for_tests()).await?;
        let authz = AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(owner, Role::admin())],
            AuthPath::HostBearer,
        );
        let fact_permit = built
            .engine
            .authorize_owner_write(&authz, &owner, proxima_core::AccessKind::Fact)
            .await?;

        let fact = FacadeFact {
            note_id: Uuid::now_v7(),
            title: "Observed facade gap".to_string(),
            body: "A consumer needs a single facade crate.".to_string(),
        };
        let fact_for_sidecar = fact.clone();
        let fact_outcome = proxima_storage_pg::verbs::fact_ingest::ingest_fact_for_owner(
            built.pool_for_tests(),
            &fact_permit,
            &fact,
            None,
            move |tx, outcome| {
                Box::pin(async move {
                    fact_for_sidecar
                        .insert_memory_sidecar(tx, outcome.memory_id)
                        .await
                })
            },
        )
        .await?;

        let mut conn = built.pool_for_tests().acquire().await?;
        let fact_entity_id = fact_entity_id_for(
            conn.as_mut(),
            &owner,
            &SchemaId::new(FacadeFact::SCHEMA_ID.to_string()),
            SchemaVersion::new(FacadeFact::SCHEMA_VERSION),
            &[fact.note_id.to_string()],
        )
        .await?
        .expect("stateful fact has an aggregate entity id");

        let source_batch_id: Uuid = sqlx::query_scalar(
            "SELECT fr.source_batch_id
               FROM proxima_core.memories m
               JOIN proxima_core.fact_receipts fr ON fr.receipt_id = m.receipt_id
              WHERE m.memory_id = $1",
        )
        .bind(fact_outcome.memory_id.into_inner())
        .fetch_one(built.pool_for_tests())
        .await?;
        proxima_storage_pg::verbs::close_batch::close_batch(
            built.pool_for_tests(),
            &fact_permit,
            proxima_core::SourceBatchId::new(source_batch_id),
        )
        .await?;
        let derived_id = MemoryId::new(Uuid::now_v7());
        let derived_relation = built
            .engine
            .registry()
            .resolve_relation(DERIVED_FROM_FACT_RELATION)
            .expect("derived relation registered");
        let derived_edges = [AuthorDerivedEdgeInput {
            relation: derived_relation,
            source_kind: EntityKind::Abstraction,
            source_memory_id: derived_id,
            target_kind: EntityKind::Fact,
            target_memory_id: fact_outcome.memory_id,
            authorship_kind: EdgeAuthorshipKind::OperatorFtoA,
            authorship_owner_memory_id: None,
        }];
        let derived_outcome = built
            .engine
            .author_derived_authorized(
                &authz,
                AuthorDerivedRequestInput {
                    memory_id: derived_id,
                    owner,
                    kind: EntityKind::Abstraction,
                    text: "Single facade dependency is enough for flavor authors.".to_string(),
                    schema_id: SchemaId::new(FacadeAbstraction::SCHEMA_ID.to_string()),
                    schema_version: SchemaVersion::new(FacadeAbstraction::SCHEMA_VERSION),
                    operator_kind: MemoryOperatorKind::FtoA,
                    operator_id: OperatorId::new(Uuid::now_v7()),
                    input_contract_id: InputContractId::new(Uuid::now_v7()),
                    source_batch_id: Some(proxima_core::SourceBatchId::new(source_batch_id)),
                    model_id: "facade-test",
                    prompt_version: "v1",
                    sidecar_payload: SidecarPayload::abstraction(FacadeAbstraction {
                        title: "Facade surface".to_string(),
                        body: "Single facade dependency is enough for flavor authors.".to_string(),
                        source_count: 1,
                    }),
                    supersedes: None,
                    lexical_language: None,
                    edges: &derived_edges,
                },
            )
            .await?;
        assert_eq!(derived_outcome.memory_id, derived_id);
        assert_eq!(derived_outcome.edge_ids.len(), 1);

        let embedding_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)
               FROM proxima_core.embeddings
              WHERE entity_kind = 'Abstraction'
                AND entity_id = $1",
        )
        .bind(derived_id.into_inner())
        .fetch_one(built.pool_for_tests())
        .await?;
        assert_eq!(
            embedding_rows, 0,
            "no embedding client writes no vector row"
        );

        let lineage = built
            .engine
            .walk_memory_lineage(
                &authz,
                &MemoryLineageRequest {
                    owner,
                    start_memory_id: derived_id,
                    direction: MemoryLineageDirection::Ancestors,
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
                .any(|node| node.memory_id == fact_outcome.memory_id),
            "lineage includes source fact"
        );
        assert!(
            lineage.edges.iter().any(|edge| {
                edge.source_memory_id == derived_id
                    && matches!(
                        edge.target,
                        EdgeTargetProjection::Visible {
                            target: EntityRef::Memory(target_memory_id),
                        } if target_memory_id == fact_outcome.memory_id
                    )
            }),
            "lineage includes derived-from edge"
        );

        let fact_entity_relation = built
            .engine
            .registry()
            .resolve_relation(FACT_ENTITY_EDGE_RELATION)
            .expect("fact-entity relation registered");
        let (owner_kind, owner_id) = owner.columns();
        sqlx::query(
            "INSERT INTO proxima_core.edges
                (edge_id, relation, relation_class,
                 source_kind, source_memory_id,
                 target_kind, target_fact_entity_id,
                 authorship_kind, owner_kind, owner_id)
             VALUES ($1, $2, $3, 'Abstraction', $4, 'Fact', $5, $6, $7, $8)",
        )
        .bind(Uuid::now_v7())
        .bind(fact_entity_relation.descriptor.relation.as_str())
        .bind(fact_entity_relation.descriptor.class)
        .bind(derived_id.into_inner())
        .bind(fact_entity_id.into_inner())
        .bind(EdgeAuthorshipKind::OperatorAtoA)
        .bind(owner_kind)
        .bind(owner_id)
        .execute(conn.as_mut())
        .await?;

        let present_filter = EdgeFilter {
            relation: Some(FACT_ENTITY_EDGE_RELATION.to_string()),
            source: Some(EntityRef::Memory(derived_id)),
            target: Some(EntityRef::FactEntity(fact_entity_id)),
        };
        let exists = built
            .engine
            .edge_exists(
                &authz,
                &EdgeExistsRequest {
                    owner,
                    edge_ids: Vec::new(),
                    filter: present_filter.clone(),
                },
            )
            .await?;
        assert!(exists.exists);

        let missing = built
            .engine
            .edge_exists(
                &authz,
                &EdgeExistsRequest {
                    owner,
                    edge_ids: Vec::new(),
                    filter: EdgeFilter {
                        target: Some(EntityRef::FactEntity(proxima_core::FactEntityId::new(
                            Uuid::now_v7(),
                        ))),
                        ..present_filter.clone()
                    },
                },
            )
            .await?;
        assert!(!missing.exists);

        let read = built
            .engine
            .read_edges(
                &authz,
                &EdgeReadRequest {
                    owner,
                    edge_ids: Vec::new(),
                    filter: present_filter,
                    limit: 5,
                    cursor: None,
                    include_payloads: false,
                },
            )
            .await?;
        assert_eq!(read.edges.len(), 1);
        assert_eq!(read.edges[0].source, EntityRef::Memory(derived_id));
        assert_eq!(
            read.edges[0].target,
            EdgeTargetProjection::Visible {
                target: EntityRef::Memory(fact_outcome.memory_id),
            }
        );

        drop(conn);
        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

async fn create_sidecar_tables(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE public.facade_surface_fact_v1 (
            memory_id uuid PRIMARY KEY,
            note_id uuid NOT NULL,
            title text NOT NULL,
            body text NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE public.facade_surface_abstraction_v1 (
            memory_id uuid PRIMARY KEY,
            title text NOT NULL,
            body text NOT NULL,
            source_count integer NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}
