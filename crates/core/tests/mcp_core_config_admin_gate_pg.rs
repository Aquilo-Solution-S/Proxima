#![cfg(any())] // QUARANTINED pending group-ownership port (grant-era access setup); re-enable in Phase 3/6
mod common;

use std::collections::HashSet;
use std::sync::Arc;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::access::GrantSubject;
use proxima_core::authz::{AuthPath, AuthzContext, CapabilitySet, Identity, ToolScope};
use proxima_core::mcp::core_tools::add_wake_entry::AddWakeEntryArgs;
use proxima_core::mcp::core_tools::personality::{CorePersonalityArgs, CorePersonalityTool};
use proxima_core::mcp::core_tools::remove_wake_entry::RemoveWakeEntryArgs;
use proxima_core::mcp::core_tools::set_read_scope::SetReadScopeArgs;
use proxima_core::mcp::core_tools::update_wake_entry::{UpdateWakeEntryArgs, WakeEntryPatch};
use proxima_core::mcp::core_tools::wake::{CoreWakeArgs, CoreWakeOutput, CoreWakeTool};
use proxima_core::mcp::core_tools::wake_entry_input::WakeEntryDraftInput;
use proxima_core::mcp::{McpAuthorContext, McpTool, McpToolCtx, McpToolExtensions, OutputMode};
use proxima_core::personality::{
    InstantiatePersonalityRequest, ListReadScopeRequest, SetWakeEntriesRequest,
};
use proxima_core::storage::Storage;
use proxima_core::{
    AccessScope, Engine, FlavorRegistry, GrantResource, NewAccessGrant, Owner,
    PersonalityInstanceId, Principal, Relation, UserId, WakeEntryAuthoredBy, WakeEntryDraft,
    WakeEntryTriggerKind,
};
use uuid::Uuid;

async fn seed_space_grant(
    pg: &proxima_storage_pg::PgStorage,
    space_owner: &Principal,
    relation: Relation,
    subject: &Principal,
) {
    pg.insert_space_binding(&NewAccessGrant {
        space_owner: space_owner.clone(),
        resource: GrantResource::Space,
        relation,
        subject: GrantSubject::Principal(subject.clone()),
        granted_by: PersonalityInstanceId::new(Uuid::now_v7()),
    })
    .await
    .expect("seed grant");
}

async fn non_admin_authz(pg: &proxima_storage_pg::PgStorage, owner: &Owner) -> AuthzContext {
    let editor_user = Principal::User(UserId::new(Uuid::now_v7()));
    seed_space_grant(pg, owner, Relation::Editor, &editor_user).await;
    AuthzContext {
        identity: Identity {
            principal: editor_user,
            accessible_principals: HashSet::new(),
            expires_at: None,
            auth_epoch: 0,
        },
        capabilities: CapabilitySet {
            tool_scope: ToolScope::All,
            access: AccessScope::Granted,
        },
        auth_path: AuthPath::HostBearer,
    }
}

