//! `core_recall` — cue-driven sketch packet. This is how Self is retrieved.

use crate::PinTargetKind;
use crate::mcp::ToolEffect;
use std::collections::BTreeMap;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::MemoryId;
use crate::engine::SearchReadRequest;
use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::protocol::tool as protocol_tool;
use crate::verbs::goal_write::GoalState;
use crate::verbs::query::{
    EntityKind, MemorySearchRequest, QueryRequest, SearchMode, SearchOrder, SupersessionStatus,
    TagMatch,
};
use uuid::Uuid;

const DEFAULT_LIMIT: u32 = 16;
const MAX_LIMIT: u32 = 32;
const MAX_SUBJECTS: usize = 16;
const SKETCH_CHARS: usize = 160;

#[derive(Debug, Default)]
pub struct RecallTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecallArgs {
    #[serde(default)]
    #[schemars(
        length(max = 512),
        description = "Question / situation text. Combined with subjects as the cue. At least one of question or subjects is required."
    )]
    pub question: Option<String>,
    #[serde(default)]
    #[schemars(
        length(max = 16),
        description = "Memories the situation is about: memory UUIDs."
    )]
    pub subjects: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Perspective also keeps the Active Goals assigned to the returned Perspectives and the selected owner's unassigned Active Goals."
    )]
    pub kind: Option<RecallKind>,
    #[serde(default = "default_limit")]
    #[schemars(
        range(min = 1),
        description = "Max sketches; values above 32 are clamped."
    )]
    pub limit: u32,
    #[serde(default)]
    #[schemars(description = "Memory space key from core_memory_spaces. Omit for current owner.")]
    pub space: Option<String>,
}

fn default_limit() -> u32 {
    DEFAULT_LIMIT
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
pub enum RecallKind {
    #[serde(rename = "Fact", alias = "fact")]
    Fact,
    #[serde(rename = "Abstraction", alias = "abstraction")]
    Abstraction,
    #[serde(rename = "Perspective", alias = "perspective")]
    Perspective,
    #[serde(rename = "Goal", alias = "goal")]
    Goal,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RecallOutput {
    pub sketches: Vec<RecallSketch>,
    /// The question leg ran lexical-only because the embedding client was
    /// absent or its call failed.
    ///
    /// Declared rather than logged. A silent Hybrid→Lexical degrade reads to
    /// the caller as "nothing is remembered about X" during what is actually a
    /// provider outage; `core/search_memories` already reports this case on
    /// the wire, and the two surfaces have to agree that it is declarable.
    pub degraded_to_lexical: bool,
}

#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct RecallSketch {
    pub handle: String,
    pub kind: EntityKind,
    pub sketch: String,
    pub reason: RecallReason,
}

/// Why a sketch is in the packet, in packet order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecallReason {
    Subject,
    CueTouch,
    Question,
    AssignedGoal,
    UnassignedGoal,
}

impl McpTool for RecallTool {
    const NAME: &'static str = protocol_tool::CORE_RECALL;
    const DESCRIPTION: &'static str = "Recall what is known about a question and/or subject handles: short sketches of the subjects, Perspectives about them, question matches, Active Goals assigned to recalled Perspectives, and the selected owner's unassigned Active Goals. No bodies; use core_search_memories for filtered search.";
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::ReadOnly);
    type Args = RecallArgs;
    type Output = RecallOutput;

    fn call(
        ctx: McpToolCtx,
        args: RecallArgs,
    ) -> BoxFuture<'static, Result<RecallOutput, McpToolError>> {
        Box::pin(async move { recall(ctx, args).await })
    }
}

