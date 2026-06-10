mod chat_support;
mod common;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::mcp::core_tools::{ListWakeInvocationsArgs, ListWakeInvocationsTool};
use proxima_core::{
    CHAT_MESSAGE_SCHEMA_ID, CompactChatThreadTool, EmitChatMessageTool, EmitChatReplyTool,
    EndChatTool, GetChatThreadTool, McpTool, RequestEndChatTool, SetWakeEntriesRequest,
    StartChatTool, Storage, WakeInvocationFinalize, WakeInvocationLogDraft,
    WakeInvocationLogStatus, WakeInvocationStart, WakeInvocationStatus,
};
use uuid::Uuid;

use chat_support::*;
#[tokio::test]
#[expect(clippy::too_many_lines, reason = "linear chat-lifecycle e2e fixture")]
async fn list_wake_invocations_filters_by_triggering_chat_message()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let shell = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Shell".into(),
            purpose: "Drive chat".into(),
        })
        .await?;
    let mira = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Mira".into(),
            purpose: "Reply to chat".into(),
        })
        .await?;
    let rows = pg.list_personality_instances(&owner, false).await?;
    let shell_self = self_perspective(&rows, shell.instance_id);

    let mira_wake = wake(
        mira.instance_id,
        CHAT_MESSAGE_SCHEMA_ID,
        "reply-chat-message",
        vec!["core/emit_chat_reply".into()],
    );
    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: mira.instance_id,
        entries: vec![mira_wake.clone()],
    })
    .await?;

    let message = EmitChatMessageTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::EmitChatMessageArgs {
            target_personality: mira.instance_id.into_inner().to_string(),
            thread_key: "wake-status-thread".into(),
            message: "This message should have observable wake status.".into(),
            parent: None,
            context_memories: Vec::new(),
            context_goals: Vec::new(),
            idempotency_key: "wake-status-message".into(),
        },
    )
    .await?;
    let message_id = Uuid::parse_str(&message.handle)?;
    let change_event_seq: Uuid =
        sqlx::query_scalar("SELECT seq FROM proxima_core.change_event WHERE entity_memory_id = $1")
            .bind(message_id)
            .fetch_one(pg.pool())
            .await?;

    let invocation_id = Uuid::now_v7();
    pg.start_wake_invocation(&WakeInvocationStart {
        invocation_id,
        owner: owner.clone(),
        personality_instance_id: mira.instance_id,
        wake_entry_id: mira_wake.wake_entry_id,
        change_event_seq,
        wake_token: Uuid::new_v4(),
        resolved_inference_target_ref: "test-target".into(),
        continuation: None,
    })
    .await?;
    pg.finalize_wake_invocation(&WakeInvocationFinalize {
        invocation_id,
        owner: owner.clone(),
        personality_instance_id: mira.instance_id,
        wake_entry_id: mira_wake.wake_entry_id,
        change_event_seq,
        status: WakeInvocationStatus::Succeeded,
        turn_count: Some(2),
        cost_usd: Some(0.0),
        failure_reason: None,
        exit_code: Some(0),
        duration_ms: Some(42),
        stdout_tail: Some("ok".into()),
        stderr_tail: None,
        stdout_truncated: false,
        stderr_truncated: false,
    })
    .await?;
    pg.append_wake_invocation_log(&WakeInvocationLogDraft {
        invocation_id,
        owner: owner.clone(),
        personality_instance_id: mira.instance_id,
        wake_entry_id: mira_wake.wake_entry_id,
        change_event_seq,
        phase: "tool_call".into(),
        tool_id: Some("core/emit_chat_reply".into()),
        status: WakeInvocationLogStatus::Succeeded,
        duration_ms: Some(12),
        message_tail: Some("reply emitted".into()),
    })
    .await?;

    let out = ListWakeInvocationsTool::call(
        ctx(&pg, owner.clone(), shell_self),
        ListWakeInvocationsArgs {
            personality: mira.instance_id.into_inner().to_string(),
            wake_entry: Some(mira_wake.wake_entry_id.to_string()),
            triggering_memory: Some(message.handle),
            change_event_seq: None,
            limit: None,
        },
    )
    .await?;
    assert_eq!(out.invocations.len(), 1);
    assert_eq!(out.invocations[0].invocation_id, invocation_id.to_string());
    assert_eq!(out.invocations[0].status, "succeeded");
    assert_eq!(out.invocations[0].turn_count, 2);
    assert_eq!(out.invocations[0].logs.len(), 1);
    assert_eq!(
        out.invocations[0].logs[0].tool_id.as_deref(),
        Some("core/emit_chat_reply")
    );

    let by_seq = ListWakeInvocationsTool::call(
        ctx(&pg, owner.clone(), shell_self),
        ListWakeInvocationsArgs {
            personality: mira.instance_id.into_inner().to_string(),
            wake_entry: None,
            triggering_memory: None,
            change_event_seq: Some(change_event_seq.to_string()),
            limit: Some(10),
        },
    )
    .await?;
    assert_eq!(by_seq.invocations.len(), 1);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "linear chat-lifecycle e2e fixture")]
