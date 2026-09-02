use std::collections::BTreeSet;

use proxima::flavor::{
    AbstractionPayload, EdgeEndpoint, EdgeKind, FactPayload, FlavorBundle, FlavorRegistry,
    InputContractId, MemoryId, OperatorId, PayloadKeyBuilder, PayloadReference, PgMemoryPayload,
    PgMemoryPayloadFuture, PgMemorySidecar, PgSidecarFuture, PgSidecarReadCtx, PgSidecarRegistry,
    ReferenceBinding, SchemaId, SchemaVersion, SidecarInsertPermit, SidecarPayload,
};
use proxima::{
    AppInfo, AuthPath, AuthzContext, EdgeExistsRequest, EdgeFilter, EdgeReadRequest, FlavorApp,
    GetMemoriesReadRequest, MemoryLineageDirection, MemoryLineageRequest, Proxima, QueryRequest,
    StorageError, ToolScope, company_owner,
};
use proxima_core::engine::{FactCitationReadRequest, GetMemoryReadRequest};
use proxima_core::flavor::{
    BandComparability, CounterRule, EmbeddingRecipe, EraseRule, ExportRule, FlavorContract,
    ForgetRule, KeyShape, LanguagePolicy, ProjectionDecl, ProjectionSpec, Provenance, RankSource,
    SchemaContract, SchemaRef, SearchProjectionDecl, SubstringArm, Surface, TransferRule,
    WEIGHT_UNIFORM, WeightedField,
};
use proxima_core::read_models::MemorySchemaSpec;
use proxima_core::storage::MemoryGraphIdentity;
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    AuthorDerivedRequestInput, EdgeTargetProjection, EntityKind, EntityRef, MemoryOperatorKind,
    Role, SearchProjectionColumnKind, UserId,
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
    tags: Vec<String>,
}

