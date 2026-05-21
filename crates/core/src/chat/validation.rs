use super::*;

pub(super) async fn load_existing_end_by_request(
    ctx: &McpToolCtx,
    request_memory_id: uuid::Uuid,
) -> Result<Option<EndChatOutput>, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let row: Option<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
        "SELECT e.memory_id, e.summary_memory_id
           FROM proxima_core.chat_ended_v1 e
           JOIN proxima_core.memories m USING (memory_id)
          WHERE e.request_memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY e.ended_at ASC, e.memory_id ASC
          LIMIT 1",
    )
    .bind(request_memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_sql)?;
    Ok(row.map(|(ended, summary)| EndChatOutput {
        ended: ctx.format_fact_memory(MemoryId::new(ended)),
        summary: ctx.format_abstraction_memory(MemoryId::new(summary)),
        provenance_edge_handles: Vec::new(),
        idempotent_replay: true,
    }))
}

pub(super) async fn load_summary_source_memory_ids(
    ctx: &McpToolCtx,
    thread_key: &str,
) -> Result<Vec<uuid::Uuid>, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let mut rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT s.memory_id
           FROM proxima_core.chat_started_v1 s
           JOIN proxima_core.memories m USING (memory_id)
          WHERE s.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT q.memory_id
           FROM proxima_core.chat_message_v1 q
           JOIN proxima_core.memories m USING (memory_id)
          WHERE q.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT a.memory_id
           FROM proxima_core.chat_reply_v1 a
           JOIN proxima_core.memories m USING (memory_id)
          WHERE a.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT r.memory_id
           FROM proxima_core.chat_end_requested_v1 r
           JOIN proxima_core.memories m USING (memory_id)
          WHERE r.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT s.memory_id
           FROM proxima_core.chat_summary_v1 s
           JOIN proxima_core.memories m USING (memory_id)
         WHERE s.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT c.memory_id
           FROM proxima_core.chat_compaction_v1 c
           JOIN proxima_core.memories m USING (memory_id)
          WHERE c.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_sql)?;
    let mut out: Vec<_> = rows.drain(..).map(|(id,)| id).collect();
    out.sort_unstable();
    out.dedup();
    Ok(out)
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
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.memories
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND memory_id = ANY($4::uuid[])",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(memory_ids)
    .fetch_one(&ctx.pool)
    .await
    .map_err(map_sql)?;
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
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let count: i64 = sqlx::query_scalar(
        "WITH thread_memory AS (
             SELECT s.memory_id
               FROM proxima_core.chat_started_v1 s
               JOIN proxima_core.memories m USING (memory_id)
              WHERE s.thread_key = $1
                AND m.owner_principal_kind = $2
                AND m.owner_principal_id = $3
                AND m.owner_org_id = $4
             UNION ALL
             SELECT q.memory_id
               FROM proxima_core.chat_message_v1 q
               JOIN proxima_core.memories m USING (memory_id)
              WHERE q.thread_key = $1
                AND m.owner_principal_kind = $2
                AND m.owner_principal_id = $3
                AND m.owner_org_id = $4
             UNION ALL
             SELECT r.memory_id
               FROM proxima_core.chat_reply_v1 r
               JOIN proxima_core.memories m USING (memory_id)
              WHERE r.thread_key = $1
                AND m.owner_principal_kind = $2
                AND m.owner_principal_id = $3
                AND m.owner_org_id = $4
             UNION ALL
             SELECT e.memory_id
               FROM proxima_core.chat_end_requested_v1 e
               JOIN proxima_core.memories m USING (memory_id)
              WHERE e.thread_key = $1
                AND m.owner_principal_kind = $2
                AND m.owner_principal_id = $3
                AND m.owner_org_id = $4
             UNION ALL
             SELECT c.memory_id
               FROM proxima_core.chat_compaction_v1 c
               JOIN proxima_core.memories m USING (memory_id)
              WHERE c.thread_key = $1
                AND m.owner_principal_kind = $2
                AND m.owner_principal_id = $3
                AND m.owner_org_id = $4
             UNION ALL
             SELECT s.memory_id
               FROM proxima_core.chat_summary_v1 s
               JOIN proxima_core.memories m USING (memory_id)
              WHERE s.thread_key = $1
                AND m.owner_principal_kind = $2
                AND m.owner_principal_id = $3
                AND m.owner_org_id = $4
         )
         SELECT count(DISTINCT memory_id)
           FROM thread_memory
          WHERE memory_id = ANY($5::uuid[])",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(&unique)
    .fetch_one(&ctx.pool)
    .await
    .map_err(map_sql)?;
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
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let rows: Vec<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT memory_id, COALESCE(kind::text, 'Fact') AS kind
           FROM proxima_core.memories
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND memory_id = ANY($4::uuid[])",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(&ids)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_sql)?;
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
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.goals
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND goal_id = ANY($4::uuid[])",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(goal_ids)
    .fetch_one(&ctx.pool)
    .await
    .map_err(map_sql)?;
    if count != i64::try_from(goal_ids.len()).unwrap_or(i64::MAX) {
        return Err(McpToolError::InvalidInput(
            "one or more context goals are not visible".into(),
        ));
    }
    Ok(())
}
