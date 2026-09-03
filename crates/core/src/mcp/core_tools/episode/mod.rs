//! `core_episode_commit` — one transaction, explicit `bind[]`.

mod bind;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::goal::{
    GoalPayloadArgs, GoalWakeArgs, encode_goal_payload, encode_wake_config,
    system_operator_authorship,
};
use super::memory::derive::{
    self, DerivedKind, MAX_SOURCE_HANDLES, core_derive_input_contract_id, core_derive_operator_id,
    map_derive_authoring_error,
};
use super::memory::interpret::{
    self, MAX_CLAIM_CHARS, MAX_SUBJECTS, default_confidence, interpret_input_contract_id,
    interpret_operator_id, interpretation_memory_id, reject_self_subject,
};
use super::memory::util::{dedup_resolved, normalize_idempotency_key, normalize_tags};
use crate::engine::{GoalCreatePayloadWriteRequest, TypedFactIngest};
use crate::error::ErrorCode;
use crate::mcp::{McpTool, McpToolCtx, McpToolError, MemoryHandleClass};
use crate::memory::payloads::AgentNoteV1;
use crate::memory::payloads::write_act::WriteActV1;
use crate::protocol::tool as protocol_tool;
use crate::tool::validate_trimmed_len;
use crate::verbs::goal_write::{
    GoalAssignmentTarget, GoalEvidenceRef, GoalTopologyWrite, IdempotencyKey,
};
use crate::{
    AbstractionPayload, AgentDerivationV1, AuthorDerivedRequestInput, EntityKind,
    InterpretationSubjectKind, InterpretationV1, MemoryId, PerspectivePayload, SchemaId,
    SchemaVersion, SidecarPayload, UnitOfWork,
};

use bind::{BindSet, parse_bind, reject_duplicate_keys};

const MAX_REMEMBER: usize = 16;
const MAX_STANCE: usize = 16;
const MAX_GOAL: usize = 16;
const MAX_BIND: usize = 32;
const NOTE_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x3d, 0x9a, 0x4e, 0x11, 0x8c, 0x20, 0x4f, 0x6a, 0x9b, 0x77, 0x12, 0x44, 0x88, 0x01, 0xcc, 0x55,
]);

#[derive(Debug, Default)]
pub struct EpisodeCommitTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EpisodeRememberItem {
    #[schemars(length(max = 240), description = "Fact title, 1 to 240 chars.")]
    pub title: String,
    #[schemars(length(max = 20000), description = "Fact body, 1 to 20000 chars.")]
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EpisodeDeriveItem {
    #[schemars(length(max = 240), description = "Abstraction title, 1 to 240 chars.")]
    pub title: String,
    #[schemars(
        length(max = 20000),
        description = "Abstraction body, 1 to 20000 chars."
    )]
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[schemars(
        description = "Source handles for the F→A / A→A proof. Intra-episode keys: `remember:N`."
    )]
    pub source_handles: Vec<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EpisodeStanceItem {
    #[schemars(
        length(max = 1000),
        description = "Claim about the subjects, 1 to 1000 chars."
    )]
    pub claim: String,
    #[serde(default = "default_confidence")]
    #[schemars(range(max = 100), description = "Confidence 0..=100. Defaults to 80.")]
    pub confidence: u8,
    #[schemars(
        description = "Subjects of the claim (`F:`/`A:`/`P:` or intra-episode `remember:N` / `derive` / `stance:N`)."
    )]
    pub subjects: Vec<String>,
    #[serde(default)]
    pub model_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EpisodeGoalItem {
    #[serde(flatten)]
    pub payload: GoalPayloadArgs,
    #[schemars(
        length(min = 1),
        description = "Abstraction evidence (`A:` or intra-episode `derive`). Operator-authored Goals require Abstraction evidence only."
    )]
    pub evidence: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Assignment Perspective (`P:` or intra-episode `stance:N`). Omit to use caller Perspective context."
    )]
    pub target_perspective: Option<String>,
    #[serde(default)]
    pub wake: Option<GoalWakeArgs>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EpisodeCommitArgs {
    #[serde(default)]
    #[schemars(length(max = 16), description = "Facts authored in this episode.")]
    pub remember: Vec<EpisodeRememberItem>,
    #[serde(default)]
    #[schemars(description = "Optional Abstraction derived over this episode's sources.")]
    pub derive: Option<EpisodeDeriveItem>,
    #[serde(default)]
    #[schemars(
        length(max = 16),
        description = "Interpretation Perspectives (`claim` + `subjects`)."
    )]
    pub stance: Vec<EpisodeStanceItem>,
    #[serde(default)]
    #[schemars(
        length(max = 16),
        description = "Active Goals assigned in this episode."
    )]
    pub goal: Vec<EpisodeGoalItem>,
    #[serde(default)]
    #[schemars(
        length(max = 32),
        description = "Local keys to pin to the write-act (`remember:N`, `derive`, `stance:N`, `goal:N`). Only listed produced nodes pin this write-act."
    )]
    pub bind: Vec<String>,
    #[serde(default)]
    pub space: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EpisodeCommitOutput {
    pub write_act: String,
    pub remembered: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived: Option<String>,
    pub stances: Vec<String>,
    pub goals: Vec<String>,
    pub bound: Vec<String>,
}

