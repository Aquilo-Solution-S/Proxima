mod common;

use std::sync::Arc;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::authz::{AuthPath, AuthzContext, CapabilitySet, RoleSet, ToolScope};
use proxima_core::mcp::core_tools::add_wake_entry::{AddWakeEntryArgs, AddWakeEntryTool};
use proxima_core::mcp::core_tools::remove_wake_entry::{RemoveWakeEntryArgs, RemoveWakeEntryTool};
use proxima_core::mcp::core_tools::set_read_scope::{SetReadScopeArgs, SetReadScopeTool};
use proxima_core::mcp::core_tools::update_wake_entry::{
    UpdateWakeEntryArgs, UpdateWakeEntryTool, WakeEntryPatch,
};
use proxima_core::mcp::core_tools::wake_entry_input::WakeEntryDraftInput;
use proxima_core::mcp::{McpAuthorContext, McpTool, McpToolCtx, OutputMode};
use proxima_core::personality::{
    InstantiatePersonalityRequest, ListReadScopeRequest, SetWakeEntriesRequest,
};
use proxima_core::storage::Storage;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{
    Engine, FlavorRegistry, ModelTier, Owner, PersonalityInstanceId, WakeEntryAuthoredBy,
    WakeEntryDraft, WakeEntryTriggerKind,
};
use uuid::Uuid;

fn non_admin_authz(owner: &Owner) -> AuthzContext {
    let mut authz = AuthzContext::single_owner(owner, AuthPath::HostBearer);
    authz.capabilities = CapabilitySet {
        tool_scope: ToolScope::All,
        roles: RoleSet {
            graph_read: true,
            graph_write: true,
            source_ingest: false,
            admin: false,
        },
    };
    authz
}

fn ctx(owner: &Owner, pg: &proxima_storage_pg::PgStorage, authz: AuthzContext) -> McpToolCtx {
    let registry = FlavorRegistry::default().freeze();
    let engine =
        Engine::new(registry.clone(), MemoryStore::new()).with_storage(pg.clone().into_handle());
    McpToolCtx {
        pool: pg.pool().clone(),
        owner: owner.clone(),
        authz,
        handles: None,
        mode: OutputMode::RawIds,
        registry: Arc::new(registry),
        author: McpAuthorContext {
            model_id: "test-model".into(),
            client_name: "test-client".into(),
            client_version: "0".into(),
            caller_self_perspective: None,
        },
        caller_self_perspective: None,
        master_token_id: None,
        engine: Some(Arc::new(engine)),
    }
}

async fn instantiate(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    display_name: &str,
) -> Result<PersonalityInstanceId, Box<dyn std::error::Error>> {
    let response = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
            display_name: display_name.into(),
            purpose: "admin-gate regression fixture".into(),
        })
        .await?;
    Ok(response.instance_id)
}

fn wake_entry(
    id: Uuid,
    pid: PersonalityInstanceId,
    label: &str,
    trigger_id: &str,
) -> WakeEntryDraft {
    WakeEntryDraft::new(
        id,
        pid,
        WakeEntryTriggerKind::OnMemory,
        trigger_id,
        label,
        WakeEntryAuthoredBy::Any,
        1000,
        ModelTier::Standard,
        None,
        Vec::new(),
        1,
    )
    .expect("valid wake entry")
}

async fn wake_labels(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    pid: PersonalityInstanceId,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let rows = pg.list_personality_instances(owner, true).await?;
    Ok(rows
        .into_iter()
        .find(|row| row.personality_instance_id == pid)
        .expect("personality row")
        .wake_entries
        .into_iter()
        .map(|entry| entry.label)
        .collect())
}

fn assert_admin_denied(err: &proxima_core::mcp::McpToolError) {
    assert!(err.to_string().contains("requires admin role"));
}

