//! PG coverage for `set_wake_entries_within` R-M-W primitive.

mod common;

use common::{drop_db, fresh_pg};
use proxima_core::storage::Storage;
use proxima_core::{
    InstantiatePersonalityRequest, ModelTier, OrgId, Owner, Principal, UserId, WakeEntryAuthoredBy,
    WakeEntryDraft, WakeEntryTriggerKind, WakeExecutionMode,
};
use uuid::Uuid;

fn wake_entry(
    pid: proxima_core::PersonalityInstanceId,
    trigger_id: &str,
    label: &str,
) -> WakeEntryDraft {
    WakeEntryDraft {
        wake_entry_id: Uuid::now_v7(),
        personality_instance_id: pid,
        trigger_kind: WakeEntryTriggerKind::OnMemory,
        trigger_id: trigger_id.to_string(),
        label: label.to_string(),
        enabled: true,
        execution_mode: WakeExecutionMode::SubstrateOnly,
        authored_by: WakeEntryAuthoredBy::Any,
        probability_promille: 1000,
        goal_scope: proxima_core::WakeEntryGoalScope::None,
        recipe_ref: "proxima-code/engineer".to_string(),
        model_tier: ModelTier::Standard,
        inference_target_ref: None,
        substrate_tool_palette: vec![],
        workspace_tool_palette: vec![],
        max_rounds: 3,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn set_wake_entries_within_appends_one() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;

    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    };
    let inst = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "test".into(),
            purpose: "rmw fixture".into(),
        })
        .await?;

    let pid = inst.instance_id;
    let mutator: proxima_core::WakeEntriesMutator =
        Box::new(move |current: &[WakeEntryDraft]| {
            assert!(current.is_empty(), "fresh personality has no entries");
            Ok(vec![wake_entry(
                pid,
                "core/personality_config_changed_v1",
                "rmw-test",
            )])
        });

    pg.set_wake_entries_within(&owner, pid, mutator).await?;

    let rows = pg.list_personality_instances(&owner, false).await?;
    let row = rows
        .into_iter()
        .find(|r| r.personality_instance_id == pid)
        .expect("found");
    assert_eq!(row.wake_entries.len(), 1);
    assert_eq!(row.wake_entries[0].label, "rmw-test");

    drop_db(&db).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn set_wake_entries_within_preserves_carried_entry_id()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;

    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    };
    let inst = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "test".into(),
            purpose: "rmw fixture".into(),
        })
        .await?;

    let pid = inst.instance_id;
    let first = wake_entry(pid, "core/personality_config_changed_v1", "first");
    let first_id = first.wake_entry_id;
    pg.set_wake_entries_within(&owner, pid, Box::new(move |_| Ok(vec![first.clone()])))
        .await?;

    pg.set_wake_entries_within(
        &owner,
        pid,
        Box::new(move |current: &[WakeEntryDraft]| {
            assert_eq!(current.len(), 1);
            assert_eq!(current[0].wake_entry_id, first_id);
            let mut carried = current[0].clone();
            carried.label = "first carried".into();
            Ok(vec![
                carried,
                wake_entry(pid, "proxima-code/execution-request-v1", "second"),
            ])
        }),
    )
    .await?;

    let rows = pg.list_personality_instances(&owner, false).await?;
    let row = rows
        .into_iter()
        .find(|r| r.personality_instance_id == pid)
        .expect("found");
    assert_eq!(row.wake_entries.len(), 2);
    assert!(
        row.wake_entries
            .iter()
            .any(|e| e.wake_entry_id == first_id && e.label == "first carried")
    );

    drop_db(&db).await?;
    Ok(())
}
