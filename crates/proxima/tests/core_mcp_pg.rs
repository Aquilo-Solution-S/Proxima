use std::collections::HashSet;

use proxima::{
    AppInfo, AuthPath, AuthzContext, CapabilitySet, CoreMcpError, FlavorApp, FlavorBundle,
    Identity, NamedMigrator, Proxima, RoleSet, ToolScope, company_owner,
};
use proxima_core::{FlavorRegistry, Owner};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use uuid::Uuid;

struct EmptyApp;

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
