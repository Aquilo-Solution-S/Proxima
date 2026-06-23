use std::collections::HashSet;
use std::sync::Arc;

use proxima::{
    AppInfo, AuthPath, AuthzContext, CapabilitySet, CoreMcpError, CoreMcpErrorKind, CoreMcpTools,
    FlavorApp, FlavorBundle, Identity, NamedMigrator, PgSidecarRegistry, Proxima, RoleSet,
    StorageError, ToolScope, company_owner,
};
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::{
    CitationMappingPayload, CitedObjectPayload, FlavorRegistry, MemoryId, Owner, SchemaId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::sidecars::{
    PgCitationMappingSidecar, PgCitedObjectSidecar, PgSidecarFuture,
};
use uuid::Uuid;

struct EmptyApp;
struct AgentMemoryApp;

fn test_embedding() -> Arc<ConstantEmbedding> {
    Arc::new(ConstantEmbedding::prefixed("test-embed", &[1.0, 0.0, 0.0]))
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
}

impl PgCitedObjectSidecar for TestCitedObject {
    fn insert_cited_object_sidecar<'t>(
        &'t self,
        tx: &'t mut sqlx::PgConnection,
        cited_object_id: Uuid,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.facade_cited_object_v1 (cited_object_id, locator)
                 VALUES ($1, $2)
                 ON CONFLICT (cited_object_id) DO NOTHING",
            )
            .bind(cited_object_id)
            .bind(&self.locator)
            .execute(tx)
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
}

