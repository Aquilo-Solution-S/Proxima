use std::collections::HashSet;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;

use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::personality::{
    PersonalityInstanceId, PersonalityStatus, WakeEntryRow, WakeEntryTriggerKind,
    writeable_schemas_for_palette,
};
use crate::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use crate::{
    CORE_ANSWERS_QUESTION_RELATION, CORE_RECEIVES_DIRECTED_QUESTION_RELATION, EdgeAuthorshipKind,
    EdgeId, Engine, EntityKind, FactPayload, MemoryId, Owner, OwnerPrincipalKind, Principal,
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
                caller_self_perspective: ctx.format_memory(caller_self),
                targets,
            })
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
                .map(|raw| ctx.resolve_memory(raw).map(MemoryId::into_inner))
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
                handle: ctx.format_memory(outcome.memory_id),
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
            let question_memory_id = ctx.resolve_memory(&args.question)?;
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
                handle: ctx.format_memory(outcome.memory_id),
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
            root_perspective: ctx.format_memory(row.current_root_perspective_memory_id),
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
