use proxima_core::storage_ports::*;
use std::collections::HashSet;
use std::sync::Arc;

use proxima::flavor::{FlavorBundle, NamedMigrator, PgSidecarRegistry};
use proxima::{
    AppInfo, AuthzContext, CoreMcpError, CoreMcpErrorKind, CoreMcpTools, CoreToolInfo, FlavorApp,
    FlavorServiceError, FlavorServices, Proxima, StorageError, ToolScope, company_owner,
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

#[derive(Debug)]
struct BootMark;

struct MarkApp;

impl FlavorBundle for MarkApp {
    fn register(_: &mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for MarkApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "mark-app",
            title: "Mark App",
            version: "1",
        }
    }

    fn services(_: &proxima::AppContext) -> Result<FlavorServices, FlavorServiceError> {
        let mut services = FlavorServices::new();
        services.try_insert(BootMark)?;
        Ok(services)
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
        .find(|key| *key != "current")
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
#[allow(clippy::too_many_lines)]
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
        let recall = listed
            .iter()
            .find(|tool| tool.name == "core_recall")
            .expect("core_recall registered");
        assert_eq!(recall.read_only, Some(true));
        assert_facade_projects_output_schema(built.registry(), recall);
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

        let palette_denied = tools
            .call_core_tool(
                host_authz(
                    &owner,
                    ToolScope::Palette(vec!["core_search_memories".to_string()]),
                ),
                owner,
                None,
                "core_remember",
                serde_json::json!({
                    "title": "denied",
                    "body": "palette must be a call gate",
                    "idempotency_key": "palette-denied-remember"
                }),
            )
            .await;
        assert!(
            matches!(palette_denied, Err(CoreMcpError::NotAuthorized(ref tool)) if tool == "core_remember"),
            "palette must deny the call, not only hide the tool: {palette_denied:?}"
        );

        let empty_request = built
            .core_mcp_tools_with_request_services(FlavorServices::default())
            .expect("empty request bag merges");
        let listed_after_merge = empty_request.list_core_tools();
        assert!(
            listed_after_merge
                .iter()
                .any(|tool| tool.name == "core_search_memories")
        );

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
#[allow(clippy::too_many_lines)]
async fn facade_core_recall_returns_cue_packet_and_rejects_empty_cue() {
    let db_name = unique_db_name("proxima_core_recall_mcp");
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

        let empty = tools
            .call_core_tool(
                authz.clone(),
                owner,
                Some("test-model".to_string()),
                "core_recall",
                serde_json::json!({}),
            )
            .await;
        assert!(
            empty.is_err(),
            "empty cue must fail: Self is not parameterless"
        );

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Cue fact",
                "body": "recall-subject-needle unique observation",
                "tags": ["recall"],
                "idempotency_key": "facade-core-recall-fact"
            }),
        )
        .await?;
        let fact = remembered["handle"].as_str().expect("fact handle");
        let fact_t = fact
            .strip_prefix("F:")
            .expect("fact prefix")
            .parse::<Uuid>()?;
        let stored_sketch: String =
            sqlx::query_scalar("SELECT text FROM proxima_core.sketch WHERE t = $1")
                .bind(fact_t)
                .fetch_one(built.pool_for_tests())
                .await?;
        assert_eq!(stored_sketch, "Cue fact");

        let derived = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_derive",
            serde_json::json!({
                "kind": "Abstraction",
                "title": "Recall pattern",
                "body": "recall-subject-needle repeats as a pattern",
                "tags": ["recall"],
                "source_handles": [fact],
                "model_id": "test-model",
                "idempotency_key": "facade-core-recall-abstraction"
            }),
        )
        .await?;
        let abstraction = derived["handle"].as_str().expect("abstraction handle");

        let interpreted = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_interpret",
            serde_json::json!({
                "claim": "the recall-subject-needle observation is a stance I hold",
                "confidence": 80,
                "subjects": [fact]
            }),
        )
        .await?;
        let perspective = interpreted["handle"].as_str().expect("perspective handle");

        let goal = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_goal",
            serde_json::json!({
                "action": "set",
                "schema_id": "core/simple-text-v1",
                "title": "Act on the cue stance",
                "text": "Pursue the interpretation",
                "body": {},
                "evidence": [abstraction],
                "target_perspective": perspective,
                "idempotency_key": "facade-core-recall-goal"
            }),
        )
        .await?;
        let goal_handle = goal["handle"].as_str().expect("goal handle");

        let packet = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_recall",
            serde_json::json!({
                "subjects": [fact],
                "kind": "Perspective",
                "limit": 2
            }),
        )
        .await?;
        let sketches = packet["sketches"]
            .as_array()
            .expect("sketches")
            .iter()
            .map(|row| {
                (
                    row["handle"].as_str().unwrap_or_default().to_string(),
                    row["kind"].as_str().unwrap_or_default().to_string(),
                    row["reason"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect::<Vec<_>>();
        assert!(
            packet["sketches"]
                .as_array()
                .expect("sketches")
                .iter()
                .any(|row| {
                    row["handle"].as_str() == Some(perspective)
                        && row["kind"].as_str() == Some("Perspective")
                        && row["reason"].as_str() == Some("cue_touch")
                        && row["sketch"]
                            .as_str()
                            .is_some_and(|sketch| sketch.contains("stance I hold"))
                }),
            "cue-touch Perspective missing or sketch empty: {packet}"
        );
        assert!(
            sketches
                .iter()
                .any(|(handle, kind, reason)| handle == goal_handle
                    && kind == "Goal"
                    && reason == "assigned_goal"),
            "assigned Active Goal missing from Self packet: {packet}"
        );

        let goal_t = goal_handle
            .strip_prefix("G:")
            .expect("goal prefix")
            .parse::<Uuid>()?;
        sqlx::query("DELETE FROM proxima_core.sketch WHERE t = $1")
            .bind(goal_t)
            .execute(built.pool_for_tests())
            .await?;
        let without_goal = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_recall",
            serde_json::json!({
                "subjects": [fact],
                "kind": "Perspective",
                "limit": 8
            }),
        )
        .await?;
        assert!(
            without_goal["sketches"]
                .as_array()
                .expect("sketches")
                .iter()
                .all(|row| row["handle"].as_str() != Some(goal_handle)),
            "assigned Goal without a persisted sketch must be omitted: {without_goal}"
        );

        let by_question = call_test_model_tool(
            &tools,
            authz,
            owner,
            "core_recall",
            serde_json::json!({
                "question": "recall-subject-needle",
                "limit": 16
            }),
        )
        .await?;
        let question_hits = by_question["sketches"]
            .as_array()
            .expect("sketches")
            .iter()
            .filter_map(|row| row["handle"].as_str())
            .collect::<Vec<_>>();
        assert!(
            question_hits.contains(&fact),
            "question cue must find the Fact: {by_question}"
        );

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("core recall MCP facade integration test failed");
}

/// The `PayloadOnly` arm, end to end: an interpretation's ancestors are its
/// SUBJECTS.
///
/// `core/interpretation-v1` writes `derived_from: &[]` on both of its ingest
/// paths — an interpretation is not made FROM its subjects, it is made ABOUT
/// them — so `memory.origins` is empty and a walk that expanded `origins[]`
/// and nothing else would stop dead at every interpretation. The subjects
/// live in `interpretation_v1.subject_memory_ids`, which is exactly what
/// `Provenance::PayloadOnly { subject_columns }` declares.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn facade_core_think_reaches_an_interpretations_subject_through_its_payload() {
    let db_name = unique_db_name("proxima_core_think_payload");
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

        let subject = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Interpretation subject",
                "body": "the thing being interpreted",
                "tags": ["provenance"],
                "idempotency_key": "facade-provenance-subject"
            }),
        )
        .await?;
        let subject_handle = subject["handle"].as_str().expect("subject").to_owned();

        let interpretation = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_interpret",
            serde_json::json!({
                "claim": "this is what the subject means",
                "confidence": 90,
                "subjects": [subject_handle.clone()],
                "model_id": "test-model"
            }),
        )
        .await?;
        let interpretation_handle = interpretation["handle"]
            .as_str()
            .expect("interpretation")
            .to_owned();

        // The premise, asked of the row itself: the interpretation carries
        // NO origins, and the subject is in the payload column the
        // declaration names. If either becomes false the declaration is
        // wrong, not this test.
        let interpretation_t = interpretation_handle
            .split_once(':')
            .map_or(interpretation_handle.as_str(), |(_, id)| id)
            .parse::<Uuid>()?;
        let origins: i64 = sqlx::query_scalar(
            "SELECT cardinality(origins)::bigint FROM proxima_core.memory WHERE t = $1",
        )
        .bind(interpretation_t)
        .fetch_one(built.pool_for_tests())
        .await?;
        assert_eq!(
            origins, 0,
            "an interpretation is made ABOUT its subjects, not FROM them"
        );
        let subjects: i64 = sqlx::query_scalar(
            "SELECT cardinality(subject_memory_ids)::bigint
               FROM proxima_core.interpretation_v1 WHERE t = $1",
        )
        .bind(interpretation_t)
        .fetch_one(built.pool_for_tests())
        .await?;
        assert_eq!(subjects, 1, "the subject lives in the declared column");

        let page = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_think",
            serde_json::json!({
                "seeds": [interpretation_handle],
                "direction": "ancestors",
                "depth": 2,
                "limit": 16
            }),
        )
        .await?;
        let handles = page["visits"]
            .as_array()
            .expect("visits")
            .iter()
            .filter_map(|visit| visit["handle"].as_str())
            .collect::<Vec<_>>();
        assert!(
            handles.contains(&subject_handle.as_str()),
            "the subject is reached through the declared subject_columns, not \
             through origins: {page}"
        );

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("core_think payload-provenance integration test failed");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn facade_core_think_pages_ancestors_from_a_derivation() {
    let db_name = unique_db_name("proxima_core_think_mcp");
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

        let empty = tools
            .call_core_tool(
                authz.clone(),
                owner,
                Some("test-model".to_string()),
                "core_think",
                serde_json::json!({ "seeds": [] }),
            )
            .await;
        assert!(empty.is_err(), "think requires a seed");

        let fact = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Think source",
                "body": "think-ancestor-needle observation",
                "tags": ["think"],
                "idempotency_key": "facade-core-think-fact"
            }),
        )
        .await?;
        let fact_handle = fact["handle"].as_str().expect("fact");

        let derived = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_derive",
            serde_json::json!({
                "kind": "Abstraction",
                "title": "Think pattern",
                "body": "think-ancestor-needle is a pattern",
                "tags": ["think"],
                "source_handles": [fact_handle],
                "model_id": "test-model",
                "idempotency_key": "facade-core-think-abstraction"
            }),
        )
        .await?;
        let abstraction = derived["handle"].as_str().expect("abstraction");

        let page = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_think",
            serde_json::json!({
                "seeds": [abstraction],
                "direction": "ancestors",
                "depth": 3,
                "limit": 16
            }),
        )
        .await?;
        assert_eq!(page["direction"], "ancestors");
        let handles = page["visits"]
            .as_array()
            .expect("visits")
            .iter()
            .filter_map(|visit| visit["handle"].as_str())
            .collect::<Vec<_>>();
        assert!(
            handles.contains(&fact_handle),
            "ancestor page must include the source Fact: {page}"
        );

        let down = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_think",
            serde_json::json!({
                "seeds": [fact_handle],
                "direction": "descendants",
                "depth": 3,
                "limit": 16
            }),
        )
        .await?;
        let down_handles = down["visits"]
            .as_array()
            .expect("visits")
            .iter()
            .filter_map(|visit| visit["handle"].as_str())
            .collect::<Vec<_>>();
        assert!(
            down_handles.contains(&abstraction),
            "descendant page must include the Abstraction: {down}"
        );

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("core think MCP facade integration test failed");
}

