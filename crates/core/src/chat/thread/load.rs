use super::*;

#[derive(Clone)]
pub(in crate::chat) struct LoadedStarted {
    pub(in crate::chat) memory_id: MemoryId,
    pub(in crate::chat) payload: ChatStartedV1,
}

#[derive(Clone)]
pub(in crate::chat) struct LoadedMessage {
    pub(in crate::chat) memory_id: MemoryId,
    pub(in crate::chat) payload: ChatMessageV1,
}

#[derive(Clone)]
pub(in crate::chat) struct LoadedReply {
    pub(in crate::chat) memory_id: MemoryId,
    pub(in crate::chat) payload: ChatReplyV1,
}

#[derive(Clone)]
pub(in crate::chat) struct LoadedEndRequest {
    pub(in crate::chat) memory_id: MemoryId,
    pub(in crate::chat) payload: ChatEndRequestedV1,
}

#[derive(Clone)]
pub(in crate::chat) struct LoadedEnded {
    pub(in crate::chat) memory_id: MemoryId,
    pub(in crate::chat) payload: ChatEndedV1,
}

#[derive(Clone)]
pub(in crate::chat) struct LoadedCompaction {
    pub(in crate::chat) memory_id: MemoryId,
    pub(in crate::chat) payload: ChatCompactionV1,
}

#[derive(Clone)]
pub(in crate::chat) struct LoadedSummary {
    pub(in crate::chat) memory_id: MemoryId,
    pub(in crate::chat) payload: ChatSummaryV1,
}

#[derive(Clone)]
pub(in crate::chat) struct LoadedApprovalPolicy {
    pub(in crate::chat) memory_id: MemoryId,
    pub(in crate::chat) target_kind: ApprovalTargetKind,
    pub(in crate::chat) target_memory_id: Option<uuid::Uuid>,
    pub(in crate::chat) target_goal_id: Option<uuid::Uuid>,
    pub(in crate::chat) title: String,
    pub(in crate::chat) summary: String,
    pub(in crate::chat) eligible_voters: Vec<ApprovalEligibleVoter>,
    pub(in crate::chat) requirements: Vec<ApprovalRequirement>,
    pub(in crate::chat) idempotency_key: String,
    pub(in crate::chat) created_at: OffsetDateTime,
}

#[derive(Clone)]
pub(in crate::chat) struct LoadedApprovalVote {
    pub(in crate::chat) memory_id: MemoryId,
    pub(in crate::chat) policy_memory_id: uuid::Uuid,
    pub(in crate::chat) voter_key: String,
    pub(in crate::chat) voter_kind: ApprovalVoterKind,
    pub(in crate::chat) role: Option<String>,
    pub(in crate::chat) personality_instance_id: Option<uuid::Uuid>,
    pub(in crate::chat) self_perspective_memory_id: Option<uuid::Uuid>,
    pub(in crate::chat) master_token_id: Option<uuid::Uuid>,
    pub(in crate::chat) verdict: ApprovalVoteVerdict,
    pub(in crate::chat) rationale: String,
    pub(in crate::chat) idempotency_key: String,
    pub(in crate::chat) voted_at: OffsetDateTime,
}

#[derive(Clone)]
pub(in crate::chat) struct LoadedApprovalDecision {
    pub(in crate::chat) memory_id: MemoryId,
    pub(in crate::chat) policy_memory_id: uuid::Uuid,
    pub(in crate::chat) target_kind: ApprovalTargetKind,
    pub(in crate::chat) target_memory_id: Option<uuid::Uuid>,
    pub(in crate::chat) target_goal_id: Option<uuid::Uuid>,
    pub(in crate::chat) decision: ApprovalDecision,
    pub(in crate::chat) reason: String,
    pub(in crate::chat) counted_votes: Vec<ThreadApprovalCountedVoteRaw>,
    pub(in crate::chat) idempotency_key: String,
    pub(in crate::chat) decided_at: OffsetDateTime,
}