#[tokio::test]
async fn add_wake_entry_requires_admin_and_preserves_storage_on_denial()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let pid = instantiate(&pg, &owner, "add gate").await?;

    let args = AddWakeEntryArgs {
        personality: pid.into_inner().to_string(),
        entry: WakeEntryDraftInput {
            wake_entry_id: None,
            trigger_kind: WakeEntryTriggerKind::OnMemory,
            trigger_id: "test/add-denied".into(),
            label: "blocked-add".into(),
            enabled: true,
            execution_mode: proxima_core::WakeExecutionMode::SubstrateOnly,
            authored_by: WakeEntryAuthoredBy::Any,
            probability_promille: 1000,
            goal_scope: proxima_core::WakeEntryGoalScope::None,
            instructions: String::new(),
            model_tier: ModelTier::Standard,
            inference_target_ref: None,
            substrate_tool_palette: Vec::new(),
            required_produced_schema_ids: Vec::new(),
            max_rounds: 1,
            intervention_policy: None,
        },
    };

    let err = AddWakeEntryTool::call(ctx(&owner, &pg, non_admin_authz(&owner)), args)
        .await
        .expect_err("non-admin add must be denied");
    assert_admin_denied(&err);
    assert!(wake_labels(&pg, &owner, pid).await?.is_empty());

    let admin_args = AddWakeEntryArgs {
        personality: pid.into_inner().to_string(),
        entry: WakeEntryDraftInput {
            wake_entry_id: None,
            trigger_kind: WakeEntryTriggerKind::OnMemory,
            trigger_id: "test/add-admin".into(),
            label: "admin-add".into(),
            enabled: true,
            execution_mode: proxima_core::WakeExecutionMode::SubstrateOnly,
            authored_by: WakeEntryAuthoredBy::Any,
            probability_promille: 1000,
            goal_scope: proxima_core::WakeEntryGoalScope::None,
            instructions: String::new(),
            model_tier: ModelTier::Standard,
            inference_target_ref: None,
            substrate_tool_palette: Vec::new(),
            required_produced_schema_ids: Vec::new(),
            max_rounds: 1,
            intervention_policy: None,
        },
    };
    AddWakeEntryTool::call(
        ctx(
            &owner,
            &pg,
            AuthzContext::single_owner(&owner, AuthPath::System),
        ),
        admin_args,
    )
    .await?;
    assert_eq!(wake_labels(&pg, &owner, pid).await?, vec!["admin-add"]);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn update_wake_entry_requires_admin_and_preserves_storage_on_denial()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let pid = instantiate(&pg, &owner, "update gate").await?;
    let wid = Uuid::now_v7();
    pg.set_wake_entries(&SetWakeEntriesRequest {
        principal: owner.principal.clone(),
        org_id: Some(owner.org_id),
        personality_instance_id: pid,
        entries: vec![wake_entry(wid, pid, "original", "test/update")],
    })
    .await?;

    let args = UpdateWakeEntryArgs {
        wake_entry: wid.to_string(),
        patch: WakeEntryPatch {
            label: Some("blocked-update".into()),
            ..WakeEntryPatch::default()
        },
    };
    let err = UpdateWakeEntryTool::call(ctx(&owner, &pg, non_admin_authz(&owner)), args)
        .await
        .expect_err("non-admin update must be denied");
    assert_admin_denied(&err);
    assert_eq!(wake_labels(&pg, &owner, pid).await?, vec!["original"]);

    let admin_args = UpdateWakeEntryArgs {
        wake_entry: wid.to_string(),
        patch: WakeEntryPatch {
            label: Some("admin-update".into()),
            ..WakeEntryPatch::default()
        },
    };
    UpdateWakeEntryTool::call(
        ctx(
            &owner,
            &pg,
            AuthzContext::single_owner(&owner, AuthPath::System),
        ),
        admin_args,
    )
    .await?;
    assert_eq!(wake_labels(&pg, &owner, pid).await?, vec!["admin-update"]);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn remove_wake_entry_requires_admin_and_preserves_storage_on_denial()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let pid = instantiate(&pg, &owner, "remove gate").await?;
    let wid = Uuid::now_v7();
    pg.set_wake_entries(&SetWakeEntriesRequest {
        principal: owner.principal.clone(),
        org_id: Some(owner.org_id),
        personality_instance_id: pid,
        entries: vec![wake_entry(wid, pid, "remove-me", "test/remove")],
    })
    .await?;

    let args = RemoveWakeEntryArgs {
        wake_entry: wid.to_string(),
    };
    let err = RemoveWakeEntryTool::call(ctx(&owner, &pg, non_admin_authz(&owner)), args)
        .await
        .expect_err("non-admin remove must be denied");
    assert_admin_denied(&err);
    assert_eq!(wake_labels(&pg, &owner, pid).await?, vec!["remove-me"]);

    let output = RemoveWakeEntryTool::call(
        ctx(
            &owner,
            &pg,
            AuthzContext::single_owner(&owner, AuthPath::System),
        ),
        RemoveWakeEntryArgs {
            wake_entry: wid.to_string(),
        },
    )
    .await?;
    assert!(output.removed);
    assert!(wake_labels(&pg, &owner, pid).await?.is_empty());

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn set_read_scope_requires_admin_and_preserves_storage_on_denial()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let reader = instantiate(&pg, &owner, "reader").await?;
    let readable = instantiate(&pg, &owner, "readable").await?;

    let args = SetReadScopeArgs {
        personality: reader.into_inner().to_string(),
        readable_personalities: vec![readable.into_inner().to_string()],
    };
    let err = SetReadScopeTool::call(ctx(&owner, &pg, non_admin_authz(&owner)), args)
        .await
        .expect_err("non-admin set_read_scope must be denied");
    assert_admin_denied(&err);
    assert!(
        pg.list_read_scope(&ListReadScopeRequest {
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
            reader_personality_instance_id: reader,
        })
        .await?
        .readable_personality_instance_ids
        .is_empty()
    );

    SetReadScopeTool::call(
        ctx(
            &owner,
            &pg,
            AuthzContext::single_owner(&owner, AuthPath::System),
        ),
        SetReadScopeArgs {
            personality: reader.into_inner().to_string(),
            readable_personalities: vec![readable.into_inner().to_string()],
        },
    )
    .await?;
    assert_eq!(
        pg.list_read_scope(&ListReadScopeRequest {
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
            reader_personality_instance_id: reader,
        })
        .await?
        .readable_personality_instance_ids,
        vec![readable]
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}
