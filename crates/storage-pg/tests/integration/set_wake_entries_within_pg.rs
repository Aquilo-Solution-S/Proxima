//! PG coverage for `set_wake_entries_within` R-M-W primitive.

use crate::common::{drop_db, fresh_pg};
use proxima_core::storage::Storage;
use proxima_core::{
    InstantiatePersonalityRequest, OwnerRef, SetWakeEntriesRequest, UserId, WakeEntryAuthoredBy,
    WakeEntryDraft, WakeEntryTriggerKind,
};
use std::sync::Arc;
use std::time::Duration;
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
        authored_by: WakeEntryAuthoredBy::Any,
        probability_promille: 1000,
        goal_scope: proxima_core::WakeEntryGoalScope::None,
        instructions: String::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn set_wake_entries_within_appends_one() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let inst = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            principal: owner,
            display_name: "test".into(),
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

    drop(pg);
    drop_db(&db).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn set_wake_entries_within_preserves_carried_entry_id()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let inst = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            principal: owner,
            display_name: "test".into(),
        })
        .await?;

    let pid = inst.instance_id;
    let mut first = wake_entry(pid, "core/personality_config_changed_v1", "first");
    first.instructions = "carry this instruction body".into();
    let first_id = first.wake_entry_id;
    pg.set_wake_entries_within(&owner, pid, Box::new(move |_| Ok(vec![first.clone()])))
        .await?;

    pg.set_wake_entries_within(
        &owner,
        pid,
        Box::new(move |current: &[WakeEntryDraft]| {
            assert_eq!(current.len(), 1);
            assert_eq!(current[0].wake_entry_id, first_id);
            assert_eq!(current[0].instructions, "carry this instruction body");
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
    assert!(row.wake_entries.iter().any(|e| e.wake_entry_id == first_id
        && e.label == "first carried"
        && e.instructions == "carry this instruction body"));

    drop(pg);
    drop_db(&db).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_replace_all_keeps_only_last_writer_entries()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let inst = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            principal: owner,
            display_name: "test".into(),
        })
        .await?;
    let pid = inst.instance_id;

    pg.set_wake_entries(&SetWakeEntriesRequest {
        principal: owner,
        personality_instance_id: pid,
        entries: vec![wake_entry(
            pid,
            "core/personality_config_changed_v1",
            "seed",
        )],
    })
    .await?;

    sqlx::query(
        "CREATE OR REPLACE FUNCTION proxima_core.wake_entry_insert_sleep_for_test()
         RETURNS trigger
         LANGUAGE plpgsql
         AS $$
         BEGIN
             PERFORM pg_sleep(0.20);
             RETURN NEW;
         END
         $$;",
    )
    .execute(pg.pool())
    .await?;
    sqlx::query(
        "DROP TRIGGER IF EXISTS wake_entry_insert_sleep_for_test
             ON proxima_core.personality_wake_entries;",
    )
    .execute(pg.pool())
    .await?;
    sqlx::query(
        "CREATE TRIGGER wake_entry_insert_sleep_for_test
         BEFORE INSERT ON proxima_core.personality_wake_entries
         FOR EACH ROW
         EXECUTE FUNCTION proxima_core.wake_entry_insert_sleep_for_test();",
    )
    .execute(pg.pool())
    .await?;

    let pg = Arc::new(pg);
    let first_req = SetWakeEntriesRequest {
        principal: owner,
        personality_instance_id: pid,
        entries: vec![wake_entry(
            pid,
            "proxima-code/execution-request-v1",
            "first",
        )],
    };
    let second_req = SetWakeEntriesRequest {
        principal: owner,
        personality_instance_id: pid,
        entries: vec![wake_entry(pid, "proxima-code/file-revision-v1", "second")],
    };

    let first = tokio::spawn({
        let pg = pg.clone();
        async move { pg.set_wake_entries(&first_req).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let second = tokio::spawn({
        let pg = pg.clone();
        async move { pg.set_wake_entries(&second_req).await }
    });

    first.await??;
    second.await??;

    let rows = pg.list_personality_instances(&owner, false).await?;
    let row = rows
        .into_iter()
        .find(|r| r.personality_instance_id == pid)
        .expect("found");
    assert_eq!(
        row.wake_entries.len(),
        1,
        "replace-all must leave the last writer's set, not a union"
    );
    assert_eq!(row.wake_entries[0].label, "second");
    assert_eq!(
        row.wake_entries[0].trigger_id,
        "proxima-code/file-revision-v1"
    );

    let pg = Arc::try_unwrap(pg).expect("test storage refs dropped");
    drop(pg);
    drop_db(&db).await?;
    Ok(())
}
