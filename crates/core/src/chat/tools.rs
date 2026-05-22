use super::*;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ChatWakeEntryOutput {
    pub wake_entry: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ChatTargetOutput {
    pub personality: String,
    pub display_name: String,
    pub root_perspective: String,
    pub chat_message_wake_entries: Vec<ChatWakeEntryOutput>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListChatTargetsArgs {
    #[serde(default)]
    pub include_self: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListChatTargetsOutput {
    pub caller_self_perspective: String,
    pub targets: Vec<ChatTargetOutput>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StartChatArgs {
    #[schemars(
        description = "Personality handle or UUID that should receive the first chat message."
    )]
    pub target_personality: String,
    #[serde(default)]
    #[schemars(
        description = "Optional stable thread key. Omit to allocate a new UUIDv7 thread key."
    )]
    pub thread_key: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional human title for the chat thread.")]
    pub title: Option<String>,
    #[schemars(description = "First chat message text.")]
    pub message: String,
    #[serde(default)]
    #[schemars(
        description = "Optional memory handles to include as exact context. Accepts only Fact/Abstraction/Perspective handles (F..., A..., P...) or raw memory UUIDs; do not put Goal handles here."
    )]
    pub context_memories: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional Goal handles to include as exact context. Use this field for G... handles; do not put goals in context_memories."
    )]
    pub context_goals: Vec<String>,
    #[schemars(description = "Stable idempotency key for this start-chat action.")]
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
pub struct StartChatOutput {
    pub thread_key: String,
    pub started: String,
    pub message: String,
    pub target_edge_handle: Option<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Default)]
pub struct StartChatTool;

impl McpTool for StartChatTool {
    const NAME: &'static str = "core/start_chat";
    const DESCRIPTION: &'static str = "Start a chat thread and emit the first chat message Fact.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] =
        &[ChatStartedV1::SCHEMA_ID, ChatMessageV1::SCHEMA_ID];

    type Args = StartChatArgs;
    type Output = StartChatOutput;

    fn call(
        ctx: McpToolCtx,
        args: StartChatArgs,
    ) -> BoxFuture<'static, Result<StartChatOutput, McpToolError>> {
        Box::pin(async move {
            let caller_self = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput("caller_self_perspective is required".into())
            })?;
            let target_personality = ctx.resolve_personality(&args.target_personality)?;
            let target = resolve_chat_target(&ctx, target_personality).await?;
            let thread_key = match args.thread_key.as_deref() {
                Some(raw) => normalize_text("thread_key", raw, 1, 240)?,
                None => uuid::Uuid::now_v7().to_string(),
            };
            let title = args
                .title
                .as_deref()
                .map(|raw| normalize_text("title", raw, 1, 240))
                .transpose()?;
            let message = normalize_text("message", &args.message, 1, 8000)?;
            let idempotency_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let context_memory_ids = resolve_context_memories(&ctx, &args.context_memories).await?;
            let context_goal_ids = resolve_context_goals(&ctx, &args.context_goals).await?;
            let now = OffsetDateTime::now_utc();
            let started_payload = ChatStartedV1 {
                thread_key: thread_key.clone(),
                started_by_self_perspective_memory_id: caller_self.into_inner(),
                target_personality_instance_id: target.personality_instance_id.into_inner(),
                target_self_perspective_memory_id: target.root_perspective.into_inner(),
                title,
                idempotency_key: idempotency_key.clone(),
                started_at: now,
            };
            let message_payload = ChatMessageV1 {
                thread_key: thread_key.clone(),
                message,
                target_personality_instance_id: target.personality_instance_id.into_inner(),
                target_self_perspective_memory_id: target.root_perspective.into_inner(),
                sent_by_self_perspective_memory_id: caller_self.into_inner(),
                parent_memory_id: None,
                context_memory_ids,
                context_goal_ids,
                idempotency_key,
                sent_at: now,
            };

