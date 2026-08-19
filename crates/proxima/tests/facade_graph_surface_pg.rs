use std::collections::BTreeSet;

use proxima::flavor::{
    AbstractionPayload, EdgeEndpoint, EdgeKind, FactPayload, FlavorBundle, FlavorRegistry,
    InputContractId, MemoryId, OperatorId, PayloadKeyBuilder, PayloadReference, PgMemoryPayload,
    PgMemoryPayloadFuture, PgMemorySidecar, PgSidecarFuture, PgSidecarReadCtx, PgSidecarRegistry,
    ReferenceBinding, SchemaId, SchemaVersion, SidecarPayload,
};
use proxima::{
    AppInfo, AuthPath, AuthzContext, EdgeExistsRequest, EdgeFilter, EdgeReadRequest, FlavorApp,
    MemoryLineageDirection, MemoryLineageRequest, Proxima, StorageError, ToolScope, company_owner,
};
use proxima_core::{
    AuthorDerivedRequestInput, EdgeTargetProjection, EntityKind, EntityRef, MemoryOperatorKind,
    Role, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use uuid::Uuid;

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
        Edge, EdgeEndpoint, EdgeKind, EdgeTargetProjection, EntityRef, FlavorRegistryFrozen,
        PayloadKeyBuilder, PayloadReference, ReferenceBinding, Tool, ToolCtx, ToolError,
    };
    use proxima::{
        EdgeExistsRequest, EdgeExistsResponse, EdgeFilter, EdgeReadCursor, EdgeReadRequest,
        EdgeReadResponse, FactCitationReadback, MemoryLineageDirection, MemoryLineageEdge,
        MemoryLineageNode, MemoryLineageRequest, MemoryLineageResponse, MemoryRow, QueryRequest,
        QueryResponse, SupersessionStatus, build_instructions, how_to_markdown,
    };

    #[cfg(feature = "openai-compat-embed")]
    use proxima::{OpenAiCompatConfig, OpenAiCompatEmbeddingClient};
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
    /// The Fact entity this abstraction is *about*, as opposed to the
    /// observations it was made from. A schema-declared reference field:
    /// the flavor states it, ingest turns it into one `reference` row, and
    /// nobody writes an edge. Follow-head, so re-observing the entity does
    /// not strand the pointer on a frozen observation.
    observed_entity: Uuid,
}