impl FactPayload for FacadeFact {
    const SCHEMA_ID: &'static str = "facade-test/fact-v2";
    const SCHEMA_VERSION: u32 = 2;

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
        _permit: SidecarInsertPermit,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.facade_surface_fact_v1
                    (t, note_id, title, body, tags)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (t) DO NOTHING",
            )
            .bind(memory_id.into_inner())
            .bind(self.note_id)
            .bind(&self.title)
            .bind(&self.body)
            .bind(&self.tags)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for FacadeFact {
    // The column this table stores its memory `t` under, spelled by
    // every statement below. Freeze holds it equal to the contract
    // `Surface`'s `KeyShape::MemoryT { column }` for public.facade_surface_fact_v1.
    const OWNER_PINNED: bool = false;
    const MEMORY_KEY_COLUMN: &'static str = "t";

    fn load_memory_payload(
        ctx: PgSidecarReadCtx<'_>,
        memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(Uuid, String, String, Vec<String>)> = ctx
                .fetch_optional_by_memory_id(
                    "SELECT note_id, title, body, tags
                       FROM public.facade_surface_fact_v1
                      WHERE t = $1",
                    memory_id,
                )
                .await?;
            Ok(row.map(|(note_id, title, body, tags)| {
                SidecarPayload::fact(FacadeFact {
                    note_id,
                    title,
                    body,
                    tags,
                })
            }))
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FacadeSidecarlessFact {
    observation_id: Uuid,
    body: String,
}

impl FactPayload for FacadeSidecarlessFact {
    const SCHEMA_ID: &'static str = "facade-test/sidecarless-fact-v3";
    const SCHEMA_VERSION: u32 = 3;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_uuid("observation_id", self.observation_id);
        key.finish()
    }

    fn render(&self) -> String {
        self.body.clone()
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
        _permit: SidecarInsertPermit,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.facade_surface_abstraction_v1
                    (t, title, body, source_count, observed_entity)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (t) DO NOTHING",
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
    // The column this table stores its memory `t` under, spelled by
    // every statement below. Freeze holds it equal to the contract
    // `Surface`'s `KeyShape::MemoryT { column }` for public.facade_surface_abstraction_v1.
    const OWNER_PINNED: bool = false;
    const MEMORY_KEY_COLUMN: &'static str = "t";

    fn load_memory_payload(
        ctx: PgSidecarReadCtx<'_>,
        memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(String, String, i32, Uuid)> = ctx
                .fetch_optional_by_memory_id(
                    "SELECT title, body, source_count, observed_entity
                       FROM public.facade_surface_abstraction_v1
                      WHERE t = $1",
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

const FACADE_BANDS: &[proxima_core::flavor::Band] = &[
    proxima_core::flavor0::BAND_EXACT,
    proxima_core::flavor0::BAND_RESCUE,
    proxima_core::flavor0::BAND_SUBSTRING,
];

const FACADE_PROJECTION: ProjectionSpec = ProjectionSpec {
    table: "facade_surface.projection",
    index: "facade_surface_projection_owner_tsv_gin",
    overfetch_k: 1_000,
    band_comparability: BandComparability::CoreBands,
    rank_source: RankSource::Projection,
};

const fn facade_memory_surface(table: &'static str) -> Surface {
    Surface {
        table,
        key: KeyShape::MemoryT { column: "t" },
        owner_column: None,
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::ByKey,
        export: ExportRule::Rows,
        forget: ForgetRule::DumpThenDelete,
        lexical_language_column: None,
        counter: CounterRule::Counted("sidecar_rows"),
        completeness: None,
    }
}

static FACADE_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: "facade-test",
    ordinal: 7,
    schemas: &[
        SchemaContract {
            id: SchemaRef::new("facade-test", "fact", 2),
            kind: PayloadKind::Fact,
            sidecar_table: Some("public.facade_surface_fact_v1"),
            search: SearchProjectionDecl::Projected {
                fields: &[
                    WeightedField {
                        column: "title",
                        kind: SearchProjectionColumnKind::Text,
                        weight: WEIGHT_UNIFORM,
                    },
                    WeightedField {
                        column: "body",
                        kind: SearchProjectionColumnKind::Text,
                        weight: WEIGHT_UNIFORM,
                    },
                ],
                tag_column: Some("tags"),
                language: LanguagePolicy::Pinned("simple"),
                bands: FACADE_BANDS,
                substring: SubstringArm::MemoryFirstNestedLoop,
            },
            embedding: EmbeddingRecipe::Never {
                why: "the fixture proves lexical hydration, not embedding",
            },
            transfer: TransferRule::StaysOnKey,
            provenance: Provenance::None,
            surfaces: &[facade_memory_surface("public.facade_surface_fact_v1")],
            natural_key_columns: &["note_id"],
        },
        SchemaContract {
            id: SchemaRef::new("facade-test", "sidecarless-fact", 3),
            kind: PayloadKind::Fact,
            sidecar_table: None,
            search: SearchProjectionDecl::None {
                why: "a sidecarless Memory has no payload text to project",
            },
            embedding: EmbeddingRecipe::Never {
                why: "a sidecarless Memory has no payload text to embed",
            },
            transfer: TransferRule::StaysOnKey,
            provenance: Provenance::None,
            surfaces: &[],
            natural_key_columns: &[],
        },
        SchemaContract {
            id: SchemaRef::new("facade-test", "abstraction", 1),
            kind: PayloadKind::Abstraction,
            sidecar_table: Some("public.facade_surface_abstraction_v1"),
            search: SearchProjectionDecl::None {
                why: "the fixture's search proof belongs to its version-two Fact",
            },
            embedding: EmbeddingRecipe::Never {
                why: "the fixture proves derivation without an embedding client",
            },
            transfer: TransferRule::StaysOnKey,
            provenance: Provenance::OriginEdges,
            surfaces: &[facade_memory_surface(
                "public.facade_surface_abstraction_v1",
            )],
            natural_key_columns: &[],
        },
    ],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
    projection: ProjectionDecl::Table(FACADE_PROJECTION),
};

mod facade_fixture_registry {
    use super::{FACADE_CONTRACT, FacadeAbstraction, FacadeFact, FacadeSidecarlessFact};

    proxima_core::proxima_flavor! {
        name = "facade-test",
        display_name = "Facade Surface Test",
        fact_schemas = [FacadeFact, FacadeSidecarlessFact],
        abstraction_schemas = [FacadeAbstraction],
        contract = &FACADE_CONTRACT,
    }
}

struct FacadeSurfaceApp;

impl FlavorBundle for FacadeSurfaceApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError> {
        facade_fixture_registry::register(registry)
    }

    fn register_pg_sidecars(registry: &mut PgSidecarRegistry) {
        registry.add_fact::<FacadeFact>();
        registry.add_abstraction::<FacadeAbstraction>();
    }

    fn migrators() -> Vec<proxima::NamedMigrator> {
        vec![proxima::NamedMigrator::new(
            "facade-test",
            facade_migrator(),
        )]
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

fn memory_schema_specs(registry: &proxima_core::FlavorRegistryFrozen) -> Vec<MemorySchemaSpec> {
    registry
        .schemas()
        .iter()
        .filter_map(|schema| {
            let kind = match schema.kind {
                proxima_core::verbs::schema::PayloadKind::Fact => EntityKind::Fact,
                proxima_core::verbs::schema::PayloadKind::Abstraction => EntityKind::Abstraction,
                proxima_core::verbs::schema::PayloadKind::Perspective => EntityKind::Perspective,
                _ => return None,
            };
            Some(MemorySchemaSpec {
                kind,
                schema_id: schema.schema_id.clone(),
                schema_version: schema.schema_version,
                sidecar_table: schema.sidecar_table.clone(),
            })
        })
        .collect()
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
        let authz = AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(owner, Role::admin())],
            AuthPath::HostBearer,
        );

        let fact = FacadeFact {
            note_id: Uuid::now_v7(),
            title: "Observed facade gap".to_string(),
            body: "quasarregistryprobe needs a single facade crate.".to_string(),
            tags: vec!["facade-surface-test".to_owned()],
        };
        // The public write path: `Engine` → `UnitOfWork` → the write-session
        // port. The sidecar row comes off the frozen registry, not off a
        // closure this test hands a transaction to.
        // Narrowed to the one owner it writes for: the engine stamps the
        // write owner from resolved access, so the ingest must resolve
        // exactly one.
        let write_authz = AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(owner, Role::admin())],
            AuthPath::HostBearer,
        )
        .narrowed_to_owner(owner)
        .expect("an admin on exactly this owner narrows to it");
        let fact_outcome = built
            .engine
            .ingest_typed_fact(&write_authz, "facade-surface-test", &fact)
            .await?;
        let mut query = QueryRequest::for_owner(owner);
        query.include_payloads = true;
        let queried = built.engine.query(&authz, &query).await?;
        let snapshot = queried
            .memories
            .iter()
            .find(|row| row.id == fact_outcome.memory_id)
            .expect("v2 fact snapshot");
        assert_eq!(snapshot.schema_version.into_inner(), 2);
        assert_eq!(
            snapshot
                .payload
                .as_ref()
                .and_then(SidecarPayload::graph_body),
            Some(fact.body.clone())
        );
        let single = built
            .engine
            .get_memory(
                &authz,
                &GetMemoryReadRequest {
                    memory_id: fact_outcome.memory_id,
                    include_neighbor_edges: false,
                },
            )
            .await?
            .memory
            .expect("single v2 snapshot");
        assert_eq!(single.schema_version.into_inner(), 2);
        assert_eq!(single.text.as_deref(), Some(fact.body.as_str()));
        let batch = built
            .engine
            .get_memories(
                &authz,
                &GetMemoriesReadRequest {
                    memory_ids: vec![fact_outcome.memory_id],
                },
            )
            .await?;
        assert_eq!(batch.memories.len(), 1);
        assert_eq!(batch.memories[0].schema_version.into_inner(), 2);
        assert_eq!(batch.memories[0].text.as_deref(), Some(fact.body.as_str()));

        let sidecarless_memory_id = insert_raw_fact_admission(
            built.pool_for_tests(),
            owner,
            FacadeSidecarlessFact::SCHEMA_ID,
            &[],
        )
        .await?;
        let sidecarless_snapshot = built
            .engine
            .get_memory(
                &authz,
                &GetMemoryReadRequest {
                    memory_id: sidecarless_memory_id,
                    include_neighbor_edges: false,
                },
            )
            .await?
            .memory
            .expect("sidecarless snapshot");
        assert_eq!(sidecarless_snapshot.schema_version.into_inner(), 3);
        assert!(sidecarless_snapshot.payload.is_none());
        assert!(sidecarless_snapshot.text.is_none());

        let search = built
            .engine
            .search(
                &authz,
                &proxima::SearchReadRequest {
                    search: proxima::MemorySearchRequest {
                        owner,
                        read_owners: vec![owner],
                        query: "quasarregistryprobe".to_owned(),
                        mode: proxima::SearchMode::Lexical,
                        supersession: proxima::SupersessionStatus::HeadsOnly,
                        limit: 10,
                        kind: None,
                        schema_id: None,
                        tags: vec!["facade-surface-test".to_owned()],
                        tag_match: proxima::TagMatch::Any,
                        since: None,
                        until: None,
                        order: proxima::SearchOrder::Relevance,
                        min_score: None,
                        semantic_weight: None,
                        after: None,
                        query_embedding: None,
                        embedding_model_id: None,
                    },
                    include_body: true,
                    include_neighbor_edges: false,
                },
            )
            .await?;
        assert!(
            search
                .memories
                .iter()
                .any(|row| row.memory_id == fact_outcome.memory_id),
            "search returns the registered version-two Fact"
        );
        assert!(search.payloads.iter().any(|row| {
            row.memory_id == fact_outcome.memory_id
                && row.body.as_deref() == Some(fact.body.as_str())
        }));

        let mut identity_only = QueryRequest::for_owner(owner);
        identity_only.include_payloads = false;
        let identity_page = built.engine.query(&authz, &identity_only).await?;
        let identity_row = identity_page
            .memories
            .iter()
            .find(|row| row.id == fact_outcome.memory_id)
            .expect("v2 query row without payload");
        assert_eq!(identity_row.schema_version.into_inner(), 2);
        assert!(identity_row.payload.is_none());

        let foreign_owner = company_owner(Uuid::now_v7());
        let foreign_memory_id = insert_raw_fact_admission(
            built.pool_for_tests(),
            foreign_owner,
            FacadeSidecarlessFact::SCHEMA_ID,
            &[],
        )
        .await?;

        let unknown = MemoryId::new(Uuid::now_v7());
        let collapsed = built
            .engine
            .get_memories(
                &authz,
                &GetMemoriesReadRequest {
                    memory_ids: vec![fact_outcome.memory_id, unknown, foreign_memory_id],
                },
            )
            .await?;
        assert_eq!(collapsed.memories.len(), 1);
        assert_eq!(collapsed.memories[0].memory_id, fact_outcome.memory_id);

        let citation = built
            .engine
            .read_fact_citation(
                &authz,
                &FactCitationReadRequest {
                    fact_memory_id: fact_outcome.memory_id,
                },
            )
            .await?;
        assert!(citation.is_none(), "the v2 visibility preflight succeeds");

        let graph_payloads = proxima_storage_pg::verbs::consolidate::load_memory_graph_payloads(
            built.pool_for_tests(),
            built.pg_sidecars.as_ref(),
            &[MemoryGraphIdentity {
                memory_id: fact_outcome.memory_id,
                kind: EntityKind::Fact,
                schema_id: SchemaId::new(FacadeFact::SCHEMA_ID.to_owned()),
            }],
            &memory_schema_specs(built.registry.as_ref()),
            true,
        )
        .await?;
        assert_eq!(graph_payloads.len(), 1);
        assert_eq!(graph_payloads[0].body.as_deref(), Some(fact.body.as_str()));

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

#[tokio::test]
async fn facade_query_checks_primary_sidecar_integrity_without_projecting_payloads()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = unique_db_name("proxima_facade_sidecar_integrity");
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
        let authz = AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(owner, Role::admin())],
            AuthPath::HostBearer,
        );

        let valid_with_extension = insert_raw_facade_fact(
            built.pool_for_tests(),
            owner,
            &[
                "public.facade_surface_fact_v1",
                "public.facade_surface_abstraction_v1",
            ],
            true,
        )
        .await?;
        let missing_stamp =
            insert_raw_facade_fact(built.pool_for_tests(), owner, &[], false).await?;
        let wrong_stamp = insert_raw_facade_fact(
            built.pool_for_tests(),
            owner,
            &["public.facade_surface_abstraction_v1"],
            false,
        )
        .await?;
        let missing_primary_row = insert_raw_facade_fact(
            built.pool_for_tests(),
            owner,
            &["public.facade_surface_fact_v1"],
            false,
        )
        .await?;

        let mut valid_query = QueryRequest::for_owner(owner);
        valid_query.include_payloads = false;
        valid_query.memory_ids = vec![valid_with_extension];
        let valid = built.engine.query(&authz, &valid_query).await?;
        assert_eq!(valid.memories.len(), 1);
        assert_eq!(valid.memories[0].schema_version.into_inner(), 2);
        assert!(valid.memories[0].payload.is_none());

        let Err(mixed_err) = built
            .engine
            .get_memories(
                &authz,
                &GetMemoriesReadRequest {
                    memory_ids: vec![valid_with_extension, wrong_stamp],
                },
            )
            .await
        else {
            panic!("one corrupt visible row must fail the whole batch");
        };
        assert_eq!(mixed_err.code, proxima_core::ErrorCode::Internal);

        for (label, memory_id) in [
            ("missing primary stamp", missing_stamp),
            ("wrong primary stamp", wrong_stamp),
            ("missing primary row", missing_primary_row),
        ] {
            let mut query = QueryRequest::for_owner(owner);
            query.include_payloads = false;
            query.memory_ids = vec![memory_id];
            let Err(err) = built.engine.query(&authz, &query).await else {
                panic!("{label} must fail closed");
            };
            assert_eq!(
                err.code,
                proxima_core::ErrorCode::Internal,
                "{label}: {err}"
            );
        }

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

