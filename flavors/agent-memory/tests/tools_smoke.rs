use std::sync::Arc;
use std::{future::Future, pin::Pin};

use async_trait::async_trait;
use proxima_core::engine::Engine;
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolCtx, OutputMode};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{
    AuthPath, AuthzContext, CitationMappingPayload, CitedObjectPayload, EntityKind, FlavorRegistry,
    FlavorRegistryFrozen, McpToolError, MemoryId, OrgId, Owner, OwnerPrincipalKind,
    PersonalityInstanceId, Principal, SchemaId, StorageError, UserId,
};
use proxima_pg_testkit::{db_url, drop_db, unique_db_name};
use serde_json::json;

#[derive(Debug)]
struct FixedEmbedding;

type SidecarFuture<'t> = Pin<Box<dyn Future<Output = Result<(), StorageError>> + Send + 't>>;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct RememberTestCitedObject {
    artifact_id: String,
    locator: String,
}

impl CitedObjectPayload for RememberTestCitedObject {
    const SCHEMA_ID: &'static str = "test/remember-cited-object-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "public.remember_test_cited_object_v1"
    }

    fn idempotency_key(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.artifact_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.locator.as_bytes());
        *hasher.finalize().as_bytes()
    }

    fn sidecar_insert<'t>(
        &'t self,
        tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        sidecar_row_id: uuid::Uuid,
    ) -> SidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.remember_test_cited_object_v1
                    (cited_object_id, artifact_id, locator)
                 VALUES ($1, $2, $3)
                 ON CONFLICT (cited_object_id) DO NOTHING",
            )
            .bind(sidecar_row_id)
            .bind(&self.artifact_id)
            .bind(&self.locator)
            .execute(&mut **tx)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct RememberTestCitationMapping {
    section: String,
    byte_start: i32,
    byte_end: i32,
}

impl CitationMappingPayload for RememberTestCitationMapping {
    const SCHEMA_ID: &'static str = "test/remember-citation-mapping-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        Some("public.remember_test_citation_mapping_v1")
    }

    fn cited_object_schema() -> SchemaId {
        RememberTestCitedObject::schema_id()
    }

    fn sidecar_insert<'t>(
        &'t self,
        tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        sidecar_row_id: uuid::Uuid,
    ) -> SidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.remember_test_citation_mapping_v1
                    (citation_mapping_id, section, byte_start, byte_end)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (citation_mapping_id) DO NOTHING",
            )
            .bind(sidecar_row_id)
            .bind(&self.section)
            .bind(self.byte_start)
            .bind(self.byte_end)
            .execute(&mut **tx)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

#[async_trait]
impl EmbeddingClient for FixedEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![1.0, 0.0, 0.0])
    }

    fn model_id(&self) -> &'static str {
        "test-embed"
    }

    fn dim(&self) -> usize {
        3
    }
}