            let outcome = chat_storage(&ctx)?
                .start_chat_atomic(
                    &ctx.registry,
                    &StartChatInput {
                        owner: ctx.owner.clone(),
                        started: started_payload,
                        message: message_payload,
                        edge_authorship: edge_authorship_for_ctx(&ctx),
                        caller_self,
                    },
                )
                .await?;
            Ok(StartChatOutput {
                thread_key,
                started: ctx.format_fact_memory(outcome.started_memory_id),
                message: ctx.format_fact_memory(outcome.message_memory_id),
                target_edge_handle: outcome
                    .message_edge_id
                    .map(|id| ctx.format_edge(EdgeId::new(id))),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmitChatMessageArgs {
    #[schemars(description = "Personality handle or UUID that should receive this chat message.")]
    pub target_personality: String,
    #[schemars(
        description = "Thread key returned by core/start_chat or a caller-chosen stable key."
    )]
    pub thread_key: String,
    #[schemars(description = "Chat message text.")]
    pub message: String,
    #[serde(default)]
    #[schemars(
        description = "Optional parent chat Fact handle/UUID. Accepts either a core/chat-message-v1 Fact or a core/chat-reply-v1 Fact in the same thread. Do not pass message text."
    )]
    pub parent: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional memory handles to include as exact context. Accepts only Fact/Abstraction/Perspective handles (F..., A..., P...) or raw memory UUIDs; do not put Goal handles here."
    )]
    pub context_memories: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional Goal handles to include as exact context. Use this field for G... handles; do not put goals in context_memories."
    )]
    pub context_goals: Vec<String>,
    #[schemars(description = "Stable idempotency key for this chat message.")]
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
pub struct EmitChatMessageOutput {
    pub handle: String,
    pub target_edge_handle: Option<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Default)]
pub struct EmitChatMessageTool;

impl McpTool for EmitChatMessageTool {
    const NAME: &'static str = "core/emit_chat_message";
    const DESCRIPTION: &'static str =
        "Emit a chat message Fact addressed to one active personality.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[ChatMessageV1::SCHEMA_ID];

    type Args = EmitChatMessageArgs;
    type Output = EmitChatMessageOutput;

    fn call(
        ctx: McpToolCtx,
        args: EmitChatMessageArgs,
    ) -> BoxFuture<'static, Result<EmitChatMessageOutput, McpToolError>> {
        Box::pin(async move {
            let caller_self = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput("caller_self_perspective is required".into())
            })?;
            let target_personality = ctx.resolve_personality(&args.target_personality)?;
            let target = resolve_chat_target(&ctx, target_personality).await?;
            let thread_key = normalize_text("thread_key", &args.thread_key, 1, 240)?;
            let message = normalize_text("message", &args.message, 1, 8000)?;
            let idempotency_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let parent_memory_id = args
                .parent
                .as_deref()
                .map(|raw| ctx.resolve_fact_memory(raw).map(MemoryId::into_inner))
                .transpose()?;
            if let Some(parent) = parent_memory_id {
                let parent_thread =
                    load_chat_parent_thread_key(&ctx, MemoryId::new(parent)).await?;
                if parent_thread != thread_key {
                    return Err(McpToolError::InvalidInput(
                        "chat parent belongs to a different thread".into(),
                    ));
                }
            }
            let context_memory_ids = resolve_context_memories(&ctx, &args.context_memories).await?;
            let context_goal_ids = resolve_context_goals(&ctx, &args.context_goals).await?;
            let payload = ChatMessageV1 {
                thread_key,
                message,
                target_personality_instance_id: target.personality_instance_id.into_inner(),
                target_self_perspective_memory_id: target.root_perspective.into_inner(),
                sent_by_self_perspective_memory_id: caller_self.into_inner(),
                parent_memory_id,
                context_memory_ids,
                context_goal_ids,
                idempotency_key,
                sent_at: OffsetDateTime::now_utc(),
            };
            let outcome = chat_storage(&ctx)?
                .emit_chat_message_atomic(
                    &ctx.registry,
                    &EmitChatMessageInput {
                        owner: ctx.owner.clone(),
                        message: payload,
                        edge_authorship: edge_authorship_for_ctx(&ctx),
                        caller_self,
                    },
                )
                .await?;
            Ok(EmitChatMessageOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                target_edge_handle: outcome.edge_id.map(|id| ctx.format_edge(EdgeId::new(id))),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmitChatReplyArgs {
    #[schemars(
        description = "Chat-message Fact handle/UUID to reply to, for example F1 in a wake. Do not pass the message text."
    )]
    pub reply_to: String,
    #[schemars(description = "Reply text.")]
    pub reply: String,
    #[serde(default)]
    #[schemars(
        description = "Optional memory handles actually used while producing this reply. Accepts only Fact/Abstraction/Perspective handles (F..., A..., P...) or raw memory UUIDs; never pass Goal handles (G...) here."
    )]
    pub context_memories_used: Vec<String>,
    #[schemars(description = "Stable idempotency key for this reply.")]
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
pub struct EmitChatReplyOutput {
    pub handle: String,
    pub reply_edge_handle: Option<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Default)]
