use proxima_core::storage_ports::*;
use std::collections::HashSet;
use std::sync::Arc;

use proxima::flavor::{FlavorBundle, NamedMigrator, PgSidecarRegistry};
use proxima::{
    AppInfo, AuthzContext, CoreMcpError, CoreMcpErrorKind, CoreMcpTools, CoreToolInfo, FlavorApp,
    Proxima, StorageError, ToolScope, company_owner,
};
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::{
    AuthPath, CitationMappingPayload, CitedObjectPayload, FlavorRegistry, FlavorRegistryFrozen,
    GroupId, MemoryId, Owner, OwnerRef, Relation, Role, SchemaId, UserId, all_core_resources,
    provider_safe_tool_name,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::sidecars::{
    PgCitationMappingSidecar, PgCitedObjectSidecar, PgSidecarFuture,
};
use uuid::Uuid;

type ResolvedAuthz = AuthzContext;

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
    fn register(_registry: &mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError> {
        Ok(())
    }

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
    fn register(registry: &mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError> {
        registry.try_add_cited_object_schema::<TestCitedObject>()?;
        registry.try_add_citation_mapping_schema::<TestCitationMapping>()?;
        Ok(())
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

fn host_authz(owner: &Owner, tool_scope: ToolScope) -> ResolvedAuthz {
    let authz = match *owner {
        OwnerRef::Personal(subject) => AuthzContext::for_subject(subject, AuthPath::HostBearer),
        OwnerRef::Group(_) => AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(*owner, Role::admin())],
            AuthPath::HostBearer,
        ),
        OwnerRef::World => AuthzContext::denied_for_owner(owner),
    };
    authz.with_tool_scope(tool_scope)
}

fn space_authz(subject: OwnerRef, owners: Vec<Owner>, group_role: Role) -> ResolvedAuthz {
    let OwnerRef::Personal(user) = subject else {
        panic!("space auth test subjects must be users");
    };
    let roles = owners
        .into_iter()
        .filter(|owner| matches!(owner, OwnerRef::Group(_)))
        .map(|owner| (owner, group_role));
    AuthzContext::for_subject_with_role(user, roles, AuthPath::HostBearer)
}

async fn seed_group_membership(
    pg: &PgStorage,
    space_owner: &OwnerRef,
    relation: Relation,
    subject: &OwnerRef,
) {
    let OwnerRef::Group(group) = space_owner else {
        panic!("group membership can only seed group-owned spaces");
    };
    let OwnerRef::Personal(user) = subject else {
        panic!("group membership can only seed user members");
    };
    let engine = proxima_core::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    let authz = AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(*space_owner, Role::admin())],
        AuthPath::HostBearer,
    );
    let permit = engine
        .authorize_owner_write(&authz, space_owner, proxima_core::AccessKind::Goal)
        .await
        .expect("seed group membership permit");
    pg.add_group_member(&permit, *group, *user, relation, Uuid::now_v7())
        .await
        .expect("seed group membership");
}

