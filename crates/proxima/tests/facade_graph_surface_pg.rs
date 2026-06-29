use std::collections::BTreeSet;

use proxima::{
    AbstractionPayload, AppInfo, AuthPath, AuthorDerivedEdgeInput, AuthorDerivedRequestInput,
    AuthorshipKindMask, AuthzContext, EdgeAuthorshipKind, EdgeExistsRequest, EdgeFilter,
    EdgeReadRequest, EdgeTargetProjection, EndpointBinding, EntityKind, EntityKindMask, EntityRef,
    FactPayload, FlavorApp, FlavorBundle, FlavorRegistry, MemoryId, MemoryLineageDirection,
    MemoryLineageRequest, MemoryOperatorKind, PayloadKeyBuilder, PgMemoryPayload,
    PgMemoryPayloadFuture, PgMemorySidecar, PgSidecarFuture, PgSidecarRegistry, Proxima, Relation,
    Role, SchemaId, SchemaVersion, SidecarPayload, StorageError, UserId, company_owner,
    fact_entity_id_for,
};
use proxima::{RelationClass, RelationDescriptor};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
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
    use proxima::{
        AuthorshipKindMask, DerivedDraft, EdgeExistsRequest, EdgeExistsResponse, EdgeFilter,
        EdgeReadRequest, EdgeReadResponse, EdgeRow, EndpointBinding, EntityKindMask,
        FactCitationReadback, FlavorRegistryFrozen, McpTool, McpToolCtx, McpToolError,
        MemoryLineageDirection, MemoryLineageEdge, MemoryLineageNode, MemoryLineageRequest,
        MemoryLineageResponse, MemoryRow, PayloadKeyBuilder, PersonalityRootFilter, QueryRequest,
        QueryResponse, RelationClass, RelationDescriptor, SupersessionStatus, TombstoneFilter,
        append_derived_in_tx, build_instructions, fact_entity_id_for, how_to_markdown,
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
    fn load_memory_payload(pool: &sqlx::PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(Uuid, String, String)> = sqlx::query_as(
                "SELECT note_id, title, body
                   FROM public.facade_surface_fact_v1
                  WHERE memory_id = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
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
    fn load_memory_payload(pool: &sqlx::PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(String, String, i32)> = sqlx::query_as(
                "SELECT title, body, source_count
                   FROM public.facade_surface_abstraction_v1
                  WHERE memory_id = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
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
    fn register(registry: &mut FlavorRegistry) {
        registry.add_fact_schema::<FacadeFact>();
        registry.add_abstraction_schema::<FacadeAbstraction>();
        registry.add_relation(RelationDescriptor::substrate(
            DERIVED_FROM_FACT_RELATION,
            RelationClass::Provenance,
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::abstraction(),
            EntityKindMask::fact(),
            AuthorshipKindMask::external_agent(),
        ));
        registry.add_relation(RelationDescriptor::substrate(
            FACT_ENTITY_EDGE_RELATION,
            RelationClass::Provenance,
            EndpointBinding::Pin,
            EndpointBinding::FollowHead,
            EntityKindMask::abstraction(),
            EntityKindMask::fact(),
            AuthorshipKindMask::external_agent(),
        ));
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
        AuthorshipKindMask::external_agent(),
    );
    assert_eq!(descriptor.class, RelationClass::Provenance);

    let advertised_tools = BTreeSet::new();
    let advertised_resources = BTreeSet::new();
    assert!(proxima::build_instructions(&advertised_tools, &advertised_resources).is_empty());
    assert!(proxima::how_to_markdown(&advertised_tools, &advertised_resources).contains("Proxima"));
    let _ctx_size = std::mem::size_of::<Option<proxima::McpToolCtx>>();
    let _ = proxima::McpToolError::InvalidInput("bad input".to_string());

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
            .build()
            .await?;
        create_sidecar_tables(&built.pool).await?;
        let authz = AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(owner, Role::admin())],
            AuthPath::System,
        );

        let fact = FacadeFact {
            note_id: Uuid::now_v7(),
            title: "Observed facade gap".to_string(),
            body: "A consumer needs a single facade crate.".to_string(),
        };
        let fact_for_sidecar = fact.clone();
        let fact_outcome = proxima_storage_pg::verbs::fact_ingest::ingest_fact_for_owner(
            &built.pool,
            built.engine.as_ref(),
            &authz,
            &owner,
            Relation::Ingest,
            &fact,
            move |tx, outcome| {
                Box::pin(async move {
                    fact_for_sidecar
                        .insert_memory_sidecar(tx, outcome.memory_id)
                        .await
                })
            },
        )
        .await?;

        let mut conn = built.pool.acquire().await?;
        let fact_entity_id = fact_entity_id_for(
            conn.as_mut(),
            &owner,
            &SchemaId::new(FacadeFact::SCHEMA_ID.to_string()),
            SchemaVersion::new(FacadeFact::SCHEMA_VERSION),
            &[fact.note_id.to_string()],
        )
        .await?
        .expect("stateful fact has an aggregate entity id");

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
            authorship_kind: EdgeAuthorshipKind::ExternalAgent,
            authorship_owner_memory_id: None,
        }];
        let derived_outcome = built
            .engine
            .author_derived(AuthorDerivedRequestInput {
                memory_id: derived_id,
                owner,
                kind: EntityKind::Abstraction,
                text: "Single facade dependency is enough for flavor authors.".to_string(),
                schema_id: SchemaId::new(FacadeAbstraction::SCHEMA_ID.to_string()),
                schema_version: SchemaVersion::new(FacadeAbstraction::SCHEMA_VERSION),
                operator_kind: MemoryOperatorKind::ExternalAgent,
                model_id: "facade-test",
                prompt_version: "v1",
                author_personality_instance_id: None,
                sidecar_payload: SidecarPayload::abstraction(FacadeAbstraction {
                    title: "Facade surface".to_string(),
                    body: "Single facade dependency is enough for flavor authors.".to_string(),
                    source_count: 1,
                }),
                supersedes: None,
                edges: &derived_edges,
            })
            .await?;
        assert_eq!(derived_outcome.memory_id, derived_id);
        assert_eq!(derived_outcome.edge_count, 1);

        let embedding_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)
               FROM proxima_core.embeddings
              WHERE entity_kind = 'Abstraction'
                AND entity_id = $1",
        )
        .bind(derived_id.into_inner())
        .fetch_one(&built.pool)
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
                    principal: owner,
                    start_memory_id: derived_id,
                    direction: MemoryLineageDirection::Ancestors,
                    depth: 2,
                    limit: 10,
                    reader_personality_instance_id: None,
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
        .bind(EdgeAuthorshipKind::ExternalAgent)
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
                    principal: owner,
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
                    principal: owner,
                    edge_ids: Vec::new(),
                    filter: EdgeFilter {
                        target: Some(EntityRef::FactEntity(proxima::FactEntityId::new(
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
                    principal: owner,
                    edge_ids: Vec::new(),
                    filter: present_filter,
                    limit: 5,
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