async fn insert_raw_facade_fact(
    pool: &sqlx::PgPool,
    owner: proxima::Owner,
    stamped_tables: &[&str],
    insert_primary_row: bool,
) -> Result<MemoryId, sqlx::Error> {
    let memory_id =
        insert_raw_fact_admission(pool, owner, FacadeFact::SCHEMA_ID, stamped_tables).await?;
    if !insert_primary_row {
        // Since `0009_declared_sidecar_presence.sql` the database refuses
        // both ways into a stamp with no row: the admission above cannot skip
        // the row, and the DELETE below cannot take it while the stamp
        // stands. So the fixture writes the pair and deletes the row with the
        // orphan guard switched off — which is the only shape this state has
        // in a live database too, one whose trigger someone removed or whose
        // rows predate it. That IS the state this test needs the reader to
        // fail closed on.
        sqlx::query(
            "ALTER TABLE public.facade_surface_fact_v1
                 DISABLE TRIGGER facade_surface_fact_v1_declared_by_memory_on_delete",
        )
        .execute(pool)
        .await?;
        sqlx::query("DELETE FROM public.facade_surface_fact_v1 WHERE t = $1")
            .bind(memory_id.into_inner())
            .execute(pool)
            .await?;
        sqlx::query(
            "ALTER TABLE public.facade_surface_fact_v1
                 ENABLE TRIGGER facade_surface_fact_v1_declared_by_memory_on_delete",
        )
        .execute(pool)
        .await?;
    }
    Ok(memory_id)
}