async fn server_issued_group_space_selector(
    tools: &CoreMcpTools,
    authz: AuthzContext,
    owner: Owner,
) -> String {
    let spaces = call_test_model_tool(
        tools,
        authz,
        owner,
        "core_memory_spaces",
        serde_json::json!({}),
    )
    .await
    .expect("core_memory_spaces succeeds");
    spaces["spaces"]
        .as_array()
        .expect("spaces is an array")
        .iter()
        .filter_map(|space| space["key"].as_str())
        .find(|key| *key != "current" && *key != "world")
        .expect("group space exists")
        .to_string()
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

async fn read_test_model_resource(
    tools: &CoreMcpTools,
    authz: AuthzContext,
    owner: Owner,
    uri: &str,
) -> Result<serde_json::Value, CoreMcpError> {
    tools
        .read_core_resource(authz, owner, Some("test-model".to_string()), uri)
        .await
}

#[tokio::test]
async fn core_memory_tools_route_by_explicit_space_grants() {
    let db_name = unique_db_name("proxima_core_memory_spaces_route");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let personal = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let shared = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(db_url.clone())
            .owner(personal)
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let pg = PgStorage::connect(&db_url).await?;
        seed_group_membership(&pg, &shared, Relation::Viewer, &personal).await;
        let authz = space_authz(
            personal,
            vec![personal, shared],
            Role::viewer(),
        );
        let shared_space = server_issued_group_space_selector(&tools, authz.clone(), personal).await;

        let spaces = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_memory_spaces",
            serde_json::json!({}),
        )
        .await?;
        let space_keys = spaces["spaces"]
            .as_array()
            .expect("spaces is an array")
            .iter()
            .map(|space| space["key"].as_str().expect("space key").to_string())
            .collect::<Vec<_>>();
        assert_eq!(space_keys.first().map(String::as_str), Some("current"));
        assert!(space_keys.contains(&shared_space));

        call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_remember",
            serde_json::json!({"space":"current","title":"private","body":"private body","tags":[]}),
        )
        .await?;

        let denied = call_test_model_tool(
            &tools,
            authz,
            personal,
            "core_remember",
            serde_json::json!({"space":shared_space,"title":"leak","body":"should deny","tags":[]}),
        )
        .await;
        assert!(denied.is_err(), "shared write must be denied");

        drop(pg);
        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("explicit memory-space routing test failed");
}

#[tokio::test]
async fn shared_space_include_body_uses_shared_owner() {
    let db_name = unique_db_name("proxima_core_memory_spaces_body");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let personal = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let shared = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(db_url)
            .owner(personal)
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let authz = space_authz(
            personal,
            vec![personal, shared],
            Role::admin(),
        );
        let shared_space = server_issued_group_space_selector(&tools, authz.clone(), personal).await;

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_remember",
            serde_json::json!({"space":shared_space.clone(),"title":"shared note","body":"shared body unique needle","tags":["shared"]}),
        )
        .await?;
        let remembered_handle = remembered["handle"].as_str().expect("handle");

        let search = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_search_memories",
            serde_json::json!({
                "query": "unique needle",
                "mode": "lexical",
                "kind": "Fact",
                "spaces": [shared_space.clone()],
                "include_body": true,
                "limit": 5
            }),
        )
        .await?;
        assert_eq!(search["memories"][0]["memory"], remembered_handle);
        assert_eq!(search["memories"][0]["space"], shared_space);
        assert!(search["memories"][0]["body"].as_str().unwrap().contains("shared body"));

        // This deployment has no embedding client, so a hybrid request
        // degrades to lexical ranking. The search verb rejects a fusion
        // weight paired with a non-hybrid mode, and the caller broke no
        // such rule — they asked for hybrid. The tool must therefore drop
        // the weight along with the semantic component it was weighting,
        // rather than forward a pairing the verb refuses.
        let degraded = call_test_model_tool(
            &tools,
            authz,
            personal,
            "core_search_memories",
            serde_json::json!({
                "query": "unique needle",
                "mode": "hybrid",
                "semantic_weight": 0.7,
                "kind": "Fact",
                "spaces": [shared_space.clone()],
                "limit": 5
            }),
        )
        .await?;
        assert_eq!(
            degraded["degraded_to_lexical"], true,
            "no embedding client means hybrid ranks lexically: {degraded}"
        );
        assert_eq!(
            degraded["memories"][0]["memory"], remembered_handle,
            "the degraded search still returns the hit: {degraded}"
        );

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("shared-space search body test failed");
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // legal Fact→A setup plus cross-space A→A assertion is intentionally end-to-end
async fn cross_space_derive_succeeds_when_sources_readable() {
    let db_name = unique_db_name("proxima_core_memory_spaces_derive");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let personal = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let shared = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(db_url)
            .owner(personal)
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let authz = space_authz(
            personal,
            vec![personal, shared],
            Role::admin(),
        );
        let shared_space = server_issued_group_space_selector(&tools, authz.clone(), personal).await;

        let personal_fact = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_remember",
            serde_json::json!({"space":"current","title":"personal fact","body":"personal source","tags":[]}),
        )
        .await?;
        let shared_fact = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_remember",
            serde_json::json!({"space":shared_space,"title":"shared fact","body":"shared source","tags":[]}),
        )
        .await?;

        let personal_abstraction = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_derive",
            serde_json::json!({
                "space": "current",
                "kind": "Abstraction",
                "title": "personal abstraction",
                "body": "personal source abstraction",
                "tags": [],
                "source_handles": [personal_fact["handle"].as_str().unwrap()],
                "model_id": "test-model"
            }),
        )
        .await?;
        let shared_abstraction = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_derive",
            serde_json::json!({
                "space": shared_space,
                "kind": "Abstraction",
                "title": "shared abstraction",
                "body": "shared source abstraction",
                "tags": [],
                "source_handles": [shared_fact["handle"].as_str().unwrap()],
                "model_id": "test-model"
            }),
        )
        .await?;

        let derived = call_test_model_tool(
            &tools,
            authz,
            personal,
            "core_derive",
            serde_json::json!({
                "space": "current",
                "kind": "Abstraction",
                "title": "cross-space pattern",
                "body": "personal and shared abstractions are readable",
                "tags": [],
                "source_handles": [personal_abstraction["handle"].as_str().unwrap(), shared_abstraction["handle"].as_str().unwrap()],
                "model_id": "test-model"
            }),
        )
        .await?;
        assert!(
            derived["handle"]
                .as_str()
                .is_some_and(|handle| handle.starts_with("A:")),
            "cross-space derive must return an Abstraction handle, got {derived}"
        );
        // Two `origin` rows, reported as a count: an edge has no handle to
        // hand back, and re-running the derivation re-asserts the same rows.
        assert_eq!(derived["edge_count"], serde_json::json!(2));

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("cross-space derive success test failed");
}

