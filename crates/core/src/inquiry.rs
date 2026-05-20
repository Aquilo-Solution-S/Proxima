use std::collections::{HashMap, HashSet};

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;

use crate::approval::{
    ApprovalDecision, ApprovalEligibleVoter, ApprovalRequirement, ApprovalTargetKind,
    ApprovalVoteVerdict, ApprovalVoterKind,
};
use crate::mcp::{McpTool, McpToolCtx, McpToolError, MemoryHandleClass};
use crate::personality::{
    PersonalityInstanceId, PersonalityStatus, WakeEntryRow, WakeEntryTriggerKind,
    writeable_schemas_for_palette,
};
use crate::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use crate::{
    CORE_ANSWERS_QUESTION_RELATION, CORE_DERIVED_FROM_RELATION,
    CORE_HAS_APPROVAL_DECISION_RELATION, CORE_HAS_APPROVAL_POLICY_RELATION,
    CORE_RECEIVES_DIRECTED_QUESTION_RELATION, CORE_VOTES_ON_RELATION, EdgeAuthorshipKind, EdgeId,
    Engine, EntityKind, FactPayload, GoalId, MemoryId, Owner, OwnerPrincipalKind, Principal,
    SchemaId, SchemaVersion, SourceBatchId, SourceId, StorageError,
};

pub const DIRECTED_QUESTION_SCHEMA_ID: &str = "core/directed-question-v1";
pub const DIRECTED_ANSWER_SCHEMA_ID: &str = "core/directed-answer-v1";

const INQUIRY_SOURCE_ID: &str = "core/directed-inquiry";
const QUESTION_OBJECT_SCHEMA: &str = "core/directed-question-object-v1";
const QUESTION_WHOLE_SCHEMA: &str = "core/directed-question-whole-v1";
const ANSWER_OBJECT_SCHEMA: &str = "core/directed-answer-object-v1";
const ANSWER_WHOLE_SCHEMA: &str = "core/directed-answer-whole-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct DirectedQuestionV1 {
    pub thread_key: String,
    pub question: String,
    pub target_personality_instance_id: uuid::Uuid,
    pub target_self_perspective_memory_id: uuid::Uuid,
    pub asked_by_self_perspective_memory_id: uuid::Uuid,
    #[serde(default)]
    pub parent_question_memory_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub context_memory_ids: Vec<uuid::Uuid>,
    #[serde(default)]
    pub context_goal_ids: Vec<uuid::Uuid>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub asked_at: OffsetDateTime,
}