fn ctx(owner: &Owner, pg: &proxima_storage_pg::PgStorage, authz: AuthzContext) -> McpToolCtx {
    let registry = FlavorRegistry::default().freeze();
    let engine = Engine::new(registry.clone()).with_storage(pg.clone().into_handle());
    McpToolCtx {
        owner: owner.clone(),
        authz,
        handles: None,
        mode: OutputMode::RawIds,
        registry: Arc::new(registry),
        author: McpAuthorContext {
            model_id: "test-model".into(),
            client_name: "test-client".into(),
            client_version: "0".into(),
            personality_instance_id: None,
            caller_self_perspective: None,
        },
        caller_self_perspective: None,
        master_token_id: None,
        extensions: McpToolExtensions::with(pg.pool().clone()),
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
            principal: owner.clone(),
            display_name: display_name.into(),
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
    assert!(err.to_string().contains("requires admin on this owner"));
}

#[tokio::test]
async fn add_wake_entry_requires_admin_and_preserves_storage_on_denial()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
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
            authored_by: WakeEntryAuthoredBy::Any,
            probability_promille: 1000,
            goal_scope: proxima_core::WakeEntryGoalScope::None,
            instructions: String::new(),
        },
    };

    let err = CoreWakeTool::call(
        ctx(&owner, &pg, non_admin_authz(&pg, &owner).await),
        CoreWakeArgs::Add(args),
    )
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
            authored_by: WakeEntryAuthoredBy::Any,
            probability_promille: 1000,
            goal_scope: proxima_core::WakeEntryGoalScope::None,
            instructions: String::new(),
        },
    };
    CoreWakeTool::call(
        ctx(
            &owner,
            &pg,
            AuthzContext::single_owner(&owner, AuthPath::System),
        ),
        CoreWakeArgs::Add(admin_args),
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
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let pid = instantiate(&pg, &owner, "update gate").await?;
    let wid = Uuid::now_v7();
    pg.set_wake_entries(&SetWakeEntriesRequest {
        principal: owner.clone(),
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
    let err = CoreWakeTool::call(
        ctx(&owner, &pg, non_admin_authz(&pg, &owner).await),
        CoreWakeArgs::Update(args),
    )
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
    CoreWakeTool::call(
        ctx(
            &owner,
            &pg,
            AuthzContext::single_owner(&owner, AuthPath::System),
        ),
        CoreWakeArgs::Update(admin_args),
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
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let pid = instantiate(&pg, &owner, "remove gate").await?;
    let wid = Uuid::now_v7();
    pg.set_wake_entries(&SetWakeEntriesRequest {
        principal: owner.clone(),
        personality_instance_id: pid,
        entries: vec![wake_entry(wid, pid, "remove-me", "test/remove")],
    })
    .await?;

    let args = RemoveWakeEntryArgs {
        wake_entry: wid.to_string(),
    };
    let err = CoreWakeTool::call(
        ctx(&owner, &pg, non_admin_authz(&pg, &owner).await),
        CoreWakeArgs::Remove(args),
    )
    .await
    .expect_err("non-admin remove must be denied");
    assert_admin_denied(&err);
    assert_eq!(wake_labels(&pg, &owner, pid).await?, vec!["remove-me"]);

    let output = CoreWakeTool::call(
        ctx(
            &owner,
            &pg,
            AuthzContext::single_owner(&owner, AuthPath::System),
        ),
        CoreWakeArgs::Remove(RemoveWakeEntryArgs {
            wake_entry: wid.to_string(),
        }),
    )
    .await?;
    let CoreWakeOutput::Remove(output) = output else {
        panic!("expected remove output");
    };
    assert!(output.removed);
    assert!(wake_labels(&pg, &owner, pid).await?.is_empty());

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn set_read_scope_requires_admin_and_preserves_storage_on_denial()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let reader = instantiate(&pg, &owner, "reader").await?;
    let readable = instantiate(&pg, &owner, "readable").await?;

    let args = SetReadScopeArgs {
        personality: reader.into_inner().to_string(),
        readable_personalities: vec![readable.into_inner().to_string()],
    };
    let err = CorePersonalityTool::call(
        ctx(&owner, &pg, non_admin_authz(&pg, &owner).await),
        CorePersonalityArgs::SetReadScope(args),
    )
    .await
    .expect_err("non-admin set_read_scope must be denied");
    assert_admin_denied(&err);
    assert!(
        pg.list_read_scope(&ListReadScopeRequest {
            principal: owner.clone(),
            reader_personality_instance_id: reader,
        })
        .await?
        .readable_personality_instance_ids
        .is_empty()
    );

    CorePersonalityTool::call(
        ctx(
            &owner,
            &pg,
            AuthzContext::single_owner(&owner, AuthPath::System),
        ),
        CorePersonalityArgs::SetReadScope(SetReadScopeArgs {
            personality: reader.into_inner().to_string(),
            readable_personalities: vec![readable.into_inner().to_string()],
        }),
    )
    .await?;
    assert_eq!(
        pg.list_read_scope(&ListReadScopeRequest {
            principal: owner.clone(),
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