/// The facade projects the descriptor's output schema, like the MCP handler
/// and the REST document already do. A host that bound a call's result off
/// this listing had to guess its shape.
fn assert_facade_projects_output_schema(registry: &FlavorRegistryFrozen, tool: &CoreToolInfo) {
    let descriptor = registry
        .list_mcp_tools()
        .iter()
        .find(|descriptor| provider_safe_tool_name(descriptor.name) == tool.name)
        .expect("listed tool has a registered descriptor");
    assert!(
        tool.output_schema
            .as_object()
            .is_some_and(|object| !object.is_empty()),
        "{}: a known core tool has a non-empty output schema",
        tool.name
    );
    assert_eq!(tool.output_schema, descriptor.output_schema);
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
            .owner(owner)
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let tools = built.core_mcp_tools();

        let listed = tools.list_core_tools();
        assert!(!listed.is_empty(), "core tool registry is non-empty");
        let retired_personality = format!("core_{}", "personality");
        assert!(
            !listed
                .iter()
                .any(|tool| tool.name == retired_personality.as_str())
        );
        let search = listed
            .iter()
            .find(|tool| tool.name == "core_search_memories")
            .expect("core/search_memories registered");
        assert!(
            search
                .args_schema
                .as_object()
                .is_some_and(|object| !object.is_empty()),
            "known core tool has a non-empty args schema"
        );
        assert_eq!(search.read_only, Some(true));
        assert_eq!(search.open_world, Some(false));
        assert_facade_projects_output_schema(built.registry(), search);
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
        assert!(!palette_names.contains(retired_personality.as_str()));

        let admin_authz = host_authz(&owner, ToolScope::All);
        let output = call_test_model_tool(
            &tools,
            admin_authz,
            owner,
            "core_search_memories",
            serde_json::json!({
                "query": "nothing yet",
                "mode": "lexical",
                "kind": "Fact",
                "limit": 5
            }),
        )
        .await?;
        assert!(output.get("memories").is_some(), "read tool returns JSON");

        let retired = tools
            .call_core_tool(
                host_authz(&owner, ToolScope::All),
                owner,
                None,
                &retired_personality,
                serde_json::json!({ "action": "list" }),
            )
            .await;
        assert!(
            matches!(retired, Err(CoreMcpError::NotFound(tool)) if tool == retired_personality)
        );

        let invalid = tools
            .read_core_resource(
                host_authz(&owner, ToolScope::All),
                owner,
                None,
                "proxima://memory/not-a-memory-id",
            )
            .await
            .expect_err("invalid memory handle rejected");
        assert_eq!(invalid.kind(), CoreMcpErrorKind::InvalidInput);

        let unknown = tools
            .call_core_tool(
                host_authz(&owner, ToolScope::All),
                owner,
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
async fn facade_reads_core_resources_with_resource_scope() {
    let db_name = unique_db_name("proxima_core_resource_mcp");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(db_url)
            .owner(owner)
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Resource facade",
                "body": "Read through MCP resource surface.",
                "tags": ["resource"],
                "idempotency_key": "facade-core-resource-read"
            }),
        )
        .await?;
        let memory = remembered["handle"].as_str().expect("remembered handle");

        let resource_memory = read_test_model_resource(
            &tools,
            authz.clone(),
            owner,
            &format!("proxima://memory/{memory}"),
        )
        .await?;
        assert_eq!(resource_memory["memory"], memory);

        let resource_schemas =
            read_test_model_resource(&tools, authz.clone(), owner, "proxima://schemas").await?;
        assert!(
            !resource_schemas["schemas"]
                .as_array()
                .expect("schemas")
                .is_empty()
        );

        let denied = read_test_model_resource(
            &tools,
            host_authz(
                &owner,
                ToolScope::Palette(vec!["resource:schemas".to_string()]),
            ),
            owner,
            &format!("proxima://memory/{memory}"),
        )
        .await;
        assert!(
            matches!(denied, Err(CoreMcpError::NotAuthorized(scope)) if scope == "resource:memory")
        );

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("core MCP resource facade integration test failed");
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
            .owner(owner)
            .embed_client(test_embedding())
            .tool_scope(ToolScope::All)
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
        assert!(!tool_names.contains("core_get_memory"));
        assert!(!tool_names.contains("core_list_substrate_tools"));
        assert!(tool_names.contains("core_fact"));
        assert!(
            all_core_resources().any(|resource| resource.scope_key == "resource:memory"),
            "resource:memory must stay in the core resource catalog"
        );
        let resource_listing =
            read_test_model_resource(&tools, authz.clone(), owner, "proxima://tools").await?;
        let substrate_tool_ids: HashSet<_> = resource_listing["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(|tool| tool["tool_id"].as_str())
            .collect();
        assert!(substrate_tool_ids.contains("core_search_memories"));
        assert!(!substrate_tool_ids.contains("core_get_memory"));
        assert!(!substrate_tool_ids.contains("core_list_substrate_tools"));
        assert!(substrate_tool_ids.contains("core_fact"));

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
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
            owner,
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
            owner,
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

        let fetched =
            read_test_model_resource(&tools, authz, owner, &format!("proxima://memory/{memory}"))
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
            .owner(owner)
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
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
            owner,
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
            owner,
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
            .owner(owner)
            .embed_client(test_embedding())
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        create_citation_sidecars(built.pool_for_tests()).await?;
        let tools = built.core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
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
            "SELECT blob_id
               FROM proxima_core.memory
              WHERE t = $1",
        )
        .bind(fact_id)
        .fetch_one(built.pool_for_tests())
        .await?;

        let citing = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_fact",
            serde_json::json!({ "action": "facts_citing_object", "cited_object_id": cited_object_id.to_string() }),
        )
        .await?;
        assert_eq!(citing["facts"][0]["memory"], memory);

        let citation = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_fact",
            serde_json::json!({ "action": "citation_of_fact", "fact": memory }),
        )
        .await?;
        assert_eq!(
            citation["citation"]["cited_object_id"],
            cited_object_id.to_string()
        );
        assert_eq!(
            citation["citation"]["cited_object_schema_id"],
            TestCitedObject::SCHEMA_ID
        );
        assert_eq!(
            citation["citation"]["mapping_schema_id"],
            TestCitedObject::SCHEMA_ID
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

/// The authorized flavor-read facade
/// (`proxima::flavor::authorized_memory_ids` and friends) routes candidate
/// filtering through `Engine::query`, which must treat a World-owned
/// (published) memory as visible to any caller, not just its original
/// owner. Publish keeps the same `t` and moves `owner_id` to World.
/// This is the read half of the raw-PgPool boundary breach fix —
/// a flavor's own owner-equality-only candidate SQL would have hidden a
/// published memory from a non-owner caller even though it is supposed to
/// be universally readable.
#[tokio::test]
async fn facade_authorized_read_surfaces_world_published_fact_to_non_owner() {
    let db_name = unique_db_name("proxima_authorized_read_world");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let other_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let built = Proxima::<EmptyApp>::app()
            .database_url(db_url)
            .owner(owner)
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Publish candidate",
                "body": "world-visible body unique needle",
                "tags": [],
                "idempotency_key": "facade-authorized-read-world-publish"
            }),
        )
        .await?;
        let handle = remembered["handle"].as_str().expect("remembered handle");
        let memory_id = handle
            .strip_prefix("F:")
            .expect("prefixed fact handle")
            .parse::<Uuid>()?;

        built
            .engine
            .publish_to_world(&authz, proxima_core::EntityId::Memory(MemoryId::new(memory_id)))
            .await?;
        let published_owner: Uuid = sqlx::query_scalar(
            "SELECT owner_id FROM proxima_core.memory WHERE t = $1",
        )
        .bind(memory_id)
        .fetch_one(built.pool_for_tests())
        .await?;
        assert_eq!(
            published_owner,
            proxima_core::OwnerRef::World.stored_owner_id(),
            "publish keeps the same t and moves owner to World"
        );
        let still_private: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory
              WHERE t = $1 AND owner_id = $2",
        )
        .bind(memory_id)
        .bind(owner.stored_owner_id())
        .fetch_one(built.pool_for_tests())
        .await?;
        assert_eq!(still_private, 0);

        // A caller with no relationship to `owner` — not a group co-member,
        // no share, nothing — must still see the transferred t
        // through the same authorized-read helper the Code flavor calls.
        let other_authz = host_authz(&other_owner, ToolScope::All);
        let visible = proxima::flavor::authorized_memory_ids(
            &built.engine,
            &other_authz,
            other_owner,
            &[memory_id],
            proxima_core::verbs::query::EntityKind::Fact,
            None,
            10,
        )
        .await?;
        assert_eq!(
            visible,
            vec![MemoryId::new(memory_id)],
            "a World-published Fact must surface through the authorized-read facade for a non-owner caller"
        );

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("authorized-read World visibility test failed");
}

