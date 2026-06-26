use std::collections::HashSet;
use std::sync::Arc;

use proxima::{
    AppInfo, AuthPath, AuthzContext, CapabilitySet, CoreMcpError, CoreMcpErrorKind, CoreMcpTools,
    FlavorApp, FlavorBundle, Identity, NamedMigrator, PgSidecarRegistry, Proxima, RoleSet,
    StorageError, ToolScope, company_owner,
};
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::{
    CitationMappingPayload, CitedObjectPayload, FlavorRegistry, GroupId, MemoryActionSet, MemoryId,
    MemorySpaceGrant, MemorySpaceGrants, Owner, Principal, SchemaId, UserId, all_core_resources,
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
            memory_spaces: MemorySpaceGrants::legacy(),
        },
        auth_path: AuthPath::HostBearer,
    }
}

fn explicit_space_authz(principal: Principal, grants: Vec<MemorySpaceGrant>) -> AuthzContext {
    let accessible_principals = grants
        .iter()
        .map(|grant| grant.owner.clone())
        .collect::<HashSet<_>>();
    AuthzContext {
        identity: Identity {
            principal,
            accessible_principals,
            expires_at: None,
            auth_epoch: 0,
        },
        capabilities: CapabilitySet {
            tool_scope: ToolScope::All,
            roles: RoleSet::all(),
            memory_spaces: MemorySpaceGrants::explicit(grants),
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
        let personal = Principal::User(UserId::new(Uuid::now_v7()));
        let shared = Principal::Group(GroupId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(db_url)
            .owner(personal.clone())
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let authz = explicit_space_authz(
            personal.clone(),
            vec![
                MemorySpaceGrant {
                    key: "personal".into(),
                    label: "Personal".into(),
                    owner: personal.clone(),
                    actions: MemoryActionSet::read_write_publish_admin(),
                },
                MemorySpaceGrant {
                    key: "shared".into(),
                    label: "Shared".into(),
                    owner: shared.clone(),
                    actions: MemoryActionSet::read_only(),
                },
            ],
        );

        let spaces = call_test_model_tool(
            &tools,
            authz.clone(),
            personal.clone(),
            "core_memory_spaces",
            serde_json::json!({}),
        )
        .await?;
        assert_eq!(spaces["spaces"][0]["key"], "personal");
        assert_eq!(spaces["spaces"][1]["key"], "shared");

        call_test_model_tool(
            &tools,
            authz.clone(),
            personal.clone(),
            "core_remember",
            serde_json::json!({"space":"personal","title":"private","body":"private body","tags":[]}),
        )
        .await?;

        let denied = call_test_model_tool(
            &tools,
            authz,
            personal.clone(),
            "core_remember",
            serde_json::json!({"space":"shared","title":"leak","body":"should deny","tags":[]}),
        )
        .await;
        assert!(denied.is_err(), "shared write must be denied");

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
        let personal = Principal::User(UserId::new(Uuid::now_v7()));
        let shared = Principal::Group(GroupId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(db_url)
            .owner(personal.clone())
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let authz = explicit_space_authz(
            personal.clone(),
            vec![
                MemorySpaceGrant {
                    key: "personal".into(),
                    label: "Personal".into(),
                    owner: personal.clone(),
                    actions: MemoryActionSet::read_write_publish_admin(),
                },
                MemorySpaceGrant {
                    key: "shared".into(),
                    label: "Shared".into(),
                    owner: shared.clone(),
                    actions: MemoryActionSet::read_write_publish_admin(),
                },
            ],
        );

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            personal.clone(),
            "core_remember",
            serde_json::json!({"space":"shared","title":"shared note","body":"shared body unique needle","tags":["shared"]}),
        )
        .await?;
        let remembered_handle = remembered["handle"].as_str().expect("handle");

        let search = call_test_model_tool(
            &tools,
            authz,
            personal.clone(),
            "core_search_memories",
            serde_json::json!({
                "query": "unique needle",
                "mode": "lexical",
                "kind": "Fact",
                "spaces": ["shared"],
                "include_body": true,
                "limit": 5
            }),
        )
        .await?;
        assert_eq!(search["memories"][0]["memory"], remembered_handle);
        assert_eq!(search["memories"][0]["space"], "shared");
        assert!(search["memories"][0]["body"].as_str().unwrap().contains("shared body"));

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("shared-space search body test failed");
}

#[tokio::test]
async fn cross_space_derive_is_rejected_with_single_space_message() {
    let db_name = unique_db_name("proxima_core_memory_spaces_derive");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let personal = Principal::User(UserId::new(Uuid::now_v7()));
        let shared = Principal::Group(GroupId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(db_url)
            .owner(personal.clone())
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let authz = explicit_space_authz(
            personal.clone(),
            vec![
                MemorySpaceGrant {
                    key: "personal".into(),
                    label: "Personal".into(),
                    owner: personal.clone(),
                    actions: MemoryActionSet::read_write_publish_admin(),
                },
                MemorySpaceGrant {
                    key: "shared".into(),
                    label: "Shared".into(),
                    owner: shared.clone(),
                    actions: MemoryActionSet::read_write_publish_admin(),
                },
            ],
        );

        let personal_fact = call_test_model_tool(
            &tools,
            authz.clone(),
            personal.clone(),
            "core_remember",
            serde_json::json!({"space":"personal","title":"personal fact","body":"personal source","tags":[]}),
        )
        .await?;
        let shared_fact = call_test_model_tool(
            &tools,
            authz.clone(),
            personal.clone(),
            "core_remember",
            serde_json::json!({"space":"shared","title":"shared fact","body":"shared source","tags":[]}),
        )
        .await?;

        let err = call_test_model_tool(
            &tools,
            authz,
            personal.clone(),
            "core_derive",
            serde_json::json!({
                "space": "personal",
                "kind": "Abstraction",
                "title": "cross-space pattern",
                "body": "should be rejected",
                "tags": [],
                "source_handles": [personal_fact["handle"].as_str().unwrap(), shared_fact["handle"].as_str().unwrap()],
                "model_id": "test-model"
            }),
        )
        .await
        .expect_err("cross-space derive must be rejected");
        assert!(
            err.to_string()
                .contains("cross-space derive/link is not supported; choose one memory space"),
            "unexpected error: {err}"
        );

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("cross-space derive test failed");
}

#[tokio::test]
async fn publish_agent_note_copies_to_target_owner_without_cross_owner_edge() {
    let db_name = unique_db_name("proxima_core_memory_publish");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let personal = Principal::User(UserId::new(Uuid::now_v7()));
        let shared = Principal::Group(GroupId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(db_url)
            .owner(personal.clone())
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let authz = explicit_space_authz(
            personal.clone(),
            vec![
                MemorySpaceGrant {
                    key: "personal".into(),
                    label: "Personal".into(),
                    owner: personal.clone(),
                    actions: MemoryActionSet::read_write_publish_admin(),
                },
                MemorySpaceGrant {
                    key: "shared".into(),
                    label: "Shared".into(),
                    owner: shared.clone(),
                    actions: MemoryActionSet::read_write_publish_admin(),
                },
            ],
        );

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            personal.clone(),
            "core_remember",
            serde_json::json!({"space":"personal","title":"private note","body":"publish unique needle","tags":["private"]}),
        )
        .await?;
        let source = remembered["handle"].as_str().expect("source handle");
        let published = call_test_model_tool(
            &tools,
            authz.clone(),
            personal.clone(),
            "core_publish_memory",
            serde_json::json!({
                "memory": source,
                "from_space": "personal",
                "to_space": "shared",
                "confirm": true
            }),
        )
        .await?;
        let published_handle = published["published"].as_str().expect("published handle");
        assert_ne!(source, published_handle, "publish must copy, not move/mutate");

        let personal_search = call_test_model_tool(
            &tools,
            authz.clone(),
            personal.clone(),
            "core_search_memories",
            serde_json::json!({"spaces":["personal"],"query":"publish unique needle","mode":"lexical","kind":"Fact","include_body":true}),
        )
        .await?;
        assert_eq!(personal_search["memories"][0]["memory"], source);
        assert_eq!(personal_search["memories"][0]["space"], "personal");

        let shared_search = call_test_model_tool(
            &tools,
            authz,
            personal.clone(),
            "core_search_memories",
            serde_json::json!({"spaces":["shared"],"query":"publish unique needle","mode":"lexical","kind":"Fact","include_body":true}),
        )
        .await?;
        assert_eq!(shared_search["memories"][0]["memory"], published_handle);
        assert_eq!(shared_search["memories"][0]["space"], "shared");

        let source_id = source.strip_prefix("F:").unwrap().parse::<Uuid>()?;
        let published_id = published_handle.strip_prefix("F:").unwrap().parse::<Uuid>()?;
        let edge_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.edges
             WHERE (source_memory_id = $1 AND target_memory_id = $2)
                OR (source_memory_id = $2 AND target_memory_id = $1)",
        )
        .bind(source_id)
        .bind(published_id)
        .fetch_one(&built.pool)
        .await?;
        assert_eq!(edge_count, 0, "publish must not create cross-owner edges");

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("publish memory test failed");
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "single fixture covers the publish tool's independent negative authorization cases"
)]
async fn publish_memory_rejects_unsupported_payloads_and_missing_grants() {
    let db_name = unique_db_name("proxima_core_memory_publish_negative");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let personal = Principal::User(UserId::new(Uuid::now_v7()));
        let shared = Principal::Group(GroupId::new(Uuid::now_v7()));
        let hidden = Principal::Group(GroupId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(db_url)
            .owner(personal.clone())
            .build()
            .await?;
        let tools = built.core_mcp_tools();
        let full_authz = explicit_space_authz(
            personal.clone(),
            vec![
                MemorySpaceGrant { key: "personal".into(), label: "Personal".into(), owner: personal.clone(), actions: MemoryActionSet::read_write_publish_admin() },
                MemorySpaceGrant { key: "shared".into(), label: "Shared".into(), owner: shared.clone(), actions: MemoryActionSet::read_write_publish_admin() },
                MemorySpaceGrant { key: "hidden".into(), label: "Hidden".into(), owner: hidden.clone(), actions: MemoryActionSet::read_write_publish_admin() },
            ],
        );

        let note = call_test_model_tool(
            &tools,
            full_authz.clone(),
            personal.clone(),
            "core_remember",
            serde_json::json!({"space":"personal","title":"publish negative","body":"negative note","tags":[]}),
        ).await?;
        let note_handle = note["handle"].as_str().unwrap();
        let utterance = call_test_model_tool(
            &tools,
            full_authz.clone(),
            personal.clone(),
            "core_record_utterance",
            serde_json::json!({"space":"personal","speaker":"user","conversation_id":"publish-negative","text":"not an AgentNote"}),
        ).await?;
        let utterance_handle = utterance["handle"].as_str().unwrap();
        let hidden_note = call_test_model_tool(
            &tools,
            full_authz.clone(),
            personal.clone(),
            "core_remember",
            serde_json::json!({"space":"hidden","title":"hidden note","body":"hidden note","tags":[]}),
        ).await?;
        let hidden_handle = hidden_note["handle"].as_str().unwrap();

        let unsupported = call_test_model_tool(
            &tools,
            full_authz.clone(),
            personal.clone(),
            "core_publish_memory",
            serde_json::json!({"memory": utterance_handle, "from_space":"personal", "to_space":"shared", "confirm": true}),
        ).await.expect_err("non-AgentNote publish must fail");
        assert!(unsupported.to_string().contains("supports only core/agent-note-v1"));

        let no_publish = explicit_space_authz(
            personal.clone(),
            vec![
                MemorySpaceGrant { key: "personal".into(), label: "Personal".into(), owner: personal.clone(), actions: MemoryActionSet { search: true, read: true, write: true, publish: false, admin: false } },
                MemorySpaceGrant { key: "shared".into(), label: "Shared".into(), owner: shared.clone(), actions: MemoryActionSet::read_write_publish_admin() },
            ],
        );
        let publish_denied = call_test_model_tool(
            &tools,
            no_publish,
            personal.clone(),
            "core_publish_memory",
            serde_json::json!({"memory": note_handle, "from_space":"personal", "to_space":"shared", "confirm": true}),
        ).await.expect_err("source publish grant required");
        assert!(publish_denied.to_string().contains("requires memory.publish"));

        let target_read_only = explicit_space_authz(
            personal.clone(),
            vec![
                MemorySpaceGrant { key: "personal".into(), label: "Personal".into(), owner: personal.clone(), actions: MemoryActionSet::read_write_publish_admin() },
                MemorySpaceGrant { key: "shared".into(), label: "Shared".into(), owner: shared.clone(), actions: MemoryActionSet::read_only() },
            ],
        );
        let write_denied = call_test_model_tool(
            &tools,
            target_read_only,
            personal.clone(),
            "core_publish_memory",
            serde_json::json!({"memory": note_handle, "from_space":"personal", "to_space":"shared", "confirm": true}),
        ).await.expect_err("target write grant required");
        assert!(write_denied.to_string().contains("requires memory.write"));

        let restricted_visibility = AuthzContext {
            identity: Identity {
                principal: personal.clone(),
                accessible_principals: HashSet::from([personal.clone(), shared.clone()]),
                expires_at: None,
                auth_epoch: 0,
            },
            capabilities: CapabilitySet {
                tool_scope: ToolScope::All,
                roles: RoleSet::all(),
                memory_spaces: MemorySpaceGrants::explicit(vec![
                    MemorySpaceGrant { key: "hidden".into(), label: "Hidden".into(), owner: hidden.clone(), actions: MemoryActionSet::read_write_publish_admin() },
                    MemorySpaceGrant { key: "shared".into(), label: "Shared".into(), owner: shared.clone(), actions: MemoryActionSet::read_write_publish_admin() },
                ]),
            },
            auth_path: AuthPath::HostBearer,
        };
        let hidden_denied = call_test_model_tool(
            &tools,
            restricted_visibility,
            personal.clone(),
            "core_publish_memory",
            serde_json::json!({"memory": hidden_handle, "from_space":"hidden", "to_space":"shared", "confirm": true}),
        ).await.expect_err("hidden owner visibility required");
        assert!(hidden_denied.to_string().contains("unknown memory space: hidden"));

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("publish negative test failed");
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

        let mut admin_authz = host_authz(&owner, ToolScope::All);
        admin_authz.capabilities.roles.admin = true;
        let output = call_test_model_tool(
            &tools,
            admin_authz,
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
                {
                    let mut authz = host_authz(&owner, ToolScope::Palette(Vec::new()));
                    authz.capabilities.roles.admin = true;
                    authz
                },
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
            .read_core_resource(
                host_authz(&owner, ToolScope::All),
                owner.clone(),
                None,
                "proxima://memory/not-a-memory-id",
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
async fn facade_reads_core_resources_with_resource_scope() {
    let db_name = unique_db_name("proxima_core_resource_mcp");
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
        let authz = host_authz(&owner, ToolScope::All);

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner.clone(),
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
            owner.clone(),
            &format!("proxima://memory/{memory}"),
        )
        .await?;
        assert_eq!(resource_memory["memory"], memory);

        let resource_schemas =
            read_test_model_resource(&tools, authz.clone(), owner.clone(), "proxima://schemas")
                .await?;
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
            owner.clone(),
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
        assert!(!tool_names.contains("core_get_memory"));
        assert!(!tool_names.contains("core_list_substrate_tools"));
        assert!(tool_names.contains("core_fact"));
        assert!(
            all_core_resources().any(|resource| resource.scope_key == "resource:memory"),
            "resource:memory must stay in the core resource catalog"
        );
        let resource_listing =
            read_test_model_resource(&tools, authz.clone(), owner.clone(), "proxima://tools")
                .await?;
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

        let fetched = read_test_model_resource(
            &tools,
            authz,
            owner.clone(),
            &format!("proxima://memory/{memory}"),
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
async fn facade_core_fact_tombstone_is_idempotent() {
    let db_name = unique_db_name("proxima_core_fact_tombstone_mcp");
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
        let authz = host_authz(&owner, ToolScope::All);

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner.clone(),
            "core_remember",
            serde_json::json!({
                "title": "Tombstone surface",
                "body": "Fact erased through core_fact tombstone.",
                "tags": ["tombstone"],
                "idempotency_key": "facade-core-fact-tombstone"
            }),
        )
        .await?;
        let memory = remembered["handle"].as_str().expect("remembered handle");

        let first = call_test_model_tool(
            &tools,
            authz.clone(),
            owner.clone(),
            "core_fact",
            serde_json::json!({ "action": "tombstone", "fact": memory, "confirm": true, "expect_handle": memory }),
        )
        .await?;
        assert_eq!(first["fact_erased"], true);
        assert_eq!(first["idempotent_replay"], false);

        let second = call_test_model_tool(
            &tools,
            authz,
            owner.clone(),
            "core_fact",
            serde_json::json!({ "action": "tombstone", "fact": memory, "confirm": true, "expect_handle": memory }),
        )
        .await?;
        assert_eq!(second["fact_erased"], false);
        assert_eq!(second["idempotent_replay"], true);

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("core_fact tombstone MCP facade integration test failed");
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