async fn recall(ctx: McpToolCtx, args: RecallArgs) -> Result<RecallOutput, McpToolError> {
    crate::reject_zero_limit(Some(args.limit))?;
    require_cue(args.question.as_deref(), &args.subjects)?;
    if args.subjects.len() > MAX_SUBJECTS {
        return Err(McpToolError::InvalidInput(format!(
            "at most {MAX_SUBJECTS} subjects; got {}",
            args.subjects.len()
        )));
    }
    let question = args
        .question
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());
    let limit = args.limit.min(MAX_LIMIT);
    let space = super::memory_spaces::resolve_space_owner(
        &ctx,
        args.space.as_deref(),
        super::memory_spaces::SpaceDefault::Current,
    )?;
    let engine = ctx.require_engine()?;
    let mut subjects = Vec::new();
    for raw in &args.subjects {
        subjects.push(ctx.resolve_memory(raw)?);
    }

    let mut packet = Packet::new();
    let mut degraded_to_lexical = false;
    collect_subjects(&ctx, engine, &subjects, &mut packet).await?;
    collect_cue_touch(&ctx, engine, space.owner, &subjects, &mut packet).await?;
    if let Some(query) = question {
        degraded_to_lexical = collect_question(
            &ctx,
            engine,
            space.owner,
            query,
            args.kind,
            limit,
            &mut packet,
        )
        .await?;
    }
    mark_perspective_heads(engine, &ctx, &mut packet).await?;
    if includes_goals(args.kind) {
        collect_assigned_goals(&ctx, engine, space.owner, limit, &mut packet).await?;
        collect_unassigned_goals(&ctx, engine, space.owner, limit, &mut packet).await?;
    }

    Ok(RecallOutput {
        sketches: packet.finish(args.kind, limit),
        degraded_to_lexical,
    })
}

async fn collect_subjects(
    ctx: &McpToolCtx,
    engine: &crate::Engine,
    subjects: &[MemoryId],
    packet: &mut Packet,
) -> Result<(), McpToolError> {
    if subjects.is_empty() {
        return Ok(());
    }
    let sketches = engine.load_sketches(&ctx.authz, subjects).await?;
    for sketch in sketches {
        packet.insert(
            ctx,
            sketch.id,
            sketch.kind,
            &sketch.text,
            RecallReason::Subject,
        );
        packet.set_meta(
            sketch.id,
            Some(sketch.owner),
            sketch.kind != EntityKind::Perspective,
        );
    }
    Ok(())
}

async fn collect_cue_touch(
    ctx: &McpToolCtx,
    engine: &crate::Engine,
    owner: crate::OwnerId,
    subjects: &[MemoryId],
    packet: &mut Packet,
) -> Result<(), McpToolError> {
    if subjects.is_empty() {
        return Ok(());
    }
    let inbound = engine
        .inbound_pin_nodes(
            &ctx.authz,
            crate::InboundPinQuery {
                targets: subjects,
                target_kind: PinTargetKind::Memory,
                kind: None,
                supersession: SupersessionStatus::HeadsOnly,
                after: None,
                limit: MAX_LIMIT,
            },
        )
        .await?;
    let touch_ids: Vec<MemoryId> = inbound
        .into_iter()
        .filter(|node| node.kind == EntityKind::Perspective && !packet.contains(node.id))
        .map(|node| node.id)
        .collect();
    if touch_ids.is_empty() {
        return Ok(());
    }
    let sketches = engine.load_sketches(&ctx.authz, &touch_ids).await?;
    for sketch in sketches {
        if sketch.owner != owner {
            continue;
        }
        packet.insert(
            ctx,
            sketch.id,
            sketch.kind,
            &sketch.text,
            RecallReason::CueTouch,
        );
        packet.set_meta(sketch.id, Some(sketch.owner), true);
    }
    Ok(())
}

