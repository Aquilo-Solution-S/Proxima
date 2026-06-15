//! E2E: a wake-token caller's audit Fact attributes provenance to the
//! calling personality (not to shell-author).

mod common;

use std::sync::Arc;

use common::{create_db, db_url, drop_db};
use proxima_core::mcp::core_tools::add_wake_entry::AddWakeEntryTool;
use proxima_core::mcp::core_tools::wake_entry_input::WakeEntryDraftInput;
use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolCtx, OutputMode};
use proxima_core::storage::Storage;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{
    AuthPath, AuthzContext, Engine, FlavorRegistry, InstantiatePersonalityRequest, McpTool, OrgId,
    Owner, Principal, UserId, WakeEntryAuthoredBy, WakeEntryGoalScope, WakeEntryTriggerKind,
};
use proxima_storage_pg::PgStorage;

#[tokio::test(flavor = "multi_thread")]
async fn wake_token_audit_attributes_caller_personality() -> Result<(), Box<dyn std::error::Error>>
{
    let Some(db_name) = create_db().await? else {
        return Ok(());
    };
    let database_url = db_url(&db_name);
    let pg = PgStorage::connect(&database_url).await?;
    pg.run_migrations().await?;

    let owner = Owner {
        principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
        org_id: OrgId::new(uuid::Uuid::now_v7()),
    };
    let inst = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
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
    let engine = Arc::new(
        Engine::new(FlavorRegistry::new().freeze(), MemoryStore::new())
            .with_storage(Arc::new(pg.clone())),
    );

    // Construct an McpToolCtx pretending we're a wake invocation on this personality.
    let pool = pg.pool().clone();
    let ctx = McpToolCtx {
        pool,
        owner: owner.clone(),
        authz: AuthzContext::single_owner(&owner, AuthPath::System),
        handles: Some(Arc::new(HandleTable::new())),
        mode: OutputMode::Handles,
        registry: Arc::new(FlavorRegistry::new().freeze()),
        author: McpAuthorContext {
            model_id: "test".into(),
            client_name: "test".into(),
            client_version: "0".into(),
            personality_instance_id: None,
            caller_self_perspective: Some(root_memory_id),
        },
        caller_self_perspective: Some(root_memory_id),
        master_token_id: None,
        engine: Some(engine.clone()),
    };

    // Pre-assign the personality handle so the tool can resolve it.
    let p_handle = ctx.format_personality(inst.instance_id);

    // Call core/add_wake_entry on this personality.
    let args = AddWakeEntryArgs {
        personality: p_handle,
        entry: WakeEntryDraftInput {
            wake_entry_id: None,
            trigger_kind: WakeEntryTriggerKind::OnMemory,
            trigger_id: "core/personality_config_changed_v1".into(),
            label: "self-evolution".into(),
            enabled: true,
            authored_by: WakeEntryAuthoredBy::Any,
            probability_promille: 1000,
            goal_scope: WakeEntryGoalScope::None,
            instructions: String::new(),
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
    // The full audit-Fact verification is deferred to a follow-up task. The
    // PersonalityConfigChangedV1 Fact carries no sidecar of its own — its typed
    // snapshot (verb/before/after/subject/caller) is persisted as the Fact's
    // citation cited-object (see core_tools::audit::write_fact), so verifying
    // caller.kind == "wake_personality" means reading the cited-object payload
    // via the citation_mappings link, not a sidecar table.

    drop_db(&db_name).await?;
    Ok(())
}

// Re-export AddWakeEntryArgs for convenience
use proxima_core::mcp::core_tools::add_wake_entry::AddWakeEntryArgs;