struct Slot {
    id: MemoryId,
    handle: String,
    class: MemoryHandleClass,
}

#[derive(Default)]
struct EpisodeSlots {
    remember: Vec<Slot>,
    derive: Option<Slot>,
    stance: Vec<Slot>,
}

/// The one context every `episode_commit` write phase needs: the open
/// transaction, the handles minted so far, and the ordered ledger of
/// handles that pinned the write-act.
struct EpisodeWrite<'a> {
    ctx: &'a McpToolCtx,
    uow: UnitOfWork<'a>,
    bind: &'a BindSet,
    owner: crate::Owner,
    act_id: MemoryId,
    slots: EpisodeSlots,
    bound: Vec<String>,
}

impl McpTool for EpisodeCommitTool {
    const NAME: &'static str = protocol_tool::CORE_EPISODE_COMMIT;
    const DESCRIPTION: &'static str = "Commit one episode in a single transaction: remember Facts, optional derive, stance[], goal[], mint a write-act Fact, and pin only bind[] members to that act (`remember:N`, `derive`, `stance:N`, `goal:N`). Not a connect verb.";
    type Args = EpisodeCommitArgs;
    type Output = EpisodeCommitOutput;

    fn call(
        ctx: McpToolCtx,
        args: EpisodeCommitArgs,
    ) -> BoxFuture<'static, Result<EpisodeCommitOutput, McpToolError>> {
        Box::pin(async move { episode_commit(ctx, args).await })
    }
}

async fn episode_commit(
    ctx: McpToolCtx,
    args: EpisodeCommitArgs,
) -> Result<EpisodeCommitOutput, McpToolError> {
    validate_episode_shape(&args)?;
    let bind = parse_bind(
        &args.bind,
        args.remember.len(),
        args.derive.is_some(),
        args.stance.len(),
        args.goal.len(),
    )?;
    let (space, authz) =
        super::memory_spaces::resolve_space_for_write(&ctx, args.space.as_deref())?;
    let engine = ctx.require_engine()?;
    let mut uow = engine.unit_of_work(&authz).await?;
    let write_act = uow
        .ingest_fact(
            "core/episode-commit",
            &WriteActV1 {
                episode_id: uuid::Uuid::now_v7(),
            },
        )
        .await?;
    let act_id = write_act.memory_id;
    let mut write = EpisodeWrite {
        ctx: &ctx,
        uow,
        bind: &bind,
        owner: space.owner,
        act_id,
        slots: EpisodeSlots::default(),
        bound: Vec::new(),
    };
    write.write_remembered(&args.remember).await?;
    let derived = match args.derive.as_ref() {
        Some(item) => Some(write.write_derive(item).await?),
        None => None,
    };
    write.write_stances(&args.stance).await?;
    let goals = write.write_goals(&args.goal).await?;
    let (slots, bound) = write.commit().await?;
    Ok(EpisodeCommitOutput {
        write_act: ctx.format_memory_with_class(write_act.memory_id, MemoryHandleClass::Fact),
        remembered: slots.remember.into_iter().map(|slot| slot.handle).collect(),
        derived,
        stances: slots.stance.into_iter().map(|slot| slot.handle).collect(),
        goals,
        bound,
    })
}