async fn compact_chat_thread_writes_abstraction_and_can_be_context()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let shell = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Shell".into(),
            purpose: "Compact chat threads".into(),
        })
        .await?;
    let mira = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Mira".into(),
            purpose: "Reply before compaction".into(),
        })
        .await?;
    let rows = pg.list_personality_instances(&owner, false).await?;
    let shell_self = self_perspective(&rows, shell.instance_id);
    let mira_self = self_perspective(&rows, mira.instance_id);

    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: mira.instance_id,
        entries: vec![wake(
            mira.instance_id,
            CHAT_MESSAGE_SCHEMA_ID,
            "reply-chat-message",
            vec!["core/emit_chat_reply".into()],
        )],
    })
    .await?;

    let started = StartChatTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::StartChatArgs {
            target_personality: mira.instance_id.into_inner().to_string(),
            thread_key: Some("compact-chat-roundtrip".into()),
            title: Some("Compact chat roundtrip".into()),
            message: "We need a durable mid-chat compaction.".into(),
            context_memories: Vec::new(),
            context_goals: Vec::new(),
            idempotency_key: "compact-chat-start".into(),
        },
    )
    .await?;
    let reply = EmitChatReplyTool::call(
        ctx(&pg, owner.clone(), mira_self),
        proxima_core::EmitChatReplyArgs {
            reply_to: started.message.clone(),
            reply: "The compaction should be an Abstraction with provenance.".into(),
            context_memories_used: vec![started.message.clone()],
            idempotency_key: "compact-chat-reply".into(),
        },
    )
    .await?;

    let compacted = CompactChatThreadTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::CompactChatThreadArgs {
            thread_key: Some("compact-chat-roundtrip".into()),
            anchor: None,
            summary: "Compaction: the thread established that mid-chat compaction is a typed Abstraction with provenance.".into(),
            source_memories: Vec::new(),
            context_memories_used: vec![started.message.clone(), reply.handle.clone()],
            idempotency_key: "compact-chat-1".into(),
        },
    )
    .await?;
    assert!(!compacted.idempotent_replay);
    assert!(!compacted.provenance_edge_handles.is_empty());

    let compaction_kind: Option<proxima_core::EntityKind> =
        sqlx::query_scalar("SELECT kind FROM proxima_core.memories WHERE memory_id = $1")
            .bind(Uuid::parse_str(&compacted.compaction)?)
            .fetch_one(pg.pool())
            .await?;
    assert_eq!(compaction_kind, Some(proxima_core::EntityKind::Abstraction));

    let continued = EmitChatMessageTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::EmitChatMessageArgs {
            target_personality: mira.instance_id.into_inner().to_string(),
            thread_key: "compact-chat-roundtrip".into(),
            message: "Continue using the compaction context.".into(),
            parent: Some(started.message),
            context_memories: vec![compacted.compaction.clone()],
            context_goals: Vec::new(),
            idempotency_key: "compact-chat-continue".into(),
        },
    )
    .await?;
    assert!(continued.target_edge_handle.is_some());

    let thread = GetChatThreadTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::GetChatThreadArgs {
            thread_key: Some("compact-chat-roundtrip".into()),
            anchor: None,
            limit: None,
        },
    )
    .await?;
    assert_eq!(thread.compactions.len(), 1);
    assert_eq!(thread.compactions[0].handle, compacted.compaction);
    assert!(thread.compactions[0].summary.contains("typed Abstraction"));
    assert_eq!(
        thread
            .messages
            .iter()
            .find(|message| message.handle == continued.handle)
            .expect("continued message")
            .context_memories,
        vec![compacted.compaction]
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "linear chat-lifecycle e2e fixture")]
async fn end_chat_requires_target_and_writes_summary_abstraction()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let shell = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Shell".into(),
            purpose: "Request chat closure".into(),
        })
        .await?;
    let mira = pg
        .instantiate_personality(&proxima_core::InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Mira".into(),
            purpose: "Summarize ended chats".into(),
        })
        .await?;
    let rows = pg.list_personality_instances(&owner, false).await?;
    let shell_self = self_perspective(&rows, shell.instance_id);
    let mira_self = self_perspective(&rows, mira.instance_id);

    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: mira.instance_id,
        entries: vec![
            wake(
                mira.instance_id,
                CHAT_MESSAGE_SCHEMA_ID,
                "reply-chat-message",
                vec!["core/emit_chat_reply".into()],
            ),
            wake(
                mira.instance_id,
                proxima_core::CHAT_END_REQUESTED_SCHEMA_ID,
                "summarize-chat-end",
                vec!["core/end_chat".into()],
            ),
        ],
    })
    .await?;

    let started = StartChatTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::StartChatArgs {
            target_personality: mira.instance_id.into_inner().to_string(),
            thread_key: Some("end-chat-roundtrip".into()),
            title: Some("End chat roundtrip".into()),
            message: "Please capture the main decision when this closes.".into(),
            context_memories: Vec::new(),
            context_goals: Vec::new(),
            idempotency_key: "end-chat-start".into(),
        },
    )
    .await?;
    let reply = EmitChatReplyTool::call(
        ctx(&pg, owner.clone(), mira_self),
        proxima_core::EmitChatReplyArgs {
            reply_to: started.message.clone(),
            reply: "The main decision is to close chats via a target-authored summary.".into(),
            context_memories_used: vec![started.message.clone()],
            idempotency_key: "end-chat-reply".into(),
        },
    )
    .await?;
    let request = RequestEndChatTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::RequestEndChatArgs {
            thread_key: Some("end-chat-roundtrip".into()),
            anchor: None,
            target_personality: None,
            reason: Some("The roundtrip has enough content to summarize.".into()),
            idempotency_key: "end-chat-request".into(),
        },
    )
    .await?;
    assert!(request.target_edge_handle.is_some());

    let wrong_caller = EndChatTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::EndChatArgs {
            request: request.handle.clone(),
            summary: "Shell must not be allowed to summarize Mira's request.".into(),
            context_memories_used: Vec::new(),
            idempotency_key: "end-chat-wrong".into(),
        },
    )
    .await
    .expect_err("only addressed personality can end chat");
    assert!(
        wrong_caller
            .to_string()
            .contains("addressed end-chat target")
    );

    let ended = EndChatTool::call(
        ctx(&pg, owner.clone(), mira_self),
        proxima_core::EndChatArgs {
            request: request.handle.clone(),
            summary: "Mira summary: the chat established target-authored closure and summary as an Abstraction.".into(),
            context_memories_used: vec![
                started.message.clone(),
                reply.handle.clone(),
                mira_self.into_inner().to_string(),
            ],
            idempotency_key: "end-chat-finish".into(),
        },
    )
    .await?;
    assert!(!ended.provenance_edge_handles.is_empty());

    let summary_kind: Option<proxima_core::EntityKind> =
        sqlx::query_scalar("SELECT kind FROM proxima_core.memories WHERE memory_id = $1")
            .bind(Uuid::parse_str(&ended.summary)?)
            .fetch_one(pg.pool())
            .await?;
    assert_eq!(summary_kind, Some(proxima_core::EntityKind::Abstraction));

    let thread = GetChatThreadTool::call(
        ctx(&pg, owner.clone(), shell_self),
        proxima_core::GetChatThreadArgs {
            thread_key: Some("end-chat-roundtrip".into()),
            anchor: None,
            limit: None,
        },
    )
    .await?;
    assert_eq!(thread.end_requests.len(), 1);
    assert_eq!(
        thread.ended.expect("ended projection").summary,
        ended.summary
    );
    assert_eq!(thread.summaries.len(), 1);
    assert!(
        thread.summaries[0]
            .summary
            .contains("target-authored closure")
    );
    assert!(
        thread
            .edges
            .iter()
            .any(|edge| edge.relation == "core/receives-chat-end-request")
    );
    assert!(
        thread
            .edges
            .iter()
            .any(|edge| edge.relation == "core/derived-from")
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}
