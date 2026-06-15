use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use proxima::{
    AppInfo, AuthPath, AuthzContext, CapabilitySet, CoreMcpError, FlavorApp, FlavorBundle,
    Identity, NamedMigrator, Proxima, RoleSet, ToolScope, company_owner,
};
use proxima_core::llm::{EMBEDDING_DIM, EmbeddingClient, LlmError};
use proxima_core::{
    CitationMappingPayload, CitedObjectPayload, FlavorRegistry, MemoryId, Owner, SchemaId,
    StorageError,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use uuid::Uuid;

struct EmptyApp;
struct AgentMemoryApp;

#[derive(Debug)]
struct FixedEmbedding;

#[async_trait]
impl EmbeddingClient for FixedEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(padded_embedding([1.0, 0.0, 0.0]))
    }

    fn model_id(&self) -> &'static str {
        "test-embed"
    }

    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }
}

fn padded_embedding(prefix: [f32; 3]) -> Vec<f32> {
    let mut embedding = vec![0.0; EMBEDDING_DIM];
    embedding[..prefix.len()].copy_from_slice(&prefix);
    embedding
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct TestCitedObject {
    locator: String,
}

impl CitedObjectPayload for TestCitedObject {
    const SCHEMA_ID: &'static str = "test/facade-cited-object-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "public.facade_cited_object_v1"
    }

    fn idempotency_key(&self) -> [u8; 32] {
        let mut out = [0; 32];
        let bytes = self.locator.as_bytes();
        let len = bytes.len().min(out.len());
        out[..len].copy_from_slice(&bytes[..len]);
        out
    }

    fn sidecar_insert<'t>(
        &'t self,
        tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        sidecar_row_id: uuid::Uuid,
    ) -> futures::future::BoxFuture<'t, Result<(), StorageError>> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.facade_cited_object_v1 (cited_object_id, locator)
                 VALUES ($1, $2)
                 ON CONFLICT (cited_object_id) DO NOTHING",
            )
            .bind(sidecar_row_id)
            .bind(&self.locator)
            .execute(&mut **tx)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct TestCitationMapping {
    section: String,
}

impl CitationMappingPayload for TestCitationMapping {
    const SCHEMA_ID: &'static str = "test/facade-citation-mapping-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        Some("public.facade_citation_mapping_v1")
    }

    fn cited_object_schema() -> SchemaId {
        TestCitedObject::schema_id()
    }

    fn sidecar_insert<'t>(
        &'t self,
        tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        sidecar_row_id: uuid::Uuid,
    ) -> futures::future::BoxFuture<'t, Result<(), StorageError>> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.facade_citation_mapping_v1 (citation_mapping_id, section)
                 VALUES ($1, $2)
                 ON CONFLICT (citation_mapping_id) DO NOTHING",
            )
            .bind(sidecar_row_id)
            .bind(&self.section)
            .execute(&mut **tx)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl FlavorBundle for EmptyApp {
    fn register(_registry: &mut FlavorRegistry) {}

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for EmptyApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "core-mcp-test",
            title: "Core MCP Test",
            version: "1",
        }
    }
}

impl FlavorBundle for AgentMemoryApp {
    fn register(registry: &mut FlavorRegistry) {
        registry.add_cited_object_schema::<TestCitedObject>();
        registry.add_citation_mapping_schema::<TestCitationMapping>();
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for AgentMemoryApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "agent-memory-core-mcp-test",
            title: "Agent Memory Core MCP Test",
            version: "1",
        }
    }
}

fn host_authz(owner: &Owner, tool_scope: ToolScope) -> AuthzContext {
    let accessible_principals = HashSet::from([owner.principal.clone()]);
    AuthzContext {
        identity: Identity {
            principal: owner.principal.clone(),
            org_id: owner.org_id,
            accessible_principals,
            expires_at: None,
            auth_epoch: 0,
        },
        capabilities: CapabilitySet {
            tool_scope,
            roles: RoleSet {
                graph_read: true,
                graph_write: true,
                source_ingest: true,
                admin: false,
            },
        },
        auth_path: AuthPath::HostBearer,
    }
}