#[tokio::test]
async fn facade_core_episode_commit_binds_only_listed_members() {
    let db_name = unique_db_name("proxima_core_episode_mcp");
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

        let committed = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            serde_json::json!({
                "remember": [
                    {"title": "Episode a", "body": "first bound observation", "tags": ["ep"]},
                    {"title": "Episode b", "body": "second bound observation", "tags": ["ep"]}
                ],
                "bind": ["remember:0", "remember:1"]
            }),
        )
        .await?;
        let a = committed["remembered"][0].as_str().expect("a");
        let b = committed["remembered"][1].as_str().expect("b");
        assert_eq!(committed["bound"].as_array().map(Vec::len), Some(2));

        let siblings = call_test_model_tool(
            &tools,
            authz,
            owner,
            "core_think",
            serde_json::json!({
                "seeds": [a],
                "direction": "episode_siblings",
                "limit": 8
            }),
        )
        .await?;
        let handles = siblings["visits"]
            .as_array()
            .expect("visits")
            .iter()
            .filter_map(|visit| visit["handle"].as_str())
            .collect::<Vec<_>>();
        assert!(
            handles.contains(&b),
            "bound sibling must appear: {siblings}"
        );

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("core episode commit MCP facade integration test failed");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn facade_core_episode_commit_binds_derive_stance_and_goal() {
    let db_name = unique_db_name("proxima_core_episode_dsg");
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

        let committed = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            serde_json::json!({
                "remember": [
                    {"title": "Bound fact", "body": "first observation", "tags": ["ep"]},
                    {"title": "Unbound fact", "body": "second observation", "tags": ["ep"]}
                ],
                "derive": {
                    "title": "Episode pattern",
                    "body": "the two observations form a pattern",
                    "source_handles": ["remember:0", "remember:1"],
                    "model_id": "test-model",
                    "idempotency_key": "episode-derive-1"
                },
                "stance": [{
                    "claim": "the pattern is the stance I hold in this episode",
                    "confidence": 80,
                    "subjects": ["remember:0", "derive"]
                }],
                "goal": [{
                    "schema_id": "core/simple-text-v1",
                    "title": "Pursue the episode stance",
                    "text": "Act on the derived pattern",
                    "body": {},
                    "evidence": ["derive"],
                    "target_perspective": "stance:0",
                    "idempotency_key": "episode-goal-1"
                }],
                "bind": ["remember:0", "derive", "stance:0", "goal:0"]
            }),
        )
        .await?;
        let write_act = committed["write_act"].as_str().expect("write_act");
        let bound_fact = committed["remembered"][0].as_str().expect("remembered 0");
        let unbound_fact = committed["remembered"][1].as_str().expect("remembered 1");
        let derived = committed["derived"].as_str().expect("derived");
        let stance = committed["stances"][0].as_str().expect("stance");
        let goal = committed["goals"][0].as_str().expect("goal");
        let bound = committed["bound"]
            .as_array()
            .expect("bound")
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>();
        assert_eq!(bound, vec![bound_fact, derived, stance, goal]);
        assert!(!bound.contains(&unbound_fact));

        let act_t = write_act
            .strip_prefix("F:")
            .expect("write-act prefix")
            .parse::<Uuid>()?;
        let derived_t = derived
            .strip_prefix("A:")
            .expect("derive prefix")
            .parse::<Uuid>()?;
        let stance_t = stance
            .strip_prefix("P:")
            .expect("stance prefix")
            .parse::<Uuid>()?;
        let goal_t = goal
            .strip_prefix("G:")
            .expect("goal prefix")
            .parse::<Uuid>()?;
        let unbound_t = unbound_fact
            .strip_prefix("F:")
            .expect("unbound prefix")
            .parse::<Uuid>()?;

        let derived_refs: Vec<Uuid> =
            sqlx::query_scalar("SELECT refs FROM proxima_core.memory WHERE t = $1")
                .bind(derived_t)
                .fetch_one(built.pool_for_tests())
                .await?;
        let stance_refs: Vec<Uuid> =
            sqlx::query_scalar("SELECT refs FROM proxima_core.memory WHERE t = $1")
                .bind(stance_t)
                .fetch_one(built.pool_for_tests())
                .await?;
        let unbound_refs: Vec<Uuid> =
            sqlx::query_scalar("SELECT refs FROM proxima_core.memory WHERE t = $1")
                .bind(unbound_t)
                .fetch_one(built.pool_for_tests())
                .await?;
        let goal_act: Option<Uuid> =
            sqlx::query_scalar("SELECT write_act_t FROM proxima_core.goal WHERE t = $1")
                .bind(goal_t)
                .fetch_one(built.pool_for_tests())
                .await?;
        assert!(
            derived_refs.contains(&act_t),
            "bound derive must ref this write-act: {derived_refs:?}"
        );
        assert!(
            stance_refs.contains(&act_t),
            "bound stance must ref this write-act: {stance_refs:?}"
        );
        assert!(
            !unbound_refs.contains(&act_t),
            "unbound remember must not ref this write-act: {unbound_refs:?}"
        );
        assert_eq!(goal_act, Some(act_t), "bound goal write_act_t");

        let siblings = call_test_model_tool(
            &tools,
            authz,
            owner,
            "core_think",
            serde_json::json!({
                "seeds": [bound_fact],
                "direction": "episode_siblings",
                "limit": 16
            }),
        )
        .await?;
        let handles = siblings["visits"]
            .as_array()
            .expect("visits")
            .iter()
            .filter_map(|visit| visit["handle"].as_str())
            .collect::<Vec<_>>();
        assert!(
            handles.contains(&derived),
            "bound derive sibling: {siblings}"
        );
        assert!(
            handles.contains(&stance),
            "bound stance sibling: {siblings}"
        );
        assert!(
            !handles.contains(&unbound_fact),
            "unbound remember is not a sibling: {siblings}"
        );

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("episode derive/stance/goal bind test failed");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn facade_core_episode_commit_bound_replay_fails() {
    let db_name = unique_db_name("proxima_core_episode_replay");
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

        call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            serde_json::json!({
                "remember": [{
                    "title": "Replay seed",
                    "body": "seed observation",
                    "tags": ["ep"],
                    "idempotency_key": "episode-replay-seed"
                }]
            }),
        )
        .await?;
        let remember_err = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            serde_json::json!({
                "remember": [{
                    "title": "Replay seed",
                    "body": "seed observation",
                    "tags": ["ep"],
                    "idempotency_key": "episode-replay-seed"
                }],
                "bind": ["remember:0"]
            }),
        )
        .await
        .expect_err("bound remember replay must fail");
        assert!(
            remember_err.to_string().contains("replayed"),
            "remember H9: {remember_err}"
        );

        let derive_unbound = serde_json::json!({
            "remember": [{
                "title": "Derive source",
                "body": "fresh fact for derive replay",
                "tags": ["ep"],
                "idempotency_key": "episode-replay-derive-src"
            }],
            "derive": {
                "title": "Replay pattern",
                "body": "same pattern body",
                "source_handles": ["remember:0"],
                "model_id": "test-model",
                "idempotency_key": "episode-replay-derive"
            }
        });
        call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            derive_unbound.clone(),
        )
        .await?;
        let mut derive_bound = derive_unbound;
        derive_bound["bind"] = serde_json::json!(["derive"]);
        let derive_err = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            derive_bound,
        )
        .await
        .expect_err("bound derive replay must fail");
        assert!(
            derive_err.to_string().contains("replayed")
                || derive_err.to_string().contains("changed declared refs"),
            "derive H9: {derive_err}"
        );

        let stance_unbound = serde_json::json!({
            "remember": [{
                "title": "Stance source",
                "body": "fresh fact for stance replay",
                "tags": ["ep"],
                "idempotency_key": "episode-replay-stance-src"
            }],
            "stance": [{
                "claim": "same stance claim about the replay fact",
                "confidence": 80,
                "subjects": ["remember:0"]
            }]
        });
        call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            stance_unbound.clone(),
        )
        .await?;
        let mut stance_bound = stance_unbound;
        stance_bound["bind"] = serde_json::json!(["stance:0"]);
        let stance_err = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            stance_bound,
        )
        .await
        .expect_err("bound stance replay must fail");
        assert!(
            stance_err.to_string().contains("replayed")
                || stance_err.to_string().contains("changed declared refs"),
            "stance H9: {stance_err}"
        );

        let goal_unbound = serde_json::json!({
            "remember": [{
                "title": "Goal source fact",
                "body": "fresh fact for goal replay",
                "tags": ["ep"],
                "idempotency_key": "episode-replay-goal-src"
            }],
            "derive": {
                "title": "Goal evidence",
                "body": "abstraction for the replayed goal",
                "source_handles": ["remember:0"],
                "model_id": "test-model",
                "idempotency_key": "episode-replay-goal-abs"
            },
            "stance": [{
                "claim": "assignment stance for the replayed goal",
                "confidence": 80,
                "subjects": ["derive"]
            }],
            "goal": [{
                "schema_id": "core/simple-text-v1",
                "title": "Replay goal",
                "text": "same goal text",
                "body": {},
                "evidence": ["derive"],
                "target_perspective": "stance:0",
                "idempotency_key": "episode-replay-goal"
            }]
        });
        call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            goal_unbound.clone(),
        )
        .await?;
        let mut goal_bound = goal_unbound;
        goal_bound["bind"] = serde_json::json!(["goal:0"]);
        let goal_err =
            call_test_model_tool(&tools, authz, owner, "core_episode_commit", goal_bound)
                .await
                .expect_err("bound goal replay must fail");
        assert!(
            goal_err.to_string().contains("replayed"),
            "goal H9: {goal_err}"
        );

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("episode bound replay test failed");
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
/// filtering through `Engine::query`, which resolves the CALLER'S whole
/// read-owner set — not owner-equality against the request's owner scope.
/// A memory transferred into a group must therefore surface for a group
/// member who has no relationship at all to the original owner. This is the
/// read half of the raw-PgPool boundary breach fix: a flavor's own
/// owner-equality-only candidate SQL would have hidden it.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn facade_authorized_read_surfaces_group_transferred_fact_to_group_member() {
    let db_name = unique_db_name("proxima_authorized_read_transfer");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let author = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(author);
        let group = GroupId::new(Uuid::now_v7());
        let group_owner = OwnerRef::Group(group);
        let member = UserId::new(Uuid::now_v7());
        let member_owner = OwnerRef::Personal(member);
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
                "title": "Transfer candidate",
                "body": "group-visible body unique needle",
                "tags": [],
                "idempotency_key": "facade-authorized-read-owner-transfer"
            }),
        )
        .await?;
        let handle = remembered["handle"].as_str().expect("remembered handle");
        let memory_id = handle
            .strip_prefix("F:")
            .expect("prefixed fact handle")
            .parse::<Uuid>()?;

        // Admin on BOTH sides: the author's own personal owner (source) and
        // the destination group (receiving-side consent).
        let transfer_authz = AuthzContext::for_subject_with_role(
            author,
            [(group_owner, Role::admin())],
            AuthPath::HostBearer,
        );
        built
            .engine
            .transfer_to_owner(
                &transfer_authz,
                proxima_core::EntityId::Memory(MemoryId::new(memory_id)),
                group_owner,
            )
            .await?;
        let transferred_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(memory_id)
                .fetch_one(built.pool_for_tests())
                .await?;
        assert_eq!(
            transferred_owner,
            group_owner.stored_owner_id(),
            "a transfer keeps the same t and moves owner to the destination"
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

        // A caller with no relationship to `owner` — not a co-member of any
        // of the author's other groups, no share, nothing — but a viewer of
        // the destination group. The request's owner scope is the caller's
        // OWN personal owner, so only the read-set union can surface the row.
        let member_authz = AuthzContext::for_subject_with_role(
            member,
            [(group_owner, Role::viewer())],
            AuthPath::HostBearer,
        );
        let visible = proxima::flavor::authorized_memory_ids(
            &built.engine,
            &member_authz,
            member_owner,
            &[memory_id],
            proxima_core::verbs::query::EntityKind::Fact,
            None,
            10,
        )
        .await?;
        assert_eq!(
            visible,
            vec![MemoryId::new(memory_id)],
            "a group-transferred Fact must surface through the authorized-read facade for a group member"
        );

        // The same caller without the group role sees nothing: the read set,
        // not the transfer, is what grants visibility.
        let stranger = UserId::new(Uuid::now_v7());
        let stranger_authz = AuthzContext::for_subject(stranger, AuthPath::HostBearer);
        let hidden = proxima::flavor::authorized_memory_ids(
            &built.engine,
            &stranger_authz,
            OwnerRef::Personal(stranger),
            &[memory_id],
            proxima_core::verbs::query::EntityKind::Fact,
            None,
            10,
        )
        .await?;
        assert!(
            hidden.is_empty(),
            "a transfer is not a publish: a caller outside the destination group sees nothing"
        );

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("authorized-read owner-transfer visibility test failed");
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

#[tokio::test]
async fn request_services_reject_duplicate_boot_type() {
    let db_name = unique_db_name("proxima_request_services");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<MarkApp>::app()
            .database_url(db_url)
            .owner(owner)
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let mut request = FlavorServices::new();
        request.try_insert(BootMark)?;
        let err = built
            .core_mcp_tools_with_request_services(request)
            .expect_err("boot Marker + request Marker");
        assert!(
            matches!(err, FlavorServiceError::DuplicateService { .. }),
            "{err:?}"
        );
        built.shutdown();
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("duplicate request service must fail");
}

/// A `core_remember` body at the surface's 20,000-char cap must land.
///
/// The `(owner, source, ingest_key)` replay key is the fixed-width BLAKE3
/// digest of the schema-owned receipt key. Before it was hashed, the raw
/// key — which embeds the full title/body/tags verbatim — was hex-encoded
/// into the `ingest_keys` primary-key btree, so any body past roughly
/// 1.3KB failed ingest with an index-row-size error while the surface
/// admits 20,000 chars. Replay semantics ride the digest unchanged: an
/// exact replay is a no-op that leaves the row count alone, and a change
/// deep in the body (same title, same idempotency key) digests apart and
/// lands as a new version.
#[tokio::test]
async fn remember_lands_a_20k_body_and_replays_by_digest() {
    let db_name = unique_db_name("proxima_remember_long_body");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let personal = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(db_url.clone())
            .owner(personal)
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let authz = host_authz(&personal, ToolScope::All);
        let pg = PgStorage::connect(&db_url).await?;
        let note_count = || async {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM proxima_core.memory
                  WHERE schema_id = 'core/agent-note-v1'",
            )
            .fetch_one(pg.pool_for_tests())
            .await?;
            Ok::<i64, Box<dyn std::error::Error>>(count)
        };

        // Exactly the surface's 20,000-char cap, and deliberately
        // high-entropy: btree index entries are pglz-compressed before the
        // ~2.7KB row-size ceiling applies, so a repetitive filler body
        // would squeeze under the limit and mask the very overflow this
        // test exists to catch. A deterministic xorshift keeps the corpus
        // reproducible without a rand dependency.
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut words = Vec::with_capacity(1_250);
        while words.len() < 1_250 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            words.push(format!("{state:016x}"));
        }
        let long_body = words.concat();
        assert_eq!(long_body.len(), 20_000);
        let remember_args = |body: &str| {
            serde_json::json!({
                "title": "long note",
                "body": body,
                "tags": [],
                "idempotency_key": "long-note",
            })
        };

        let first = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_remember",
            remember_args(&long_body),
        )
        .await?;
        assert_eq!(first["idempotent_replay"], false, "{first}");
        assert_eq!(note_count().await?, 1);

        let replay = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_remember",
            remember_args(&long_body),
        )
        .await?;
        assert_eq!(replay["idempotent_replay"], true, "{replay}");
        assert_eq!(replay["handle"], first["handle"]);
        assert_eq!(note_count().await?, 1, "an exact replay writes nothing");

        let deep_change = format!("{}Y", &long_body[..long_body.len() - 1]);
        let changed = call_test_model_tool(
            &tools,
            authz,
            personal,
            "core_remember",
            remember_args(&deep_change),
        )
        .await?;
        assert_eq!(changed["idempotent_replay"], false, "{changed}");
        assert_ne!(changed["handle"], first["handle"]);
        assert_eq!(note_count().await?, 2, "a deep body change is a new Fact");

        drop(pg);
        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("20k-body remember must land and replay by digest");
}