impl AbstractionPayload for FacadeAbstraction {
    const SCHEMA_ID: &'static str = "facade-test/abstraction-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "public.facade_surface_abstraction_v1"
    }

    fn references(&self) -> Vec<PayloadReference> {
        vec![PayloadReference::memory(
            "observed_entity",
            EntityKind::Fact,
            MemoryId::new(self.observed_entity),
        )]
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
                    (memory_id, title, body, source_count, observed_entity)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (memory_id) DO NOTHING",
            )
            .bind(memory_id.into_inner())
            .bind(&self.title)
            .bind(&self.body)
            .bind(self.source_count)
            .bind(self.observed_entity)
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
            let row: Option<(String, String, i32, Uuid)> = ctx
                .fetch_optional_by_memory_id(
                    "SELECT title, body, source_count, observed_entity
                       FROM public.facade_surface_abstraction_v1
                      WHERE memory_id = $1",
                    memory_id,
                )
                .await?;
            Ok(row.map(|(title, body, source_count, observed_entity)| {
                SidecarPayload::abstraction(FacadeAbstraction {
                    title,
                    body,
                    source_count,
                    observed_entity,
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

    // The connection vocabulary a flavor is allowed to speak: a reference
    // field it declares, and the two kinds it may read back. There is no
    // descriptor to build, because there is no vocabulary to extend.
    let reference = PayloadReference::memory(
        "symbol_source",
        EntityKind::Fact,
        MemoryId::new(Uuid::nil()),
    );
    assert_eq!(reference.binding, ReferenceBinding::Pin);
    reference.validate().expect("Pin addresses a memory row");
    assert_eq!(EdgeKind::Origin.as_str(), "origin");
    assert_eq!(
        EdgeEndpoint::memory(EntityKind::Abstraction, MemoryId::new(Uuid::nil())).layer(),
        Some(1)
    );

    let advertised_tools = BTreeSet::new();
    let advertised_resources = BTreeSet::new();
    assert!(proxima::build_instructions(&advertised_tools, &advertised_resources).is_empty());
    assert!(proxima::how_to_markdown(&advertised_tools, &advertised_resources).contains("Proxima"));
    let _ctx_size = std::mem::size_of::<Option<proxima::flavor::ToolCtx>>();
    let _ = proxima::flavor::ToolError::InvalidInput("bad input".to_string());

    #[cfg(feature = "openai-compat-embed")]
    {
        let config = proxima::OpenAiCompatConfig::new(
            "https://embeddings.example/v1",
            Some("token".to_string()),
        );
        assert_eq!(config.base_url, "https://embeddings.example/v1");
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

        let derived_handle = MemoryId::new(Uuid::now_v7());
        let derived_from = [EdgeEndpoint::memory(
            EntityKind::Fact,
            fact_outcome.memory_id,
        )];
        let derived_outcome = built
            .engine
            .author_derived_authorized(
                &authz,
                AuthorDerivedRequestInput {
                    memory_id: derived_handle,
                    owner,
                    kind: EntityKind::Abstraction,
                    text: "Single facade dependency is enough for flavor authors.".to_string(),
                    schema_id: SchemaId::new(FacadeAbstraction::SCHEMA_ID.to_string()),
                    schema_version: SchemaVersion::new(FacadeAbstraction::SCHEMA_VERSION),
                    operator_kind: MemoryOperatorKind::FtoA,
                    operator_id: OperatorId::new(Uuid::now_v7()),
                    input_contract_id: InputContractId::new(Uuid::now_v7()),
                    model_id: "facade-test",
                    sidecar_payload: SidecarPayload::abstraction(FacadeAbstraction {
                        title: "Facade surface".to_string(),
                        body: "Single facade dependency is enough for flavor authors.".to_string(),
                        source_count: 1,
                        observed_entity: fact_outcome.memory_id.into_inner(),
                    }),
                    derived_from: &derived_from,
                    extra_refs: &[],
                    supersedes: None,
                    lexical_language: None,
                },
            )
            .await?;
        let derived_t = derived_outcome.memory_id;
        assert_ne!(derived_t, fact_outcome.memory_id);
        assert_eq!(derived_outcome.edge_count, 2);

        let embedding_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)
               FROM proxima_core.embeddings
              WHERE entity_id = $1",
        )
        .bind(derived_t.into_inner())
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
                    start_memory_id: derived_t,
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
                edge.edge.kind == EdgeKind::Origin
                    && edge.edge.source.memory_id() == Some(derived_t)
                    && matches!(
                        edge.edge.target,
                        EdgeTargetProjection::Visible { target }
                            if target.memory_id() == Some(fact_outcome.memory_id)
                    )
            }),
            "lineage traverses origin"
        );

        let head_filter = EdgeFilter {
            kind: Some(EdgeKind::Reference),
            source: Some(EntityRef::Memory(derived_t)),
            target: Some(EntityRef::Memory(fact_outcome.memory_id)),
        };
        let exists = built
            .engine
            .edge_exists(
                &authz,
                &EdgeExistsRequest {
                    owner,
                    filter: head_filter.clone(),
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
                    filter: EdgeFilter {
                        target: Some(EntityRef::Memory(MemoryId::new(Uuid::now_v7()))),
                        ..head_filter.clone()
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
                    filter: head_filter,
                    limit: 5,
                    cursor: None,
                },
            )
            .await?;
        assert_eq!(read.edges.len(), 1);
        assert_eq!(read.edges[0].kind, EdgeKind::Reference);
        assert_eq!(
            read.edges[0].source,
            EdgeEndpoint::memory(EntityKind::Abstraction, derived_t)
        );
        assert_eq!(
            read.edges[0].target,
            EdgeTargetProjection::visible(EdgeEndpoint::memory(
                EntityKind::Fact,
                fact_outcome.memory_id
            ))
        );

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
            source_count integer NOT NULL,
            observed_entity uuid NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}