async fn insert_raw_fact_admission(
    pool: &sqlx::PgPool,
    owner: proxima::Owner,
    schema_id: &str,
    stamped_tables: &[&str],
) -> Result<MemoryId, sqlx::Error> {
    let handle = Uuid::now_v7();
    let t = Uuid::now_v7();
    let owner_id = owner.stored_owner_id();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind)
         ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .bind(proxima_core::OwnerRefKind::of(&owner).as_str())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'fact', $2, $3, $4)",
    )
    .bind(handle)
    .bind(schema_id)
    .bind(owner_id)
    .bind(t)
    .execute(pool)
    .await?;
    let stamped_tables = stamped_tables
        .iter()
        .map(|table| (*table).to_owned())
        .collect::<Vec<_>>();
    // The stamp and the rows it promises land in one transaction: a memory
    // row that names a sidecar table it has no row in is refused at COMMIT.
    let mut stamped = pool.begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory
            (handle, t, kind, owner_id, schema_id, sidecar_tables)
         VALUES ($1, $2, 'fact', $3, $4, $5)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .bind(schema_id)
    .bind(&stamped_tables)
    .execute(&mut *stamped)
    .await?;
    for table in &stamped_tables {
        match table.as_str() {
            "public.facade_surface_fact_v1" => {
                sqlx::query(
                    "INSERT INTO public.facade_surface_fact_v1 (t, note_id, title, body)
                     VALUES ($1, $2, 'integrity fixture', 'payload present')",
                )
                .bind(t)
                .bind(Uuid::now_v7())
                .execute(&mut *stamped)
                .await?;
            }
            "public.facade_surface_abstraction_v1" => {
                sqlx::query(
                    "INSERT INTO public.facade_surface_abstraction_v1
                        (t, title, body, source_count, observed_entity)
                     VALUES ($1, 'integrity fixture', 'extension present', 1, $2)",
                )
                .bind(t)
                .bind(Uuid::now_v7())
                .execute(&mut *stamped)
                .await?;
            }
            other => panic!("fixture stamps an unknown sidecar table: {other}"),
        }
    }
    stamped.commit().await?;
    Ok(MemoryId::new(t))
}