#[tokio::test]
async fn facade_lists_and_dispatches_core_mcp_tools() {
    let db_name = unique_db_name("proxima_core_mcp");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(db_url)
            .owner(owner.clone())
            .build()
            .await?;
        let tools = built.core_mcp_tools();

        let listed = tools.list_core_tools();
        let list_personalities = listed
            .iter()
            .find(|tool| tool.name == "core/list_personalities")
            .expect("core/list_personalities registered");
        assert!(!listed.is_empty(), "core tool registry is non-empty");
        assert!(
            list_personalities
                .args_schema
                .as_object()
                .is_some_and(|object| !object.is_empty()),
            "known core tool has a non-empty args schema"
        );

        let output = tools
            .call_core_tool(
                host_authz(&owner, ToolScope::All),
                owner.clone(),
                Some("test-model".to_string()),
                "core/list_personalities",
                serde_json::json!({}),
            )
            .await?;
        assert!(
            output.get("personalities").is_some(),
            "read tool returns its JSON payload"
        );

        let denied = tools
            .call_core_tool(
                host_authz(&owner, ToolScope::Palette(Vec::new())),
                owner.clone(),
                None,
                "core/list_personalities",
                serde_json::json!({}),
            )
            .await;
        assert!(matches!(denied, Err(CoreMcpError::NotAuthorized(tool)) if tool == "core/list_personalities"));

        let unknown = tools
            .call_core_tool(
                host_authz(&owner, ToolScope::All),
                owner.clone(),
                None,
                "core/not_a_tool",
                serde_json::json!({}),
            )
            .await;
        assert!(matches!(unknown, Err(CoreMcpError::NotFound(tool)) if tool == "core/not_a_tool"));

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("core MCP facade integration test failed");
}

