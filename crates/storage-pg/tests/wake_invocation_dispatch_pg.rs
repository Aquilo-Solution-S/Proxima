//! Phase 1d: Wake invocation dispatch columns survive INSERT/UPDATE
//! roundtrip via the storage trait.

mod common;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::personality::{
    InstantiatePersonalityRequest, ListWakeInvocationsRequest, PersonalityInstanceId,
    SetWakeEntriesRequest, WakeEntryAuthoredBy, WakeEntryDraft, WakeEntryTriggerKind,
    WakeInvocationFinalize, WakeInvocationLogDraft, WakeInvocationLogStatus, WakeInvocationStart,
    WakeInvocationStatus,
};
use proxima_core::storage::Storage;
use proxima_core::{ModelTier, Owner, Principal};
use uuid::Uuid;

#[derive(Debug)]
struct WakeInvocationDispatchRow {
    wake_token: Option<Uuid>,
    resolved_inference_target_ref: Option<String>,
    failure_reason: Option<String>,
    exit_code: Option<i32>,
    duration_ms: Option<i64>,
    stdout_tail: Option<String>,
    stderr_tail: Option<String>,
    stdout_truncated: bool,
    stderr_truncated: bool,
    status: WakeInvocationStatus,
}

async fn seed_personality_with_entry(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
) -> Result<(PersonalityInstanceId, Uuid), Box<dyn std::error::Error>> {
    let response = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Engineer A".into(),
            purpose: "exercise wake invocation dispatch columns".into(),
        })
        .await?;
    let entry = WakeEntryDraft::new(
        Uuid::now_v7(),
        response.instance_id,
        WakeEntryTriggerKind::OnMemory,
        "proxima-test/fact-v1",
        "on_test_fact",
        WakeEntryAuthoredBy::Any,
        1000,
        ModelTier::Fast,
        Some("primary".to_string()),
        vec!["core/query".to_string()],
        4,
    )
    .expect("valid wake entry");
    let wake_entry_id = entry.wake_entry_id;
    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: response.instance_id,
        entries: vec![entry],
    })
    .await?;
    Ok((response.instance_id, wake_entry_id))
}

