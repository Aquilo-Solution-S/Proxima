use super::*;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetChatThreadArgs {
    #[serde(default)]
    #[schemars(description = "Thread key returned by core/start_chat.")]
    pub thread_key: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional chat Fact/Abstraction handle used to resolve the thread. In a chat-end-requested wake, pass the triggering Fact handle here before calling core/end_chat."
    )]
    pub anchor: Option<String>,
    #[serde(default)]
    #[schemars(description = "Maximum projected items per section. Defaults to 100, max 200.")]
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct GetChatThreadOutput {
    pub thread_key: String,
    pub started: Option<ThreadStarted>,
    pub end_requests: Vec<ThreadEndRequest>,
    pub ended: Option<ThreadEnded>,
    pub compactions: Vec<ThreadCompaction>,
    pub summaries: Vec<ThreadSummary>,
    pub messages: Vec<ThreadMessage>,
    pub replies: Vec<ThreadReply>,
    pub approval_policies: Vec<ThreadApprovalPolicy>,
    pub approval_votes: Vec<ThreadApprovalVote>,
    pub approval_decisions: Vec<ThreadApprovalDecision>,
    pub edges: Vec<ThreadEdge>,
    pub open_items: ThreadOpenItems,
}

#[derive(Debug, Serialize)]
pub struct ThreadStarted {
    pub handle: String,
    pub thread_key: String,
    pub started_by_self_perspective: String,
    pub target_personality: String,
    pub target_self_perspective: String,
    pub title: Option<String>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct ThreadMessage {
    pub handle: String,
    pub thread_key: String,
    pub message: String,
    pub target_personality: String,
    pub target_self_perspective: String,
    pub sent_by_self_perspective: String,
    pub parent_message: Option<String>,
    pub context_memories: Vec<String>,
    pub context_goals: Vec<String>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub sent_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct ThreadReply {
    pub handle: String,
    pub reply_to: String,
    pub thread_key: String,
    pub reply: String,
    pub replied_by_personality: String,
    pub replied_by_self_perspective: String,
    pub context_memories_used: Vec<String>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub replied_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct ThreadEndRequest {
    pub handle: String,
    pub thread_key: String,
    pub target_personality: String,
    pub target_self_perspective: String,
    pub requested_by_self_perspective: String,
    pub reason: Option<String>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub requested_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct ThreadEnded {
    pub handle: String,
    pub thread_key: String,
    pub request: String,
    pub ended_by_personality: String,
    pub ended_by_self_perspective: String,
    pub summary: String,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub ended_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct ThreadCompaction {
    pub handle: String,
    pub thread_key: String,
    pub compacted_by_personality: String,
    pub compacted_by_self_perspective: String,
    pub summary: String,
    pub included_memories: Vec<String>,
    pub context_memories_used: Vec<String>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub compacted_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct ThreadSummary {
    pub handle: String,
    pub thread_key: String,
    pub request: String,
    pub ended: String,
    pub summarized_by_personality: String,
    pub summarized_by_self_perspective: String,
    pub summary: String,
    pub included_memories: Vec<String>,
    pub context_memories_used: Vec<String>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub summarized_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct ThreadApprovalPolicy {
    pub handle: String,
    pub target_kind: ApprovalTargetKind,
    pub target: String,
    pub title: String,
    pub summary: String,
    pub eligible_voters: Vec<ApprovalEligibleVoter>,
    pub requirements: Vec<ApprovalRequirement>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct ThreadApprovalVote {
    pub handle: String,
    pub policy: String,
    pub voter_key: String,
    pub voter_kind: ApprovalVoterKind,
    pub role: Option<String>,
    pub personality: Option<String>,
    pub self_perspective: Option<String>,
    pub master_token_id: Option<uuid::Uuid>,
    pub verdict: ApprovalVoteVerdict,
    pub rationale: String,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub voted_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct ThreadApprovalDecision {
    pub handle: String,
    pub policy: String,
    pub target_kind: ApprovalTargetKind,
    pub target: String,
    pub decision: ApprovalDecision,
    pub reason: String,
    pub counted_votes: Vec<ThreadApprovalCountedVote>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub decided_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct ThreadApprovalCountedVote {
    pub vote: String,
    pub voter_key: String,
    pub verdict: ApprovalVoteVerdict,
}

#[derive(Debug, Serialize)]
pub struct ThreadEdge {
    pub handle: String,
    pub relation: String,
    pub source_kind: String,
    pub source: String,
    pub target_kind: String,
    pub target: String,
    pub authorship_kind: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Default, Serialize)]
pub struct ThreadOpenItems {
    pub unreplied_messages: Vec<String>,
    pub undecided_policies: Vec<String>,
}

#[derive(Debug, Default)]
pub struct ListChatTargetsTool;

impl McpTool for ListChatTargetsTool {
    const NAME: &'static str = "core/list_chat_targets";
    const DESCRIPTION: &'static str =
        "List active personalities this caller can send chat messages to.";

    type Args = ListChatTargetsArgs;
    type Output = ListChatTargetsOutput;

    fn call(
        ctx: McpToolCtx,
        args: ListChatTargetsArgs,
    ) -> BoxFuture<'static, Result<ListChatTargetsOutput, McpToolError>> {
        Box::pin(async move {
            let caller_self = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput("caller_self_perspective is required".into())
            })?;
            let targets = list_chat_targets(&ctx, Some(caller_self), args.include_self).await?;
            Ok(ListChatTargetsOutput {
                caller_self_perspective: ctx.format_perspective_memory(caller_self),
                targets,
            })
        })
    }
}

#[derive(Debug, Default)]
pub struct GetChatThreadTool;

impl McpTool for GetChatThreadTool {
    const NAME: &'static str = "core/get_chat_thread";
    const DESCRIPTION: &'static str = "Return the graph-derived chat thread for one thread key or anchor. If both are provided they must resolve to the same thread. Use anchor with the triggering chat Fact handle when a wake receives a chat message or chat-end request.";

    type Args = GetChatThreadArgs;
    type Output = GetChatThreadOutput;

    fn call(
        ctx: McpToolCtx,
        args: GetChatThreadArgs,
    ) -> BoxFuture<'static, Result<GetChatThreadOutput, McpToolError>> {
        Box::pin(async move {
            let thread_key =
                resolve_thread_key_arg(&ctx, args.thread_key.as_deref(), args.anchor.as_deref())
                    .await?;
            let limit = args.limit.unwrap_or(100).clamp(1, 200);
            load_chat_thread(&ctx, thread_key, i64::from(limit)).await
        })
    }
}

mod items;
mod load;
mod render;

pub(super) use items::*;
pub(super) use load::*;
pub(super) use render::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{HandleTable, McpAuthorContext, OutputMode};
    use crate::{FlavorRegistry, OrgId, UserId};
    use std::sync::Arc;

    fn test_ctx(handles: Arc<HandleTable>) -> McpToolCtx {
        McpToolCtx {
            pool: sqlx::PgPool::connect_lazy("postgres://x/x").expect("lazy pool"),
            owner: Owner {
                principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
                org_id: OrgId::new(uuid::Uuid::now_v7()),
            },
            handles: Some(handles),
            mode: OutputMode::Handles,
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "test/model".into(),
                client_name: "test".into(),
                client_version: "1".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            engine: None,
        }
    }

    #[tokio::test]
    async fn chat_context_memories_render_with_actual_memory_class() {
        let handles = Arc::new(HandleTable::new());
        let ctx = test_ctx(handles.clone());
        let message_id = MemoryId::new(uuid::Uuid::now_v7());
        let reply_id = MemoryId::new(uuid::Uuid::now_v7());
        let fact_id = uuid::Uuid::now_v7();
        let abstraction_id = uuid::Uuid::now_v7();
        let perspective_id = uuid::Uuid::now_v7();
        handles.assign_abstraction_memory(MemoryId::new(abstraction_id));
        handles.assign_perspective_memory(MemoryId::new(perspective_id));
        let memory_classes = HashMap::from([
            (fact_id, MemoryHandleClass::Fact),
            (abstraction_id, MemoryHandleClass::Abstraction),
            (perspective_id, MemoryHandleClass::Perspective),
        ]);
        let sent_at = OffsetDateTime::now_utc();
        let message = LoadedMessage {
            memory_id: message_id,
            payload: ChatMessageV1 {
                thread_key: "thread".into(),
                message: "Message?".into(),
                target_personality_instance_id: uuid::Uuid::now_v7(),
                target_self_perspective_memory_id: uuid::Uuid::now_v7(),
                sent_by_self_perspective_memory_id: uuid::Uuid::now_v7(),
                parent_message_memory_id: None,
                context_memory_ids: vec![fact_id, abstraction_id, perspective_id],
                context_goal_ids: Vec::new(),
                idempotency_key: "q".into(),
                sent_at,
            },
        };
        let reply = LoadedReply {
            memory_id: reply_id,
            payload: ChatReplyV1 {
                message_memory_id: message_id.into_inner(),
                thread_key: "thread".into(),
                reply: "Reply.".into(),
                replied_by_personality_instance_id: uuid::Uuid::now_v7(),
                replied_by_self_perspective_memory_id: perspective_id,
                context_memory_ids_used: vec![abstraction_id, perspective_id],
                idempotency_key: "a".into(),
                replied_at: sent_at,
            },
        };

        let rendered_message =
            render_thread_message(&ctx, message, &memory_classes).expect("message");
        let rendered_reply = render_thread_reply(&ctx, reply, &memory_classes).expect("reply");

        assert_eq!(
            rendered_message.context_memories,
            vec!["F2".to_string(), "A1".to_string(), "P1".to_string()]
        );
        assert_eq!(
            rendered_reply.context_memories_used,
            vec!["A1".to_string(), "P1".to_string()]
        );
    }
}
