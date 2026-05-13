use std::sync::Arc;

use std::collections::HashMap;

use futures::future::BoxFuture;
use proxima_core::auth::NoAuth;
use proxima_core::harness::{HarnessProgram, ProviderTarget, SubstrateToolBinding};
use proxima_core::mcp::{
    HarnessSubstrateBridge, HarnessSubstrateCall, McpAuthorContext, McpTool, McpToolCtx,
    McpToolError,
};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::wake::token_store::WakeTokenContext;
use proxima_core::{
    Engine, FlavorRegistry, HandleTable, MemoryId, OrgId, Owner, Principal, UserId, WakeChainDepth,
};
use proxima_harness::program::resolve;
use proxima_mcp_server::McpToolHost;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use uuid::Uuid;

fn owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

fn binding(canonical: &str) -> SubstrateToolBinding {
    SubstrateToolBinding {
        canonical_name: canonical.into(),
        description: "stub".into(),
        args_schema: json!({"type": "object"}),
    }
}

fn empty_program(bindings: &[SubstrateToolBinding], workspace: bool) -> HarnessProgram {
    HarnessProgram {
        system_prompt: "sys".into(),
        instructions: "do".into(),
        context_params: HashMap::default(),
        substrate_tool_palette: bindings.iter().map(|b| b.canonical_name.clone()).collect(),
        workspace_root: workspace.then(|| std::path::PathBuf::from("/tmp/x")),
        max_rounds: 5,
        provider: ProviderTarget::MistralChat {
            base_url: "http://x".into(),
            model_id: "m".into(),
            api_key: "k".into(),
            temperature: None,
            max_completion_tokens: None,
        },
    }
}

#[test]
fn provider_safe_names_reverse_map_back_to_canonical() {
    let bindings = vec![binding("core/emit_abstraction")];
    let program = empty_program(&bindings, false);
    let resolved = resolve(program, &bindings);
    let spec = resolved
        .tools
        .iter()
        .find(|tool| tool.canonical == "core/emit_abstraction")
        .expect("tool spec");

    assert_eq!(spec.provider_safe, "core_emit_abstraction");
    assert_eq!(
        resolved.reverse_map.get("core_emit_abstraction").unwrap(),
        "core/emit_abstraction"
    );
}

#[test]
fn workspace_tools_appear_only_when_workspace_root_is_set() {
    let without_workspace = resolve(empty_program(&[], false), &[]);
    assert!(
        !without_workspace
            .tools
            .iter()
            .any(|tool| tool.canonical.starts_with("workspace_"))
    );

    let with_workspace = resolve(empty_program(&[], true), &[]);
    let names: Vec<&str> = with_workspace
        .tools
        .iter()
        .map(|tool| tool.canonical.as_str())
        .collect();
    assert!(names.contains(&"workspace_shell"));
    assert!(names.contains(&"workspace_text_editor"));
    assert!(names.contains(&"workspace_list_files"));
}

#[tokio::test]
async fn bridge_inventory_includes_registry_and_personality_pack_tools() {
    let pool = sqlx::PgPool::connect_lazy("postgres://placeholder/db").expect("lazy pool");
    let host = McpToolHost::from_pool(pool, owner(), Arc::new(FlavorRegistry::new().freeze()));
    let palette = vec![
        "core/fetch_memory".to_string(),
        "core/emit_perspective".to_string(),
        "core/list_substrate_tools".to_string(),
    ];

    let specs = host.list_harness_tools(&palette);
    let names: Vec<&str> = specs
        .iter()
        .map(|spec| spec.canonical_name.as_str())
        .collect();

    assert!(names.contains(&"core/fetch_memory"));
    assert!(names.contains(&"core/emit_perspective"));
    assert!(names.contains(&"core/list_substrate_tools"));

    let bindings: Vec<_> = specs
        .into_iter()
        .map(|spec| SubstrateToolBinding {
            canonical_name: spec.canonical_name,
            description: spec.description,
            args_schema: spec.args_schema,
        })
        .collect();
    let resolved = resolve(empty_program(&bindings, false), &bindings);
    let safe_names: Vec<&str> = resolved
        .tools
        .iter()
        .map(|tool| tool.provider_safe.as_str())
        .collect();
    assert!(safe_names.contains(&"core_fetch_memory"));
    assert!(safe_names.contains(&"core_emit_perspective"));
    assert!(safe_names.contains(&"core_list_substrate_tools"));
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RequiresSelfArgs {}

#[derive(Debug, Serialize)]
struct RequiresSelfOutput {
    caller_self_perspective: Uuid,
}

struct RequiresSelfTool;

impl McpTool for RequiresSelfTool {
    const NAME: &'static str = "test/requires_self";
    const DESCRIPTION: &'static str = "Assert caller self perspective is populated.";

    type Args = RequiresSelfArgs;
    type Output = RequiresSelfOutput;

    fn call(
        ctx: McpToolCtx,
        _args: Self::Args,
    ) -> BoxFuture<'static, Result<Self::Output, McpToolError>> {
        Box::pin(async move {
            let caller_self_perspective = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::Other(
                    "caller_self_perspective is required for harness dispatch".into(),
                )
            })?;
            Ok(RequiresSelfOutput {
                caller_self_perspective: caller_self_perspective.into_inner(),
            })
        })
    }
}

#[tokio::test]
async fn bridge_defaults_caller_self_perspective_for_wake_calls() {
    let owner = owner();
    let mut registry = FlavorRegistry::new();
    registry.add_mcp_tool::<RequiresSelfTool>("test");
    let frozen = Arc::new(registry.freeze());
    let engine = Arc::new(Engine::new(
        FlavorRegistry::new().freeze(),
        MemoryStore::new(),
        Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
    ));
    let pool = sqlx::PgPool::connect_lazy("postgres://placeholder/db").expect("lazy pool");
    let host = McpToolHost::from_pool(pool, owner.clone(), frozen).with_engine(engine.clone());
    let root = MemoryId::new(Uuid::now_v7());
    let token = engine
        .wake_token_store()
        .mint(WakeTokenContext {
            invocation_id: Uuid::now_v7(),
            personality_instance_id: Uuid::now_v7(),
            wake_entry_id: Uuid::now_v7(),
            change_event_seq: Uuid::now_v7(),
            owner: owner.clone(),
            palette: vec!["test/requires_self".into()],
            model_id: "test-model".into(),
            max_rounds: 1,
            current_root_perspective_memory_id: root,
            triggering_event_memory_id: MemoryId::new(Uuid::now_v7()),
            triggering_event_depth: WakeChainDepth::zero(),
            read_log: Arc::new(Mutex::new(Vec::new())),
            handles: Arc::new(HandleTable::new()),
        })
        .await;

    let output = host
        .call_harness_tool(HarnessSubstrateCall {
            canonical_name: "test/requires_self".into(),
            args: json!({}),
            owner,
            wake_token: token,
            author: McpAuthorContext {
                model_id: "test-model".into(),
                client_name: "proxima-harness-test".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
        })
        .await
        .expect("caller_self_perspective is required regression should pass");

    assert_eq!(
        output["caller_self_perspective"],
        json!(root.into_inner().to_string())
    );
}
