use super::*;

pub(in crate::chat) fn endpoint_in_thread(
    endpoint: &Option<uuid::Uuid>,
    thread_memory_ids: &[uuid::Uuid],
) -> bool {
    endpoint
        .as_ref()
        .is_some_and(|id| thread_memory_ids.contains(id))
}

pub(in crate::chat) fn render_thread_started(
    ctx: &McpToolCtx,
    started: LoadedStarted,
) -> Result<ThreadStarted, McpToolError> {
    let payload = started.payload;
    Ok(ThreadStarted {
        handle: ctx.format_fact_memory(started.memory_id),
        thread_key: payload.thread_key,
        started_by_self_perspective: ctx.format_perspective_memory(MemoryId::new(
            payload.started_by_self_perspective_memory_id,
        )),
        target_personality: ctx.format_personality(PersonalityInstanceId::new(
            payload.target_personality_instance_id,
        )),
        target_self_perspective: ctx
            .format_perspective_memory(MemoryId::new(payload.target_self_perspective_memory_id)),
        title: payload.title,
        idempotency_key: payload.idempotency_key,
        started_at: payload.started_at,
    })
}

pub(in crate::chat) fn render_thread_message(
    ctx: &McpToolCtx,
    message: LoadedMessage,
    memory_classes: &HashMap<uuid::Uuid, MemoryHandleClass>,
) -> Result<ThreadMessage, McpToolError> {
    let payload = message.payload;
    Ok(ThreadMessage {
        handle: ctx.format_fact_memory(message.memory_id),
        thread_key: payload.thread_key,
        message: payload.message,
        target_personality: ctx.format_personality(PersonalityInstanceId::new(
            payload.target_personality_instance_id,
        )),
        target_self_perspective: ctx
            .format_perspective_memory(MemoryId::new(payload.target_self_perspective_memory_id)),
        sent_by_self_perspective: ctx
            .format_perspective_memory(MemoryId::new(payload.sent_by_self_perspective_memory_id)),
        parent_message: payload
            .parent_message_memory_id
            .map(|id| ctx.format_fact_memory(MemoryId::new(id))),
        context_memories: payload
            .context_memory_ids
            .into_iter()
            .map(|id| format_memory_from_class_map(ctx, memory_classes, id))
            .collect::<Result<_, _>>()?,
        context_goals: payload
            .context_goal_ids
            .into_iter()
            .map(|id| ctx.format_goal(GoalId::new(id)))
            .collect(),
        idempotency_key: payload.idempotency_key,
        sent_at: payload.sent_at,
    })
}

pub(in crate::chat) fn render_thread_reply(
    ctx: &McpToolCtx,
    reply: LoadedReply,
    memory_classes: &HashMap<uuid::Uuid, MemoryHandleClass>,
) -> Result<ThreadReply, McpToolError> {
    let payload = reply.payload;
    Ok(ThreadReply {
        handle: ctx.format_fact_memory(reply.memory_id),
        reply_to: ctx.format_fact_memory(MemoryId::new(payload.message_memory_id)),
        thread_key: payload.thread_key,
        reply: payload.reply,
        replied_by_personality: ctx.format_personality(PersonalityInstanceId::new(
            payload.replied_by_personality_instance_id,
        )),
        replied_by_self_perspective: ctx.format_perspective_memory(MemoryId::new(
            payload.replied_by_self_perspective_memory_id,
        )),
        context_memories_used: payload
            .context_memory_ids_used
            .into_iter()
            .map(|id| format_memory_from_class_map(ctx, memory_classes, id))
            .collect::<Result<_, _>>()?,
        idempotency_key: payload.idempotency_key,
        replied_at: payload.replied_at,
    })
}