async fn fetch_wake_invocation(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    instance_id: PersonalityInstanceId,
    wake_entry_id: Uuid,
    change_event_seq: Uuid,
) -> Result<WakeInvocationDispatchRow, Box<dyn std::error::Error>> {
    type DispatchRow = (
        Option<Uuid>,
        Option<String>,
        Option<String>,
        Option<i32>,
        Option<i64>,
        Option<String>,
        Option<String>,
        bool,
        bool,
        WakeInvocationStatus,
    );
    let principal_id = match owner.principal {
        Principal::User(id) => id.into_inner(),
        Principal::Group(id) => id.into_inner(),
    };
    let (
        wake_token,
        resolved_inference_target_ref,
        failure_reason,
        exit_code,
        duration_ms,
        stdout_tail,
        stderr_tail,
        stdout_truncated,
        stderr_truncated,
        status,
    ): DispatchRow = sqlx::query_as(
        "SELECT wake_token, resolved_inference_target_ref,
                failure_reason, exit_code, duration_ms, stdout_tail,
                stderr_tail, stdout_truncated, stderr_truncated, status
         FROM proxima_core.personality_wake_invocations
         WHERE owner_principal_id = $1
           AND owner_org_id = $2
           AND personality_instance_id = $3
           AND wake_entry_id = $4
           AND change_event_seq = $5",
    )
    .bind(principal_id)
    .bind(owner.org_id.into_inner())
    .bind(instance_id.into_inner())
    .bind(wake_entry_id)
    .bind(change_event_seq)
    .fetch_one(pg.pool())
    .await?;
    Ok(WakeInvocationDispatchRow {
        wake_token,
        resolved_inference_target_ref,
        failure_reason,
        exit_code,
        duration_ms,
        stdout_tail,
        stderr_tail,
        stdout_truncated,
        stderr_truncated,
        status,
    })
}

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)]
async fn wake_invocation_carries_dispatch_columns() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let (instance_id, wake_entry_id) = seed_personality_with_entry(&pg, &owner).await?;
        let change_event_seq = Uuid::now_v7();

        let wake_token = Uuid::new_v4();
        let resolved_target = "default-standard";

        let start = WakeInvocationStart {
            owner: owner.clone(),
            personality_instance_id: instance_id,
            wake_entry_id,
            change_event_seq,
            wake_token,
            resolved_inference_target_ref: resolved_target.to_string(),
        };
        pg.start_wake_invocation(&start).await.expect("start ok");

        let finalize = WakeInvocationFinalize {
            owner: owner.clone(),
            personality_instance_id: instance_id,
            wake_entry_id,
            change_event_seq,
            status: WakeInvocationStatus::Failed,
            turn_count: None,
            cost_usd: None,
            failure_reason: Some("workspace_mode_not_yet_implemented".to_string()),
            exit_code: Some(2),
            duration_ms: Some(123),
            stdout_tail: Some("stdout tail".to_string()),
            stderr_tail: Some("stderr tail".to_string()),
            stdout_truncated: false,
            stderr_truncated: false,
        };
        pg.finalize_wake_invocation(&finalize)
            .await
            .expect("finalize ok");

        let row = fetch_wake_invocation(&pg, &owner, instance_id, wake_entry_id, change_event_seq)
            .await?;
        assert_eq!(row.wake_token, Some(wake_token));
        assert_eq!(
            row.resolved_inference_target_ref.as_deref(),
            Some(resolved_target)
        );
        assert_eq!(
            row.failure_reason.as_deref(),
            Some("workspace_mode_not_yet_implemented")
        );
        assert_eq!(row.exit_code, Some(2));
        assert_eq!(row.duration_ms, Some(123));
        assert_eq!(row.stdout_tail.as_deref(), Some("stdout tail"));
        assert_eq!(row.stderr_tail.as_deref(), Some("stderr tail"));
        assert!(!row.stdout_truncated);
        assert!(!row.stderr_truncated);
        assert!(matches!(row.status, WakeInvocationStatus::Failed));

        pg.append_wake_invocation_log(&WakeInvocationLogDraft {
            owner: owner.clone(),
            personality_instance_id: instance_id,
            wake_entry_id,
            change_event_seq,
            phase: "tool_call".to_string(),
            tool_id: Some("proxima-mcp/proxima_derive".to_string()),
            status: WakeInvocationLogStatus::Failed,
            duration_ms: Some(77),
            message_tail: Some("tool failed".to_string()),
        })
        .await?;
        pg.append_wake_invocation_log(&WakeInvocationLogDraft {
            owner: owner.clone(),
            personality_instance_id: instance_id,
            wake_entry_id,
            change_event_seq,
            phase: "session_artifact".to_string(),
            tool_id: None,
            status: WakeInvocationLogStatus::Started,
            duration_ms: None,
            message_tail: Some(
                "~/.proxima/wake-runs/user/example/worker-session.jsonl".to_string(),
            ),
        })
        .await?;
        let listed = pg
            .list_wake_invocations(&ListWakeInvocationsRequest {
                owner: owner.clone(),
                personality_instance_id: instance_id,
                wake_entry_id: Some(wake_entry_id),
                limit: 10,
            })
            .await?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].exit_code, Some(2));
        assert_eq!(listed[0].stdout_tail.as_deref(), Some("stdout tail"));
        assert_eq!(listed[0].logs.len(), 2);
        assert_eq!(
            listed[0].logs[0].tool_id.as_deref(),
            Some("proxima-mcp/proxima_derive")
        );
        assert_eq!(
            listed[0].logs[0].message_tail.as_deref(),
            Some("tool failed")
        );
        assert_eq!(listed[0].logs[1].phase, "session_artifact");
        assert_eq!(listed[0].logs[1].status, WakeInvocationLogStatus::Started);
        assert_eq!(
            listed[0].logs[1].message_tail.as_deref(),
            Some("~/.proxima/wake-runs/user/example/worker-session.jsonl")
        );
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("wake invocation dispatch columns roundtrip failed");
}