#[tokio::test]
async fn facade_core_search_memories_finds_remembered_fact_lexical_and_semantic() {
    let db_name = unique_db_name("proxima_core_search_mcp");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(db_url)
            .owner(owner.clone())
            .embed_client(Arc::new(FixedEmbedding))
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        let tool_names: HashSet<_> = tools
            .list_core_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert!(tool_names.contains("core/search_memories"));
        assert!(tool_names.contains("core/fetch_memory"));
        assert!(tool_names.contains("core/facts_citing_object"));
        assert!(tool_names.contains("core/citation_of_fact"));
        let substrate_listing = tools
            .call_core_tool(
                authz.clone(),
                owner.clone(),
                Some("test-model".to_string()),
                "core/list_substrate_tools",
                serde_json::json!({}),
            )
            .await?;
        let substrate_tool_ids: HashSet<_> = substrate_listing["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(|tool| tool["tool_id"].as_str())
            .collect();
        assert!(substrate_tool_ids.contains("core/search_memories"));
        assert!(substrate_tool_ids.contains("core/fetch_memory"));
        assert!(substrate_tool_ids.contains("core/facts_citing_object"));
        assert!(substrate_tool_ids.contains("core/citation_of_fact"));

        let remembered = tools
            .call_core_tool(
                authz.clone(),
                owner.clone(),
                Some("test-model".to_string()),
                "core/remember",
                serde_json::json!({
                    "title": "Retrieval surface",
                    "body": "hybrid substrate keyword needle",
                    "tags": ["retrieval"],
                    "idempotency_key": "facade-core-search-memory"
                }),
            )
            .await?;
        let memory = remembered["handle"].as_str().expect("remembered handle");
        ensure_fact_embedding_for_handle(&built.engine, &owner, memory).await?;

        let lexical = tools
            .search_memories(
                authz.clone(),
                owner.clone(),
                Some("test-model".to_string()),
                serde_json::json!({
                    "query": "keyword needle",
                    "mode": "lexical",
                    "kind": "Fact",
                    "limit": 5
                }),
            )
            .await?;
        assert_eq!(lexical["memories"][0]["memory"], memory);

        let semantic = tools
            .search_memories(
                authz.clone(),
                owner.clone(),
                Some("test-model".to_string()),
                serde_json::json!({
                    "query": "unrelated semantic query",
                    "mode": "semantic",
                    "kind": "Fact",
                    "limit": 5
                }),
            )
            .await?;
        assert_eq!(semantic["memories"][0]["memory"], memory);

        let fetched = tools
            .fetch_memory(
                authz,
                owner.clone(),
                Some("test-model".to_string()),
                serde_json::json!({ "memory": memory }),
            )
            .await?;
        assert_eq!(fetched["memory"], memory);

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("core search MCP facade integration test failed");
}

#[tokio::test]
async fn facade_core_citation_readback_is_owner_scoped() {
    let db_name = unique_db_name("proxima_core_citation_mcp");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let other_owner = company_owner(Uuid::now_v7());
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(db_url)
            .owner(owner.clone())
            .embed_client(Arc::new(FixedEmbedding))
            .build()
            .await?;
        create_citation_sidecars(&built.pool).await?;
        let tools = built.core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        let remembered = tools
            .call_core_tool(
                authz.clone(),
                owner.clone(),
                Some("test-model".to_string()),
                "core/remember",
                serde_json::json!({
                    "title": "Cited retrieval",
                    "body": "Fact cites external object.",
                    "tags": ["citation"],
                    "idempotency_key": "facade-core-citation-memory",
                    "citation": {
                        "object_schema_id": TestCitedObject::SCHEMA_ID,
                        "object_schema_version": TestCitedObject::SCHEMA_VERSION,
                        "object_payload": { "locator": "artifact://facade/core-citation" },
                        "mapping_schema_id": TestCitationMapping::SCHEMA_ID,
                        "mapping_schema_version": TestCitationMapping::SCHEMA_VERSION,
                        "mapping_payload": { "section": "body" }
                    }
                }),
            )
            .await?;
        let memory = remembered["handle"].as_str().expect("remembered handle");
        let fact_id = memory
            .strip_prefix("F:")
            .expect("prefixed fact id")
            .parse::<Uuid>()?;
        let cited_object_id: Uuid = sqlx::query_scalar(
            "SELECT cm.cited_object_id
               FROM proxima_core.citation_mappings cm
              WHERE cm.memory_id = $1",
        )
        .bind(fact_id)
        .fetch_one(&built.pool)
        .await?;

        let citing = tools
            .facts_citing_object(
                authz.clone(),
                owner.clone(),
                Some("test-model".to_string()),
                serde_json::json!({ "cited_object_id": cited_object_id.to_string() }),
            )
            .await?;
        assert_eq!(citing["facts"][0]["memory"], memory);

        let citation = tools
            .citation_of_fact(
                authz.clone(),
                owner.clone(),
                Some("test-model".to_string()),
                serde_json::json!({ "fact": memory }),
            )
            .await?;
        assert_eq!(
            citation["citation"]["cited_object_id"],
            cited_object_id.to_string()
        );
        assert_eq!(
            citation["citation"]["mapping_schema_id"],
            TestCitationMapping::SCHEMA_ID
        );

        let cross_owner = tools
            .facts_citing_object(
                host_authz(&other_owner, ToolScope::All),
                other_owner,
                Some("test-model".to_string()),
                serde_json::json!({ "cited_object_id": cited_object_id.to_string() }),
            )
            .await?;
        assert!(cross_owner["facts"].as_array().expect("facts").is_empty());

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("core citation MCP facade integration test failed");
}

async fn create_citation_sidecars(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE public.facade_cited_object_v1 (
            cited_object_id uuid PRIMARY KEY,
            locator text NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE public.facade_citation_mapping_v1 (
            citation_mapping_id uuid PRIMARY KEY,
            section text NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn ensure_fact_embedding_for_handle(
    engine: &proxima_core::Engine,
    owner: &Owner,
    handle: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let memory_id = handle
        .strip_prefix("F:")
        .expect("prefixed fact id")
        .parse::<Uuid>()?;
    engine
        .ensure_fact_embedding(owner, MemoryId::new(memory_id))
        .await?;
    Ok(())
}
