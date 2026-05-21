use super::*;

pub(in crate::chat) async fn load_thread_messages(
    ctx: &McpToolCtx,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedMessage>, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let rows: Vec<(
        uuid::Uuid,
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
        "SELECT q.memory_id, q.thread_key, q.message, q.target_personality_instance_id,
                q.target_self_perspective_memory_id, q.sent_by_self_perspective_memory_id,
                q.parent_memory_id, q.context_memory_ids, q.context_goal_ids,
                q.idempotency_key, q.sent_at
           FROM proxima_core.chat_message_v1 q
           JOIN proxima_core.memories m USING (memory_id)
          WHERE q.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY q.sent_at ASC, q.memory_id ASC
          LIMIT $5",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_sql)?;
    Ok(rows
        .into_iter()
        .map(
            |(
                memory_id,
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
            )| LoadedMessage {
                memory_id: MemoryId::new(memory_id),
                payload: ChatMessageV1 {
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
                },
            },
        )
        .collect())
}

pub(in crate::chat) async fn load_thread_replies(
    ctx: &McpToolCtx,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedReply>, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let rows: Vec<(
        uuid::Uuid,
        uuid::Uuid,
        String,
        String,
        uuid::Uuid,
        uuid::Uuid,
        Vec<uuid::Uuid>,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT a.memory_id, a.message_memory_id, a.thread_key, a.reply,
                a.replied_by_personality_instance_id,
                a.replied_by_self_perspective_memory_id,
                a.context_memory_ids_used, a.idempotency_key, a.replied_at
           FROM proxima_core.chat_reply_v1 a
           JOIN proxima_core.memories m USING (memory_id)
          WHERE a.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY a.replied_at ASC, a.memory_id ASC
          LIMIT $5",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_sql)?;
    Ok(rows
        .into_iter()
        .map(
            |(
                memory_id,
                message_memory_id,
                thread_key,
                reply,
                replied_by_personality_instance_id,
                replied_by_self_perspective_memory_id,
                context_memory_ids_used,
                idempotency_key,
                replied_at,
            )| LoadedReply {
                memory_id: MemoryId::new(memory_id),
                payload: ChatReplyV1 {
                    message_memory_id,
                    thread_key,
                    reply,
                    replied_by_personality_instance_id,
                    replied_by_self_perspective_memory_id,
                    context_memory_ids_used,
                    idempotency_key,
                    replied_at,
                },
            },
        )
        .collect())
}

pub(in crate::chat) async fn load_thread_end_requests(
    ctx: &McpToolCtx,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedEndRequest>, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let rows: Vec<(
        uuid::Uuid,
        String,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        Option<String>,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT r.memory_id, r.thread_key, r.target_personality_instance_id,
                r.target_self_perspective_memory_id,
                r.requested_by_self_perspective_memory_id, r.reason,
                r.idempotency_key, r.requested_at
           FROM proxima_core.chat_end_requested_v1 r
           JOIN proxima_core.memories m USING (memory_id)
          WHERE r.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY r.requested_at ASC, r.memory_id ASC
          LIMIT $5",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_sql)?;
    Ok(rows
        .into_iter()
        .map(
            |(
                memory_id,
                thread_key,
                target_personality_instance_id,
                target_self_perspective_memory_id,
                requested_by_self_perspective_memory_id,
                reason,
                idempotency_key,
                requested_at,
            )| LoadedEndRequest {
                memory_id: MemoryId::new(memory_id),
                payload: ChatEndRequestedV1 {
                    thread_key,
                    target_personality_instance_id,
                    target_self_perspective_memory_id,
                    requested_by_self_perspective_memory_id,
                    reason,
                    idempotency_key,
                    requested_at,
                },
            },
        )
        .collect())
}

pub(in crate::chat) async fn load_thread_ended(
    ctx: &McpToolCtx,
    thread_key: &str,
) -> Result<Option<LoadedEnded>, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let row: Option<(
        uuid::Uuid,
        String,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT e.memory_id, e.thread_key, e.request_memory_id,
                e.ended_by_personality_instance_id,
                e.ended_by_self_perspective_memory_id, e.summary_memory_id,
                e.idempotency_key, e.ended_at
           FROM proxima_core.chat_ended_v1 e
           JOIN proxima_core.memories m USING (memory_id)
          WHERE e.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY e.ended_at DESC, e.memory_id DESC
          LIMIT 1",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_sql)?;
    Ok(row.map(
        |(
            memory_id,
            thread_key,
            request_memory_id,
            ended_by_personality_instance_id,
            ended_by_self_perspective_memory_id,
            summary_memory_id,
            idempotency_key,
            ended_at,
        )| LoadedEnded {
            memory_id: MemoryId::new(memory_id),
            payload: ChatEndedV1 {
                thread_key,
                request_memory_id,
                ended_by_personality_instance_id,
                ended_by_self_perspective_memory_id,
                summary_memory_id,
                idempotency_key,
                ended_at,
            },
        },
    ))
}

pub(in crate::chat) async fn load_thread_compactions(
    ctx: &McpToolCtx,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedCompaction>, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let rows: Vec<(
        uuid::Uuid,
        String,
        uuid::Uuid,
        uuid::Uuid,
        String,
        Vec<uuid::Uuid>,
        Vec<uuid::Uuid>,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT c.memory_id, c.thread_key, c.compacted_by_personality_instance_id,
                c.compacted_by_self_perspective_memory_id, c.summary,
                c.included_memory_ids, c.context_memory_ids_used,
                c.idempotency_key, c.compacted_at
           FROM proxima_core.chat_compaction_v1 c
           JOIN proxima_core.memories m USING (memory_id)
          WHERE c.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY c.compacted_at ASC, c.memory_id ASC
          LIMIT $5",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_sql)?;
    Ok(rows
        .into_iter()
        .map(
            |(
                memory_id,
                thread_key,
                compacted_by_personality_instance_id,
                compacted_by_self_perspective_memory_id,
                summary,
                included_memory_ids,
                context_memory_ids_used,
                idempotency_key,
                compacted_at,
            )| LoadedCompaction {
                memory_id: MemoryId::new(memory_id),
                payload: ChatCompactionV1 {
                    thread_key,
                    compacted_by_personality_instance_id,
                    compacted_by_self_perspective_memory_id,
                    summary,
                    included_memory_ids,
                    context_memory_ids_used,
                    idempotency_key,
                    compacted_at,
                },
            },
        )
        .collect())
}

pub(in crate::chat) async fn load_thread_summaries(
    ctx: &McpToolCtx,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedSummary>, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let rows: Vec<(
        uuid::Uuid,
        String,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        String,
        Vec<uuid::Uuid>,
        Vec<uuid::Uuid>,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT s.memory_id, s.thread_key, s.request_memory_id, s.ended_memory_id,
                s.summarized_by_personality_instance_id,
                s.summarized_by_self_perspective_memory_id, s.summary,
                s.included_memory_ids, s.context_memory_ids_used,
                s.idempotency_key, s.summarized_at
           FROM proxima_core.chat_summary_v1 s
           JOIN proxima_core.memories m USING (memory_id)
          WHERE s.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY s.summarized_at ASC, s.memory_id ASC
          LIMIT $5",
    )
    .bind(thread_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_sql)?;
    Ok(rows
        .into_iter()
        .map(
            |(
                memory_id,
                thread_key,
                request_memory_id,
                ended_memory_id,
                summarized_by_personality_instance_id,
                summarized_by_self_perspective_memory_id,
                summary,
                included_memory_ids,
                context_memory_ids_used,
                idempotency_key,
                summarized_at,
            )| LoadedSummary {
                memory_id: MemoryId::new(memory_id),
                payload: ChatSummaryV1 {
                    thread_key,
                    request_memory_id,
                    ended_memory_id,
                    summarized_by_personality_instance_id,
                    summarized_by_self_perspective_memory_id,
                    summary,
                    included_memory_ids,
                    context_memory_ids_used,
                    idempotency_key,
                    summarized_at,
                },
            },
        )
        .collect())
}

pub(in crate::chat) async fn load_thread_approval_policies(
    ctx: &McpToolCtx,
    target_memory_ids: &[uuid::Uuid],
    limit: i64,
) -> Result<Vec<LoadedApprovalPolicy>, McpToolError> {
    if target_memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let rows: Vec<(
        uuid::Uuid,
        ApprovalTargetKind,
        Option<uuid::Uuid>,
        Option<uuid::Uuid>,
        String,
        String,
        serde_json::Value,
        serde_json::Value,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT p.memory_id, p.target_kind, p.target_memory_id, p.target_goal_id,
                p.title, p.summary, p.eligible_voters_json, p.requirements_json,
                p.idempotency_key, p.created_at
           FROM proxima_core.approval_policy_v1 p
           JOIN proxima_core.memories m USING (memory_id)
          WHERE p.target_kind = 'fact'
            AND p.target_memory_id = ANY($1::uuid[])
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY p.created_at ASC, p.memory_id ASC
          LIMIT $5",
    )
    .bind(target_memory_ids)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_sql)?;
    rows.into_iter()
        .map(
            |(
                memory_id,
                target_kind,
                target_memory_id,
                target_goal_id,
                title,
                summary,
                eligible_voters_json,
                requirements_json,
                idempotency_key,
                created_at,
            )| {
                Ok(LoadedApprovalPolicy {
                    memory_id: MemoryId::new(memory_id),
                    target_kind,
                    target_memory_id,
                    target_goal_id,
                    title,
                    summary,
                    eligible_voters: serde_json::from_value(eligible_voters_json).map_err(
                        |err| McpToolError::Other(format!("decode eligible voters: {err}")),
                    )?,
                    requirements: serde_json::from_value(requirements_json).map_err(|err| {
                        McpToolError::Other(format!("decode approval requirements: {err}"))
                    })?,
                    idempotency_key,
                    created_at,
                })
            },
        )
        .collect()
}

pub(in crate::chat) async fn load_thread_approval_votes(
    ctx: &McpToolCtx,
    policy_memory_ids: &[uuid::Uuid],
    limit: i64,
) -> Result<Vec<LoadedApprovalVote>, McpToolError> {
    if policy_memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let rows: Vec<(
        uuid::Uuid,
        uuid::Uuid,
        String,
        ApprovalVoterKind,
        Option<String>,
        Option<uuid::Uuid>,
        Option<uuid::Uuid>,
        Option<uuid::Uuid>,
        ApprovalVoteVerdict,
        String,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT v.memory_id, v.policy_memory_id, v.voter_key, v.voter_kind,
                v.role, v.personality_instance_id, v.self_perspective_memory_id,
                v.master_token_id, v.verdict, v.rationale, v.idempotency_key, v.voted_at
           FROM proxima_core.approval_vote_v1 v
           JOIN proxima_core.memories m USING (memory_id)
          WHERE v.policy_memory_id = ANY($1::uuid[])
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY v.voted_at ASC, v.memory_id ASC
          LIMIT $5",
    )
    .bind(policy_memory_ids)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_sql)?;
    Ok(rows
        .into_iter()
        .map(
            |(
                memory_id,
                policy_memory_id,
                voter_key,
                voter_kind,
                role,
                personality_instance_id,
                self_perspective_memory_id,
                master_token_id,
                verdict,
                rationale,
                idempotency_key,
                voted_at,
            )| LoadedApprovalVote {
                memory_id: MemoryId::new(memory_id),
                policy_memory_id,
                voter_key,
                voter_kind,
                role,
                personality_instance_id,
                self_perspective_memory_id,
                master_token_id,
                verdict,
                rationale,
                idempotency_key,
                voted_at,
            },
        )
        .collect())
}

pub(in crate::chat) async fn load_thread_approval_decisions(
    ctx: &McpToolCtx,
    policy_memory_ids: &[uuid::Uuid],
    limit: i64,
) -> Result<Vec<LoadedApprovalDecision>, McpToolError> {
    if policy_memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let rows: Vec<(
        uuid::Uuid,
        uuid::Uuid,
        ApprovalTargetKind,
        Option<uuid::Uuid>,
        Option<uuid::Uuid>,
        ApprovalDecision,
        String,
        serde_json::Value,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT d.memory_id, d.policy_memory_id, d.target_kind, d.target_memory_id,
                d.target_goal_id, d.decision, d.reason, d.counted_votes_json,
                d.idempotency_key, d.decided_at
           FROM proxima_core.approval_decision_v1 d
           JOIN proxima_core.memories m USING (memory_id)
          WHERE d.policy_memory_id = ANY($1::uuid[])
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY d.decided_at ASC, d.memory_id ASC
          LIMIT $5",
    )
    .bind(policy_memory_ids)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(limit)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_sql)?;
    rows.into_iter()
        .map(
            |(
                memory_id,
                policy_memory_id,
                target_kind,
                target_memory_id,
                target_goal_id,
                decision,
                reason,
                counted_votes_json,
                idempotency_key,
                decided_at,
            )| {
                Ok(LoadedApprovalDecision {
                    memory_id: MemoryId::new(memory_id),
                    policy_memory_id,
                    target_kind,
                    target_memory_id,
                    target_goal_id,
                    decision,
                    reason,
                    counted_votes: serde_json::from_value(counted_votes_json).map_err(|err| {
                        McpToolError::Other(format!("decode counted votes: {err}"))
                    })?,
                    idempotency_key,
                    decided_at,
                })
            },
        )
        .collect()
}

pub(in crate::chat) async fn load_thread_edges(
    ctx: &McpToolCtx,
    thread_memory_ids: &[uuid::Uuid],
    limit: i64,
) -> Result<Vec<LoadedThreadEdge>, McpToolError> {
    if thread_memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let rows: Vec<(
        uuid::Uuid,
        String,
        EntityKind,
        Option<uuid::Uuid>,
        Option<uuid::Uuid>,
        EntityKind,
        Option<uuid::Uuid>,
        Option<uuid::Uuid>,
        EdgeAuthorshipKind,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT edge_id, relation, source_kind, source_memory_id, source_goal_id,
                target_kind, target_memory_id, target_goal_id, authorship_kind, created_at
           FROM proxima_core.edges
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND relation = ANY($4::text[])
            AND (source_memory_id = ANY($5::uuid[])
                 OR target_memory_id = ANY($5::uuid[]))
          ORDER BY created_at ASC, edge_id ASC
          LIMIT $6",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(&[
        CORE_RECEIVES_CHAT_MESSAGE_RELATION,
        CORE_REPLIES_TO_MESSAGE_RELATION,
        CORE_HAS_APPROVAL_POLICY_RELATION,
        CORE_VOTES_ON_RELATION,
        CORE_HAS_APPROVAL_DECISION_RELATION,
        CORE_DERIVED_FROM_RELATION,
        CORE_RECEIVES_CHAT_END_REQUEST_RELATION,
    ])
    .bind(thread_memory_ids)
    .bind(limit)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_sql)?;
    Ok(rows
        .into_iter()
        .filter(
            |(_, relation, _, source_memory_id, _, _, target_memory_id, _, _, _)| {
                if relation == CORE_RECEIVES_CHAT_MESSAGE_RELATION
                    || relation == CORE_RECEIVES_CHAT_END_REQUEST_RELATION
                {
                    endpoint_in_thread(target_memory_id, thread_memory_ids)
                } else {
                    endpoint_in_thread(source_memory_id, thread_memory_ids)
                        && endpoint_in_thread(target_memory_id, thread_memory_ids)
                }
            },
        )
        .map(
            |(
                edge_id,
                relation,
                source_kind,
                source_memory_id,
                source_goal_id,
                target_kind,
                target_memory_id,
                target_goal_id,
                authorship_kind,
                created_at,
            )| LoadedThreadEdge {
                edge_id: EdgeId::new(edge_id),
                relation,
                source_kind,
                source_memory_id,
                source_goal_id,
                target_kind,
                target_memory_id,
                target_goal_id,
                authorship_kind,
                created_at,
            },
        )
        .collect())
}