#[derive(Clone, Deserialize)]
pub(in crate::chat) struct ThreadApprovalCountedVoteRaw {
    pub(in crate::chat) vote_memory_id: uuid::Uuid,
    pub(in crate::chat) voter_key: String,
    pub(in crate::chat) verdict: ApprovalVoteVerdict,
}

pub(in crate::chat) struct LoadedThreadEdge {
    pub(in crate::chat) edge_id: EdgeId,
    pub(in crate::chat) relation: String,
    pub(in crate::chat) source_kind: EntityKind,
    pub(in crate::chat) source_memory_id: Option<uuid::Uuid>,
    pub(in crate::chat) source_goal_id: Option<uuid::Uuid>,
    pub(in crate::chat) target_kind: EntityKind,
    pub(in crate::chat) target_memory_id: Option<uuid::Uuid>,
    pub(in crate::chat) target_goal_id: Option<uuid::Uuid>,
    pub(in crate::chat) authorship_kind: EdgeAuthorshipKind,
    pub(in crate::chat) created_at: OffsetDateTime,
}

pub(in crate::chat) async fn load_chat_thread(
    ctx: &McpToolCtx,
    thread_key: String,
    limit: i64,
) -> Result<GetChatThreadOutput, McpToolError> {
    let started = load_thread_started(ctx, &thread_key).await?;
    let end_requests = load_thread_end_requests(ctx, &thread_key, limit).await?;
    let ended = load_thread_ended(ctx, &thread_key).await?;
    let compactions = load_thread_compactions(ctx, &thread_key, limit).await?;
    let summaries = load_thread_summaries(ctx, &thread_key, limit).await?;
    let messages = load_thread_messages(ctx, &thread_key, limit).await?;
    let replies = load_thread_replies(ctx, &thread_key, limit).await?;
    let chat_memory_ids: Vec<_> = messages
        .iter()
        .map(|message| message.memory_id.into_inner())
        .chain(replies.iter().map(|reply| reply.memory_id.into_inner()))
        .chain(started.iter().map(|started| started.memory_id.into_inner()))
        .chain(
            end_requests
                .iter()
                .map(|request| request.memory_id.into_inner()),
        )
        .chain(ended.iter().map(|ended| ended.memory_id.into_inner()))
        .chain(
            compactions
                .iter()
                .map(|compaction| compaction.memory_id.into_inner()),
        )
        .chain(
            summaries
                .iter()
                .map(|summary| summary.memory_id.into_inner()),
        )
        .collect();
    let policies = load_thread_approval_policies(ctx, &chat_memory_ids, limit).await?;
    let policy_ids: Vec<_> = policies
        .iter()
        .map(|policy| policy.memory_id.into_inner())
        .collect();
    let votes = load_thread_approval_votes(ctx, &policy_ids, limit).await?;
    let decisions = load_thread_approval_decisions(ctx, &policy_ids, limit).await?;
    let thread_memory_ids: Vec<_> = chat_memory_ids
        .iter()
        .copied()
        .chain(policy_ids.iter().copied())
        .chain(votes.iter().map(|vote| vote.memory_id.into_inner()))
        .chain(
            decisions
                .iter()
                .map(|decision| decision.memory_id.into_inner()),
        )
        .collect();
    let edges = load_thread_edges(ctx, &thread_memory_ids, limit).await?;
    let context_memory_ids: Vec<_> = messages
        .iter()
        .flat_map(|message| message.payload.context_memory_ids.iter().copied())
        .chain(
            replies
                .iter()
                .flat_map(|reply| reply.payload.context_memory_ids_used.iter().copied()),
        )
        .chain(
            compactions
                .iter()
                .flat_map(|compaction| compaction.payload.context_memory_ids_used.iter().copied()),
        )
        .chain(
            compactions
                .iter()
                .flat_map(|compaction| compaction.payload.included_memory_ids.iter().copied()),
        )
        .chain(
            summaries
                .iter()
                .flat_map(|summary| summary.payload.context_memory_ids_used.iter().copied()),
        )
        .chain(
            summaries
                .iter()
                .flat_map(|summary| summary.payload.included_memory_ids.iter().copied()),
        )
        .collect();
    let context_memory_classes = load_memory_handle_classes(ctx, &context_memory_ids).await?;

    let replied_message_ids: HashSet<_> = replies
        .iter()
        .map(|reply| reply.payload.message_memory_id)
        .collect();
    let decided_policy_ids: HashSet<_> = decisions
        .iter()
        .map(|decision| decision.policy_memory_id)
        .collect();
    let open_items = ThreadOpenItems {
        unreplied_messages: messages
            .iter()
            .filter(|message| !replied_message_ids.contains(&message.memory_id.into_inner()))
            .map(|message| ctx.format_fact_memory(message.memory_id))
            .collect(),
        undecided_policies: policies
            .iter()
            .filter(|policy| !decided_policy_ids.contains(&policy.memory_id.into_inner()))
            .map(|policy| ctx.format_fact_memory(policy.memory_id))
            .collect(),
    };

    Ok(GetChatThreadOutput {
        thread_key,
        started: started
            .map(|started| render_thread_started(ctx, started))
            .transpose()?,
        end_requests: end_requests
            .into_iter()
            .map(|request| render_thread_end_request(ctx, request))
            .collect::<Result<_, _>>()?,
        ended: ended
            .map(|ended| render_thread_ended(ctx, ended))
            .transpose()?,
        compactions: compactions
            .into_iter()
            .map(|compaction| render_thread_compaction(ctx, compaction, &context_memory_classes))
            .collect::<Result<_, _>>()?,
        summaries: summaries
            .into_iter()
            .map(|summary| render_thread_summary(ctx, summary, &context_memory_classes))
            .collect::<Result<_, _>>()?,
        messages: messages
            .into_iter()
            .map(|message| render_thread_message(ctx, message, &context_memory_classes))
            .collect::<Result<_, _>>()?,
        replies: replies
            .into_iter()
            .map(|reply| render_thread_reply(ctx, reply, &context_memory_classes))
            .collect::<Result<_, _>>()?,
        approval_policies: policies
            .into_iter()
            .map(|policy| render_thread_policy(ctx, policy))
            .collect::<Result<_, _>>()?,
        approval_votes: votes
            .into_iter()
            .map(|vote| render_thread_vote(ctx, vote))
            .collect(),
        approval_decisions: decisions
            .into_iter()
            .map(|decision| render_thread_decision(ctx, decision))
            .collect::<Result<_, _>>()?,
        edges: edges
            .into_iter()
            .map(|edge| render_thread_edge(ctx, edge))
            .collect::<Result<_, _>>()?,
        open_items,
    })
}