pub(in crate::chat) fn render_thread_end_request(
    ctx: &McpToolCtx,
    request: LoadedEndRequest,
) -> Result<ThreadEndRequest, McpToolError> {
    let payload = request.payload;
    Ok(ThreadEndRequest {
        handle: ctx.format_fact_memory(request.memory_id),
        thread_key: payload.thread_key,
        target_personality: ctx.format_personality(PersonalityInstanceId::new(
            payload.target_personality_instance_id,
        )),
        target_self_perspective: ctx
            .format_perspective_memory(MemoryId::new(payload.target_self_perspective_memory_id)),
        requested_by_self_perspective: ctx.format_perspective_memory(MemoryId::new(
            payload.requested_by_self_perspective_memory_id,
        )),
        reason: payload.reason,
        idempotency_key: payload.idempotency_key,
        requested_at: payload.requested_at,
    })
}

pub(in crate::chat) fn render_thread_ended(
    ctx: &McpToolCtx,
    ended: LoadedEnded,
) -> Result<ThreadEnded, McpToolError> {
    let payload = ended.payload;
    Ok(ThreadEnded {
        handle: ctx.format_fact_memory(ended.memory_id),
        thread_key: payload.thread_key,
        request: ctx.format_fact_memory(MemoryId::new(payload.request_memory_id)),
        ended_by_personality: ctx.format_personality(PersonalityInstanceId::new(
            payload.ended_by_personality_instance_id,
        )),
        ended_by_self_perspective: ctx
            .format_perspective_memory(MemoryId::new(payload.ended_by_self_perspective_memory_id)),
        summary: ctx.format_abstraction_memory(MemoryId::new(payload.summary_memory_id)),
        idempotency_key: payload.idempotency_key,
        ended_at: payload.ended_at,
    })
}

pub(in crate::chat) fn render_thread_compaction(
    ctx: &McpToolCtx,
    compaction: LoadedCompaction,
    memory_classes: &HashMap<uuid::Uuid, MemoryHandleClass>,
) -> Result<ThreadCompaction, McpToolError> {
    let payload = compaction.payload;
    Ok(ThreadCompaction {
        handle: ctx.format_abstraction_memory(compaction.memory_id),
        thread_key: payload.thread_key,
        compacted_by_personality: ctx.format_personality(PersonalityInstanceId::new(
            payload.compacted_by_personality_instance_id,
        )),
        compacted_by_self_perspective: ctx.format_perspective_memory(MemoryId::new(
            payload.compacted_by_self_perspective_memory_id,
        )),
        summary: payload.summary,
        included_memories: payload
            .included_memory_ids
            .into_iter()
            .map(|id| format_memory_from_class_map(ctx, memory_classes, id))
            .collect::<Result<_, _>>()?,
        context_memories_used: payload
            .context_memory_ids_used
            .into_iter()
            .map(|id| format_memory_from_class_map(ctx, memory_classes, id))
            .collect::<Result<_, _>>()?,
        idempotency_key: payload.idempotency_key,
        compacted_at: payload.compacted_at,
    })
}

pub(in crate::chat) fn render_thread_summary(
    ctx: &McpToolCtx,
    summary: LoadedSummary,
    memory_classes: &HashMap<uuid::Uuid, MemoryHandleClass>,
) -> Result<ThreadSummary, McpToolError> {
    let payload = summary.payload;
    Ok(ThreadSummary {
        handle: ctx.format_abstraction_memory(summary.memory_id),
        thread_key: payload.thread_key,
        request: ctx.format_fact_memory(MemoryId::new(payload.request_memory_id)),
        ended: ctx.format_fact_memory(MemoryId::new(payload.ended_memory_id)),
        summarized_by_personality: ctx.format_personality(PersonalityInstanceId::new(
            payload.summarized_by_personality_instance_id,
        )),
        summarized_by_self_perspective: ctx.format_perspective_memory(MemoryId::new(
            payload.summarized_by_self_perspective_memory_id,
        )),
        summary: payload.summary,
        included_memories: payload
            .included_memory_ids
            .into_iter()
            .map(|id| format_memory_from_class_map(ctx, memory_classes, id))
            .collect::<Result<_, _>>()?,
        context_memories_used: payload
            .context_memory_ids_used
            .into_iter()
            .map(|id| format_memory_from_class_map(ctx, memory_classes, id))
            .collect::<Result<_, _>>()?,
        idempotency_key: payload.idempotency_key,
        summarized_at: payload.summarized_at,
    })
}

