//! `core_recall` — cue-driven sketch packet. This is how Self is retrieved.

use std::collections::BTreeMap;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::SearchReadRequest;
use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::protocol::tool as protocol_tool;
use crate::verbs::goal_write::GoalState;
use crate::verbs::query::{
    EntityKind, MemorySearchRequest, QueryRequest, SearchMode, SearchOrder, SupersessionStatus,
    TagMatch,
};
use crate::{MemoryHandleClass, MemoryId};
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
        description = "Situation handles (`F:`/`A:`/`P:`), at most 16. Kernel cue set."
    )]
    pub subjects: Vec<String>,
    #[serde(default)]
    #[schemars(description = "Optional kind filter: Fact, Abstraction, Perspective, or Goal.")]
    pub kind: Option<RecallKind>,
    #[serde(default = "default_limit")]
    #[schemars(
        range(min = 1),
        description = "Hard cap. Default 16, max 32, 0 rejected."
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
}

#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct RecallSketch {
    pub handle: String,
    pub kind: String,
    pub sketch: String,
    pub reason: String,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RecallReason {
    Subject,
    CueTouch,
    Question,
    AssignedGoal,
}

impl RecallReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::CueTouch => "cue_touch",
            Self::Question => "question",
            Self::AssignedGoal => "assigned_goal",
        }
    }
}