impl PgCitationMappingSidecar for TestCitationMapping {
    fn insert_citation_mapping_sidecar<'t>(
        &'t self,
        tx: &'t mut sqlx::PgConnection,
        citation_mapping_id: Uuid,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.facade_citation_mapping_v1 (citation_mapping_id, section)
                 VALUES ($1, $2)",
            )
            .bind(citation_mapping_id)
            .bind(&self.section)
            .execute(tx)
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

    fn register_pg_sidecars(registry: &mut PgSidecarRegistry) {
        registry.add_cited_object::<TestCitedObject>();
        registry.add_citation_mapping::<TestCitationMapping>();
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
    let accessible_principals = HashSet::from([owner.clone()]);
    AuthzContext {
        identity: Identity {
            principal: owner.clone(),
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

async fn call_test_model_tool(
    tools: &CoreMcpTools,
    authz: AuthzContext,
    owner: Owner,
    name: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, CoreMcpError> {
    tools
        .call_core_tool(authz, owner, Some("test-model".to_string()), name, args)
        .await
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
            .find(|tool| tool.name == "core_personality")
            .expect("core_personality registered");
        assert!(!listed.is_empty(), "core tool registry is non-empty");
        assert!(
            list_personalities
                .args_schema
                .as_object()
                .is_some_and(|object| !object.is_empty()),
            "known core tool has a non-empty args schema"
        );
        let search = listed
            .iter()
            .find(|tool| tool.name == "core_search_memories")
            .expect("core/search_memories registered");
        assert_eq!(search.read_only, Some(true));
        assert_eq!(search.open_world, Some(false));
        let remember = listed
            .iter()
            .find(|tool| tool.name == "core_remember")
            .expect("core/remember registered");
        assert_eq!(remember.read_only, Some(false));
        assert_eq!(remember.destructive, Some(false));

        let palette = tools.list_core_tools_for_scope(&ToolScope::Palette(vec![
            "core_search_memories".to_string(),
        ]));
        let palette_names: HashSet<_> = palette.into_iter().map(|tool| tool.name).collect();
        assert!(palette_names.contains("core_search_memories"));
        assert!(!palette_names.contains("core_personality"));

        let output = call_test_model_tool(
            &tools,
            host_authz(&owner, ToolScope::All),
            owner.clone(),
            "core_personality",
            serde_json::json!({ "action": "list" }),
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
                "core_personality",
                serde_json::json!({ "action": "list" }),
            )
            .await;
        assert!(
            matches!(denied, Err(CoreMcpError::NotAuthorized(tool)) if tool == "core_personality")
        );

        let invalid = tools
            .call_core_tool(
                host_authz(&owner, ToolScope::All),
                owner.clone(),
                None,
                "core_get_memory",
                serde_json::json!({ "memory": "not-a-memory-id" }),
            )
            .await
            .expect_err("invalid memory handle rejected");
        assert_eq!(invalid.kind(), CoreMcpErrorKind::InvalidInput);

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
            .embed_client(test_embedding())
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        let tool_names: HashSet<_> = tools
            .list_core_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert!(tool_names.contains("core_search_memories"));
        assert!(tool_names.contains("core_get_memory"));
        assert!(tool_names.contains("core_fact"));
        let substrate_listing = tools
            .call_core_tool(
                authz.clone(),
                owner.clone(),
                Some("test-model".to_string()),
                "core_list_substrate_tools",
                serde_json::json!({}),
            )
            .await?;
        let substrate_tool_ids: HashSet<_> = substrate_listing["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(|tool| tool["tool_id"].as_str())
            .collect();
        assert!(substrate_tool_ids.contains("core_search_memories"));
        assert!(substrate_tool_ids.contains("core_get_memory"));
        assert!(substrate_tool_ids.contains("core_fact"));

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner.clone(),
            "core_remember",
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

        let lexical = call_test_model_tool(
            &tools,
            authz.clone(),
            owner.clone(),
            "core_search_memories",
            serde_json::json!({
                "query": "keyword needle",
                "mode": "lexical",
                "kind": "Fact",
                "limit": 5
            }),
        )
        .await?;
        assert_eq!(lexical["memories"][0]["memory"], memory);

        let semantic = call_test_model_tool(
            &tools,
            authz.clone(),
            owner.clone(),
            "core_search_memories",
            serde_json::json!({
                "query": "unrelated semantic query",
                "mode": "semantic",
                "kind": "Fact",
                "limit": 5
            }),
        )
        .await?;
        assert_eq!(semantic["memories"][0]["memory"], memory);

        let fetched = call_test_model_tool(
            &tools,
            authz,
            owner.clone(),
            "core_get_memory",
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
async fn facade_core_search_memories_degrades_to_lexical_without_embed_client() {
    // Finding A: a deployment with NO embedding client must not hard-fail on the
    // DEFAULT (Hybrid) search — it degrades to lexical (selfdoc's promise). Only
    // an EXPLICIT semantic search errors when embeddings are unavailable.
    let db_name = unique_db_name("proxima_core_search_degrade");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        // No .embed_client(...) — engine.embed_client() is None.
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(db_url)
            .owner(owner.clone())
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        call_test_model_tool(
            &tools,
            authz.clone(),
            owner.clone(),
            "core_remember",
            serde_json::json!({
                "title": "Degrade path",
                "body": "lexical needle without embeddings",
                "tags": ["retrieval"],
                "idempotency_key": "facade-core-search-degrade"
            }),
        )
        .await?;

        // DEFAULT mode (omitted → Hybrid) must SUCCEED, return the fact via
        // lexical ranking, and flag the degrade — not error.
        let default_search = call_test_model_tool(
            &tools,
            authz.clone(),
            owner.clone(),
            "core_search_memories",
            serde_json::json!({
                "query": "lexical needle",
                "kind": "Fact",
                "limit": 5
            }),
        )
        .await?;
        assert_eq!(
            default_search["degraded_to_lexical"].as_bool(),
            Some(true),
            "default Hybrid search must flag degrade when no embed client is configured"
        );
        assert!(
            !default_search["memories"]
                .as_array()
                .expect("memories array")
                .is_empty(),
            "degraded search must still return lexical matches"
        );

        // EXPLICIT semantic must still hard-error when embeddings are unavailable.
        let semantic = call_test_model_tool(
            &tools,
            authz,
            owner.clone(),
            "core_search_memories",
            serde_json::json!({
                "query": "lexical needle",
                "mode": "semantic",
                "kind": "Fact",
                "limit": 5
            }),
        )
        .await;
        assert!(
            semantic.is_err(),
            "explicit semantic search must error without an embedding client"
        );

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("core search degrade integration test failed");
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
            .embed_client(test_embedding())
            .build()
            .await?;
        create_citation_sidecars(&built.pool).await?;
        let tools = built.core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner.clone(),
            "core_remember",
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

        let citing = call_test_model_tool(
            &tools,
            authz.clone(),
            owner.clone(),
            "core_fact",
            serde_json::json!({ "action": "facts_citing_object", "cited_object_id": cited_object_id.to_string() }),
        )
        .await?;
        assert_eq!(citing["facts"][0]["memory"], memory);

        let citation = call_test_model_tool(
            &tools,
            authz.clone(),
            owner.clone(),
            "core_fact",
            serde_json::json!({ "action": "citation_of_fact", "fact": memory }),
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

        let cross_owner = call_test_model_tool(
            &tools,
            host_authz(&other_owner, ToolScope::All),
            other_owner,
            "core_fact",
            serde_json::json!({ "action": "facts_citing_object", "cited_object_id": cited_object_id.to_string() }),
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