fn validate_episode_shape(args: &EpisodeCommitArgs) -> Result<(), McpToolError> {
    if args.remember.is_empty()
        && args.derive.is_none()
        && args.stance.is_empty()
        && args.goal.is_empty()
    {
        return Err(McpToolError::InvalidInput(
            "episode_commit requires at least one remember, derive, stance, or goal item".into(),
        ));
    }
    if args.remember.len() > MAX_REMEMBER {
        return Err(McpToolError::InvalidInput(format!(
            "at most {MAX_REMEMBER} remember items"
        )));
    }
    if args.stance.len() > MAX_STANCE {
        return Err(McpToolError::InvalidInput(format!(
            "at most {MAX_STANCE} stance items"
        )));
    }
    if args.goal.len() > MAX_GOAL {
        return Err(McpToolError::InvalidInput(format!(
            "at most {MAX_GOAL} goal items"
        )));
    }
    if args.bind.len() > MAX_BIND {
        return Err(McpToolError::InvalidInput(format!(
            "at most {MAX_BIND} bind keys"
        )));
    }
    reject_duplicate_keys(
        args.remember
            .iter()
            .map(|item| item.idempotency_key.clone()),
        "remember",
    )?;
    reject_duplicate_keys(
        args.goal.iter().map(|item| item.idempotency_key.clone()),
        "goal",
    )?;
    Ok(())
}

