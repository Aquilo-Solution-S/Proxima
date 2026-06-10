use super::{
    CHAT_END_REQUESTED_SCHEMA_ID, CHAT_MESSAGE_SCHEMA_ID, ChatEndRequestedV1, ChatMessageV1,
    ChatTargetOutput, ChatWakeEntryOutput, McpToolCtx, McpToolError, MemoryId,
    PersonalityInstanceId, PersonalityStatus, WakeEntryRow, WakeEntryTriggerKind, chat_storage,
    load_thread_ended, load_thread_started,
};

#[derive(Clone)]
pub(super) struct ChatTarget {
    pub(super) personality_instance_id: PersonalityInstanceId,
    pub(super) root_perspective: MemoryId,
}

pub(super) async fn list_chat_targets(
    ctx: &McpToolCtx,
    caller_self: Option<MemoryId>,
    include_self: bool,
) -> Result<Vec<ChatTargetOutput>, McpToolError> {
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let rows = storage
        .list_personality_instances(&ctx.owner, false)
        .await
        .map_err(McpToolError::Storage)?;
    let mut targets = Vec::new();
    for row in rows {
        if row.status != PersonalityStatus::Active {
            continue;
        }
        if !include_self && Some(row.current_root_perspective_memory_id) == caller_self {
            continue;
        }
        let wake_entries: Vec<_> = row
            .wake_entries
            .iter()
            .filter(|entry| is_enabled_chat_message_wake(entry))
            .map(|entry| ChatWakeEntryOutput {
                wake_entry: ctx.format_wake_entry(entry.wake_entry_id),
                label: entry.label.clone(),
            })
            .collect();
        if wake_entries.is_empty() {
            continue;
        }
        targets.push(ChatTargetOutput {
            personality: ctx.format_personality(row.personality_instance_id),
            display_name: row.display_name,
            root_perspective: ctx.format_perspective_memory(row.current_root_perspective_memory_id),
            chat_message_wake_entries: wake_entries,
        });
    }
    Ok(targets)
}

pub(super) async fn resolve_chat_target(
    ctx: &McpToolCtx,
    target_personality: PersonalityInstanceId,
) -> Result<ChatTarget, McpToolError> {
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let rows = storage
        .list_personality_instances(&ctx.owner, false)
        .await
        .map_err(McpToolError::Storage)?;
    let row = rows
        .into_iter()
        .find(|row| row.personality_instance_id == target_personality)
        .ok_or_else(|| McpToolError::InvalidInput("target personality is not visible".into()))?;
    if row.status != PersonalityStatus::Active {
        return Err(McpToolError::InvalidInput(
            "target personality is not active".into(),
        ));
    }
    let wake_entries: Vec<_> = row
        .wake_entries
        .into_iter()
        .filter(is_enabled_chat_message_wake)
        .collect();
    if wake_entries.is_empty() {
        return Err(McpToolError::InvalidInput(
            "target personality has no enabled chat-message wake entry".into(),
        ));
    }
    Ok(ChatTarget {
        personality_instance_id: row.personality_instance_id,
        root_perspective: row.current_root_perspective_memory_id,
    })
}

pub(super) async fn resolve_end_request_target(
    ctx: &McpToolCtx,
    personality_instance_id: PersonalityInstanceId,
) -> Result<ChatTarget, McpToolError> {
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let rows = storage
        .list_personality_instances(&ctx.owner, false)
        .await
        .map_err(McpToolError::Storage)?;
    let row = rows
        .into_iter()
        .find(|row| {
            row.personality_instance_id == personality_instance_id
                && row.status == PersonalityStatus::Active
        })
        .ok_or_else(|| McpToolError::InvalidInput("target personality is not active".into()))?;
    let wake_entries: Vec<_> = row
        .wake_entries
        .iter()
        .filter(|entry| is_enabled_chat_end_request_wake(entry))
        .collect();
    if wake_entries.is_empty() {
        return Err(McpToolError::InvalidInput(
            "target personality has no enabled chat-end-request wake entry".into(),
        ));
    }
    Ok(ChatTarget {
        personality_instance_id: row.personality_instance_id,
        root_perspective: row.current_root_perspective_memory_id,
    })
}

pub(super) fn is_enabled_chat_message_wake(entry: &WakeEntryRow) -> bool {
    entry.enabled
        && entry.trigger_kind == WakeEntryTriggerKind::OnMemory
        && entry.trigger_id == CHAT_MESSAGE_SCHEMA_ID
}

pub(super) fn is_enabled_chat_end_request_wake(entry: &WakeEntryRow) -> bool {
    entry.enabled
        && entry.trigger_kind == WakeEntryTriggerKind::OnMemory
        && entry.trigger_id == CHAT_END_REQUESTED_SCHEMA_ID
}

pub(super) async fn resolve_personality_for_self(
    ctx: &McpToolCtx,
    caller_self: MemoryId,
) -> Result<PersonalityInstanceId, McpToolError> {
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let rows = storage
        .list_personality_instances(&ctx.owner, false)
        .await
        .map_err(McpToolError::Storage)?;
    rows.into_iter()
        .find(|row| {
            row.status == PersonalityStatus::Active
                && row.current_root_perspective_memory_id == caller_self
        })
        .map(|row| row.personality_instance_id)
        .ok_or_else(|| {
            McpToolError::InvalidInput(
                "caller_self_perspective is not an active personality".into(),
            )
        })
}

pub(super) async fn load_message(
    ctx: &McpToolCtx,
    message_memory_id: MemoryId,
) -> Result<ChatMessageV1, McpToolError> {
    chat_storage(ctx)?
        .chat_load_message(&ctx.owner, message_memory_id)
        .await?
        .ok_or_else(|| McpToolError::InvalidInput("chat message is not visible".into()))
}

pub(super) async fn load_chat_parent_thread_key(
    ctx: &McpToolCtx,
    parent_memory_id: MemoryId,
) -> Result<String, McpToolError> {
    chat_storage(ctx)?
        .chat_parent_thread_key(&ctx.owner, parent_memory_id)
        .await?
        .ok_or_else(|| {
            McpToolError::InvalidInput(
                "chat parent must be a visible chat message or chat reply Fact".into(),
            )
        })
}

pub(super) async fn load_end_request(
    ctx: &McpToolCtx,
    request_memory_id: MemoryId,
) -> Result<ChatEndRequestedV1, McpToolError> {
    chat_storage(ctx)?
        .chat_load_end_request(&ctx.owner, request_memory_id)
        .await?
        .ok_or_else(|| McpToolError::InvalidInput("chat end request is not visible".into()))
}

pub(super) async fn load_started_target(
    ctx: &McpToolCtx,
    thread_key: &str,
) -> Result<PersonalityInstanceId, McpToolError> {
    load_thread_started(ctx, thread_key)
        .await?
        .map(|started| PersonalityInstanceId::new(started.payload.target_personality_instance_id))
        .ok_or_else(|| {
            McpToolError::InvalidInput(
                "target_personality is required for a thread without core/start_chat".into(),
            )
        })
}

pub(super) async fn thread_is_ended(
    ctx: &McpToolCtx,
    thread_key: &str,
) -> Result<bool, McpToolError> {
    Ok(load_thread_ended(ctx, thread_key).await?.is_some())
}