pub struct EmitChatReplyTool;

impl McpTool for EmitChatReplyTool {
    const NAME: &'static str = "core/emit_chat_reply";
    const DESCRIPTION: &'static str = "Emit a chat reply Fact for a message addressed to this caller. reply_to must be the triggering chat-message Fact handle, not message text. context_memories_used accepts only F/A/P memory handles, never G Goal handles.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[ChatReplyV1::SCHEMA_ID];

    type Args = EmitChatReplyArgs;
    type Output = EmitChatReplyOutput;

    fn call(
        ctx: McpToolCtx,
        args: EmitChatReplyArgs,
    ) -> BoxFuture<'static, Result<EmitChatReplyOutput, McpToolError>> {
        Box::pin(async move {
            let caller_self = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput("caller_self_perspective is required".into())
            })?;
            let message_memory_id = ctx.resolve_fact_memory(&args.reply_to)?;
            let message = load_message(&ctx, message_memory_id).await?;
            if message.target_self_perspective_memory_id != caller_self.into_inner() {
                return Err(McpToolError::InvalidInput(
                    "caller_self_perspective is not the addressed target".into(),
                ));
            }
            let caller_personality = resolve_personality_for_self(&ctx, caller_self).await?;
            if message.target_personality_instance_id != caller_personality.into_inner() {
                return Err(McpToolError::InvalidInput(
                    "caller personality is not the addressed target".into(),
                ));
            }
            let reply = normalize_text("reply", &args.reply, 1, 12000)?;
            let idempotency_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let context_memory_ids_used =
                resolve_context_memories(&ctx, &args.context_memories_used).await?;
            let payload = ChatReplyV1 {
                message_memory_id: message_memory_id.into_inner(),
                thread_key: message.thread_key,
                reply,
                replied_by_personality_instance_id: caller_personality.into_inner(),
                replied_by_self_perspective_memory_id: caller_self.into_inner(),
                context_memory_ids_used,
                idempotency_key,
                replied_at: OffsetDateTime::now_utc(),
            };
            let outcome = chat_storage(&ctx)?
                .emit_chat_reply_atomic(
                    &ctx.registry,
                    &EmitChatReplyInput {
                        owner: ctx.owner.clone(),
                        reply: payload,
                        message_memory_id,
                        edge_authorship: edge_authorship_for_ctx(&ctx),
                        caller_self,
                    },
                )
                .await?;
            Ok(EmitChatReplyOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                reply_edge_handle: outcome.edge_id.map(|id| ctx.format_edge(EdgeId::new(id))),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CompactChatThreadArgs {
    #[serde(default)]
    #[schemars(description = "Thread key returned by core/start_chat.")]
    pub thread_key: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional chat Fact/Abstraction handle used to resolve the thread. Accepts F... or A... chat handles, not Goal handles."
    )]
    pub anchor: Option<String>,
    #[schemars(description = "Compaction summary for the covered chat turns.")]
    pub summary: String,
    #[serde(default)]
    #[schemars(
        description = "Optional source memory handles to cover. Accepts only Fact/Abstraction/Perspective handles (F..., A..., P...) or raw memory UUIDs; never Goal handles. When omitted, the tool covers current chat thread memories."
    )]
    pub source_memories: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional memory handles actually used while producing this compaction. Accepts only Fact/Abstraction/Perspective handles (F..., A..., P...) or raw memory UUIDs; never Goal handles."
    )]
    pub context_memories_used: Vec<String>,
    #[schemars(description = "Stable idempotency key for this chat compaction.")]
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
pub struct CompactChatThreadOutput {
    pub compaction: String,
    pub provenance_edge_handles: Vec<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Default)]