/// The test flavor's own baseline, as a real flavor ships one.
///
/// It used to be a `create_sidecar_tables(pool)` called AFTER `build()`,
/// which is after boot — so this flavor's tables did not exist while the
/// substrate was checking the deployment, and the declaration triggers that
/// guard them did not exist at all. A flavor's DDL belongs in its migrator;
/// that is the whole of what changed here.
///
/// The trigger statements are read off the frozen registry rather than
/// written out, because that is what a flavor's migration author does:
/// `declaration_trigger_artifacts` and `presence_trigger_artifacts` emit
/// them, the migration carries them.
fn facade_migrator() -> sqlx::migrate::Migrator {
    use sqlx::SqlSafeStr;

    let mut registry = FlavorRegistry::new();
    FacadeSurfaceApp::register(&mut registry).expect("the facade test flavor registers");
    let registry = registry.try_freeze().expect("and freezes");
    let mut sidecars = PgSidecarRegistry::new();
    proxima_storage_pg::register_core_pg_sidecars(&mut sidecars);
    FacadeSurfaceApp::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars
        .freeze_against(&registry)
        .expect("the facade test PG registrations match its contract");
    let projection = proxima_storage_pg::projection::projection_artifacts(&FACADE_CONTRACT)
        .expect("the facade projection declaration is valid")
        .expect("the facade contract declares a projection table");

    let mut statements = vec![
        "CREATE SCHEMA facade_surface".to_owned(),
        "CREATE TABLE public.facade_surface_fact_v1 (
            t uuid PRIMARY KEY,
            note_id uuid NOT NULL,
            title text NOT NULL,
            body text NOT NULL,
            tags text[] NOT NULL DEFAULT '{}'
        )"
        .to_owned(),
        "CREATE TABLE public.facade_surface_abstraction_v1 (
            t uuid PRIMARY KEY,
            title text NOT NULL,
            body text NOT NULL,
            source_count integer NOT NULL,
            observed_entity uuid NOT NULL
        )"
        .to_owned(),
        // A test flavor is a flavor: `memory.sidecar_tables` is constrained
        // to be a subset of `proxima_core.flavor_surface`, so a fixture that
        // stamps a sidecar has to declare it like any other.
        "INSERT INTO proxima_core.flavor_surface (table_name, flavor_id) VALUES
             ('public.facade_surface_fact_v1', 'facade-test'),
             ('public.facade_surface_abstraction_v1', 'facade-test')"
            .to_owned(),
    ];
    statements.extend(
        projection
            .forward()
            .into_iter()
            .map(|statement| statement.trim_end_matches(';').to_owned()),
    );
    statements.extend(
        sidecars
            .declaration_trigger_artifacts("facade-test")
            .expect("the facade test flavor's declaration triggers")
            .into_iter()
            .map(|artifact| artifact.forward),
    );
    statements.extend(
        sidecars
            .presence_trigger_artifacts("facade-test")
            .expect("the facade test flavor's presence triggers")
            .into_iter()
            .map(|artifact| artifact.forward),
    );

    sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(vec![sqlx::migrate::Migration::new(
            FACADE_MIGRATION_VERSION,
            std::borrow::Cow::Borrowed("facade test surfaces"),
            sqlx::migrate::MigrationType::Simple,
            sqlx::AssertSqlSafe(statements.join(";\n")).into_sql_str(),
            false,
        )]),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

/// Host/example lane (`docs/09` §Migrations: timestamp versions ending
/// `00..=19`), so this fixture cannot collide with a first-party flavor.
const FACADE_MIGRATION_VERSION: i64 = 20_260_824_000_010;