impl EpisodeWrite<'_> {
    async fn write_remembered(
        &mut self,
        items: &[EpisodeRememberItem],
    ) -> Result<(), McpToolError> {
        let ctx = self.ctx;
        let bind = self.bind;
        let act_id = self.act_id;
        for (idx, item) in items.iter().enumerate() {
            let title = validate_trimmed_len("title", &item.title, 240)?;
            let body = validate_trimmed_len("body", &item.body, 20_000)?;
            let idempotency_key = normalize_idempotency_key(item.idempotency_key.clone())?;
            let tags = normalize_tags(item.tags.clone())?;
            let note_id = idempotency_key
                .as_deref()
                .map_or_else(uuid::Uuid::now_v7, |key| {
                    uuid::Uuid::new_v5(&NOTE_NAMESPACE, key.as_bytes())
                });
            let payload = AgentNoteV1 {
                note_id,
                title: title.to_string(),
                body: body.to_string(),
                tags,
                idempotency_key,
            };
            let pin = bind.remember.contains(&idx);
            // A remember item takes no `language`, so this write keeps the
            // builder's default: an explicit request for the deployment
            // configuration, which is what an omitted `language` resolves to
            // on the standalone `core_remember` too.
            let mut spec = TypedFactIngest::new("core/episode-remember", &payload);
            if pin {
                spec = spec.refs([act_id.into_inner()]);
            }
            let outcome = self
                .uow
                .ingest_typed(spec)
                .await
                .map_err(|err| map_bound_fact_error(err, "remember", pin))?;
            reject_bound_replay(pin, outcome.idempotent_replay, "remember")?;
            let handle = ctx.format_memory_with_class(outcome.memory_id, MemoryHandleClass::Fact);
            if pin {
                self.bound.push(handle.clone());
            }
            self.slots.remember.push(Slot {
                id: outcome.memory_id,
                handle,
                class: MemoryHandleClass::Fact,
            });
        }
        Ok(())
    }

    async fn write_derive(&mut self, item: &EpisodeDeriveItem) -> Result<String, McpToolError> {
        let ctx = self.ctx;
        let owner = self.owner;
        let act_id = self.act_id;
        let pin = self.bind.derive;
        let authored = derive::authored_derivation(
            ctx,
            derive::DerivationFields {
                title: &item.title,
                body: &item.body,
                tags: &item.tags,
                model_id: item.model_id.as_deref(),
                idempotency_key: item.idempotency_key.as_deref(),
            },
        )?;
        if item.source_handles.len() > MAX_SOURCE_HANDLES {
            return Err(McpToolError::InvalidInput(format!(
                "source_handles must contain at most {MAX_SOURCE_HANDLES} handles"
            )));
        }
        let sources = dedup_resolved(
            &item.source_handles,
            MAX_SOURCE_HANDLES,
            "source_handles",
            "source_handles must be nonempty for operator derivation",
            |handle| resolve_memory_source(ctx, &self.slots, handle),
        )?;
        let lexical_language = crate::lexical_language::resolve_lexical_language(
            item.language.as_deref(),
            &format!("{}\n{}", authored.title, authored.body),
        )
        .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
        let kind = DerivedKind::Abstraction;
        let derive::DerivationPlan {
            memory_id,
            operator_kind,
            derived_from,
            sidecar,
            authored,
        } = derive::plan_derivation(ctx, &owner, kind, authored, &sources)?;
        let extra_refs = pin.then_some(act_id).into_iter().collect::<Vec<_>>();
        let outcome = self
            .uow
            .author_derived(AuthorDerivedRequestInput {
                memory_id,
                owner,
                kind: kind.to_entity_kind(),
                text: authored.body.clone(),
                schema_id: SchemaId::new(
                    <AgentDerivationV1 as AbstractionPayload>::SCHEMA_ID.into(),
                ),
                schema_version: SchemaVersion::new(
                    <AgentDerivationV1 as AbstractionPayload>::SCHEMA_VERSION,
                ),
                operator_kind,
                operator_id: core_derive_operator_id(kind),
                input_contract_id: core_derive_input_contract_id(kind),
                model_id: &authored.model_id,
                sidecar_payload: SidecarPayload::abstraction(sidecar),
                derived_from: &derived_from,
                extra_refs: &extra_refs,
                supersedes: None,
                lexical_language: Some(lexical_language.as_str()),
            })
            .await
            .map_err(|err| map_bound_derived_error(err, "derive", pin))?;
        reject_bound_replay(pin, outcome.idempotent_replay, "derive")?;
        let handle =
            ctx.format_memory_with_class(outcome.memory_id, MemoryHandleClass::Abstraction);
        if pin {
            self.bound.push(handle.clone());
        }
        self.slots.derive = Some(Slot {
            id: outcome.memory_id,
            handle: handle.clone(),
            class: MemoryHandleClass::Abstraction,
        });
        Ok(handle)
    }

    async fn write_stances(&mut self, items: &[EpisodeStanceItem]) -> Result<(), McpToolError> {
        let ctx = self.ctx;
        let owner = self.owner;
        let bind = self.bind;
        let act_id = self.act_id;
        for (idx, item) in items.iter().enumerate() {
            let claim = validate_trimmed_len("claim", &item.claim, MAX_CLAIM_CHARS)?.to_string();
            if item.confidence > 100 {
                return Err(McpToolError::InvalidInput(
                    "confidence must be 0..=100".into(),
                ));
            }
            if item.subjects.is_empty() {
                return Err(McpToolError::InvalidInput(
                    "an interpretation must be about at least one memory".into(),
                ));
            }
            if item.subjects.len() > MAX_SUBJECTS {
                return Err(McpToolError::InvalidInput(format!(
                    "subjects must contain at most {MAX_SUBJECTS} handles"
                )));
            }
            let model_id = super::memory::util::operator_label(ctx, item.model_id.as_deref())?;
            let (subject_memory_ids, subject_kinds): (Vec<uuid::Uuid>, Vec<_>) = dedup_resolved(
                &item.subjects,
                MAX_SUBJECTS,
                "subjects",
                "an interpretation must be about at least one memory",
                |handle| resolve_stance_subject(ctx, &self.slots, handle),
            )?
            .into_iter()
            .map(|(memory_id, kind)| (memory_id.into_inner(), kind))
            .unzip();
            let memory_id = MemoryId::new(interpretation_memory_id(
                &owner,
                &model_id,
                &claim,
                item.confidence,
                &subject_memory_ids,
            ));
            reject_self_subject(memory_id, &subject_memory_ids)?;
            let payload = InterpretationV1 {
                claim: claim.clone(),
                confidence: item.confidence,
                subject_memory_ids,
                subject_kinds,
                model_id: model_id.clone(),
                client_name: ctx.author.client_name.clone(),
                client_version: ctx.author.client_version.clone(),
            };
            let pin = bind.stance.contains(&idx);
            let extra_refs = pin.then_some(act_id).into_iter().collect::<Vec<_>>();
            let outcome = self
                .uow
                .author_derived(AuthorDerivedRequestInput {
                    memory_id,
                    owner,
                    kind: EntityKind::Perspective,
                    text: claim,
                    schema_id: SchemaId::new(
                        <InterpretationV1 as PerspectivePayload>::SCHEMA_ID.into(),
                    ),
                    schema_version: SchemaVersion::new(
                        <InterpretationV1 as PerspectivePayload>::SCHEMA_VERSION,
                    ),
                    operator_kind: crate::MemoryOperatorKind::AtoP,
                    operator_id: interpret_operator_id(),
                    input_contract_id: interpret_input_contract_id(),
                    model_id: &model_id,
                    sidecar_payload: SidecarPayload::perspective(payload),
                    derived_from: &[],
                    extra_refs: &extra_refs,
                    supersedes: None,
                    // A stance item takes no `language`, and the
                    // interpretation schema declares `LanguagePolicy::PerRow`.
                    // The write names the deployment configuration rather than
                    // carrying no language at all — see `memory::interpret`.
                    lexical_language: Some(
                        crate::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT,
                    ),
                })
                .await
                .map_err(|err| map_bound_derived_error(err, "stance", pin))?;
            reject_bound_replay(pin, outcome.idempotent_replay, "stance")?;
            let handle =
                ctx.format_memory_with_class(outcome.memory_id, MemoryHandleClass::Perspective);
            if pin {
                self.bound.push(handle.clone());
            }
            self.slots.stance.push(Slot {
                id: outcome.memory_id,
                handle,
                class: MemoryHandleClass::Perspective,
            });
        }
        Ok(())
    }

    async fn write_goals(
        &mut self,
        items: &[EpisodeGoalItem],
    ) -> Result<Vec<String>, McpToolError> {
        let ctx = self.ctx;
        let owner = self.owner;
        let bind = self.bind;
        let act_id = self.act_id;
        let mut goals = Vec::new();
        for (idx, item) in items.iter().enumerate() {
            if item.evidence.is_empty() {
                return Err(McpToolError::InvalidInput(
                    "goal set requires >=1 Abstraction evidence handle motivating the goal".into(),
                ));
            }
            let payload = encode_goal_payload(ctx, item.payload.clone())?;
            let evidence = resolve_goal_evidence(ctx, &self.slots, &item.evidence)?;
            let assignment =
                resolve_goal_assignment(ctx, &self.slots, item.target_perspective.as_deref())?;
            let wake = item
                .wake
                .clone()
                .map(|wake| encode_wake_config(ctx, wake))
                .transpose()?;
            let topology = GoalTopologyWrite::new(assignment, Vec::new(), evidence)
                .map_err(McpToolError::Protocol)?;
            let request_id =
                IdempotencyKey::optional_or_generated("episode_goal", item.idempotency_key.clone())
                    .map_err(McpToolError::InvalidInput)?;
            let pin = bind.goal.contains(&idx);
            let outcome = self
                .uow
                .create_goal(
                    GoalCreatePayloadWriteRequest {
                        owner,
                        topology,
                        wake,
                        payload,
                        request_id,
                        authorship: system_operator_authorship(ctx, "episode_commit")?,
                        author_self_perspective_id: ctx.caller_self_perspective,
                    },
                    pin.then_some(act_id),
                )
                .await?;
            reject_bound_replay(pin, outcome.idempotent_replay, "goal")?;
            let handle = ctx.format_goal(outcome.goal_id);
            if pin {
                self.bound.push(handle.clone());
            }
            goals.push(handle);
        }
        Ok(goals)
    }

    /// Commit the transaction and hand back the minted slots and the
    /// bound-handle ledger, in write order.
    async fn commit(self) -> Result<(EpisodeSlots, Vec<String>), McpToolError> {
        self.uow.commit().await?;
        Ok((self.slots, self.bound))
    }
}