pub struct CompactChatThreadTool;

impl McpTool for CompactChatThreadTool {
    const NAME: &'static str = "core/compact_chat_thread";
    const DESCRIPTION: &'static str = "Author a chat-compaction Abstraction for a live chat thread. Use thread_key or an F/A chat anchor, write a concise summary, and omit source_memories to cover the current thread graph. Memory fields accept F/A/P handles only, never G Goal handles.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[ChatCompactionV1::SCHEMA_ID];

    type Args = CompactChatThreadArgs;
    type Output = CompactChatThreadOutput;

    fn call(
        ctx: McpToolCtx,
        args: CompactChatThreadArgs,
    ) -> BoxFuture<'static, Result<CompactChatThreadOutput, McpToolError>> {
        Box::pin(async move {
            let caller_self = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput("caller_self_perspective is required".into())
            })?;
            let caller_personality = resolve_personality_for_self(&ctx, caller_self).await?;
            let thread_key =
                resolve_thread_key_arg(&ctx, args.thread_key.as_deref(), args.anchor.as_deref())
                    .await?;
            let summary = normalize_text("summary", &args.summary, 1, 20_000)?;
            let idempotency_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let context_memory_ids_used =
                resolve_context_memories(&ctx, &args.context_memories_used).await?;
            let included_memory_ids = if args.source_memories.is_empty() {
                load_summary_source_memory_ids(&ctx, &thread_key).await?
            } else {
                let sources = resolve_context_memories(&ctx, &args.source_memories).await?;
                validate_chat_source_memories(&ctx, &thread_key, &sources).await?
            };
            if included_memory_ids.is_empty() {
                return Err(McpToolError::InvalidInput(
                    "chat compaction requires at least one source memory".into(),
                ));
            }
            let now = OffsetDateTime::now_utc();
            let compaction_memory_id =
                chat_compaction_memory_id(&ctx.owner, &thread_key, &idempotency_key);
            let payload = ChatCompactionV1 {
                thread_key,
                compacted_by_personality_instance_id: caller_personality.into_inner(),
                compacted_by_self_perspective_memory_id: caller_self.into_inner(),
                summary,
                included_memory_ids,
                context_memory_ids_used,
                idempotency_key,
                compacted_at: now,
            };
            let source_classes =
                load_memory_handle_classes(&ctx, &payload.included_memory_ids).await?;
            let mut classified_sources = Vec::with_capacity(payload.included_memory_ids.len());
            for source in payload.included_memory_ids.iter().copied() {
                let target_kind = entity_kind_for_class_map(&source_classes, source)?;
                if target_kind == EntityKind::Perspective {
                    return Err(McpToolError::InvalidInput(
                        "source_memories for chat compaction cannot include Perspective memories"
                            .into(),
                    ));
                }
                classified_sources.push((source, target_kind));
            }
            let outcome = chat_storage(&ctx)?
                .compact_chat_thread_atomic(
                    &ctx.registry,
                    &CompactChatThreadInput {
                        owner: ctx.owner.clone(),
                        model_id: ctx.author.model_id.clone(),
                        compaction_memory_id,
                        payload,
                        classified_sources,
                        caller_self,
                    },
                )
                .await?;
            Ok(CompactChatThreadOutput {
                compaction: ctx.format_abstraction_memory(MemoryId::new(compaction_memory_id)),
                provenance_edge_handles: outcome
                    .edge_ids
                    .into_iter()
                    .map(|id| ctx.format_edge(EdgeId::new(id)))
                    .collect(),
                idempotent_replay: !outcome.inserted,
            })
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RequestEndChatArgs {
    #[serde(default)]
    #[schemars(description = "Thread key returned by core/start_chat.")]
    pub thread_key: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional chat Fact/Abstraction handle used to resolve the thread. Accepts F... or A... chat handles, not Goal handles."
    )]
    pub anchor: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional target personality handle/UUID. When omitted, the chat-start target is used."
    )]
    pub target_personality: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional closure reason for the summarizing personality.")]
    pub reason: Option<String>,
    #[schemars(description = "Stable idempotency key for this end-chat request.")]
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
pub struct RequestEndChatOutput {
    pub handle: String,
    pub target_edge_handle: Option<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Default)]