pub(in crate::chat) fn format_memory_from_class_map(
    ctx: &McpToolCtx,
    memory_classes: &HashMap<uuid::Uuid, MemoryHandleClass>,
    memory_id: uuid::Uuid,
) -> Result<String, McpToolError> {
    let class = memory_classes.get(&memory_id).copied().ok_or_else(|| {
        McpToolError::Other(format!("chat context memory class not found: {memory_id}"))
    })?;
    Ok(ctx.format_memory_with_class(MemoryId::new(memory_id), class))
}

pub(in crate::chat) fn entity_kind_for_class_map(
    memory_classes: &HashMap<uuid::Uuid, MemoryHandleClass>,
    memory_id: uuid::Uuid,
) -> Result<EntityKind, McpToolError> {
    let class = memory_classes.get(&memory_id).copied().ok_or_else(|| {
        McpToolError::Other(format!(
            "chat provenance memory class not found: {memory_id}"
        ))
    })?;
    Ok(match class {
        MemoryHandleClass::Fact => EntityKind::Fact,
        MemoryHandleClass::Abstraction => EntityKind::Abstraction,
        MemoryHandleClass::Perspective => EntityKind::Perspective,
    })
}

pub(in crate::chat) fn render_thread_policy(
    ctx: &McpToolCtx,
    policy: LoadedApprovalPolicy,
) -> Result<ThreadApprovalPolicy, McpToolError> {
    Ok(ThreadApprovalPolicy {
        handle: ctx.format_fact_memory(policy.memory_id),
        target_kind: policy.target_kind,
        target: format_target(
            ctx,
            policy.target_kind,
            policy.target_memory_id,
            policy.target_goal_id,
        )?,
        title: policy.title,
        summary: policy.summary,
        eligible_voters: policy.eligible_voters,
        requirements: policy.requirements,
        idempotency_key: policy.idempotency_key,
        created_at: policy.created_at,
    })
}

pub(in crate::chat) fn render_thread_vote(
    ctx: &McpToolCtx,
    vote: LoadedApprovalVote,
) -> ThreadApprovalVote {
    ThreadApprovalVote {
        handle: ctx.format_fact_memory(vote.memory_id),
        policy: ctx.format_fact_memory(MemoryId::new(vote.policy_memory_id)),
        voter_key: vote.voter_key,
        voter_kind: vote.voter_kind,
        role: vote.role,
        personality: vote
            .personality_instance_id
            .map(|id| ctx.format_personality(PersonalityInstanceId::new(id))),
        self_perspective: vote
            .self_perspective_memory_id
            .map(|id| ctx.format_perspective_memory(MemoryId::new(id))),
        master_token_id: vote.master_token_id,
        verdict: vote.verdict,
        rationale: vote.rationale,
        idempotency_key: vote.idempotency_key,
        voted_at: vote.voted_at,
    }
}

pub(in crate::chat) fn render_thread_decision(
    ctx: &McpToolCtx,
    decision: LoadedApprovalDecision,
) -> Result<ThreadApprovalDecision, McpToolError> {
    Ok(ThreadApprovalDecision {
        handle: ctx.format_fact_memory(decision.memory_id),
        policy: ctx.format_fact_memory(MemoryId::new(decision.policy_memory_id)),
        target_kind: decision.target_kind,
        target: format_target(
            ctx,
            decision.target_kind,
            decision.target_memory_id,
            decision.target_goal_id,
        )?,
        decision: decision.decision,
        reason: decision.reason,
        counted_votes: decision
            .counted_votes
            .into_iter()
            .map(|vote| ThreadApprovalCountedVote {
                vote: ctx.format_fact_memory(MemoryId::new(vote.vote_memory_id)),
                voter_key: vote.voter_key,
                verdict: vote.verdict,
            })
            .collect(),
        idempotency_key: decision.idempotency_key,
        decided_at: decision.decided_at,
    })
}