impl McpTool for RecallTool {
    const NAME: &'static str = protocol_tool::CORE_RECALL;
    const DESCRIPTION: &'static str = "Cue-driven recall packet of sketches (kind + one-liner). Self is this query, not a parameterless dump of Perspective heads. Pass a question and/or subject handles. Does not hydrate sidecar bodies. Search stays a separate precision tool.";
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
    collect_subjects(&ctx, engine, &subjects, &mut packet).await?;
    collect_cue_touch(&ctx, engine, space.owner, &subjects, &mut packet).await?;
    if let Some(query) = question {
        collect_question(
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
    mark_perspective_heads(engine, &ctx, space.owner, &mut packet).await?;
    if includes_assigned_goals(args.kind) {
        collect_assigned_goals(&ctx, engine, space.owner, limit, &mut packet).await?;
    }

    Ok(RecallOutput {
        sketches: packet.finish(args.kind, limit),
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
    owner: crate::OwnerRef,
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
                goal_targets: false,
                kind: None,
                heads_only: true,
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
    owner: crate::OwnerRef,
    query: &str,
    kind: Option<RecallKind>,
    limit: u32,
    packet: &mut Packet,
) -> Result<(), McpToolError> {
    let query = crate::validate_search_query(query)?;
    let kind = match kind {
        Some(RecallKind::Fact) => Some(EntityKind::Fact),
        Some(RecallKind::Abstraction) => Some(EntityKind::Abstraction),
        Some(RecallKind::Perspective) => Some(EntityKind::Perspective),
        Some(RecallKind::Goal) | None => None,
    };
    let (mode, query_embedding, embedding_model_id) = if engine.embed_client().is_some() {
        match embed_query(engine, query).await {
            Ok((embedding, model_id)) => (SearchMode::Hybrid, Some(embedding), Some(model_id)),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "recall hybrid embedding unavailable; degrading to lexical"
                );
                (SearchMode::Lexical, None, None)
            }
        }
    } else {
        (SearchMode::Lexical, None, None)
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
                    query_embedding,
                    embedding_model_id,
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
    Ok(())
}

async fn mark_perspective_heads(
    engine: &crate::Engine,
    ctx: &McpToolCtx,
    owner: crate::OwnerRef,
    packet: &mut Packet,
) -> Result<(), McpToolError> {
    let candidates = packet.unverified_perspective_ids();
    if candidates.is_empty() {
        return Ok(());
    }
    let mut req = QueryRequest::for_owner(owner);
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

async fn collect_assigned_goals(
    ctx: &McpToolCtx,
    engine: &crate::Engine,
    owner: crate::OwnerRef,
    limit: u32,
    packet: &mut Packet,
) -> Result<(), McpToolError> {
    let perspectives = packet.head_perspective_ids(owner);
    for p in perspectives {
        let mut req = QueryRequest::for_owner(owner);
        req.entity_kind = Some(EntityKind::Goal);
        req.goal_state = Some(GoalState::Active);
        req.assignment = Some(p);
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
            );
        }
    }
    Ok(())
}

struct Entry {
    reason: RecallReason,
    sketch: RecallSketch,
    owner: Option<crate::OwnerRef>,
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
        ctx: &McpToolCtx,
        id: MemoryId,
        kind: EntityKind,
        sketch: &str,
        reason: RecallReason,
    ) {
        let key = id.into_inner();
        if self.by_id.contains_key(&key) {
            return;
        }
        let class = super::get_memory::memory_class(kind).unwrap_or(MemoryHandleClass::Fact);
        self.by_id.insert(
            key,
            Entry {
                reason,
                sketch: RecallSketch {
                    handle: ctx.format_memory_with_class(id, class),
                    kind: kind.as_str().to_string(),
                    sketch: truncate_sketch(sketch),
                    reason: reason.as_str().to_string(),
                },
                owner: None,
                is_head: false,
            },
        );
    }

    fn set_meta(&mut self, id: MemoryId, owner: Option<crate::OwnerRef>, is_head: bool) {
        if let Some(entry) = self.by_id.get_mut(&id.into_inner()) {
            entry.owner = owner;
            entry.is_head = is_head;
        }
    }

    fn insert_goal(&mut self, ctx: &McpToolCtx, id: crate::GoalId, title: &str) {
        let key = id.into_inner();
        if self.by_id.contains_key(&key) {
            return;
        }
        self.by_id.insert(
            key,
            Entry {
                reason: RecallReason::AssignedGoal,
                sketch: RecallSketch {
                    handle: ctx.format_goal(id),
                    kind: EntityKind::Goal.as_str().to_string(),
                    sketch: truncate_sketch(title),
                    reason: RecallReason::AssignedGoal.as_str().to_string(),
                },
                owner: None,
                is_head: true,
            },
        );
    }

    fn unverified_perspective_ids(&self) -> Vec<MemoryId> {
        self.by_id
            .iter()
            .filter(|(_, entry)| {
                entry.sketch.kind == EntityKind::Perspective.as_str() && !entry.is_head
            })
            .map(|(id, _)| MemoryId::new(*id))
            .collect()
    }

    fn mark_heads(&mut self, heads: &std::collections::HashSet<Uuid>) {
        for (id, entry) in &mut self.by_id {
            if entry.sketch.kind == EntityKind::Perspective.as_str() && heads.contains(id) {
                entry.is_head = true;
            }
        }
    }

    fn head_perspective_ids(&self, owner: crate::OwnerRef) -> Vec<MemoryId> {
        self.by_id
            .iter()
            .filter(|(_, entry)| {
                entry.sketch.kind == EntityKind::Perspective.as_str()
                    && entry.is_head
                    && entry.owner == Some(owner)
            })
            .map(|(id, _)| MemoryId::new(*id))
            .collect()
    }

    fn finish(self, filter: Option<RecallKind>, limit: u32) -> Vec<RecallSketch> {
        let mut rows: Vec<Entry> = self.by_id.into_values().collect();
        if let Some(filter) = filter {
            rows.retain(|entry| kind_matches(filter, &entry.sketch.kind, entry.is_head));
        }
        rows.sort_by(|a, b| {
            a.reason
                .cmp(&b.reason)
                .then_with(|| a.sketch.handle.cmp(&b.sketch.handle))
        });
        rows.into_iter()
            .take(limit as usize)
            .map(|entry| entry.sketch)
            .collect()
    }
}

fn kind_matches(filter: RecallKind, kind: &str, is_head: bool) -> bool {
    match filter {
        RecallKind::Fact => kind == EntityKind::Fact.as_str(),
        RecallKind::Abstraction => kind == EntityKind::Abstraction.as_str(),
        RecallKind::Perspective => {
            (kind == EntityKind::Perspective.as_str() && is_head)
                || kind == EntityKind::Goal.as_str()
        }
        RecallKind::Goal => kind == EntityKind::Goal.as_str(),
    }
}

fn includes_assigned_goals(kind: Option<RecallKind>) -> bool {
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

async fn embed_query(engine: &crate::Engine, query: &str) -> Result<(Vec<f32>, String), String> {
    let embed = engine
        .embed_client()
        .ok_or_else(|| "no embedding client".to_string())?;
    let embedding = embed.embed(query).await.map_err(|err| {
        tracing::warn!(error = %err, "embedding provider failed");
        "embedding provider error".to_string()
    })?;
    Ok((embedding, embed.model_id().to_string()))
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
        assert!(require_cue(None, &["P:x".into()]).is_ok());
    }

    #[test]
    fn kind_strings_match_entity_kind() {
        assert!(kind_matches(RecallKind::Perspective, "Perspective", true));
        assert!(!kind_matches(RecallKind::Perspective, "Perspective", false));
        assert!(kind_matches(RecallKind::Perspective, "Goal", true));
        assert!(!kind_matches(RecallKind::Perspective, "Fact", true));
        assert!(kind_matches(RecallKind::Goal, "Goal", true));
        assert!(!kind_matches(RecallKind::Fact, "fact", true));
    }

    #[test]
    fn payload_sketch_skips_empty_earlier_keys() {
        let value = serde_json::json!({
            "title": "   ",
            "claim": "the stance",
            "body": "long body"
        });
        let first = ["title", "claim", "body", "text"]
            .iter()
            .find_map(|key| {
                value
                    .get(*key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
            })
            .map(ToOwned::to_owned);
        assert_eq!(first.as_deref(), Some("the stance"));
    }
}