pub struct RequestEndChatTool;

impl McpTool for RequestEndChatTool {
    const NAME: &'static str = "core/request_end_chat";
    const DESCRIPTION: &'static str = "Request that a target personality end a chat and summarize it from its own perspective. This emits a chat-end-requested Fact; the addressed personality should answer that Fact with core/end_chat.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[ChatEndRequestedV1::SCHEMA_ID];

    type Args = RequestEndChatArgs;
    type Output = RequestEndChatOutput;

    fn call(
        ctx: McpToolCtx,
        args: RequestEndChatArgs,
    ) -> BoxFuture<'static, Result<RequestEndChatOutput, McpToolError>> {
        Box::pin(async move {
            let caller_self = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput("caller_self_perspective is required".into())
            })?;
            let thread_key =
                resolve_thread_key_arg(&ctx, args.thread_key.as_deref(), args.anchor.as_deref())
                    .await?;
            let target_personality = match args.target_personality.as_deref() {
                Some(raw) => ctx.resolve_personality(raw)?,
                None => load_started_target(&ctx, &thread_key).await?,
            };
            let target = resolve_end_request_target(&ctx, target_personality).await?;
            if thread_is_ended(&ctx, &thread_key).await? {
                return Err(McpToolError::InvalidInput(
                    "chat thread is already ended".into(),
                ));
            }
            let reason = args
                .reason
                .as_deref()
                .map(|raw| normalize_text("reason", raw, 1, 4000))
                .transpose()?;
            let idempotency_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let payload = ChatEndRequestedV1 {
                thread_key,
                target_personality_instance_id: target.personality_instance_id.into_inner(),
                target_self_perspective_memory_id: target.root_perspective.into_inner(),
                requested_by_self_perspective_memory_id: caller_self.into_inner(),
                reason,
                idempotency_key,
                requested_at: OffsetDateTime::now_utc(),
            };
            let outcome = chat_storage(&ctx)?
                .request_end_chat_atomic(
                    &ctx.registry,
                    &RequestEndChatInput {
                        owner: ctx.owner.clone(),
                        request: payload,
                        edge_authorship: edge_authorship_for_ctx(&ctx),
                        caller_self,
                    },
                )
                .await?;
            Ok(RequestEndChatOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                target_edge_handle: outcome.edge_id.map(|id| ctx.format_edge(EdgeId::new(id))),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EndChatArgs {
    #[schemars(
        description = "Chat-end-requested Fact handle/UUID that woke this personality. In a core/chat-end-requested-v1 wake, pass the triggering memory handle from wake context. Do not pass the request text."
    )]
    pub request: String,
    #[schemars(
        description = "Summary authored by the addressed personality from its perspective after inspecting the thread, usually via core/get_chat_thread with anchor set to the same request handle."
    )]
    pub summary: String,
    #[serde(default)]
    #[schemars(
        description = "Optional memory handles actually used while producing this summary. Accepts only Fact/Abstraction/Perspective handles (F..., A..., P...) or raw memory UUIDs; never pass Goal handles (G...) here."
    )]
    pub context_memories_used: Vec<String>,
    #[schemars(description = "Stable idempotency key for this end-chat action.")]
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
pub struct EndChatOutput {
    pub ended: String,
    pub summary: String,
    pub provenance_edge_handles: Vec<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Default)]
pub struct EndChatTool;

impl McpTool for EndChatTool {
    const NAME: &'static str = "core/end_chat";
    const DESCRIPTION: &'static str = "End a chat after a chat-end-requested Fact and author a chat-summary Abstraction. In a chat-end-requested wake, first inspect the thread with core/get_chat_thread(anchor=request), then call this with request set to the triggering Fact handle. context_memories_used accepts only F/A/P memory handles, never G Goal handles.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] =
        &[ChatEndedV1::SCHEMA_ID, ChatSummaryV1::SCHEMA_ID];