fn reject_bound_replay(pin: bool, replay: bool, kind: &str) -> Result<(), McpToolError> {
    if pin && replay {
        return Err(McpToolError::InvalidInput(format!(
            "bound {kind} replayed an existing row; bind requires a new admission that pins this write-act"
        )));
    }
    Ok(())
}

fn map_bound_fact_error(err: crate::error::ProtocolError, kind: &str, pin: bool) -> McpToolError {
    if pin
        && matches!(err.code, ErrorCode::InvalidArgument | ErrorCode::Internal)
        && err.message.contains("fact replay changed declared refs")
    {
        return McpToolError::InvalidInput(format!(
            "bound {kind} replayed an existing row; bind requires a new admission that pins this write-act"
        ));
    }
    McpToolError::Protocol(err)
}

fn map_bound_derived_error(
    err: crate::error::ProtocolError,
    kind: &str,
    pin: bool,
) -> McpToolError {
    if pin
        && matches!(err.code, ErrorCode::InvalidArgument | ErrorCode::Internal)
        && err.message.contains("derived replay changed declared refs")
    {
        return McpToolError::InvalidInput(format!(
            "bound {kind} replayed an existing memory; bind requires a new admission that pins this write-act"
        ));
    }
    map_derive_authoring_error(err)
}

fn resolve_memory_source(
    ctx: &McpToolCtx,
    slots: &EpisodeSlots,
    handle: &str,
) -> Result<(MemoryId, MemoryHandleClass), McpToolError> {
    if let Some(slot) = resolve_local_slot(slots, handle)? {
        return Ok((slot.id, slot.class));
    }
    derive::resolve_source_memory(ctx, handle)
}

