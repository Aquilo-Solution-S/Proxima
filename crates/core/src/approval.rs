use std::collections::{HashMap, HashSet};

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;

use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use crate::{
    CORE_HAS_APPROVAL_DECISION_RELATION, CORE_HAS_APPROVAL_POLICY_RELATION, CORE_VOTES_ON_RELATION,
    EdgeAuthorshipKind, EdgeId, EntityKind, FactPayload, MemoryId, Owner, OwnerPrincipalKind,
    Principal, SchemaId, SchemaVersion, SourceBatchId, SourceId, StorageError,
};

const APPROVAL_SOURCE_ID: &str = "core/approval";
const APPROVAL_POLICY_OBJECT_SCHEMA: &str = "core/approval-policy-object-v1";
const APPROVAL_POLICY_WHOLE_SCHEMA: &str = "core/approval-policy-whole-v1";
const APPROVAL_VOTE_OBJECT_SCHEMA: &str = "core/approval-vote-object-v1";
const APPROVAL_VOTE_WHOLE_SCHEMA: &str = "core/approval-vote-whole-v1";
const APPROVAL_DECISION_OBJECT_SCHEMA: &str = "core/approval-decision-object-v1";
const APPROVAL_DECISION_WHOLE_SCHEMA: &str = "core/approval-decision-whole-v1";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_core.approval_target_kind",
    rename_all = "snake_case"
)]
pub enum ApprovalTargetKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
}

impl ApprovalTargetKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Abstraction => "abstraction",
            Self::Perspective => "perspective",
            Self::Goal => "goal",
        }
    }

    #[must_use]
    pub const fn entity_kind(self) -> EntityKind {
        match self {
            Self::Fact => EntityKind::Fact,
            Self::Abstraction => EntityKind::Abstraction,
            Self::Perspective => EntityKind::Perspective,
            Self::Goal => EntityKind::Goal,
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_core.approval_voter_kind",
    rename_all = "snake_case"
)]
pub enum ApprovalVoterKind {
    Personality,
    ShellAuthor,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_core.approval_vote_verdict",
    rename_all = "snake_case"
)]
pub enum ApprovalVoteVerdict {
    Approved,
    RequestChanges,
    Abstain,
}

impl ApprovalVoteVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::RequestChanges => "request_changes",
            Self::Abstain => "abstain",
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_core.approval_decision",
    rename_all = "snake_case"
)]
pub enum ApprovalDecision {
    Approved,
    Blocked,
}

impl ApprovalDecision {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_core.approval_requirement_kind",
    rename_all = "snake_case"
)]
pub enum ApprovalRequirementKind {
    AllOfVoters,
    RoleQuorum,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type)]
pub struct ApprovalTargetRef {
    pub kind: ApprovalTargetKind,
    #[serde(default)]
    pub memory_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub goal_id: Option<uuid::Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type)]
pub struct ApprovalEligibleVoter {
    pub voter_key: String,
    pub kind: ApprovalVoterKind,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub personality_instance_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub self_perspective_memory_id: Option<uuid::Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type)]