pub(in crate::chat) fn render_thread_edge(
    ctx: &McpToolCtx,
    edge: LoadedThreadEdge,
) -> Result<ThreadEdge, McpToolError> {
    Ok(ThreadEdge {
        handle: ctx.format_edge(edge.edge_id),
        relation: edge.relation,
        source_kind: edge.source_kind.as_str().to_string(),
        source: format_endpoint(
            ctx,
            edge.source_kind,
            edge.source_memory_id,
            edge.source_goal_id,
        )?,
        target_kind: edge.target_kind.as_str().to_string(),
        target: format_endpoint(
            ctx,
            edge.target_kind,
            edge.target_memory_id,
            edge.target_goal_id,
        )?,
        authorship_kind: edge.authorship_kind.as_str().to_string(),
        created_at: edge.created_at,
    })
}

pub(in crate::chat) fn format_target(
    ctx: &McpToolCtx,
    target_kind: ApprovalTargetKind,
    target_memory_id: Option<uuid::Uuid>,
    target_goal_id: Option<uuid::Uuid>,
) -> Result<String, McpToolError> {
    match target_kind {
        ApprovalTargetKind::Goal => target_goal_id
            .map(|id| ctx.format_goal(GoalId::new(id)))
            .ok_or_else(|| McpToolError::Other("approval target missing goal_id".into())),
        ApprovalTargetKind::Fact
        | ApprovalTargetKind::Abstraction
        | ApprovalTargetKind::Perspective => target_memory_id
            .map(|id| format_memory_by_approval_kind(ctx, target_kind, MemoryId::new(id)))
            .ok_or_else(|| McpToolError::Other("approval target missing memory_id".into())),
    }
}

pub(in crate::chat) fn format_endpoint(
    ctx: &McpToolCtx,
    kind: EntityKind,
    memory_id: Option<uuid::Uuid>,
    goal_id: Option<uuid::Uuid>,
) -> Result<String, McpToolError> {
    match kind {
        EntityKind::Goal => goal_id
            .map(|id| ctx.format_goal(GoalId::new(id)))
            .ok_or_else(|| McpToolError::Other("edge endpoint missing goal_id".into())),
        EntityKind::Fact | EntityKind::Abstraction | EntityKind::Perspective => memory_id
            .map(|id| format_memory_by_entity_kind(ctx, kind, MemoryId::new(id)))
            .ok_or_else(|| McpToolError::Other("edge endpoint missing memory_id".into())),
    }
}

pub(in crate::chat) fn format_memory_by_approval_kind(
    ctx: &McpToolCtx,
    kind: ApprovalTargetKind,
    memory_id: MemoryId,
) -> String {
    match kind {
        ApprovalTargetKind::Fact => ctx.format_fact_memory(memory_id),
        ApprovalTargetKind::Abstraction => ctx.format_abstraction_memory(memory_id),
        ApprovalTargetKind::Perspective => ctx.format_perspective_memory(memory_id),
        ApprovalTargetKind::Goal => ctx.format_fact_memory(memory_id),
    }
}

pub(in crate::chat) fn format_memory_by_entity_kind(
    ctx: &McpToolCtx,
    kind: EntityKind,
    memory_id: MemoryId,
) -> String {
    match kind {
        EntityKind::Fact => ctx.format_fact_memory(memory_id),
        EntityKind::Abstraction => ctx.format_abstraction_memory(memory_id),
        EntityKind::Perspective => ctx.format_perspective_memory(memory_id),
        EntityKind::Goal => ctx.format_fact_memory(memory_id),
    }
}
