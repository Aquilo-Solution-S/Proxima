use super::*;

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
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let row: Option<(
        String,
        String,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        Option<uuid::Uuid>,
        Vec<uuid::Uuid>,
        Vec<uuid::Uuid>,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT q.thread_key, q.message, q.target_personality_instance_id,
                q.target_self_perspective_memory_id, q.sent_by_self_perspective_memory_id,
                q.parent_memory_id, q.context_memory_ids, q.context_goal_ids,
                q.idempotency_key, q.sent_at
           FROM proxima_core.chat_message_v1 q
           JOIN proxima_core.memories m USING (memory_id)
          WHERE q.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(message_memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_sql)?;
    let Some((
        thread_key,
        message,
        target_personality_instance_id,
        target_self_perspective_memory_id,
        sent_by_self_perspective_memory_id,
        parent_memory_id,
        context_memory_ids,
        context_goal_ids,
        idempotency_key,
        sent_at,
    )) = row
    else {
        return Err(McpToolError::InvalidInput(
            "chat message is not visible".into(),
        ));
    };
    Ok(ChatMessageV1 {
        thread_key,
        message,
        target_personality_instance_id,
        target_self_perspective_memory_id,
        sent_by_self_perspective_memory_id,
        parent_memory_id,
        context_memory_ids,
        context_goal_ids,
        idempotency_key,
        sent_at,
    })
}

pub(super) async fn load_chat_parent_thread_key(
    ctx: &McpToolCtx,
    parent_memory_id: MemoryId,
) -> Result<String, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT parent.thread_key
           FROM (
                 SELECT q.thread_key
                   FROM proxima_core.chat_message_v1 q
                   JOIN proxima_core.memories m USING (memory_id)
                  WHERE q.memory_id = $1
                    AND m.owner_principal_kind = $2
                    AND m.owner_principal_id = $3
                    AND m.owner_org_id = $4
                 UNION ALL
                 SELECT r.thread_key
                   FROM proxima_core.chat_reply_v1 r
                   JOIN proxima_core.memories m USING (memory_id)
                  WHERE r.memory_id = $1
                    AND m.owner_principal_kind = $2
                    AND m.owner_principal_id = $3
                    AND m.owner_org_id = $4
                ) parent
          LIMIT 1",
    )
    .bind(parent_memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_sql)?;
    row.map(|(thread_key,)| thread_key).ok_or_else(|| {
        McpToolError::InvalidInput(
            "chat parent must be a visible chat message or chat reply Fact".into(),
        )
    })
}

pub(super) async fn load_end_request(
    ctx: &McpToolCtx,
    request_memory_id: MemoryId,
) -> Result<ChatEndRequestedV1, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let row: Option<(
        String,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        Option<String>,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT r.thread_key, r.target_personality_instance_id,
                r.target_self_perspective_memory_id,
                r.requested_by_self_perspective_memory_id, r.reason,
                r.idempotency_key, r.requested_at
           FROM proxima_core.chat_end_requested_v1 r
           JOIN proxima_core.memories m USING (memory_id)
          WHERE r.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(request_memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_sql)?;
    let Some((
        thread_key,
        target_personality_instance_id,
        target_self_perspective_memory_id,
        requested_by_self_perspective_memory_id,
        reason,
        idempotency_key,
        requested_at,
    )) = row
    else {
        return Err(McpToolError::InvalidInput(
            "chat end request is not visible".into(),
        ));
    };
    Ok(ChatEndRequestedV1 {
        thread_key,
        target_personality_instance_id,
        target_self_perspective_memory_id,
        requested_by_self_perspective_memory_id,
        reason,
        idempotency_key,
        requested_at,
    })
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