async fn collect_question(
    ctx: &McpToolCtx,
    engine: &crate::Engine,
    owner: crate::OwnerId,
    query: &str,
    kind: Option<RecallKind>,
    limit: u32,
    packet: &mut Packet,
) -> Result<bool, McpToolError> {
    let query = crate::validate_search_query(query)?;
    let kind = match kind {
        Some(RecallKind::Fact) => Some(EntityKind::Fact),
        Some(RecallKind::Abstraction) => Some(EntityKind::Abstraction),
        Some(RecallKind::Perspective) => Some(EntityKind::Perspective),
        Some(RecallKind::Goal) | None => None,
    };
    // Recall always intends Hybrid, so every way of not getting it — no
    // route for the owner, or a client whose call failed — is the same
    // fact to the caller: the semantic leg did not run. `degraded` carries it
    // to the wire instead of leaving it in a log line the caller cannot see.
    let mut degraded = false;
    let route = engine.search_route(&owner).await;
    let (mode, semantic) = if let Some(embed) = route.current_client() {
        match embed_query(embed, query).await {
            Ok(semantic) => (SearchMode::Hybrid, Some(semantic)),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "recall hybrid embedding unavailable; degrading to lexical"
                );
                degraded = true;
                (SearchMode::Lexical, None)
            }
        }
    } else {
        degraded = true;
        (SearchMode::Lexical, None)
    };
    let page = engine
        .search(
            &ctx.authz,
            &SearchReadRequest {
                search: MemorySearchRequest {
                    owner,
                    read_owners: Vec::new(),
                    query: query.to_string(),
                    mode,
                    supersession: SupersessionStatus::HeadsOnly,
                    limit,
                    kind,
                    schema_id: None,
                    tags: Vec::new(),
                    tag_match: TagMatch::Any,
                    since: None,
                    until: None,
                    order: SearchOrder::Relevance,
                    min_score: None,
                    semantic_weight: None,
                    after: None,
                    semantic,
                },
                include_body: false,
                include_neighbor_edges: false,
            },
        )
        .await?;
    let ids: Vec<MemoryId> = page.memories.iter().map(|row| row.memory_id).collect();
    let sketches = engine.load_sketches(&ctx.authz, &ids).await?;
    for sketch in sketches {
        packet.insert(
            ctx,
            sketch.id,
            sketch.kind,
            &sketch.text,
            RecallReason::Question,
        );
        packet.set_meta(sketch.id, Some(owner), true);
    }
    Ok(degraded)
}

async fn mark_perspective_heads(
    engine: &crate::Engine,
    ctx: &McpToolCtx,
    packet: &mut Packet,
) -> Result<(), McpToolError> {
    let candidates = packet.unverified_perspective_ids();
    if candidates.is_empty() {
        return Ok(());
    }
    let mut req = QueryRequest::readable();
    req.entity_kind = Some(EntityKind::Perspective);
    req.memory_ids.clone_from(&candidates);
    req.include_payloads = false;
    req.limit = u32::try_from(candidates.len()).unwrap_or(u32::MAX).max(1);
    let page = engine.query(&ctx.authz, &req).await?;
    let heads: std::collections::HashSet<Uuid> = page
        .memories
        .into_iter()
        .map(|row| row.id.into_inner())
        .collect();
    packet.mark_heads(&heads);
    Ok(())
}

/// The Active Goals assigned to the packet's head Perspectives of `owner`, in
/// one query and one sketch load whatever their number.
async fn collect_assigned_goals(
    ctx: &McpToolCtx,
    engine: &crate::Engine,
    owner: crate::OwnerId,
    limit: u32,
    packet: &mut Packet,
) -> Result<(), McpToolError> {
    let perspectives = packet.head_perspective_ids(owner);
    if perspectives.is_empty() {
        return Ok(());
    }
    let mut req = QueryRequest::readable();
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_states.insert(GoalState::Active);
    req.assignments = perspectives;
    req.include_payloads = false;
    req.limit = limit;
    let page = engine.query(&ctx.authz, &req).await?;
    let goal_ids: Vec<MemoryId> = page
        .goals
        .iter()
        .map(|goal| MemoryId::new(goal.id.into_inner()))
        .collect();
    let sketches = engine.load_sketches(&ctx.authz, &goal_ids).await?;
    for sketch in sketches {
        packet.insert_goal(
            ctx,
            crate::GoalId::new(sketch.id.into_inner()),
            &sketch.text,
            RecallReason::AssignedGoal,
        );
    }
    Ok(())
}