#[tokio::test]
async fn core_forget_cools_a_remembered_fact() {
    let db_name = unique_db_name("proxima_core_forget");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(db_url)
            .owner(owner)
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let listed = tools.list_core_tools();
        assert!(
            listed.iter().any(|tool| tool.name == "core_forget"),
            "served catalog must include core_forget: {listed:?}"
        );
        assert!(
            listed.iter().all(|tool| !tool.name.contains("open_batch")),
            "open_batch must stay deleted"
        );

        let authz = host_authz(&owner, ToolScope::All);
        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "forget me",
                "body": "hot row that must cool",
                "tags": []
            }),
        )
        .await?;
        let handle = remembered["handle"].as_str().expect("handle").to_string();
        let t = handle
            .strip_prefix("F:")
            .expect("prefixed fact")
            .parse::<Uuid>()?;

        let forgotten = call_test_model_tool(
            &tools,
            authz,
            owner,
            "core_forget",
            serde_json::json!({ "memory": handle }),
        )
        .await?;
        assert_eq!(forgotten["ok"], serde_json::json!(true));

        let hot: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(built.pool_for_tests())
                .await?;
        assert_eq!(hot, 0, "forget must delete the hot row");
        let cooled: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(built.pool_for_tests())
                .await?;
        assert_eq!(cooled, 1, "forget must leave the cooled stub");
        let announce: String = sqlx::query_scalar(
            "SELECT op::text FROM proxima_core.announce WHERE t = $1 ORDER BY seq DESC LIMIT 1",
        )
        .bind(t)
        .fetch_one(built.pool_for_tests())
        .await?;
        assert_eq!(announce, "forget");

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("core_forget must drive shipped forget_memory");
}
