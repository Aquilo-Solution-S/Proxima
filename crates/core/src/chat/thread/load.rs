use super::{
    GetChatThreadOutput, HashSet, LoadedStarted, McpToolCtx, McpToolError, ThreadOpenItems,
    chat_storage, load_memory_handle_classes, load_thread_approval_decisions,
    load_thread_approval_policies, load_thread_approval_votes, load_thread_compactions,
    load_thread_edges, load_thread_end_requests, load_thread_ended, load_thread_messages,
    load_thread_replies, load_thread_summaries, normalize_text, render_thread_compaction,
    render_thread_decision, render_thread_edge, render_thread_end_request, render_thread_ended,
    render_thread_message, render_thread_policy, render_thread_reply, render_thread_started,
    render_thread_summary, render_thread_vote,
};

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
    let target_memory_id = chat_storage(ctx)?
        .chat_policy_fact_target(&ctx.owner, policy_memory_id)
        .await?;
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
    Ok(chat_storage(ctx)?
        .chat_policy_id_for_vote_or_decision(&ctx.owner, memory_id)
        .await?)
}

pub(in crate::chat) async fn thread_key_for_chat_memory(
    ctx: &McpToolCtx,
    memory_id: uuid::Uuid,
) -> Result<Option<String>, McpToolError> {
    Ok(chat_storage(ctx)?
        .chat_thread_key_for_memory(&ctx.owner, memory_id)
        .await?)
}

pub(in crate::chat) async fn load_thread_started(
    ctx: &McpToolCtx,
    thread_key: &str,
) -> Result<Option<LoadedStarted>, McpToolError> {
    Ok(chat_storage(ctx)?
        .chat_thread_started(&ctx.owner, thread_key)
        .await?)
}