async fn collect_unassigned_goals(
    ctx: &McpToolCtx,
    engine: &crate::Engine,
    owner: crate::OwnerId,
    limit: u32,
    packet: &mut Packet,
) -> Result<(), McpToolError> {
    let authz = ctx
        .authz
        .clone()
        .narrowed_to_owner(owner)
        .ok_or_else(|| McpToolError::NotAuthorized("memory space read".into()))?;
    let mut req = QueryRequest::readable();
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_states.insert(GoalState::Active);
    req.include_payloads = false;
    req.limit = limit;
    let mut collected = 0;
    loop {
        let page = engine.query(&authz, &req).await?;
        let goal_ids: Vec<MemoryId> = page
            .goals
            .into_iter()
            .filter(|goal| goal.assignment.is_none())
            .map(|goal| MemoryId::new(goal.id.into_inner()))
            .filter(|id| !packet.contains(*id))
            .collect();
        let sketches = engine.load_sketches(&authz, &goal_ids).await?;
        collected += sketches.len();
        for sketch in sketches {
            packet.insert_goal(
                ctx,
                crate::GoalId::new(sketch.id.into_inner()),
                &sketch.text,
                RecallReason::UnassignedGoal,
            );
        }
        if collected >= limit as usize {
            break;
        }
        let Some(after) = page.next_cursor else {
            break;
        };
        req.page.after = Some(after);
    }
    Ok(())
}

struct Entry {
    handle: Uuid,
    kind: EntityKind,
    reason: RecallReason,
    sketch: String,
    owner: Option<crate::OwnerId>,
    is_head: bool,
}

struct Packet {
    by_id: BTreeMap<Uuid, Entry>,
}

impl Packet {
    fn new() -> Self {
        Self {
            by_id: BTreeMap::new(),
        }
    }

    fn contains(&self, id: MemoryId) -> bool {
        self.by_id.contains_key(&id.into_inner())
    }

    fn insert(
        &mut self,
        _ctx: &McpToolCtx,
        id: MemoryId,
        kind: EntityKind,
        sketch: &str,
        reason: RecallReason,
    ) {
        let key = id.into_inner();
        if self.by_id.contains_key(&key) {
            return;
        }
        self.by_id.insert(
            key,
            Entry {
                handle: key,
                kind,
                reason,
                sketch: truncate_sketch(sketch),
                owner: None,
                is_head: false,
            },
        );
    }

    fn set_meta(&mut self, id: MemoryId, owner: Option<crate::OwnerId>, is_head: bool) {
        if let Some(entry) = self.by_id.get_mut(&id.into_inner()) {
            entry.owner = owner;
            entry.is_head = is_head;
        }
    }

    fn insert_goal(
        &mut self,
        _ctx: &McpToolCtx,
        id: crate::GoalId,
        title: &str,
        reason: RecallReason,
    ) {
        let key = id.into_inner();
        if self.by_id.contains_key(&key) {
            return;
        }
        self.by_id.insert(
            key,
            Entry {
                handle: key,
                kind: EntityKind::Goal,
                reason,
                sketch: truncate_sketch(title),
                owner: None,
                is_head: true,
            },
        );
    }

    fn unverified_perspective_ids(&self) -> Vec<MemoryId> {
        self.by_id
            .iter()
            .filter(|(_, entry)| entry.kind == EntityKind::Perspective && !entry.is_head)
            .map(|(id, _)| MemoryId::new(*id))
            .collect()
    }

    fn mark_heads(&mut self, heads: &std::collections::HashSet<Uuid>) {
        for (id, entry) in &mut self.by_id {
            if entry.kind == EntityKind::Perspective && heads.contains(id) {
                entry.is_head = true;
            }
        }
    }

