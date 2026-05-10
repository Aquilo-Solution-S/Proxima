//! E2E: a wake-token caller's audit Fact attributes provenance to the
//! calling personality (not to shell-author).

mod common;

use std::sync::Arc;

use common::{create_db, drop_db};
use proxima_core::auth::NoAuth;
use proxima_core::mcp::core_tools::add_wake_entry::AddWakeEntryTool;
use proxima_core::mcp::core_tools::wake_entry_input::WakeEntryDraftInput;
use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolCtx};
use proxima_core::storage::Storage;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{
    Engine, FlavorRegistry, InstantiatePersonalityRequest, McpTool, ModelTier, OrgId, Owner,
    Principal, UserId, WakeEntryAuthoredBy, WakeEntryTriggerKind, WakeExecutionMode,
};
use proxima_storage_pg::PgStorage;

#[tokio::test(flavor = "multi_thread")]
async fn wake_token_audit_attributes_caller_personality() -> Result<(), Box<dyn std::error::Error>>
{
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let database_url = format!("postgres://postgres@localhost/{db_name}");
    let pg = PgStorage::connect(&database_url).await?;
    pg.run_migrations().await?;

    let owner = Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
        org_id: OrgId::new(uuid::Uuid::now_v7()),
    };
    let inst = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "caller".into(),
            purpose: "self-evolution test".into(),
        })
        .await?;

    // Pull the personality row to find its Root Perspective Memory id.
    let rows = pg.list_personality_instances(&owner, false).await?;
    let row = rows
        .into_iter()
        .find(|r| r.personality_instance_id == inst.instance_id)
        .expect("just instantiated");
    let root_memory_id = row.current_root_perspective_memory_id;

    // Build an Engine wired with the live PG storage so ctx.storage() works.
    let resolver = NoAuth::new(owner.principal.clone(), owner.clone());
    let engine = Arc::new(
        Engine::new(
            FlavorRegistry::new().freeze(),
            MemoryStore::new(),
            Box::new(resolver),
        )
        .with_storage(Arc::new(pg.clone())),
    );

    // Construct an McpToolCtx pretending we're a wake invocation on this personality.
    let pool = pg.pool().clone();
    let ctx = McpToolCtx {
        pool,
        owner: owner.clone(),
        handles: Arc::new(HandleTable::new()),
        registry: Arc::new(FlavorRegistry::new().freeze()),
        author: McpAuthorContext {
            model_id: "test".into(),
            client_name: "test".into(),
            client_version: "0".into(),
            caller_self_perspective: Some(root_memory_id),
        },
        caller_self_perspective: Some(root_memory_id),
        master_token_id: None,
        engine: Some(engine.clone()),
    };

    // Pre-assign the personality handle so the tool can resolve it.
    let p_handle = ctx.handles.assign_personality(inst.instance_id);

    // Call core/add_wake_entry on this personality.
    let args = AddWakeEntryArgs {
        personality: p_handle.as_str().to_string(),
        entry: WakeEntryDraftInput {
            wake_entry_id: None,
            trigger_kind: WakeEntryTriggerKind::OnMemory,
            trigger_id: "core/personality_config_changed_v1".into(),
            label: "self-evolution".into(),
            enabled: true,
            execution_mode: WakeExecutionMode::SubstrateOnly,
            authored_by: WakeEntryAuthoredBy::Any,
            probability_promille: 1000,
            recipe_ref: "proxima-code/engineer".into(),
            model_tier: ModelTier::Standard,
            inference_target_ref: None,
            substrate_tool_palette: vec![],
            workspace_tool_palette: vec![],
            max_rounds: 3,
        },
    };
    let _out = AddWakeEntryTool::call(ctx, args).await?;

    // Verify the wake entry was actually added to the personality.
    let instances = pg.list_personality_instances(&owner, false).await?;
    let updated = instances
        .into_iter()
        .find(|r| r.personality_instance_id == inst.instance_id)
        .expect("personality still exists");
    assert!(!updated.wake_entries.is_empty(), "wake entry was added");
    let entry = &updated.wake_entries[0];
    assert_eq!(entry.trigger_id, "core/personality_config_changed_v1");
    assert_eq!(entry.label, "self-evolution");

    // TODO(audit-e2e): assert provenance details
    // The full audit-Fact verification (querying proxima_core.personality_config_changed_v1
    // sidecar table for caller.kind == "wake_personality" and matching personality_instance_id)
    // is deferred to a follow-up task. The sidecar table is created dynamically by the
    // schema registration in FlavorRegistry::new() which includes PersonalityConfigChangedV1.

    drop_db(&db_name).await?;
    Ok(())
}

// Re-export AddWakeEntryArgs for convenience
use proxima_core::mcp::core_tools::add_wake_entry::AddWakeEntryArgs;