pub struct ApprovalRequirement {
    pub kind: ApprovalRequirementKind,
    #[serde(default)]
    pub voter_keys: Vec<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub min_approvals: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ApprovalPolicyV1 {
    pub target: ApprovalTargetRef,
    pub title: String,
    pub summary: String,
    pub eligible_voters: Vec<ApprovalEligibleVoter>,
    pub requirements: Vec<ApprovalRequirement>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl FactPayload for ApprovalPolicyV1 {
    const SCHEMA_ID: &'static str = "core/approval-policy-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.approval_policy_v1"
    }

    fn render(&self) -> String {
        format!("Approval policy: {}", self.title)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ApprovalVoteV1 {
    pub policy_memory_id: uuid::Uuid,
    pub voter_key: String,
    pub voter_kind: ApprovalVoterKind,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub personality_instance_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub self_perspective_memory_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub master_token_id: Option<uuid::Uuid>,
    pub verdict: ApprovalVoteVerdict,
    pub rationale: String,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub voted_at: OffsetDateTime,
}

impl FactPayload for ApprovalVoteV1 {
    const SCHEMA_ID: &'static str = "core/approval-vote-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.approval_vote_v1"
    }

    fn render(&self) -> String {
        format!(
            "Approval vote: {} by {}",
            self.verdict.as_str(),
            self.voter_key
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, specta::Type)]
pub struct ApprovalCountedVote {
    pub vote_memory_id: uuid::Uuid,
    pub voter_key: String,
    pub verdict: ApprovalVoteVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct ApprovalDecisionV1 {
    pub policy_memory_id: uuid::Uuid,
    pub target: ApprovalTargetRef,
    pub decision: ApprovalDecision,
    pub reason: String,
    pub counted_votes: Vec<ApprovalCountedVote>,
    pub idempotency_key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub decided_at: OffsetDateTime,
}

impl FactPayload for ApprovalDecisionV1 {
    const SCHEMA_ID: &'static str = "core/approval-decision-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.approval_decision_v1"
    }

    fn render(&self) -> String {
        format!("Approval decision: {}", self.decision.as_str())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmitApprovalPolicyArgs {
    pub target: String,
    pub target_kind: ApprovalTargetKind,
    pub title: String,
    pub summary: String,
    pub eligible_voters: Vec<ApprovalEligibleVoter>,
    pub requirements: Vec<ApprovalRequirement>,
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
pub struct EmitApprovalPolicyOutput {
    pub handle: String,
    pub target_edge_handle: Option<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmitApprovalVoteArgs {
    pub policy: String,
    pub voter_key: String,
    pub verdict: ApprovalVoteVerdict,
    pub rationale: String,
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
pub struct EmitApprovalVoteOutput {
    pub handle: String,
    pub vote_edge_handle: Option<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TryEmitApprovalDecisionArgs {
    pub policy: String,
    pub idempotency_key: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TryEmitApprovalDecisionOutput {
    NotReady {
        reason: String,
    },
    Written {
        handle: String,
        decision: ApprovalDecision,
        reason: String,
        edge_handles: Vec<String>,
        idempotent_replay: bool,
    },
}

#[derive(Debug, Default)]
pub struct EmitApprovalPolicyTool;

impl McpTool for EmitApprovalPolicyTool {
    const NAME: &'static str = "core/emit_approval_policy";
    const DESCRIPTION: &'static str =
        "Emit a core approval policy Fact that gates a Fact, Abstraction, Perspective, or Goal.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[ApprovalPolicyV1::SCHEMA_ID];

    type Args = EmitApprovalPolicyArgs;
    type Output = EmitApprovalPolicyOutput;

    fn call(
        ctx: McpToolCtx,
        args: EmitApprovalPolicyArgs,
    ) -> BoxFuture<'static, Result<EmitApprovalPolicyOutput, McpToolError>> {
        Box::pin(async move {
            let title = normalize_text("title", &args.title, 1, 300)?;
            let summary = normalize_text("summary", &args.summary, 1, 4000)?;
            let idempotency_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            validate_policy_inputs(&args.eligible_voters, &args.requirements)?;
            let target = resolve_target(&ctx, args.target_kind, &args.target).await?;
            validate_target_visible(&ctx, &target).await?;
            let payload = ApprovalPolicyV1 {
                target,
                title,
                summary,
                eligible_voters: args.eligible_voters,
                requirements: args.requirements,
                idempotency_key,
                created_at: OffsetDateTime::now_utc(),
            };
            let mut tx = ctx.pool.begin().await.map_err(map_sql)?;
            let outcome = ingest_approval_fact(&mut tx, &ctx, &payload).await?;
            let edge_id = if outcome.idempotent_replay {
                None
            } else {
                insert_policy_sidecar(&mut tx, outcome.memory_id, &payload).await?;
                Some(
                    append_target_to_fact_edge(
                        &mut tx,
                        &ctx,
                        &payload.target,
                        CORE_HAS_APPROVAL_POLICY_RELATION,
                        outcome.memory_id,
                        edge_authorship_for_ctx(&ctx),
                    )
                    .await?,
                )
            };
            tx.commit().await.map_err(map_sql)?;
            Ok(EmitApprovalPolicyOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                target_edge_handle: edge_id.map(|id| ctx.format_edge(EdgeId::new(id))),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

#[derive(Debug, Default)]
pub struct EmitApprovalVoteTool;

impl McpTool for EmitApprovalVoteTool {
    const NAME: &'static str = "core/emit_approval_vote";
    const DESCRIPTION: &'static str =
        "Emit a core approval vote Fact for an existing approval policy.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[ApprovalVoteV1::SCHEMA_ID];

    type Args = EmitApprovalVoteArgs;
    type Output = EmitApprovalVoteOutput;

    fn call(
        ctx: McpToolCtx,
        args: EmitApprovalVoteArgs,
    ) -> BoxFuture<'static, Result<EmitApprovalVoteOutput, McpToolError>> {
        Box::pin(async move {
            let policy_memory_id = ctx.resolve_fact_memory(&args.policy)?;
            let voter_key = normalize_text("voter_key", &args.voter_key, 1, 120)?;
            let rationale = normalize_text("rationale", &args.rationale, 1, 4000)?;
            let idempotency_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let policy = load_policy(&ctx, policy_memory_id).await?;
            let voter = policy
                .payload
                .eligible_voters
                .iter()
                .find(|candidate| candidate.voter_key == voter_key)
                .ok_or_else(|| McpToolError::InvalidInput("voter_key is not eligible".into()))?;
            validate_caller_for_voter(&ctx, voter).await?;
            let payload = ApprovalVoteV1 {
                policy_memory_id: policy_memory_id.into_inner(),
                voter_key,
                voter_kind: voter.kind,
                role: voter.role.clone(),
                personality_instance_id: voter.personality_instance_id,
                self_perspective_memory_id: match voter.kind {
                    ApprovalVoterKind::Personality => voter.self_perspective_memory_id,
                    ApprovalVoterKind::ShellAuthor => Some(
                        ctx.caller_self_perspective
                            .ok_or_else(|| {
                                McpToolError::InvalidInput(
                                    "caller_self_perspective is required for shell_author vote"
                                        .into(),
                                )
                            })?
                            .into_inner(),
                    ),
                },
                master_token_id: match voter.kind {
                    ApprovalVoterKind::Personality => None,
                    ApprovalVoterKind::ShellAuthor => ctx.master_token_id,
                },
                verdict: args.verdict,
                rationale,
                idempotency_key,
                voted_at: OffsetDateTime::now_utc(),
            };
            let mut tx = ctx.pool.begin().await.map_err(map_sql)?;
            let outcome = ingest_approval_fact(&mut tx, &ctx, &payload).await?;
            let edge_id = if outcome.idempotent_replay {
                None
            } else {
                insert_vote_sidecar(&mut tx, outcome.memory_id, &payload).await?;
                Some(
                    append_fact_to_fact_edge(
                        &mut tx,
                        &ctx,
                        CORE_VOTES_ON_RELATION,
                        outcome.memory_id,
                        policy_memory_id,
                        edge_authorship_for_ctx(&ctx),
                    )
                    .await?,
                )
            };
            tx.commit().await.map_err(map_sql)?;
            Ok(EmitApprovalVoteOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                vote_edge_handle: edge_id.map(|id| ctx.format_edge(EdgeId::new(id))),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

#[derive(Debug, Default)]
pub struct TryEmitApprovalDecisionTool;

impl McpTool for TryEmitApprovalDecisionTool {
    const NAME: &'static str = "core/try_emit_approval_decision";
    const DESCRIPTION: &'static str =
        "Evaluate a core approval policy and emit a decision Fact when the gate is closed.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[ApprovalDecisionV1::SCHEMA_ID];

    type Args = TryEmitApprovalDecisionArgs;
    type Output = TryEmitApprovalDecisionOutput;

    fn call(
        ctx: McpToolCtx,
        args: TryEmitApprovalDecisionArgs,
    ) -> BoxFuture<'static, Result<TryEmitApprovalDecisionOutput, McpToolError>> {
        Box::pin(async move {
            let policy_memory_id = ctx.resolve_fact_memory(&args.policy)?;
            let idempotency_key = normalize_text("idempotency_key", &args.idempotency_key, 1, 240)?;
            let policy = load_policy(&ctx, policy_memory_id).await?;
            let votes = load_latest_votes(&ctx, policy_memory_id).await?;
            let evaluation = evaluate_policy(&policy.payload, &votes)?;
            let Some(decision) = evaluation.decision else {
                return Ok(TryEmitApprovalDecisionOutput::NotReady {
                    reason: evaluation.reason,
                });
            };
            let payload = ApprovalDecisionV1 {
                policy_memory_id: policy_memory_id.into_inner(),
                target: policy.payload.target.clone(),
                decision,
                reason: evaluation.reason.clone(),
                counted_votes: evaluation.counted_votes,
                idempotency_key,
                decided_at: OffsetDateTime::now_utc(),
            };
            let mut tx = ctx.pool.begin().await.map_err(map_sql)?;
            let outcome = ingest_approval_fact(&mut tx, &ctx, &payload).await?;
            let mut edge_ids = Vec::new();
            if !outcome.idempotent_replay {
                insert_decision_sidecar(&mut tx, outcome.memory_id, &payload).await?;
                edge_ids.push(
                    append_target_to_fact_edge(
                        &mut tx,
                        &ctx,
                        &payload.target,
                        CORE_HAS_APPROVAL_DECISION_RELATION,
                        outcome.memory_id,
                        EdgeAuthorshipKind::Engine,
                    )
                    .await?,
                );
                edge_ids.push(
                    append_fact_to_fact_edge(
                        &mut tx,
                        &ctx,
                        crate::CORE_DERIVED_FROM_RELATION,
                        outcome.memory_id,
                        policy_memory_id,
                        EdgeAuthorshipKind::Engine,
                    )
                    .await?,
                );
                for vote in &payload.counted_votes {
                    edge_ids.push(
                        append_fact_to_fact_edge(
                            &mut tx,
                            &ctx,
                            crate::CORE_DERIVED_FROM_RELATION,
                            outcome.memory_id,
                            MemoryId::new(vote.vote_memory_id),
                            EdgeAuthorshipKind::Engine,
                        )
                        .await?,
                    );
                }
            }
            tx.commit().await.map_err(map_sql)?;
            Ok(TryEmitApprovalDecisionOutput::Written {
                handle: ctx.format_fact_memory(outcome.memory_id),
                decision,
                reason: evaluation.reason,
                edge_handles: edge_ids
                    .into_iter()
                    .map(|id| ctx.format_edge(EdgeId::new(id)))
                    .collect(),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

struct LoadedPolicy {
    payload: ApprovalPolicyV1,
}

#[derive(Clone)]
struct LoadedVote {
    memory_id: MemoryId,
    payload: ApprovalVoteV1,
}

struct Evaluation {
    decision: Option<ApprovalDecision>,
    reason: String,
    counted_votes: Vec<ApprovalCountedVote>,
}

fn evaluate_policy(
    policy: &ApprovalPolicyV1,
    votes: &[LoadedVote],
) -> Result<Evaluation, McpToolError> {
    let eligible: HashMap<&str, &ApprovalEligibleVoter> = policy
        .eligible_voters
        .iter()
        .map(|voter| (voter.voter_key.as_str(), voter))
        .collect();
    let latest_by_key: HashMap<&str, &LoadedVote> = votes
        .iter()
        .filter(|vote| eligible.contains_key(vote.payload.voter_key.as_str()))
        .map(|vote| (vote.payload.voter_key.as_str(), vote))
        .collect();

    for requirement in &policy.requirements {
        match requirement.kind {
            ApprovalRequirementKind::AllOfVoters => {
                for voter_key in &requirement.voter_keys {
                    let Some(vote) = latest_by_key.get(voter_key.as_str()) else {
                        return Ok(Evaluation {
                            decision: None,
                            reason: format!("missing required approval from {voter_key}"),
                            counted_votes: counted_votes(&latest_by_key),
                        });
                    };
                    match vote.payload.verdict {
                        ApprovalVoteVerdict::Approved => {}
                        ApprovalVoteVerdict::RequestChanges => {
                            return Ok(Evaluation {
                                decision: Some(ApprovalDecision::Blocked),
                                reason: format!("{voter_key} requested changes"),
                                counted_votes: counted_votes(&latest_by_key),
                            });
                        }
                        ApprovalVoteVerdict::Abstain => {
                            return Ok(Evaluation {
                                decision: None,
                                reason: format!("{voter_key} abstained"),
                                counted_votes: counted_votes(&latest_by_key),
                            });
                        }
                    }
                }
            }
            ApprovalRequirementKind::RoleQuorum => {
                let role = requirement.role.as_deref().ok_or_else(|| {
                    McpToolError::InvalidInput("role_quorum requirement missing role".into())
                })?;
                let min_approvals = requirement.min_approvals.unwrap_or(1);
                let mut approvals = 0_u32;
                for vote in latest_by_key.values() {
                    let voter_role = eligible
                        .get(vote.payload.voter_key.as_str())
                        .and_then(|voter| voter.role.as_deref());
                    if voter_role != Some(role) {
                        continue;
                    }
                    match vote.payload.verdict {
                        ApprovalVoteVerdict::Approved => approvals += 1,
                        ApprovalVoteVerdict::RequestChanges => {
                            return Ok(Evaluation {
                                decision: Some(ApprovalDecision::Blocked),
                                reason: format!("role {role} requested changes"),
                                counted_votes: counted_votes(&latest_by_key),
                            });
                        }
                        ApprovalVoteVerdict::Abstain => {}
                    }
                }
                if approvals < min_approvals {
                    return Ok(Evaluation {
                        decision: None,
                        reason: format!(
                            "role {role} has {approvals} approvals; requires {min_approvals}"
                        ),
                        counted_votes: counted_votes(&latest_by_key),
                    });
                }
            }
        }
    }

    Ok(Evaluation {
        decision: Some(ApprovalDecision::Approved),
        reason: "all approval requirements satisfied".into(),
        counted_votes: counted_votes(&latest_by_key),
    })
}

fn counted_votes(latest_by_key: &HashMap<&str, &LoadedVote>) -> Vec<ApprovalCountedVote> {
    let mut votes: Vec<_> = latest_by_key
        .values()
        .map(|vote| ApprovalCountedVote {
            vote_memory_id: vote.memory_id.into_inner(),
            voter_key: vote.payload.voter_key.clone(),
            verdict: vote.payload.verdict,
        })
        .collect();
    votes.sort_by(|a, b| a.voter_key.cmp(&b.voter_key));
    votes
}

fn validate_policy_inputs(
    voters: &[ApprovalEligibleVoter],
    requirements: &[ApprovalRequirement],
) -> Result<(), McpToolError> {
    if voters.is_empty() {
        return Err(McpToolError::InvalidInput(
            "eligible_voters must not be empty".into(),
        ));
    }
    if requirements.is_empty() {
        return Err(McpToolError::InvalidInput(
            "requirements must not be empty".into(),
        ));
    }
    let mut seen = HashSet::new();
    let keys: HashSet<_> = voters
        .iter()
        .map(|voter| voter.voter_key.as_str())
        .collect();
    for voter in voters {
        normalize_text("voter_key", &voter.voter_key, 1, 120)?;
        if !seen.insert(voter.voter_key.as_str()) {
            return Err(McpToolError::InvalidInput(format!(
                "duplicate voter_key {}",
                voter.voter_key
            )));
        }
        match voter.kind {
            ApprovalVoterKind::Personality => {
                if voter.personality_instance_id.is_none()
                    || voter.self_perspective_memory_id.is_none()
                {
                    return Err(McpToolError::InvalidInput(
                        "personality voter requires personality_instance_id and self_perspective_memory_id"
                            .into(),
                    ));
                }
            }
            ApprovalVoterKind::ShellAuthor => {
                if voter.personality_instance_id.is_some()
                    || voter.self_perspective_memory_id.is_some()
                {
                    return Err(McpToolError::InvalidInput(
                        "shell_author voter must not predeclare personality ids".into(),
                    ));
                }
            }
        }
    }
    for requirement in requirements {
        match requirement.kind {
            ApprovalRequirementKind::AllOfVoters => {
                if requirement.voter_keys.is_empty() {
                    return Err(McpToolError::InvalidInput(
                        "all_of_voters requires voter_keys".into(),
                    ));
                }
                for voter_key in &requirement.voter_keys {
                    if !keys.contains(voter_key.as_str()) {
                        return Err(McpToolError::InvalidInput(format!(
                            "requirement references unknown voter_key {voter_key}"
                        )));
                    }
                }
            }
            ApprovalRequirementKind::RoleQuorum => {
                let role = requirement.role.as_deref().ok_or_else(|| {
                    McpToolError::InvalidInput("role_quorum requires role".into())
                })?;
                normalize_text("role", role, 1, 120)?;
                let min_approvals = requirement.min_approvals.ok_or_else(|| {
                    McpToolError::InvalidInput("role_quorum requires min_approvals".into())
                })?;
                if min_approvals == 0 {
                    return Err(McpToolError::InvalidInput(
                        "role_quorum min_approvals must be positive".into(),
                    ));
                }
                let role_count = voters
                    .iter()
                    .filter(|voter| voter.role.as_deref() == Some(role))
                    .count();
                if role_count < usize::try_from(min_approvals).unwrap_or(usize::MAX) {
                    return Err(McpToolError::InvalidInput(format!(
                        "role {role} has fewer eligible voters than min_approvals"
                    )));
                }
            }
        }
    }
    Ok(())
}

async fn validate_caller_for_voter(
    ctx: &McpToolCtx,
    voter: &ApprovalEligibleVoter,
) -> Result<(), McpToolError> {
    match voter.kind {
        ApprovalVoterKind::Personality => {
            let caller_self = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput(
                    "caller_self_perspective is required for personality vote".into(),
                )
            })?;
            if Some(caller_self.into_inner()) != voter.self_perspective_memory_id {
                return Err(McpToolError::InvalidInput(
                    "caller_self_perspective does not match eligible voter".into(),
                ));
            }
        }
        ApprovalVoterKind::ShellAuthor => {
            if ctx.master_token_id.is_none() {
                return Err(McpToolError::InvalidInput(
                    "shell_author vote requires master token context".into(),
                ));
            }
            if ctx.caller_self_perspective.is_none() {
                return Err(McpToolError::InvalidInput(
                    "shell_author vote requires caller_self_perspective".into(),
                ));
            }
        }
    }
    Ok(())
}

async fn resolve_target(
    ctx: &McpToolCtx,
    target_kind: ApprovalTargetKind,
    raw: &str,
) -> Result<ApprovalTargetRef, McpToolError> {
    match target_kind {
        ApprovalTargetKind::Goal => Ok(ApprovalTargetRef {
            kind: target_kind,
            memory_id: None,
            goal_id: Some(ctx.resolve_goal(raw)?.into_inner()),
        }),
        ApprovalTargetKind::Fact
        | ApprovalTargetKind::Abstraction
        | ApprovalTargetKind::Perspective => Ok(ApprovalTargetRef {
            kind: target_kind,
            memory_id: Some(ctx.resolve_memory(raw)?.into_inner()),
            goal_id: None,
        }),
    }
}

async fn validate_target_visible(
    ctx: &McpToolCtx,
    target: &ApprovalTargetRef,
) -> Result<(), McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    match target.kind {
        ApprovalTargetKind::Goal => {
            let goal_id = target
                .goal_id
                .ok_or_else(|| McpToolError::InvalidInput("goal target missing goal_id".into()))?;
            let exists: Option<uuid::Uuid> = sqlx::query_scalar(
                "SELECT goal_id FROM proxima_core.goals
                 WHERE goal_id = $1
                   AND owner_principal_kind = $2
                   AND owner_principal_id = $3
                   AND owner_org_id = $4",
            )
            .bind(goal_id)
            .bind(owner_kind)
            .bind(owner_id)
            .bind(owner_org_id)
            .fetch_optional(&ctx.pool)
            .await
            .map_err(map_sql)?;
            if exists.is_none() {
                return Err(McpToolError::InvalidInput(
                    "target goal is not visible".into(),
                ));
            }
        }
        _ => {
            let memory_id = target.memory_id.ok_or_else(|| {
                McpToolError::InvalidInput("memory target missing memory_id".into())
            })?;
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT CASE WHEN event_id IS NOT NULL THEN 'Fact' ELSE kind::text END
                   FROM proxima_core.memories
                 WHERE memory_id = $1
                   AND owner_principal_kind = $2
                   AND owner_principal_id = $3
                   AND owner_org_id = $4",
            )
            .bind(memory_id)
            .bind(owner_kind)
            .bind(owner_id)
            .bind(owner_org_id)
            .fetch_optional(&ctx.pool)
            .await
            .map_err(map_sql)?;
            let Some((actual_kind,)) = row else {
                return Err(McpToolError::InvalidInput(
                    "target memory is not visible".into(),
                ));
            };
            if actual_kind.to_ascii_lowercase() != target.kind.as_str() {
                return Err(McpToolError::InvalidInput(format!(
                    "target kind mismatch: expected {}, got {actual_kind}",
                    target.kind.as_str()
                )));
            }
        }
    }
    Ok(())
}

async fn load_policy(
    ctx: &McpToolCtx,
    policy_memory_id: MemoryId,
) -> Result<LoadedPolicy, McpToolError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(&ctx.owner);
    let row: Option<(
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
          WHERE p.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(policy_memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_sql)?;
    let Some((
        _,
        target_kind,
        target_memory_id,
        target_goal_id,
        title,
        summary,
        eligible_voters_json,
        requirements_json,
        idempotency_key,
        created_at,
    )) = row
    else {
        return Err(McpToolError::InvalidInput(
            "approval policy is not visible".into(),
        ));
    };
    Ok(LoadedPolicy {
        payload: ApprovalPolicyV1 {
            target: ApprovalTargetRef {
                kind: target_kind,
                memory_id: target_memory_id,
                goal_id: target_goal_id,
            },
            title,
            summary,
            eligible_voters: serde_json::from_value(eligible_voters_json)
                .map_err(|err| McpToolError::Other(format!("decode eligible voters: {err}")))?,
            requirements: serde_json::from_value(requirements_json)
                .map_err(|err| McpToolError::Other(format!("decode requirements: {err}")))?,
            idempotency_key,
            created_at,
        },
    })
}

async fn load_latest_votes(
    ctx: &McpToolCtx,
    policy_memory_id: MemoryId,
) -> Result<Vec<LoadedVote>, McpToolError> {
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
        "SELECT DISTINCT ON (v.voter_key)
                v.memory_id, v.policy_memory_id, v.voter_key, v.voter_kind,
                v.role, v.personality_instance_id, v.self_perspective_memory_id,
                v.master_token_id, v.verdict, v.rationale, v.idempotency_key, v.voted_at
           FROM proxima_core.approval_vote_v1 v
           JOIN proxima_core.memories m USING (memory_id)
          WHERE v.policy_memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY v.voter_key, v.voted_at DESC, v.memory_id DESC",
    )
    .bind(policy_memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
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
            )| LoadedVote {
                memory_id: MemoryId::new(memory_id),
                payload: ApprovalVoteV1 {
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
            },
        )
        .collect())
}

async fn ingest_approval_fact<F: FactPayload + Serialize>(
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
        ApprovalPolicyV1::SCHEMA_ID => {
            (APPROVAL_POLICY_OBJECT_SCHEMA, APPROVAL_POLICY_WHOLE_SCHEMA)
        }
        ApprovalVoteV1::SCHEMA_ID => (APPROVAL_VOTE_OBJECT_SCHEMA, APPROVAL_VOTE_WHOLE_SCHEMA),
        ApprovalDecisionV1::SCHEMA_ID => (
            APPROVAL_DECISION_OBJECT_SCHEMA,
            APPROVAL_DECISION_WHOLE_SCHEMA,
        ),
        _ => return Err(McpToolError::Other("unsupported approval payload".into())),
    };
    let draft = EventDraft {
        source_id: SourceId::new(APPROVAL_SOURCE_ID),
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

async fn insert_policy_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ApprovalPolicyV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_core.approval_policy_v1
            (memory_id, target_kind, target_memory_id, target_goal_id,
             title, summary, eligible_voters_json, requirements_json,
             idempotency_key, created_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(memory_id.into_inner())
    .bind(payload.target.kind)
    .bind(payload.target.memory_id)
    .bind(payload.target.goal_id)
    .bind(&payload.title)
    .bind(&payload.summary)
    .bind(serde_json::to_value(&payload.eligible_voters).map_err(json_err)?)
    .bind(serde_json::to_value(&payload.requirements).map_err(json_err)?)
    .bind(&payload.idempotency_key)
    .bind(payload.created_at)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(())
}

async fn insert_vote_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ApprovalVoteV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_core.approval_vote_v1
            (memory_id, policy_memory_id, voter_key, voter_kind, role,
             personality_instance_id, self_perspective_memory_id, master_token_id,
             verdict, rationale, idempotency_key, voted_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(memory_id.into_inner())
    .bind(payload.policy_memory_id)
    .bind(&payload.voter_key)
    .bind(payload.voter_kind)
    .bind(payload.role.as_deref())
    .bind(payload.personality_instance_id)
    .bind(payload.self_perspective_memory_id)
    .bind(payload.master_token_id)
    .bind(payload.verdict)
    .bind(&payload.rationale)
    .bind(&payload.idempotency_key)
    .bind(payload.voted_at)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(())
}

async fn insert_decision_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ApprovalDecisionV1,
) -> Result<(), McpToolError> {
    sqlx::query(
        "INSERT INTO proxima_core.approval_decision_v1
            (memory_id, policy_memory_id, target_kind, target_memory_id,
             target_goal_id, decision, reason, counted_votes_json,
             idempotency_key, decided_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(memory_id.into_inner())
    .bind(payload.policy_memory_id)
    .bind(payload.target.kind)
    .bind(payload.target.memory_id)
    .bind(payload.target.goal_id)
    .bind(payload.decision)
    .bind(&payload.reason)
    .bind(serde_json::to_value(&payload.counted_votes).map_err(json_err)?)
    .bind(&payload.idempotency_key)
    .bind(payload.decided_at)
    .execute(&mut **tx)
    .await
    .map_err(map_sql)?;
    Ok(())
}

async fn append_target_to_fact_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    target: &ApprovalTargetRef,
    relation: &str,
    fact: MemoryId,
    authorship_kind: EdgeAuthorshipKind,
) -> Result<uuid::Uuid, McpToolError> {
    let source_kind = target.kind.entity_kind();
    let (source_memory_id, source_goal_id) = match target.kind {
        ApprovalTargetKind::Goal => (None, target.goal_id),
        _ => (target.memory_id, None),
    };
    append_edge(
        tx,
        ctx,
        relation,
        source_kind,
        source_memory_id,
        source_goal_id,
        EntityKind::Fact,
        Some(fact.into_inner()),
        None,
        authorship_kind,
    )
    .await
}

async fn append_fact_to_fact_edge(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    relation: &str,
    source: MemoryId,
    target: MemoryId,
    authorship_kind: EdgeAuthorshipKind,
) -> Result<uuid::Uuid, McpToolError> {
    append_edge(
        tx,
        ctx,
        relation,
        EntityKind::Fact,
        Some(source.into_inner()),
        None,
        EntityKind::Fact,
        Some(target.into_inner()),
        None,
        authorship_kind,
    )
    .await
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

fn json_err(err: serde_json::Error) -> McpToolError {
    McpToolError::InvalidInput(format!("serialize json: {err}"))
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

    fn shell_voter(key: &str) -> ApprovalEligibleVoter {
        ApprovalEligibleVoter {
            voter_key: key.into(),
            kind: ApprovalVoterKind::ShellAuthor,
            role: Some("review".into()),
            personality_instance_id: None,
            self_perspective_memory_id: None,
        }
    }

    #[test]
    fn role_quorum_uses_latest_votes() {
        let policy = ApprovalPolicyV1 {
            target: ApprovalTargetRef {
                kind: ApprovalTargetKind::Goal,
                memory_id: None,
                goal_id: Some(uuid::Uuid::now_v7()),
            },
            title: "test".into(),
            summary: "test".into(),
            eligible_voters: vec![shell_voter("a"), shell_voter("b")],
            requirements: vec![ApprovalRequirement {
                kind: ApprovalRequirementKind::RoleQuorum,
                voter_keys: Vec::new(),
                role: Some("review".into()),
                min_approvals: Some(2),
            }],
            idempotency_key: "policy".into(),
            created_at: OffsetDateTime::now_utc(),
        };
        let votes = ["a", "b"]
            .into_iter()
            .map(|key| LoadedVote {
                memory_id: MemoryId::new(uuid::Uuid::now_v7()),
                payload: ApprovalVoteV1 {
                    policy_memory_id: uuid::Uuid::now_v7(),
                    voter_key: key.into(),
                    voter_kind: ApprovalVoterKind::ShellAuthor,
                    role: Some("review".into()),
                    personality_instance_id: None,
                    self_perspective_memory_id: None,
                    master_token_id: Some(uuid::Uuid::now_v7()),
                    verdict: ApprovalVoteVerdict::Approved,
                    rationale: "ok".into(),
                    idempotency_key: key.into(),
                    voted_at: OffsetDateTime::now_utc(),
                },
            })
            .collect::<Vec<_>>();
        let evaluation = evaluate_policy(&policy, &votes).expect("evaluate");
        assert_eq!(evaluation.decision, Some(ApprovalDecision::Approved));
    }
}