pub(in crate::chat) async fn resolve_thread_key_from_anchor(
    ctx: &McpToolCtx,
    anchor: &str,
) -> Result<String, McpToolError> {
    let memory_id = ctx.resolve_memory(anchor)?;
    if let Some(thread_key) = thread_key_for_chat_memory(ctx, memory_id.into_inner()).await? {
        return Ok(thread_key);
    }
    if let Some(policy_id) = policy_id_for_vote_or_decision(ctx, memory_id.into_inner()).await? {
        return thread_key_for_policy(ctx, policy_id).await;
    }
    thread_key_for_policy(ctx, memory_id.into_inner()).await
}

pub(in crate::chat) async fn resolve_thread_key_arg(
    ctx: &McpToolCtx,
    thread_key: Option<&str>,
    anchor: Option<&str>,
) -> Result<String, McpToolError> {
    match (thread_key, anchor) {
        (None, None) => Err(McpToolError::InvalidInput(
            "thread_key or anchor is required".into(),
        )),
        (Some(raw), None) => normalize_text("thread_key", raw, 1, 240),
        (None, Some(anchor)) => resolve_thread_key_from_anchor(ctx, anchor).await,
        (Some(raw), Some(anchor)) => {
            let normalized = normalize_text("thread_key", raw, 1, 240)?;
            let anchored = resolve_thread_key_from_anchor(ctx, anchor).await?;
            if normalized == anchored {
                Ok(normalized)
            } else {
                Err(McpToolError::InvalidInput(
                    "thread_key and anchor resolve to different chat threads".into(),
                ))
            }
        }
    }
}