    type Args = EndChatArgs;
    type Output = EndChatOutput;

    fn call(
        ctx: McpToolCtx,
        args: EndChatArgs,
    ) -> BoxFuture<'static, Result<EndChatOutput, McpToolError>> {
        Box::pin(async move {
            let caller_self = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput("caller_self_perspective is required".into())
            })?;
            let request_memory_id = ctx.resolve_fact_memory(&args.request)?;
            let request = load_end_request(&ctx, request_memory_id).await?;
            if request.target_self_perspective_memory_id != caller_self.into_inner() {
                return Err(McpToolError::InvalidInput(
                    "caller_self_perspective is not the addressed end-chat target".into(),
                ));
            }
            let caller_personality = resolve_personality_for_self(&ctx, caller_self).await?;
            if request.target_personality_instance_id != caller_personality.into_inner() {
                return Err(McpToolError::InvalidInput(
                    "caller personality is not the addressed end-chat target".into(),
                ));
            }
            if let Some(existing) =
                load_existing_end_by_request(&ctx, request_memory_id.into_inner()).await?
            {
                return Ok(existing);
            }
            if thread_is_ended(&ctx, &request.thread_key).await? {
                return Err(McpToolError::InvalidInput(
                    "chat thread is already ended".into(),
                ));
            }
            let summary_text = normalize_text("summary", &args.summary, 1, 20_000)?;
            let idempotency_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let context_memory_ids_used =
                resolve_context_memories(&ctx, &args.context_memories_used).await?;
            let included_memory_ids =
                load_summary_source_memory_ids(&ctx, &request.thread_key).await?;
            let now = OffsetDateTime::now_utc();
            let summary_memory_id = chat_summary_memory_id(
                &ctx.owner,
                &request.thread_key,
                request_memory_id.into_inner(),
                &idempotency_key,
            );
            let ended_payload = ChatEndedV1 {
                thread_key: request.thread_key.clone(),
                request_memory_id: request_memory_id.into_inner(),
                ended_by_personality_instance_id: caller_personality.into_inner(),
                ended_by_self_perspective_memory_id: caller_self.into_inner(),
                summary_memory_id,
                idempotency_key: idempotency_key.clone(),
                ended_at: now,
            };
            // `ended_memory_id` is filled in by `end_chat_atomic` once the
            // chat-ended Fact is ingested; the nil here is a placeholder.
            let summary_payload = ChatSummaryV1 {
                thread_key: request.thread_key,
                request_memory_id: request_memory_id.into_inner(),
                ended_memory_id: uuid::Uuid::nil(),
                summarized_by_personality_instance_id: caller_personality.into_inner(),
                summarized_by_self_perspective_memory_id: caller_self.into_inner(),
                summary: summary_text,
                included_memory_ids,
                context_memory_ids_used,
                idempotency_key,
                summarized_at: now,
            };
            let mut to_classify = summary_payload.included_memory_ids.clone();
            to_classify.push(request_memory_id.into_inner());
            let classified_sources: Vec<(uuid::Uuid, EntityKind)> =
                load_memory_handle_classes(&ctx, &to_classify)
                    .await?
                    .into_iter()
                    .map(|(id, class)| (id, entity_kind_for_class(class)))
                    .collect();
            let outcome = chat_storage(&ctx)?
                .end_chat_atomic(
                    &ctx.registry,
                    &EndChatInput {
                        owner: ctx.owner.clone(),
                        model_id: ctx.author.model_id.clone(),
                        summary_memory_id,
                        ended: ended_payload,
                        summary: summary_payload,
                        classified_sources,
                        caller_self,
                    },
                )
                .await?;
            Ok(EndChatOutput {
                ended: ctx.format_fact_memory(outcome.ended_memory_id),
                summary: ctx.format_abstraction_memory(MemoryId::new(summary_memory_id)),
                provenance_edge_handles: outcome
                    .edge_ids
                    .into_iter()
                    .map(|id| ctx.format_edge(EdgeId::new(id)))
                    .collect(),
                idempotent_replay: outcome.ended_idempotent_replay,
            })
        })
    }
}