#[tokio::test]
async fn remember_then_search_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let pg = proxima_storage_pg::PgStorage::connect(&db_url(&db_name)).await?;
    pg.run_migrations().await?;
    proxima_agent_memory::migrator().run(pg.pool()).await?;

    let mut registry = FlavorRegistry::new();
    proxima_agent_memory::register(&mut registry);
    let frozen = Arc::new(registry.freeze());
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());
    let author = author_ctx();

    let remembered = call_tool(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "proxima-agent-memory/proxima_remember",
        json!({
            "title": "Atlas edges",
            "body": "Edges must be loaded from the visible node set.",
            "tags": ["atlas"],
            "idempotency_key": "tools-smoke-atlas-edge-loading"
        }),
    )
    .await?;
    assert!(
        remembered["handle"]
            .as_str()
            .expect("handle")
            .starts_with('F'),
        "remember mints a Fact handle, got: {remembered}"
    );

    let searched = call_tool(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author,
        "proxima-agent-memory/proxima_search_graph",
        json!({"query": "atlas edges", "limit": 5}),
    )
    .await?;
    assert_eq!(
        searched["matches"][0]["handle"], remembered["handle"],
        "search should reuse the session handle"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "PG integration fixture validates cited and uncited remember rows in one transaction shape"
)]
async fn remember_cited_and_uncited_persist_personality_and_citation_rows()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let pg = proxima_storage_pg::PgStorage::connect(&db_url(&db_name)).await?;
    pg.run_migrations().await?;
    proxima_agent_memory::migrator().run(pg.pool()).await?;
    create_remember_citation_sidecars(pg.pool()).await?;

    let frozen = registry_with_remember_test_citation();
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());
    let personality = PersonalityInstanceId::new(uuid::Uuid::now_v7());
    let author = author_ctx().with_personality(personality);

    let cited = call_tool(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "proxima-agent-memory/proxima_remember",
        json!({
            "title": "Cited remembered note",
            "body": "This note cites a typed test artifact.",
            "tags": ["citation"],
            "idempotency_key": "remember-cited-test-note",
            "citation": {
                "object_schema_id": RememberTestCitedObject::SCHEMA_ID,
                "object_schema_version": RememberTestCitedObject::SCHEMA_VERSION,
                "object_payload": {
                    "artifact_id": "sharepoint-test-item",
                    "locator": "https://example.invalid/sites/test/doc"
                },
                "mapping_schema_id": RememberTestCitationMapping::SCHEMA_ID,
                "mapping_schema_version": RememberTestCitationMapping::SCHEMA_VERSION,
                "mapping_payload": {
                    "section": "body",
                    "byte_start": 0,
                    "byte_end": 12
                }
            }
        }),
    )
    .await?;
    let uncited = call_tool(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author,
        "proxima-agent-memory/proxima_remember",
        json!({
            "title": "Uncited remembered note",
            "body": "This note has no citation.",
            "tags": ["citation"],
            "idempotency_key": "remember-uncited-test-note"
        }),
    )
    .await?;

    let cited_memory_id = handles
        .resolve_memory(cited["handle"].as_str().expect("cited handle"))?
        .into_inner();
    let uncited_memory_id = handles
        .resolve_memory(uncited["handle"].as_str().expect("uncited handle"))?
        .into_inner();

    let cited_row: (Option<uuid::Uuid>, uuid::Uuid) = sqlx::query_as(
        "SELECT citation_mapping_id, personality_instance_id
         FROM proxima_core.memories
         WHERE memory_id = $1",
    )
    .bind(cited_memory_id)
    .fetch_one(pg.pool())
    .await?;
    assert!(
        cited_row.0.is_some(),
        "cited remember must attach citation_mapping_id"
    );
    assert_eq!(cited_row.1, personality.into_inner());

    let uncited_row: (Option<uuid::Uuid>, uuid::Uuid) = sqlx::query_as(
        "SELECT citation_mapping_id, personality_instance_id
         FROM proxima_core.memories
         WHERE memory_id = $1",
    )
    .bind(uncited_memory_id)
    .fetch_one(pg.pool())
    .await?;
    assert!(
        uncited_row.0.is_none(),
        "uncited remember must remain a plain Fact"
    );
    assert_eq!(uncited_row.1, personality.into_inner());

    let cited_sidecar_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_agent_memory.agent_note_v1 WHERE memory_id = $1",
    )
    .bind(cited_memory_id)
    .fetch_one(pg.pool())
    .await?;
    let uncited_sidecar_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_agent_memory.agent_note_v1 WHERE memory_id = $1",
    )
    .bind(uncited_memory_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(cited_sidecar_count, 1);
    assert_eq!(uncited_sidecar_count, 1);
    assert_eq!(
        count_rows(pg.pool(), "proxima_core.cited_objects").await?,
        1
    );
    assert_eq!(
        count_rows(pg.pool(), "public.remember_test_cited_object_v1").await?,
        1
    );
    assert_eq!(
        count_rows(pg.pool(), "proxima_core.citation_mappings").await?,
        1
    );
    assert_eq!(
        count_rows(pg.pool(), "public.remember_test_citation_mapping_v1").await?,
        1
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn link_rejects_direct_fact_to_fact_interpretation() -> Result<(), Box<dyn std::error::Error>>
{
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let pg = proxima_storage_pg::PgStorage::connect(&db_url(&db_name)).await?;
    pg.run_migrations().await?;
    proxima_agent_memory::migrator().run(pg.pool()).await?;

    let mut registry = FlavorRegistry::new();
    proxima_agent_memory::register(&mut registry);
    let frozen = Arc::new(registry.freeze());
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());
    let author = author_ctx();

    let first = call_tool(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "proxima-agent-memory/proxima_remember",
        json!({
            "title": "First fact",
            "body": "A remembered observation.",
            "idempotency_key": "link-fact-a"
        }),
    )
    .await?;
    let second = call_tool(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "proxima-agent-memory/proxima_remember",
        json!({
            "title": "Second fact",
            "body": "Another remembered observation.",
            "idempotency_key": "link-fact-b"
        }),
    )
    .await?;

    let link = call_tool(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author,
        "proxima-agent-memory/proxima_link",
        json!({
            "source": first["handle"],
            "target": second["handle"],
            "reason": "semantic direct Fact-to-Fact interpretation"
        }),
    )
    .await;

    match link {
        Err(McpToolError::Storage(proxima_core::StorageError::ConstraintViolation(msg))) => {
            assert!(msg.contains("source kind Fact"));
        }
        other => panic!("expected central relation-mask rejection, got {other:?}"),
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn search_graph_hybrid_returns_embedding_only_match() -> Result<(), Box<dyn std::error::Error>>
{
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let pg = proxima_storage_pg::PgStorage::connect(&db_url(&db_name)).await?;
    pg.run_migrations().await?;
    proxima_agent_memory::migrator().run(pg.pool()).await?;

    let mut registry = FlavorRegistry::new();
    proxima_agent_memory::register(&mut registry);
    let frozen_inner = registry.freeze();
    let frozen = Arc::new(frozen_inner.clone());
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());
    let author = author_ctx();

    let remembered = call_tool(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "proxima-agent-memory/proxima_remember",
        json!({
            "title": "Operational note",
            "body": "This body deliberately omits the query token.",
            "tags": ["hybrid"],
            "idempotency_key": "tools-smoke-hybrid-embedding-only"
        }),
    )
    .await?;
    let memory_id = handles
        .resolve_memory(remembered["handle"].as_str().expect("handle"))?
        .into_inner();
    insert_embedding(pg.pool(), &owner, memory_id).await?;

    let lexical = call_tool(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "proxima-agent-memory/proxima_search_graph",
        json!({"query": "galaxy", "limit": 5}),
    )
    .await?;
    assert!(lexical["matches"].as_array().expect("matches").is_empty());

    let engine = Arc::new(
        Engine::new(frozen_inner, MemoryStore::new())
            .with_storage(pg.clone().into_handle())
            .with_embed(Arc::new(FixedEmbedding)),
    );
    let hybrid = call_tool_with_engine(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author,
        Some(engine),
        "proxima-agent-memory/proxima_search_graph",
        json!({"query": "galaxy", "mode": "hybrid", "limit": 5}),
    )
    .await?;
    assert_eq!(hybrid["matches"][0]["handle"], remembered["handle"]);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn prefixed_search_and_open_emit_author_and_keep_company_shared_visibility()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let pg = proxima_storage_pg::PgStorage::connect(&db_url(&db_name)).await?;
    pg.run_migrations().await?;
    proxima_agent_memory::migrator().run(pg.pool()).await?;

    let mut registry = FlavorRegistry::new();
    proxima_agent_memory::register(&mut registry);
    let frozen = Arc::new(registry.freeze());
    let owner = nil_owner();
    let personality_a = PersonalityInstanceId::new(uuid::Uuid::now_v7());
    let personality_b_root = MemoryId::new(uuid::Uuid::now_v7());
    // Author a Fact AS personality_a via the real remember path (T3 stamps
    // ctx.author.personality_instance_id into memories.personality_instance_id).
    let authored_handle = call_tool_prefixed(
        pg.pool(),
        &owner,
        &frozen,
        author_ctx().with_personality(personality_a),
        "proxima-agent-memory/proxima_remember",
        json!({
            "title": "Company shared author",
            "body": "Company shared alpha needle.",
            "tags": ["company-shared"],
            "idempotency_key": "company-shared-alpha"
        }),
    )
    .await?["handle"]
        .as_str()
        .expect("remember handle")
        .to_string();
    let nil_handle = call_tool_prefixed(
        pg.pool(),
        &owner,
        &frozen,
        author_ctx(),
        "proxima-agent-memory/proxima_remember",
        json!({
            "title": "Nil author",
            "body": "Company shared beta needle.",
            "tags": ["company-shared"],
            "idempotency_key": "company-shared-beta"
        }),
    )
    .await?["handle"]
        .as_str()
        .expect("remember handle")
        .to_string();

    let search = call_tool_prefixed(
        pg.pool(),
        &owner,
        &frozen,
        author_ctx().with_self_perspective(personality_b_root),
        "proxima-agent-memory/proxima_search_graph",
        json!({"query": "alpha needle", "limit": 5}),
    )
    .await?;
    assert_eq!(search["matches"][0]["handle"], authored_handle);
    assert_eq!(
        search["matches"][0]["authoring_personality_instance_id"],
        format!("I:{}", personality_a.into_inner())
    );

    let opened = call_tool_prefixed(
        pg.pool(),
        &owner,
        &frozen,
        author_ctx().with_self_perspective(personality_b_root),
        "proxima-agent-memory/proxima_open",
        json!({"handle": authored_handle.clone()}),
    )
    .await?;
    assert_eq!(
        opened["authoring_personality_instance_id"],
        format!("I:{}", personality_a.into_inner())
    );

    let nil_opened = call_tool_prefixed(
        pg.pool(),
        &owner,
        &frozen,
        author_ctx().with_self_perspective(personality_b_root),
        "proxima-agent-memory/proxima_open",
        json!({"handle": nil_handle}),
    )
    .await?;
    assert!(
        nil_opened
            .get("authoring_personality_instance_id")
            .is_none()
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "two-axis idempotency fixture: owner and kind dimensions in one linear script"
)]
async fn derive_scopes_idempotency_by_owner_and_kind() -> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let pg = proxima_storage_pg::PgStorage::connect(&db_url(&db_name)).await?;
    pg.run_migrations().await?;
    proxima_agent_memory::migrator().run(pg.pool()).await?;

    let mut registry = FlavorRegistry::new();
    proxima_agent_memory::register(&mut registry);
    let frozen = Arc::new(registry.freeze());
    let frozen_b = frozen.clone();
    let owner_a = nil_owner();
    let owner_b = Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::from_u128(1))),
        org_id: OrgId::new(uuid::Uuid::nil()),
    };

    let shared_args = || {
        json!({
            "kind": "Abstraction",
            "title": "Shared idempotency",
            "body": "Same body, same key, different owner.",
            "model_id": "codex-test",
            "idempotency_key": "shared-key-collision",
        })
    };

    let a = call_tool(
        pg.pool(),
        &owner_a,
        &Arc::new(HandleTable::new()),
        &frozen,
        author_ctx(),
        "proxima-agent-memory/proxima_derive",
        shared_args(),
    )
    .await?;
    let b = call_tool(
        pg.pool(),
        &owner_b,
        &Arc::new(HandleTable::new()),
        &frozen,
        author_ctx(),
        "proxima-agent-memory/proxima_derive",
        shared_args(),
    )
    .await?;

    let distinct_owner_memories: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT memory_id) FROM proxima_agent_memory.agent_derivation_v1
         WHERE idempotency_key = 'shared-key-collision'",
    )
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(
        distinct_owner_memories, 2,
        "owner-a and owner-b must not collide"
    );
    assert_eq!(a["idempotent_replay"], json!(false));
    assert_eq!(b["idempotent_replay"], json!(false));

    let abstraction = call_tool(
        pg.pool(),
        &owner_a,
        &Arc::new(HandleTable::new()),
        &frozen,
        author_ctx(),
        "proxima-agent-memory/proxima_derive",
        json!({
            "kind": "Abstraction",
            "title": "Same key, A vs P",
            "body": "kind dimension test.",
            "model_id": "codex-test",
            "idempotency_key": "kind-key-collision",
        }),
    )
    .await?;
    let perspective = call_tool(
        pg.pool(),
        &owner_a,
        &Arc::new(HandleTable::new()),
        &frozen_b,
        author_ctx(),
        "proxima-agent-memory/proxima_derive",
        json!({
            "kind": "Perspective",
            "title": "Same key, A vs P",
            "body": "kind dimension test.",
            "model_id": "codex-test",
            "idempotency_key": "kind-key-collision",
        }),
    )
    .await?;
    let distinct_kind_memories: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT memory_id) FROM proxima_agent_memory.agent_derivation_v1
         WHERE idempotency_key = 'kind-key-collision'",
    )
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(
        distinct_kind_memories, 2,
        "kind dimension must split memory_id"
    );
    assert_eq!(abstraction["idempotent_replay"], json!(false));
    assert_eq!(perspective["idempotent_replay"], json!(false));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn derive_rejects_upward_provenance() -> Result<(), Box<dyn std::error::Error>> {
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let pg = proxima_storage_pg::PgStorage::connect(&db_url(&db_name)).await?;
    pg.run_migrations().await?;
    proxima_agent_memory::migrator().run(pg.pool()).await?;

    let mut registry = FlavorRegistry::new();
    proxima_agent_memory::register(&mut registry);
    let frozen = Arc::new(registry.freeze());
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());
    let author = author_ctx();

    let perspective = call_tool(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "proxima-agent-memory/proxima_derive",
        json!({
            "kind": "Perspective",
            "title": "Top-layer perspective",
            "body": "A perspective with no sources, used as a layering pivot.",
            "model_id": "codex-test",
            "idempotency_key": "derive-layer-test-perspective"
        }),
    )
    .await?;
    let perspective_handle = perspective["handle"].as_str().expect("handle").to_string();

    let upward = call_tool(
        pg.pool(),
        &owner,
        &handles,
        &frozen,
        author,
        "proxima-agent-memory/proxima_derive",
        json!({
            "kind": "Abstraction",
            "title": "Should fail",
            "body": "Trying to derive an Abstraction from a Perspective is upward.",
            "model_id": "codex-test",
            "source_handles": [perspective_handle],
            "idempotency_key": "derive-layer-test-upward"
        }),
    )
    .await;

    match upward {
        Err(McpToolError::LayeringViolation(_)) => {}
        other => panic!("expected LayeringViolation, got {other:?}"),
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

async fn call_tool(
    pool: &sqlx::PgPool,
    owner: &Owner,
    handles: &Arc<HandleTable>,
    registry: &Arc<proxima_core::FlavorRegistryFrozen>,
    author: McpAuthorContext,
    name: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, proxima_core::McpToolError> {
    call_tool_with_engine(
        pool,
        owner,
        handles,
        registry,
        author,
        Some(engine_for_registry(registry)),
        name,
        args,
    )
    .await
}

async fn call_tool_prefixed(
    pool: &sqlx::PgPool,
    owner: &Owner,
    registry: &Arc<proxima_core::FlavorRegistryFrozen>,
    author: McpAuthorContext,
    name: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, proxima_core::McpToolError> {
    let descriptor = registry
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == name)
        .expect("registered tool");
    let caller_self_perspective = author.caller_self_perspective;
    (descriptor.call)(
        McpToolCtx {
            pool: pool.clone(),
            owner: owner.clone(),
            authz: AuthzContext::single_owner(owner, AuthPath::System),
            handles: None,
            mode: OutputMode::PrefixedIds,
            registry: registry.clone(),
            author,
            caller_self_perspective,
            master_token_id: None,
            engine: Some(engine_for_registry(registry)),
        },
        args,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn call_tool_with_engine(
    pool: &sqlx::PgPool,
    owner: &Owner,
    handles: &Arc<HandleTable>,
    registry: &Arc<proxima_core::FlavorRegistryFrozen>,
    author: McpAuthorContext,
    engine: Option<Arc<Engine>>,
    name: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, proxima_core::McpToolError> {
    let descriptor = registry
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == name)
        .expect("registered tool");
    (descriptor.call)(
        McpToolCtx {
            pool: pool.clone(),
            owner: owner.clone(),
            authz: AuthzContext::single_owner(owner, AuthPath::System),
            handles: Some(handles.clone()),
            mode: OutputMode::Handles,
            registry: registry.clone(),
            author,
            caller_self_perspective: None,
            master_token_id: None,
            engine,
        },
        args,
    )
    .await
}

async fn insert_embedding(
    pool: &sqlx::PgPool,
    owner: &Owner,
    memory_id: uuid::Uuid,
) -> Result<(), sqlx::Error> {
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_kind, entity_id, embedding_version, model_id, vec, dim,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1, $2, 1, 'test-embed', $3, 3, $4, $5, $6)",
    )
    .bind(EntityKind::Fact)
    .bind(memory_id)
    .bind(vec![1.0f32, 0.0, 0.0])
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .execute(pool)
    .await
    .map(|_| ())
}

fn nil_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::nil())),
        org_id: OrgId::new(uuid::Uuid::nil()),
    }
}