    fn head_perspective_ids(&self, owner: crate::OwnerId) -> Vec<MemoryId> {
        self.by_id
            .iter()
            .filter(|(_, entry)| {
                entry.kind == EntityKind::Perspective && entry.is_head && entry.owner == Some(owner)
            })
            .map(|(id, _)| MemoryId::new(*id))
            .collect()
    }

    fn finish(self, filter: Option<RecallKind>, limit: u32) -> Vec<RecallSketch> {
        let mut rows: Vec<Entry> = self.by_id.into_values().collect();
        if let Some(filter) = filter {
            rows.retain(|entry| kind_matches(filter, entry.kind, entry.is_head));
        }
        rows.sort_by(|a, b| {
            a.reason
                .cmp(&b.reason)
                .then_with(|| a.handle.cmp(&b.handle))
        });
        rows.into_iter()
            .take(limit as usize)
            .map(|entry| RecallSketch {
                handle: entry.handle.to_string(),
                kind: entry.kind,
                sketch: entry.sketch,
                reason: entry.reason,
            })
            .collect()
    }
}

fn kind_matches(filter: RecallKind, kind: EntityKind, is_head: bool) -> bool {
    match filter {
        RecallKind::Fact => kind == EntityKind::Fact,
        RecallKind::Abstraction => kind == EntityKind::Abstraction,
        RecallKind::Perspective => {
            (kind == EntityKind::Perspective && is_head) || kind == EntityKind::Goal
        }
        RecallKind::Goal => kind == EntityKind::Goal,
    }
}

fn includes_goals(kind: Option<RecallKind>) -> bool {
    matches!(
        kind,
        None | Some(RecallKind::Perspective | RecallKind::Goal)
    )
}

fn truncate_sketch(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= SKETCH_CHARS {
        return trimmed.to_string();
    }
    trimmed.chars().take(SKETCH_CHARS).collect()
}

fn require_cue(question: Option<&str>, subjects: &[String]) -> Result<(), McpToolError> {
    let question = question.map(str::trim).filter(|q| !q.is_empty());
    if question.is_none() && subjects.is_empty() {
        return Err(McpToolError::InvalidInput(
            "recall requires a question and/or subjects; Self is not parameterless".into(),
        ));
    }
    Ok(())
}

async fn embed_query(
    embed: &crate::llm::BoundEmbeddingClient,
    query: &str,
) -> Result<crate::SpaceVector, String> {
    embed
        .embed_query(query, crate::llm::QueryTask::DEFAULT)
        .await
        .map_err(|err| {
            tracing::warn!(error = %err, "embedding provider failed");
            "embedding provider error".to_string()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cue_is_rejected() {
        let err = require_cue(None, &[]).expect_err("empty");
        assert!(err.to_string().contains("not parameterless"));
    }

    #[test]
    fn question_or_subject_is_enough() {
        assert!(require_cue(Some("what do I believe"), &[]).is_ok());
        assert!(require_cue(None, &["00000000-0000-8000-8000-000000000001".into()]).is_ok());
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn a_sketch_keeps_the_words_clients_read() {
        let sketch = RecallSketch {
            handle: "h".into(),
            kind: EntityKind::Perspective,
            sketch: "s".into(),
            reason: RecallReason::CueTouch,
        };
        assert_eq!(
            serde_json::to_value(&sketch).expect("serializes"),
            serde_json::json!({
                "handle": "h", "kind": "Perspective", "sketch": "s", "reason": "cue_touch"
            })
        );
        for (reason, word) in [
            (RecallReason::Subject, "subject"),
            (RecallReason::Question, "question"),
            (RecallReason::AssignedGoal, "assigned_goal"),
            (RecallReason::UnassignedGoal, "unassigned_goal"),
        ] {
            assert_eq!(serde_json::to_value(reason).expect("serializes"), word);
        }
        let schema =
            serde_json::to_string(&schemars::schema_for!(RecallOutput)).expect("schema serializes");
        assert!(
            schema.contains("cue_touch") && schema.contains("Perspective"),
            "{schema}"
        );
    }
}