impl FactPayload for DirectedQuestionV1 {
    const SCHEMA_ID: &'static str = DIRECTED_QUESTION_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.directed_question_v1"
    }

    fn render(&self) -> String {
        format!("Directed question: {}", self.question)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct DirectedAnswerV1 {
    pub question_memory_id: uuid::Uuid,
    pub thread_key: String,
    pub answer: String,
    pub answered_by_personality_instance_id: uuid::Uuid,
    pub answered_by_self_perspective_memory_id: uuid::Uuid,
    #[serde(default)]
    pub context_memory_ids_used: Vec<uuid::Uuid>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub answered_at: OffsetDateTime,
}

impl FactPayload for DirectedAnswerV1 {
    const SCHEMA_ID: &'static str = DIRECTED_ANSWER_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.directed_answer_v1"
    }

    fn render(&self) -> String {
        "Directed answer".into()
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InquiryWakeEntryOutput {
    pub wake_entry: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InquiryTargetOutput {
    pub personality: String,
    pub display_name: String,
    pub root_perspective: String,
    pub directed_question_wake_entries: Vec<InquiryWakeEntryOutput>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListInquiryTargetsArgs {
    #[serde(default)]
    pub include_self: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListInquiryTargetsOutput {
    pub caller_self_perspective: String,
    pub targets: Vec<InquiryTargetOutput>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetInquiryThreadArgs {
    #[serde(default)]
    pub thread_key: Option<String>,
    #[serde(default)]
    pub anchor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct GetInquiryThreadOutput {
    pub thread_key: String,
    pub questions: Vec<ThreadQuestion>,
    pub answers: Vec<ThreadAnswer>,
    pub approval_policies: Vec<ThreadApprovalPolicy>,
    pub approval_votes: Vec<ThreadApprovalVote>,
    pub approval_decisions: Vec<ThreadApprovalDecision>,
    pub edges: Vec<ThreadEdge>,
    pub open_items: ThreadOpenItems,
}

#[derive(Debug, Serialize)]
pub struct ThreadQuestion {
    pub handle: String,
    pub thread_key: String,
    pub question: String,
    pub target_personality: String,
    pub target_self_perspective: String,
    pub asked_by_self_perspective: String,
    pub parent_question: Option<String>,
    pub context_memories: Vec<String>,
    pub context_goals: Vec<String>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub asked_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct ThreadAnswer {
    pub handle: String,
    pub question: String,
    pub thread_key: String,
    pub answer: String,
    pub answered_by_personality: String,
    pub answered_by_self_perspective: String,
    pub context_memories_used: Vec<String>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub answered_at: OffsetDateTime,
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
    pub unanswered_questions: Vec<String>,
    pub undecided_policies: Vec<String>,
}

#[derive(Debug, Default)]
pub struct ListInquiryTargetsTool;

impl McpTool for ListInquiryTargetsTool {
    const NAME: &'static str = "core/list_inquiry_targets";
    const DESCRIPTION: &'static str =
        "List active personalities this caller can ask through core directed inquiry.";

    type Args = ListInquiryTargetsArgs;
    type Output = ListInquiryTargetsOutput;

    fn call(
        ctx: McpToolCtx,
        args: ListInquiryTargetsArgs,
    ) -> BoxFuture<'static, Result<ListInquiryTargetsOutput, McpToolError>> {
        Box::pin(async move {
            let caller_self = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput("caller_self_perspective is required".into())
            })?;
            let targets = list_askable_targets(&ctx, Some(caller_self), args.include_self).await?;
            Ok(ListInquiryTargetsOutput {
                caller_self_perspective: ctx.format_perspective_memory(caller_self),
                targets,
            })
        })
    }
}

#[derive(Debug, Default)]
pub struct GetInquiryThreadTool;

impl McpTool for GetInquiryThreadTool {
    const NAME: &'static str = "core/get_inquiry_thread";
    const DESCRIPTION: &'static str = "Return the graph-derived directed inquiry and approval thread for one thread key or anchor.";

    type Args = GetInquiryThreadArgs;
    type Output = GetInquiryThreadOutput;

    fn call(
        ctx: McpToolCtx,
        args: GetInquiryThreadArgs,
    ) -> BoxFuture<'static, Result<GetInquiryThreadOutput, McpToolError>> {
        Box::pin(async move {
            let thread_key = match (args.thread_key.as_deref(), args.anchor.as_deref()) {
                (Some(_), Some(_)) | (None, None) => {
                    return Err(McpToolError::InvalidInput(
                        "exactly one of thread_key or anchor is required".into(),
                    ));
                }
                (Some(raw), None) => normalize_text("thread_key", raw, 1, 240)?,
                (None, Some(anchor)) => resolve_thread_key_from_anchor(&ctx, anchor).await?,
            };
            let limit = args.limit.unwrap_or(100).clamp(1, 200);
            load_inquiry_thread(&ctx, thread_key, i64::from(limit)).await
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmitDirectedQuestionArgs {
    pub target_personality: String,
    pub thread_key: String,
    pub question: String,
    #[serde(default)]
    pub parent_question: Option<String>,
    #[serde(default)]
    pub context_memories: Vec<String>,
    #[serde(default)]
    pub context_goals: Vec<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
pub struct EmitDirectedQuestionOutput {
    pub handle: String,
    pub target_edge_handle: Option<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Default)]
pub struct EmitDirectedQuestionTool;

impl McpTool for EmitDirectedQuestionTool {
    const NAME: &'static str = "core/emit_directed_question";
    const DESCRIPTION: &'static str =
        "Emit a directed question Fact addressed to one active personality.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[DirectedQuestionV1::SCHEMA_ID];

    type Args = EmitDirectedQuestionArgs;
    type Output = EmitDirectedQuestionOutput;

    fn call(
        ctx: McpToolCtx,
        args: EmitDirectedQuestionArgs,
    ) -> BoxFuture<'static, Result<EmitDirectedQuestionOutput, McpToolError>> {
        Box::pin(async move {
            let caller_self = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput("caller_self_perspective is required".into())
            })?;
            let target_personality = ctx.resolve_personality(&args.target_personality)?;
            let target = resolve_askable_target(&ctx, target_personality).await?;
            let thread_key = normalize_text("thread_key", &args.thread_key, 1, 240)?;
            let question = normalize_text("question", &args.question, 1, 8000)?;
            let idempotency_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let parent_question_memory_id = args
                .parent_question
                .as_deref()
                .map(|raw| ctx.resolve_fact_memory(raw).map(MemoryId::into_inner))
                .transpose()?;
            if let Some(parent) = parent_question_memory_id {
                load_question(&ctx, MemoryId::new(parent)).await?;
            }
            let context_memory_ids = resolve_context_memories(&ctx, &args.context_memories).await?;
            let context_goal_ids = resolve_context_goals(&ctx, &args.context_goals).await?;
            let payload = DirectedQuestionV1 {
                thread_key,
                question,
                target_personality_instance_id: target.personality_instance_id.into_inner(),
                target_self_perspective_memory_id: target.root_perspective.into_inner(),
                asked_by_self_perspective_memory_id: caller_self.into_inner(),
                parent_question_memory_id,
                context_memory_ids,
                context_goal_ids,
                idempotency_key,
                asked_at: OffsetDateTime::now_utc(),
            };
            let mut tx = ctx.pool.begin().await.map_err(map_sql)?;
            let outcome = ingest_inquiry_fact(&mut tx, &ctx, &payload).await?;
            let edge_id = if outcome.idempotent_replay {
                None
            } else {
                insert_question_sidecar(&mut tx, outcome.memory_id, &payload).await?;
                Some(
                    append_edge(
                        &mut tx,
                        &ctx,
                        CORE_RECEIVES_DIRECTED_QUESTION_RELATION,
                        EntityKind::Perspective,
                        Some(target.root_perspective.into_inner()),
                        None,
                        EntityKind::Fact,
                        Some(outcome.memory_id.into_inner()),
                        None,
                        edge_authorship_for_ctx(&ctx),
                    )
                    .await?,
                )
            };
            tx.commit().await.map_err(map_sql)?;
            Ok(EmitDirectedQuestionOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                target_edge_handle: edge_id.map(|id| ctx.format_edge(EdgeId::new(id))),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmitDirectedAnswerArgs {
    pub question: String,
    pub answer: String,
    #[serde(default)]
    pub context_memories_used: Vec<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
pub struct EmitDirectedAnswerOutput {
    pub handle: String,
    pub answer_edge_handle: Option<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Default)]
pub struct EmitDirectedAnswerTool;

impl McpTool for EmitDirectedAnswerTool {
    const NAME: &'static str = "core/emit_directed_answer";
    const DESCRIPTION: &'static str =
        "Emit a directed answer Fact for a question addressed to this caller.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[DirectedAnswerV1::SCHEMA_ID];

    type Args = EmitDirectedAnswerArgs;
    type Output = EmitDirectedAnswerOutput;

    fn call(
        ctx: McpToolCtx,
        args: EmitDirectedAnswerArgs,
    ) -> BoxFuture<'static, Result<EmitDirectedAnswerOutput, McpToolError>> {
        Box::pin(async move {
            let caller_self = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput("caller_self_perspective is required".into())
            })?;
            let question_memory_id = ctx.resolve_fact_memory(&args.question)?;
            let question = load_question(&ctx, question_memory_id).await?;
            if question.target_self_perspective_memory_id != caller_self.into_inner() {
                return Err(McpToolError::InvalidInput(
                    "caller_self_perspective is not the addressed target".into(),
                ));
            }
            let caller_personality = resolve_personality_for_self(&ctx, caller_self).await?;
            if question.target_personality_instance_id != caller_personality.into_inner() {
                return Err(McpToolError::InvalidInput(
                    "caller personality is not the addressed target".into(),
                ));
            }
            let answer = normalize_text("answer", &args.answer, 1, 12000)?;
            let idempotency_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let context_memory_ids_used =
                resolve_context_memories(&ctx, &args.context_memories_used).await?;
            let payload = DirectedAnswerV1 {
                question_memory_id: question_memory_id.into_inner(),
                thread_key: question.thread_key,
                answer,
                answered_by_personality_instance_id: caller_personality.into_inner(),
                answered_by_self_perspective_memory_id: caller_self.into_inner(),
                context_memory_ids_used,
                idempotency_key,
                answered_at: OffsetDateTime::now_utc(),
            };
            let mut tx = ctx.pool.begin().await.map_err(map_sql)?;
            let outcome = ingest_inquiry_fact(&mut tx, &ctx, &payload).await?;
            let edge_id = if outcome.idempotent_replay {
                None
            } else {
                insert_answer_sidecar(&mut tx, outcome.memory_id, &payload).await?;
                Some(
                    append_edge(
                        &mut tx,
                        &ctx,
                        CORE_ANSWERS_QUESTION_RELATION,
                        EntityKind::Fact,
                        Some(outcome.memory_id.into_inner()),
                        None,
                        EntityKind::Fact,
                        Some(question_memory_id.into_inner()),
                        None,
                        edge_authorship_for_ctx(&ctx),
                    )
                    .await?,
                )
            };
            tx.commit().await.map_err(map_sql)?;
            Ok(EmitDirectedAnswerOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                answer_edge_handle: edge_id.map(|id| ctx.format_edge(EdgeId::new(id))),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WakeCoordinationContext {
    pub askable_personalities: Vec<WakeCoordinationTarget>,
    pub wake_path: WakePath,
}

#[derive(Debug, Clone, Serialize)]
pub struct WakeCoordinationTarget {
    pub personality_instance_id: uuid::Uuid,
    pub display_name: String,
    pub root_perspective_memory_id: uuid::Uuid,
    pub directed_question_wake_entry_ids: Vec<uuid::Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WakePath {
    pub upstream: Vec<WakePathNode>,
    pub current: WakePathNode,
    pub downstream: Vec<WakePathNode>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WakePathNode {
    pub personality_instance_id: uuid::Uuid,
    pub display_name: String,
    pub root_perspective_memory_id: uuid::Uuid,
    pub wake_entry_id: uuid::Uuid,
    pub wake_entry_label: String,
    pub trigger_schema_id: String,
    pub produces_schema_ids: Vec<String>,
}

pub async fn build_wake_coordination_context(
    engine: &Engine,
    owner: &Owner,
    current_personality: PersonalityInstanceId,
    current_wake_entry: &WakeEntryRow,
) -> Result<WakeCoordinationContext, StorageError> {
    let rows = engine
        .storage()
        .list_personality_instances(owner, false)
        .await?;
    let mut askable_personalities = Vec::new();
    let mut current = None;
    let mut all_nodes = Vec::new();

    for row in rows {
        if row.status != PersonalityStatus::Active {
            continue;
        }
        let directed_question_wake_entry_ids: Vec<_> = row
            .wake_entries
            .iter()
            .filter(|entry| is_enabled_directed_question_wake(entry))
            .map(|entry| entry.wake_entry_id)
            .collect();
        if row.personality_instance_id != current_personality
            && !directed_question_wake_entry_ids.is_empty()
        {
            askable_personalities.push(WakeCoordinationTarget {
                personality_instance_id: row.personality_instance_id.into_inner(),
                display_name: row.display_name.clone(),
                root_perspective_memory_id: row.current_root_perspective_memory_id.into_inner(),
                directed_question_wake_entry_ids,
            });
        }

        for entry in row.wake_entries {
            if !entry.enabled || entry.trigger_kind != WakeEntryTriggerKind::OnMemory {
                continue;
            }
            let node = WakePathNode {
                personality_instance_id: row.personality_instance_id.into_inner(),
                display_name: row.display_name.clone(),
                root_perspective_memory_id: row.current_root_perspective_memory_id.into_inner(),
                wake_entry_id: entry.wake_entry_id,
                wake_entry_label: entry.label.clone(),
                trigger_schema_id: entry.trigger_id.clone(),
                produces_schema_ids: writeable_schemas_for_palette(
                    engine,
                    &entry.substrate_tool_palette,
                ),
            };
            if row.personality_instance_id == current_personality
                && entry.wake_entry_id == current_wake_entry.wake_entry_id
            {
                current = Some(node.clone());
            }
            all_nodes.push(node);
        }
    }

    let current = current.unwrap_or_else(|| WakePathNode {
        personality_instance_id: current_personality.into_inner(),
        display_name: String::new(),
        root_perspective_memory_id: uuid::Uuid::nil(),
        wake_entry_id: current_wake_entry.wake_entry_id,
        wake_entry_label: current_wake_entry.label.clone(),
        trigger_schema_id: current_wake_entry.trigger_id.clone(),
        produces_schema_ids: writeable_schemas_for_palette(
            engine,
            &current_wake_entry.substrate_tool_palette,
        ),
    });
    let current_produces: HashSet<_> = current.produces_schema_ids.iter().cloned().collect();
    let upstream = all_nodes
        .iter()
        .filter(|node| {
            node.wake_entry_id != current.wake_entry_id
                && node
                    .produces_schema_ids
                    .iter()
                    .any(|schema| schema == &current.trigger_schema_id)
        })
        .cloned()
        .collect();
    let downstream = all_nodes
        .into_iter()
        .filter(|node| {
            node.wake_entry_id != current.wake_entry_id
                && current_produces.contains(&node.trigger_schema_id)
        })
        .collect();

    Ok(WakeCoordinationContext {
        askable_personalities,
        wake_path: WakePath {
            upstream,
            current,
            downstream,
        },
    })
}

#[derive(Clone)]
struct AskableTarget {
    personality_instance_id: PersonalityInstanceId,
    root_perspective: MemoryId,
}

async fn list_askable_targets(
    ctx: &McpToolCtx,
    caller_self: Option<MemoryId>,
    include_self: bool,
) -> Result<Vec<InquiryTargetOutput>, McpToolError> {
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
            .filter(|entry| is_enabled_directed_question_wake(entry))
            .map(|entry| InquiryWakeEntryOutput {
                wake_entry: ctx.format_wake_entry(entry.wake_entry_id),
                label: entry.label.clone(),
            })
            .collect();
        if wake_entries.is_empty() {
            continue;
        }
        targets.push(InquiryTargetOutput {
            personality: ctx.format_personality(row.personality_instance_id),
            display_name: row.display_name,
            root_perspective: ctx.format_perspective_memory(row.current_root_perspective_memory_id),
            directed_question_wake_entries: wake_entries,
        });
    }
    Ok(targets)
}

async fn resolve_askable_target(
    ctx: &McpToolCtx,
    target_personality: PersonalityInstanceId,
) -> Result<AskableTarget, McpToolError> {
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
        .filter(is_enabled_directed_question_wake)
        .collect();
    if wake_entries.is_empty() {
        return Err(McpToolError::InvalidInput(
            "target personality has no enabled directed-question wake entry".into(),
        ));
    }
    Ok(AskableTarget {
        personality_instance_id: row.personality_instance_id,
        root_perspective: row.current_root_perspective_memory_id,
    })
}

fn is_enabled_directed_question_wake(entry: &WakeEntryRow) -> bool {
    entry.enabled
        && entry.trigger_kind == WakeEntryTriggerKind::OnMemory
        && entry.trigger_id == DIRECTED_QUESTION_SCHEMA_ID
}

async fn resolve_personality_for_self(
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

#[derive(Clone)]
struct LoadedQuestion {
    memory_id: MemoryId,
    payload: DirectedQuestionV1,
}

#[derive(Clone)]
struct LoadedAnswer {
    memory_id: MemoryId,
    payload: DirectedAnswerV1,
}

#[derive(Clone)]
struct LoadedApprovalPolicy {
    memory_id: MemoryId,
    target_kind: ApprovalTargetKind,
    target_memory_id: Option<uuid::Uuid>,
    target_goal_id: Option<uuid::Uuid>,
    title: String,
    summary: String,
    eligible_voters: Vec<ApprovalEligibleVoter>,
    requirements: Vec<ApprovalRequirement>,
    idempotency_key: String,
    created_at: OffsetDateTime,
}

#[derive(Clone)]
struct LoadedApprovalVote {
    memory_id: MemoryId,
    policy_memory_id: uuid::Uuid,
    voter_key: String,
    voter_kind: ApprovalVoterKind,
    role: Option<String>,
    personality_instance_id: Option<uuid::Uuid>,
    self_perspective_memory_id: Option<uuid::Uuid>,
    master_token_id: Option<uuid::Uuid>,
    verdict: ApprovalVoteVerdict,
    rationale: String,
    idempotency_key: String,
    voted_at: OffsetDateTime,
}

#[derive(Clone)]
struct LoadedApprovalDecision {
    memory_id: MemoryId,
    policy_memory_id: uuid::Uuid,
    target_kind: ApprovalTargetKind,
    target_memory_id: Option<uuid::Uuid>,
    target_goal_id: Option<uuid::Uuid>,
    decision: ApprovalDecision,
    reason: String,
    counted_votes: Vec<ThreadApprovalCountedVoteRaw>,
    idempotency_key: String,
    decided_at: OffsetDateTime,
}

#[derive(Clone, Deserialize)]
struct ThreadApprovalCountedVoteRaw {
    vote_memory_id: uuid::Uuid,
    voter_key: String,
    verdict: ApprovalVoteVerdict,
}

struct LoadedThreadEdge {
    edge_id: EdgeId,
    relation: String,
    source_kind: EntityKind,
    source_memory_id: Option<uuid::Uuid>,
    source_goal_id: Option<uuid::Uuid>,
    target_kind: EntityKind,
    target_memory_id: Option<uuid::Uuid>,
    target_goal_id: Option<uuid::Uuid>,
    authorship_kind: EdgeAuthorshipKind,
    created_at: OffsetDateTime,
}

async fn load_inquiry_thread(
    ctx: &McpToolCtx,
    thread_key: String,
    limit: i64,
) -> Result<GetInquiryThreadOutput, McpToolError> {
    let questions = load_thread_questions(ctx, &thread_key, limit).await?;
    let answers = load_thread_answers(ctx, &thread_key, limit).await?;
    let inquiry_memory_ids: Vec<_> = questions
        .iter()
        .map(|question| question.memory_id.into_inner())
        .chain(answers.iter().map(|answer| answer.memory_id.into_inner()))
        .collect();
    let policies = load_thread_approval_policies(ctx, &inquiry_memory_ids, limit).await?;
    let policy_ids: Vec<_> = policies
        .iter()
        .map(|policy| policy.memory_id.into_inner())
        .collect();
    let votes = load_thread_approval_votes(ctx, &policy_ids, limit).await?;
    let decisions = load_thread_approval_decisions(ctx, &policy_ids, limit).await?;
    let thread_memory_ids: Vec<_> = inquiry_memory_ids
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
    let context_memory_ids: Vec<_> = questions
        .iter()
        .flat_map(|question| question.payload.context_memory_ids.iter().copied())
        .chain(
            answers
                .iter()
                .flat_map(|answer| answer.payload.context_memory_ids_used.iter().copied()),
        )
        .collect();
    let context_memory_classes = load_memory_handle_classes(ctx, &context_memory_ids).await?;

    let answered_question_ids: HashSet<_> = answers
        .iter()
        .map(|answer| answer.payload.question_memory_id)
        .collect();
    let decided_policy_ids: HashSet<_> = decisions
        .iter()
        .map(|decision| decision.policy_memory_id)
        .collect();
    let open_items = ThreadOpenItems {
        unanswered_questions: questions
            .iter()
            .filter(|question| !answered_question_ids.contains(&question.memory_id.into_inner()))
            .map(|question| ctx.format_fact_memory(question.memory_id))
            .collect(),
        undecided_policies: policies
            .iter()
            .filter(|policy| !decided_policy_ids.contains(&policy.memory_id.into_inner()))
            .map(|policy| ctx.format_fact_memory(policy.memory_id))
            .collect(),
    };

    Ok(GetInquiryThreadOutput {
        thread_key,
        questions: questions
            .into_iter()
            .map(|question| render_thread_question(ctx, question, &context_memory_classes))
            .collect::<Result<_, _>>()?,
        answers: answers
            .into_iter()
            .map(|answer| render_thread_answer(ctx, answer, &context_memory_classes))
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

async fn resolve_thread_key_from_anchor(
    ctx: &McpToolCtx,
    anchor: &str,
) -> Result<String, McpToolError> {
    let memory_id = ctx.resolve_memory(anchor)?;
    if let Some(thread_key) = thread_key_for_inquiry_memory(ctx, memory_id.into_inner()).await? {
        return Ok(thread_key);
    }
    if let Some(policy_id) = policy_id_for_vote_or_decision(ctx, memory_id.into_inner()).await? {
        return thread_key_for_policy(ctx, policy_id).await;
    }
    thread_key_for_policy(ctx, memory_id.into_inner()).await
}

async fn thread_key_for_policy(
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
            "anchor is not an inquiry thread Fact".into(),
        ));
    };
    thread_key_for_inquiry_memory(ctx, target_memory_id)
        .await?
        .ok_or_else(|| {
            McpToolError::InvalidInput("anchor target is not in an inquiry thread".into())
        })
}

async fn policy_id_for_vote_or_decision(
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

async fn thread_key_for_inquiry_memory(
    ctx: &McpToolCtx,
    memory_id: uuid::Uuid,
) -> Result<Option<String>, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    sqlx::query_scalar(
        "SELECT q.thread_key
           FROM proxima_core.directed_question_v1 q
           JOIN proxima_core.memories m USING (memory_id)
          WHERE q.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
         UNION ALL
         SELECT a.thread_key
           FROM proxima_core.directed_answer_v1 a
           JOIN proxima_core.memories m USING (memory_id)
          WHERE a.memory_id = $1
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

async fn load_thread_questions(
    ctx: &McpToolCtx,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedQuestion>, McpToolError> {
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
        "SELECT q.memory_id, q.thread_key, q.question, q.target_personality_instance_id,
                q.target_self_perspective_memory_id, q.asked_by_self_perspective_memory_id,
                q.parent_question_memory_id, q.context_memory_ids, q.context_goal_ids,
                q.idempotency_key, q.asked_at
           FROM proxima_core.directed_question_v1 q
           JOIN proxima_core.memories m USING (memory_id)
          WHERE q.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY q.asked_at ASC, q.memory_id ASC
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
                question,
                target_personality_instance_id,
                target_self_perspective_memory_id,
                asked_by_self_perspective_memory_id,
                parent_question_memory_id,
                context_memory_ids,
                context_goal_ids,
                idempotency_key,
                asked_at,
            )| LoadedQuestion {
                memory_id: MemoryId::new(memory_id),
                payload: DirectedQuestionV1 {
                    thread_key,
                    question,
                    target_personality_instance_id,
                    target_self_perspective_memory_id,
                    asked_by_self_perspective_memory_id,
                    parent_question_memory_id,
                    context_memory_ids,
                    context_goal_ids,
                    idempotency_key,
                    asked_at,
                },
            },
        )
        .collect())
}

async fn load_thread_answers(
    ctx: &McpToolCtx,
    thread_key: &str,
    limit: i64,
) -> Result<Vec<LoadedAnswer>, McpToolError> {
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
        "SELECT a.memory_id, a.question_memory_id, a.thread_key, a.answer,
                a.answered_by_personality_instance_id,
                a.answered_by_self_perspective_memory_id,
                a.context_memory_ids_used, a.idempotency_key, a.answered_at
           FROM proxima_core.directed_answer_v1 a
           JOIN proxima_core.memories m USING (memory_id)
          WHERE a.thread_key = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY a.answered_at ASC, a.memory_id ASC
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
                question_memory_id,
                thread_key,
                answer,
                answered_by_personality_instance_id,
                answered_by_self_perspective_memory_id,
                context_memory_ids_used,
                idempotency_key,
                answered_at,
            )| LoadedAnswer {
                memory_id: MemoryId::new(memory_id),
                payload: DirectedAnswerV1 {
                    question_memory_id,
                    thread_key,
                    answer,
                    answered_by_personality_instance_id,
                    answered_by_self_perspective_memory_id,
                    context_memory_ids_used,
                    idempotency_key,
                    answered_at,
                },
            },
        )
        .collect())
}

async fn load_thread_approval_policies(
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

async fn load_thread_approval_votes(
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

async fn load_thread_approval_decisions(
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

async fn load_thread_edges(
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
        CORE_RECEIVES_DIRECTED_QUESTION_RELATION,
        CORE_ANSWERS_QUESTION_RELATION,
        CORE_HAS_APPROVAL_POLICY_RELATION,
        CORE_VOTES_ON_RELATION,
        CORE_HAS_APPROVAL_DECISION_RELATION,
        CORE_DERIVED_FROM_RELATION,
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
                if relation == CORE_RECEIVES_DIRECTED_QUESTION_RELATION {
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

fn endpoint_in_thread(endpoint: &Option<uuid::Uuid>, thread_memory_ids: &[uuid::Uuid]) -> bool {
    endpoint
        .as_ref()
        .is_some_and(|id| thread_memory_ids.contains(id))
}

fn render_thread_question(
    ctx: &McpToolCtx,
    question: LoadedQuestion,
    memory_classes: &HashMap<uuid::Uuid, MemoryHandleClass>,
) -> Result<ThreadQuestion, McpToolError> {
    let payload = question.payload;
    Ok(ThreadQuestion {
        handle: ctx.format_fact_memory(question.memory_id),
        thread_key: payload.thread_key,
        question: payload.question,
        target_personality: ctx.format_personality(PersonalityInstanceId::new(
            payload.target_personality_instance_id,
        )),
        target_self_perspective: ctx
            .format_perspective_memory(MemoryId::new(payload.target_self_perspective_memory_id)),
        asked_by_self_perspective: ctx
            .format_perspective_memory(MemoryId::new(payload.asked_by_self_perspective_memory_id)),
        parent_question: payload
            .parent_question_memory_id
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
        asked_at: payload.asked_at,
    })
}

fn render_thread_answer(
    ctx: &McpToolCtx,
    answer: LoadedAnswer,
    memory_classes: &HashMap<uuid::Uuid, MemoryHandleClass>,
) -> Result<ThreadAnswer, McpToolError> {
    let payload = answer.payload;
    Ok(ThreadAnswer {
        handle: ctx.format_fact_memory(answer.memory_id),
        question: ctx.format_fact_memory(MemoryId::new(payload.question_memory_id)),
        thread_key: payload.thread_key,
        answer: payload.answer,
        answered_by_personality: ctx.format_personality(PersonalityInstanceId::new(
            payload.answered_by_personality_instance_id,
        )),
        answered_by_self_perspective: ctx.format_perspective_memory(MemoryId::new(
            payload.answered_by_self_perspective_memory_id,
        )),
        context_memories_used: payload
            .context_memory_ids_used
            .into_iter()
            .map(|id| format_memory_from_class_map(ctx, memory_classes, id))
            .collect::<Result<_, _>>()?,
        idempotency_key: payload.idempotency_key,
        answered_at: payload.answered_at,
    })
}

fn format_memory_from_class_map(
    ctx: &McpToolCtx,
    memory_classes: &HashMap<uuid::Uuid, MemoryHandleClass>,
    memory_id: uuid::Uuid,
) -> Result<String, McpToolError> {
    let class = memory_classes.get(&memory_id).copied().ok_or_else(|| {
        McpToolError::Other(format!(
            "inquiry context memory class not found: {memory_id}"
        ))
    })?;
    Ok(ctx.format_memory_with_class(MemoryId::new(memory_id), class))
}

fn render_thread_policy(
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

fn render_thread_vote(ctx: &McpToolCtx, vote: LoadedApprovalVote) -> ThreadApprovalVote {
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

fn render_thread_decision(
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

fn render_thread_edge(
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

fn format_target(
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

fn format_endpoint(
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

fn format_memory_by_approval_kind(
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

fn format_memory_by_entity_kind(ctx: &McpToolCtx, kind: EntityKind, memory_id: MemoryId) -> String {
    match kind {
        EntityKind::Fact => ctx.format_fact_memory(memory_id),
        EntityKind::Abstraction => ctx.format_abstraction_memory(memory_id),
        EntityKind::Perspective => ctx.format_perspective_memory(memory_id),
        EntityKind::Goal => ctx.format_fact_memory(memory_id),
    }
}

async fn load_question(
    ctx: &McpToolCtx,
    question_memory_id: MemoryId,
) -> Result<DirectedQuestionV1, McpToolError> {
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
        "SELECT q.thread_key, q.question, q.target_personality_instance_id,
                q.target_self_perspective_memory_id, q.asked_by_self_perspective_memory_id,
                q.parent_question_memory_id, q.context_memory_ids, q.context_goal_ids,
                q.idempotency_key, q.asked_at
           FROM proxima_core.directed_question_v1 q
           JOIN proxima_core.memories m USING (memory_id)
          WHERE q.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(question_memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_sql)?;
    let Some((
        thread_key,
        question,
        target_personality_instance_id,
        target_self_perspective_memory_id,
        asked_by_self_perspective_memory_id,
        parent_question_memory_id,
        context_memory_ids,
        context_goal_ids,
        idempotency_key,
        asked_at,
    )) = row
    else {
        return Err(McpToolError::InvalidInput(
            "directed question is not visible".into(),
        ));
    };
    Ok(DirectedQuestionV1 {
        thread_key,
        question,
        target_personality_instance_id,
        target_self_perspective_memory_id,
        asked_by_self_perspective_memory_id,
        parent_question_memory_id,
        context_memory_ids,
        context_goal_ids,
        idempotency_key,
        asked_at,
    })
}

async fn resolve_context_memories(
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

async fn resolve_context_goals(
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

async fn validate_memory_ids_visible(
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

async fn load_memory_handle_classes(
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
            "one or more inquiry context memories were not found".into(),
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

async fn validate_goal_ids_visible(
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

async fn ingest_inquiry_fact<F: FactPayload + Serialize>(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    payload: &F,
) -> Result<crate::verbs::event_ingest::EventIngestOutcome, McpToolError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes)
        .map_err(|err| McpToolError::InvalidInput(format!("serialize payload: {err}")))?;
    let content_hash = blake3::hash(&payload_bytes);
    let now = OffsetDateTime::now_utc();
    let (object_schema, whole_schema) = match F::SCHEMA_ID {
        DirectedQuestionV1::SCHEMA_ID => (QUESTION_OBJECT_SCHEMA, QUESTION_WHOLE_SCHEMA),
        DirectedAnswerV1::SCHEMA_ID => (ANSWER_OBJECT_SCHEMA, ANSWER_WHOLE_SCHEMA),
        _ => return Err(McpToolError::Other("unsupported inquiry payload".into())),
    };
    let draft = EventDraft {
        source_id: SourceId::new(INQUIRY_SOURCE_ID),
        source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
        owner: ctx.owner.clone(),
        schema_id: SchemaId::new(F::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(F::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at: now,
        occurred_at: now,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(object_schema.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(whole_schema.into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    ingest_event_in_tx(tx, &draft).await
}

#[allow(clippy::too_many_lines)]
async fn ingest_event_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    draft: &EventDraft,
) -> Result<crate::verbs::event_ingest::EventIngestOutcome, McpToolError> {
    let event_id = draft.event_id();
    let event_id_bytes = event_id.into_inner();
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&draft.owner);
    let existing: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT memory_id FROM proxima_core.memories WHERE event_id = $1")
            .bind(&event_id_bytes[..])
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_sql)?;
    if let Some(memory_id) = existing {
        let seq: uuid::Uuid = sqlx::query_scalar(
            "SELECT seq FROM proxima_core.change_event
             WHERE entity_memory_id = $1 ORDER BY seq ASC LIMIT 1",
        )
        .bind(memory_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sql)?;
        return Ok(crate::verbs::event_ingest::EventIngestOutcome {
            event_id,
            memory_id: MemoryId::new(memory_id),
            change_event_seq: seq,
            idempotent_replay: true,
        });
    }

    let memory_id = uuid::Uuid::now_v7();
    let citation_mapping_id = uuid::Uuid::now_v7();
    let cited_object_id = uuid::Uuid::now_v7();
    let change_seq = uuid::Uuid::now_v7();
    let cited_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.cited_objects
            (cited_object_id, schema_id, owner_principal_kind,
             owner_principal_id, owner_org_id, content_hash)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (owner_principal_kind, owner_principal_id,
                      owner_org_id, schema_id, content_hash)
         DO UPDATE SET schema_id = EXCLUDED.schema_id
         RETURNING cited_object_id",
    )
    .bind(cited_object_id)
    .bind(draft.cited_object.schema_id.as_str())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(&draft.cited_object.content_hash[..])
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.source_batches
            (id, source_id, owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(draft.source_batch_id.into_inner())
    .bind(draft.source_id.as_str())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.events
            (event_id, source_id, source_batch_id,
             owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, observed_at, occurred_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(&event_id_bytes[..])
    .bind(draft.source_id.as_str())
    .bind(draft.source_batch_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(i32::MAX))
    .bind(draft.observed_at)
    .bind(draft.occurred_at)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id,
             owner_org_id, schema_id, schema_version, event_id, citation_mapping_id,
             personality_instance_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                 '00000000-0000-0000-0000-000000000000'::uuid)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(i32::MAX))
    .bind(&event_id_bytes[..])
    .bind(citation_mapping_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.citation_mappings
            (citation_mapping_id, schema_id, owner_principal_kind,
             owner_principal_id, owner_org_id, memory_id, cited_object_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(citation_mapping_id)
    .bind(draft.citation_mapping.schema_id.as_str())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(memory_id)
    .bind(cited_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, owner_org_id,
             kind, entity_memory_id, entity_kind, entity_schema_id,
             entity_schema_version)
         VALUES ($1, $2, $3, $4, 'EntityAppend', $5, 'Fact', $6, $7)",
    )
    .bind(change_seq)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(memory_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(i32::MAX))
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(crate::verbs::event_ingest::EventIngestOutcome {
        event_id,
        memory_id: MemoryId::new(memory_id),
        change_event_seq: change_seq,
        idempotent_replay: false,
    })
}

async fn insert_question_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &DirectedQuestionV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_core.directed_question_v1
            (memory_id, thread_key, question, target_personality_instance_id,
             target_self_perspective_memory_id, asked_by_self_perspective_memory_id,
             parent_question_memory_id, context_memory_ids, context_goal_ids,
             idempotency_key, asked_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(memory_id.into_inner())
    .bind(&payload.thread_key)
    .bind(&payload.question)
    .bind(payload.target_personality_instance_id)
    .bind(payload.target_self_perspective_memory_id)
    .bind(payload.asked_by_self_perspective_memory_id)
    .bind(payload.parent_question_memory_id)
    .bind(&payload.context_memory_ids)
    .bind(&payload.context_goal_ids)
    .bind(&payload.idempotency_key)
    .bind(payload.asked_at)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(())
}

async fn insert_answer_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &DirectedAnswerV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_core.directed_answer_v1
            (memory_id, question_memory_id, thread_key, answer,
             answered_by_personality_instance_id, answered_by_self_perspective_memory_id,
             context_memory_ids_used, idempotency_key, answered_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(memory_id.into_inner())
    .bind(payload.question_memory_id)
    .bind(&payload.thread_key)
    .bind(&payload.answer)
    .bind(payload.answered_by_personality_instance_id)
    .bind(payload.answered_by_self_perspective_memory_id)
    .bind(&payload.context_memory_ids_used)
    .bind(&payload.idempotency_key)
    .bind(payload.answered_at)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn append_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    relation_id: &str,
    source_kind: EntityKind,
    source_memory_id: Option<uuid::Uuid>,
    source_goal_id: Option<uuid::Uuid>,
    target_kind: EntityKind,
    target_memory_id: Option<uuid::Uuid>,
    target_goal_id: Option<uuid::Uuid>,
    authorship_kind: EdgeAuthorshipKind,
) -> Result<uuid::Uuid, McpToolError> {
    let relation = ctx
        .registry
        .resolve_relation(relation_id)
        .ok_or_else(|| McpToolError::Other(format!("relation {relation_id} not registered")))?;
    relation
        .descriptor
        .validate_edge_shape(
            source_kind.as_str(),
            target_kind.as_str(),
            authorship_kind.as_str(),
        )
        .map_err(McpToolError::LayeringViolation)?;
    let edge_id = uuid::Uuid::now_v7();
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind, authorship_owner_memory_id,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
         ON CONFLICT (edge_id) DO NOTHING",
    )
    .bind(edge_id)
    .bind(relation.descriptor.relation.as_str())
    .bind(relation.descriptor.class)
    .bind(source_kind)
    .bind(source_memory_id)
    .bind(source_goal_id)
    .bind(target_kind)
    .bind(target_memory_id)
    .bind(target_goal_id)
    .bind(authorship_kind)
    .bind(ctx.caller_self_perspective.map(MemoryId::into_inner))
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, owner_org_id, kind,
             edge_id, edge_relation,
             edge_source_kind, edge_source_memory_id, edge_source_goal_id,
             edge_target_kind, edge_target_memory_id, edge_target_goal_id)
         VALUES ($1,$2,$3,$4,'EdgeAppend',$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .bind(edge_id)
    .bind(relation.descriptor.relation.as_str())
    .bind(source_kind)
    .bind(source_memory_id)
    .bind(source_goal_id)
    .bind(target_kind)
    .bind(target_memory_id)
    .bind(target_goal_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(edge_id)
}

fn edge_authorship_for_ctx(ctx: &McpToolCtx) -> EdgeAuthorshipKind {
    if ctx.master_token_id.is_some() {
        EdgeAuthorshipKind::User
    } else {
        EdgeAuthorshipKind::ExternalAgent
    }
}

fn normalize_text(
    field: &str,
    value: &str,
    min: usize,
    max: usize,
) -> Result<String, McpToolError> {
    let trimmed = value.trim();
    if trimmed.len() < min || trimmed.len() > max {
        return Err(McpToolError::InvalidInput(format!(
            "{field} length must be between {min} and {max}"
        )));
    }
    Ok(trimmed.to_string())
}

fn map_sql(err: sqlx::Error) -> McpToolError {
    McpToolError::Storage(StorageError::Internal(err.to_string()))
}

fn owner_columns(owner: &Owner) -> (OwnerPrincipalKind, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(user) => (OwnerPrincipalKind::User, user.into_inner()),
        Principal::Group(group) => (OwnerPrincipalKind::Group, group.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

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
    async fn inquiry_context_memories_render_with_actual_memory_class() {
        let handles = Arc::new(HandleTable::new());
        let ctx = test_ctx(handles.clone());
        let question_id = MemoryId::new(uuid::Uuid::now_v7());
        let answer_id = MemoryId::new(uuid::Uuid::now_v7());
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
        let asked_at = OffsetDateTime::now_utc();
        let question = LoadedQuestion {
            memory_id: question_id,
            payload: DirectedQuestionV1 {
                thread_key: "thread".into(),
                question: "Question?".into(),
                target_personality_instance_id: uuid::Uuid::now_v7(),
                target_self_perspective_memory_id: uuid::Uuid::now_v7(),
                asked_by_self_perspective_memory_id: uuid::Uuid::now_v7(),
                parent_question_memory_id: None,
                context_memory_ids: vec![fact_id, abstraction_id, perspective_id],
                context_goal_ids: Vec::new(),
                idempotency_key: "q".into(),
                asked_at,
            },
        };
        let answer = LoadedAnswer {
            memory_id: answer_id,
            payload: DirectedAnswerV1 {
                question_memory_id: question_id.into_inner(),
                thread_key: "thread".into(),
                answer: "Answer.".into(),
                answered_by_personality_instance_id: uuid::Uuid::now_v7(),
                answered_by_self_perspective_memory_id: perspective_id,
                context_memory_ids_used: vec![abstraction_id, perspective_id],
                idempotency_key: "a".into(),
                answered_at: asked_at,
            },
        };

        let rendered_question =
            render_thread_question(&ctx, question, &memory_classes).expect("question");
        let rendered_answer = render_thread_answer(&ctx, answer, &memory_classes).expect("answer");

        assert_eq!(
            rendered_question.context_memories,
            vec!["F2".to_string(), "A1".to_string(), "P1".to_string()]
        );
        assert_eq!(
            rendered_answer.context_memories_used,
            vec!["A1".to_string(), "P1".to_string()]
        );
    }
}