fn author_ctx() -> McpAuthorContext {
    McpAuthorContext {
        model_id: "codex-test".into(),
        client_name: "codex".into(),
        client_version: "1".into(),
        personality_instance_id: None,
        caller_self_perspective: None,
    }
}

trait AuthorCtxExt {
    fn with_self_perspective(self, memory_id: MemoryId) -> Self;
    fn with_personality(self, personality: PersonalityInstanceId) -> Self;
}

impl AuthorCtxExt for McpAuthorContext {
    fn with_self_perspective(mut self, memory_id: MemoryId) -> Self {
        self.caller_self_perspective = Some(memory_id);
        self
    }
    fn with_personality(mut self, personality: PersonalityInstanceId) -> Self {
        self.personality_instance_id = Some(personality);
        self
    }
}

async fn create_db() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let db_name = unique_db_name("proxima_test");
    proxima_pg_testkit::create_db(&db_name)
        .await
        .expect("PG required for tests but admin connect failed");
    Ok(Some(db_name))
}

fn engine_for_registry(registry: &Arc<FlavorRegistryFrozen>) -> Arc<Engine> {
    Arc::new(Engine::new((**registry).clone(), MemoryStore::new()))
}

fn registry_with_remember_test_citation() -> Arc<FlavorRegistryFrozen> {
    let mut registry = FlavorRegistry::new();
    proxima_agent_memory::register(&mut registry);
    registry.add_cited_object_schema::<RememberTestCitedObject>();
    registry.add_citation_mapping_schema::<RememberTestCitationMapping>();
    Arc::new(registry.freeze())
}

async fn create_remember_citation_sidecars(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE public.remember_test_cited_object_v1 (
            cited_object_id uuid PRIMARY KEY,
            artifact_id text NOT NULL,
            locator text NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE public.remember_test_citation_mapping_v1 (
            citation_mapping_id uuid PRIMARY KEY,
            section text NOT NULL,
            byte_start integer NOT NULL,
            byte_end integer NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn count_rows(pool: &sqlx::PgPool, table: &str) -> Result<i64, sqlx::Error> {
    let sql = format!("SELECT count(*) FROM {table}");
    sqlx::query_scalar(&sql).fetch_one(pool).await
}