/// `language` reaches the index, not just the argument parser.
///
/// Three MCP tools advertise a `language` parameter.
/// `resolve_lexical_language` validates it and `FactWriteCommand` carries
/// it; storage must not drop it. A GENERATED `search_tsv` at a fixed
/// configuration would leave no column to put it in, and
/// no expression that could have read it. `FactWriteCommand`'s doc claimed
/// a memory-row stamp that did not exist.
///
/// The projection row is the first place that has both. This asserts the
/// value lands, and then asserts it MATTERS: `Häuser` is only findable by
/// the singular `Haus` under a German configuration, so a German query
/// hitting a `simple`-indexed row would return nothing.
#[tokio::test]
async fn the_language_argument_reaches_the_projection_and_changes_what_matches() {
    let db_name = unique_db_name("proxima_core_lexical_language");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let personal = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(db_url)
            .owner(personal)
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let authz = built.single_owner_authz().expect("single owner");

        let german = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_remember",
            serde_json::json!({
                "title": "Bericht",
                "body": "Die Häuser am Hafen sind alt.",
                "tags": [],
                "language": "german",
            }),
        )
        .await?;
        let default = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_remember",
            serde_json::json!({
                "title": "Report",
                "body": "Die Häuser am Kai sind neu.",
                "tags": [],
            }),
        )
        .await?;

        let stamped: Vec<(String, String)> = sqlx::query_as(
            "SELECT n.title, p.lexical_language::text
               FROM proxima_core.projection p
               JOIN proxima_core.agent_note_v1 n ON n.t = p.memory_id
              ORDER BY n.title",
        )
        .fetch_all(built.pool_for_tests())
        .await?;
        assert_eq!(
            stamped,
            vec![
                ("Bericht".to_string(), "german".to_string()),
                ("Report".to_string(), "english".to_string()),
            ],
            "the requested configuration is stamped on the projection row; \
             an omitted one takes the deployment default"
        );

        // `Haus` stems to `haus` under german and matches `Häuser`; under
        // this deployment's default (`english`) `Häuser` stems to `häuser`
        // and the singular does not reach it.
        let hits = call_test_model_tool(
            &tools,
            authz,
            personal,
            "core_search_memories",
            serde_json::json!({
                "query": "Haus",
                "mode": "lexical",
                "kind": "Fact",
                "include_body": true,
                "limit": 10,
            }),
        )
        .await?;
        let bodies: Vec<&str> = hits["memories"]
            .as_array()
            .expect("memories")
            .iter()
            .filter_map(|hit| hit["body"].as_str())
            .collect();
        assert_eq!(
            bodies.len(),
            1,
            "only the German-indexed row stems Häuser to Haus; got {hits}"
        );
        assert!(
            bodies[0].contains("Hafen"),
            "the German row is the one that matched; got {hits}"
        );
        assert_ne!(german["handle"], default["handle"]);

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("the language argument must reach the projection");
}