pub(in crate::chat) async fn thread_key_for_policy(
    ctx: &McpToolCtx,
    policy_memory_id: uuid::Uuid,
) -> Result<String, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let target_memory_id: Option<Option<uuid::Uuid>> = sqlx::query_scalar(
        "SELECT p.target_memory_id
           FROM proxima_core.approval_policy_v1 p
           JOIN proxima_core.memories m USING (memory_id)
          WHERE p.memory_id = $1
            AND p.target_kind = 'fact'
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(policy_memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_sql)?;
    let Some(Some(target_memory_id)) = target_memory_id else {
        return Err(McpToolError::InvalidInput(
            "anchor is not a chat thread Fact".into(),
        ));
    };
    thread_key_for_chat_memory(ctx, target_memory_id)
        .await?
        .ok_or_else(|| McpToolError::InvalidInput("anchor target is not in a chat thread".into()))
}

pub(in crate::chat) async fn policy_id_for_vote_or_decision(
    ctx: &McpToolCtx,
    memory_id: uuid::Uuid,
) -> Result<Option<uuid::Uuid>, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    sqlx::query_scalar(
        "SELECT policy_memory_id
           FROM proxima_core.approval_vote_v1 v
           JOIN proxima_core.memories m USING (memory_id)
          WHERE v.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT policy_memory_id
           FROM proxima_core.approval_decision_v1 d
           JOIN proxima_core.memories m USING (memory_id)
          WHERE d.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          LIMIT 1",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_sql)
}

pub(in crate::chat) async fn thread_key_for_chat_memory(
    ctx: &McpToolCtx,
    memory_id: uuid::Uuid,
) -> Result<Option<String>, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    sqlx::query_scalar(
        "SELECT s.thread_key
           FROM proxima_core.chat_started_v1 s
           JOIN proxima_core.memories m USING (memory_id)
          WHERE s.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT q.thread_key
           FROM proxima_core.chat_message_v1 q
           JOIN proxima_core.memories m USING (memory_id)
          WHERE q.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT a.thread_key
           FROM proxima_core.chat_reply_v1 a
           JOIN proxima_core.memories m USING (memory_id)
          WHERE a.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT r.thread_key
           FROM proxima_core.chat_end_requested_v1 r
           JOIN proxima_core.memories m USING (memory_id)
          WHERE r.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT e.thread_key
           FROM proxima_core.chat_ended_v1 e
           JOIN proxima_core.memories m USING (memory_id)
          WHERE e.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT s.thread_key
           FROM proxima_core.chat_summary_v1 s
           JOIN proxima_core.memories m USING (memory_id)
          WHERE s.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT c.thread_key
           FROM proxima_core.chat_compaction_v1 c
           JOIN proxima_core.memories m USING (memory_id)
          WHERE c.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          LIMIT 1",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_sql)
}

pub(in crate::chat) async fn load_thread_started(
    ctx: &McpToolCtx,
    thread_key: &str,
) -> Result<Option<LoadedStarted>, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let row: Option<(
        uuid::Uuid,
        String,
        uuid::Uuid,
        uuid::Uuid,
        uuid::Uuid,
        Option<String>,
        String,
        OffsetDateTime,
    )> = sqlx::query_as(
        "SELECT s.memory_id, s.thread_key, s.started_by_self_perspective_memory_id,
                s.target_personality_instance_id, s.target_self_perspective_memory_id,
                s.title, s.idempotency_key, s.started_at
           FROM proxima_core.chat_started_v1 s
           JOIN proxima_core.memories m USING (memory_id)
          WHERE s.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY s.started_at ASC, s.memory_id ASC
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
            started_by_self_perspective_memory_id,
            target_personality_instance_id,
            target_self_perspective_memory_id,
            title,
            idempotency_key,
            started_at,
        )| LoadedStarted {
            memory_id: MemoryId::new(memory_id),
            payload: ChatStartedV1 {
                thread_key,
                started_by_self_perspective_memory_id,
                target_personality_instance_id,
                target_self_perspective_memory_id,
                title,
                idempotency_key,
                started_at,
            },
        },
    ))
}