fn resolve_stance_subject(
    ctx: &McpToolCtx,
    slots: &EpisodeSlots,
    handle: &str,
) -> Result<(MemoryId, InterpretationSubjectKind), McpToolError> {
    if let Some(slot) = resolve_local_slot(slots, handle)? {
        let kind = match slot.class {
            MemoryHandleClass::Fact => InterpretationSubjectKind::Fact,
            MemoryHandleClass::Abstraction => InterpretationSubjectKind::Abstraction,
            MemoryHandleClass::Perspective => InterpretationSubjectKind::Perspective,
        };
        return Ok((slot.id, kind));
    }
    interpret::resolve_subject(ctx, handle)
}

fn resolve_goal_evidence(
    ctx: &McpToolCtx,
    slots: &EpisodeSlots,
    evidence: &[String],
) -> Result<Vec<GoalEvidenceRef>, McpToolError> {
    evidence
        .iter()
        .map(|handle| {
            if let Some(slot) = resolve_local_slot(slots, handle)? {
                if slot.class != MemoryHandleClass::Abstraction {
                    return Err(McpToolError::InvalidInput(
                        "operator-authored Goal evidence must be Abstraction".into(),
                    ));
                }
                return Ok(GoalEvidenceRef::new(slot.id));
            }
            super::goal::resolve_operator_goal_evidence(ctx, std::slice::from_ref(handle))
                .map(|mut evidence| evidence.remove(0))
        })
        .collect()
}

fn resolve_goal_assignment(
    ctx: &McpToolCtx,
    slots: &EpisodeSlots,
    target_perspective: Option<&str>,
) -> Result<GoalAssignmentTarget, McpToolError> {
    match target_perspective {
        Some(handle) => {
            if let Some(slot) = resolve_local_slot(slots, handle)? {
                if slot.class != MemoryHandleClass::Perspective {
                    return Err(McpToolError::InvalidInput(
                        "target_perspective must name a Perspective".into(),
                    ));
                }
                return Ok(GoalAssignmentTarget::perspective(slot.id));
            }
            ctx.resolve_perspective_memory(handle)
                .map(GoalAssignmentTarget::perspective)
        }
        None => ctx
            .caller_self_perspective
            .map(GoalAssignmentTarget::perspective)
            .ok_or_else(|| {
                McpToolError::InvalidInput(
                    "target_perspective or caller Perspective context is required".into(),
                )
            }),
    }
}

fn resolve_local_slot<'a>(
    slots: &'a EpisodeSlots,
    handle: &str,
) -> Result<Option<&'a Slot>, McpToolError> {
    if let Some(idx) = handle.strip_prefix("remember:") {
        let idx: usize = idx.parse().map_err(|_| {
            McpToolError::InvalidInput(format!("local key {handle} is not remember:<index>"))
        })?;
        return slots.remember.get(idx).map(Some).ok_or_else(|| {
            McpToolError::InvalidInput(format!("local key {handle} is out of range"))
        });
    }
    if handle == "derive" {
        return slots
            .derive
            .as_ref()
            .map(Some)
            .ok_or_else(|| McpToolError::InvalidInput("local key derive is out of range".into()));
    }
    if let Some(idx) = handle.strip_prefix("stance:") {
        let idx: usize = idx.parse().map_err(|_| {
            McpToolError::InvalidInput(format!("local key {handle} is not stance:<index>"))
        })?;
        return slots.stance.get(idx).map(Some).ok_or_else(|| {
            McpToolError::InvalidInput(format!("local key {handle} is out of range"))
        });
    }
    if handle.starts_with("goal:") {
        return Err(McpToolError::InvalidInput(
            "goal:<index> is not a memory handle".into(),
        ));
    }
    Ok(None)
}
