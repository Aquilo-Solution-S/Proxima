use super::*;

pub(super) async fn load_existing_end_by_request(
    ctx: &McpToolCtx,
    request_memory_id: uuid::Uuid,
) -> Result<Option<EndChatOutput>, McpToolError> {
    let existing = chat_storage(ctx)?
        .chat_existing_end_by_request(&ctx.owner, MemoryId::new(request_memory_id))
        .await?;
    Ok(existing.map(|end| EndChatOutput {
        ended: ctx.format_fact_memory(end.ended_memory_id),
        summary: ctx.format_abstraction_memory(end.summary_memory_id),
        provenance_edge_handles: Vec::new(),
        idempotent_replay: true,
    }))
}

pub(super) async fn load_summary_source_memory_ids(
    ctx: &McpToolCtx,
    thread_key: &str,
) -> Result<Vec<uuid::Uuid>, McpToolError> {
    Ok(chat_storage(ctx)?
        .chat_summary_source_memory_ids(&ctx.owner, thread_key)
        .await?)
}

pub(super) async fn resolve_context_memories(
    ctx: &McpToolCtx,
    raw: &[String],
) -> Result<Vec<uuid::Uuid>, McpToolError> {
    let mut out = Vec::with_capacity(raw.len());
    for handle in raw {
        out.push(ctx.resolve_memory(handle)?.into_inner());
    }
    validate_memory_ids_visible(ctx, &out).await?;
    Ok(out)
}

pub(super) async fn resolve_context_goals(
    ctx: &McpToolCtx,
    raw: &[String],
) -> Result<Vec<uuid::Uuid>, McpToolError> {
    let mut out = Vec::with_capacity(raw.len());
    for handle in raw {
        out.push(ctx.resolve_goal(handle)?.into_inner());
    }
    validate_goal_ids_visible(ctx, &out).await?;
    Ok(out)
}

pub(super) async fn validate_memory_ids_visible(
    ctx: &McpToolCtx,
    memory_ids: &[uuid::Uuid],
) -> Result<(), McpToolError> {
    if memory_ids.is_empty() {
        return Ok(());
    }
    let count = chat_storage(ctx)?
        .chat_count_visible_memories(&ctx.owner, memory_ids)
        .await?;
    if count != i64::try_from(memory_ids.len()).unwrap_or(i64::MAX) {
        return Err(McpToolError::InvalidInput(
            "one or more context memories are not visible".into(),
        ));
    }
    Ok(())
}

pub(super) async fn validate_chat_source_memories(
    ctx: &McpToolCtx,
    thread_key: &str,
    memory_ids: &[uuid::Uuid],
) -> Result<Vec<uuid::Uuid>, McpToolError> {
    let mut unique = memory_ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    if unique.is_empty() {
        return Ok(unique);
    }
    let count = chat_storage(ctx)?
        .chat_count_thread_source_memories(&ctx.owner, thread_key, &unique)
        .await?;
    if count != i64::try_from(unique.len()).unwrap_or(i64::MAX) {
        return Err(McpToolError::InvalidInput(
            "one or more source_memories are not in the chat thread".into(),
        ));
    }
    Ok(unique)
}

pub(super) async fn load_memory_handle_classes(
    ctx: &McpToolCtx,
    memory_ids: &[uuid::Uuid],
) -> Result<HashMap<uuid::Uuid, MemoryHandleClass>, McpToolError> {
    let unique_ids: HashSet<_> = memory_ids.iter().copied().collect();
    if unique_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let ids: Vec<_> = unique_ids.into_iter().collect();
    let rows = chat_storage(ctx)?
        .chat_memory_kinds(&ctx.owner, &ids)
        .await?;
    if rows.len() != ids.len() {
        return Err(McpToolError::Other(
            "one or more chat context memories were not found".into(),
        ));
    }
    rows.into_iter()
        .map(|(id, kind)| {
            MemoryHandleClass::from_memory_kind(&kind)
                .map(|class| (id, class))
                .ok_or_else(|| McpToolError::Other(format!("unknown memory kind: {kind}")))
        })
        .collect()
}

pub(super) async fn validate_goal_ids_visible(
    ctx: &McpToolCtx,
    goal_ids: &[uuid::Uuid],
) -> Result<(), McpToolError> {
    if goal_ids.is_empty() {
        return Ok(());
    }
    let count = chat_storage(ctx)?
        .chat_count_visible_goals(&ctx.owner, goal_ids)
        .await?;
    if count != i64::try_from(goal_ids.len()).unwrap_or(i64::MAX) {
        return Err(McpToolError::InvalidInput(
            "one or more context goals are not visible".into(),
        ));
    }
    Ok(())
}
