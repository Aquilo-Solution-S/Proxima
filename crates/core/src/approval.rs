use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use crate::{
    EdgeAuthorshipKind, EdgeId, EntityKind, FactPayload, FlavorRegistryFrozen, MemoryId, Owner,
    SchemaId, SchemaVersion, SearchProjection, SearchProjectionColumnKind, SearchProjectionField,
    SourceBatchId, SourceId, Storage, StorageError,
};

/// Source id stamped on every approval-gate Fact event.
pub const APPROVAL_SOURCE_ID: &str = "core/approval";
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

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[
                SearchProjectionField {
                    column: "title",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "summary",
                    kind: SearchProjectionColumnKind::Text,
                },
            ],
        })
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

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[
                SearchProjectionField {
                    column: "verdict",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "rationale",
                    kind: SearchProjectionColumnKind::Text,
                },
            ],
        })
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

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[
                SearchProjectionField {
                    column: "decision",
                    kind: SearchProjectionColumnKind::Text,
                },
                SearchProjectionField {
                    column: "reason",
                    kind: SearchProjectionColumnKind::Text,
                },
            ],
        })
    }

    fn render(&self) -> String {
        format!("Approval decision: {}", self.decision.as_str())
    }
}

// ---------------------------------------------------------------------------
// ApprovalStore — the storage capability the approval tools depend on.
//
// A supertrait of `Storage` (see `crate::storage`). All data access for the
// approval gate lives behind this trait; the Postgres implementation is in
// `proxima-storage-pg`. The approval tools never touch a `PgPool` — they
// build typed payloads and hand them to these verbs. Default method bodies
// keep non-Postgres `Storage` impls (test fakes, `NoopStorage`) trivial.
// ---------------------------------------------------------------------------

/// One latest-per-voter approval vote, loaded for decision evaluation.
#[derive(Debug, Clone)]
pub struct ApprovalVoteRecord {
    pub memory_id: MemoryId,
    pub payload: ApprovalVoteV1,
}

/// Input to [`ApprovalStore::emit_approval_policy_atomic`].
#[derive(Debug, Clone)]
pub struct EmitApprovalPolicyInput {
    pub owner: Owner,
    pub payload: ApprovalPolicyV1,
    pub edge_authorship: EdgeAuthorshipKind,
    pub authorship_owner: Option<MemoryId>,
}

/// Outcome of [`ApprovalStore::emit_approval_policy_atomic`].
#[derive(Debug, Clone)]
pub struct ApprovalPolicyEmitOutcome {
    pub memory_id: MemoryId,
    pub target_edge_id: Option<uuid::Uuid>,
    pub idempotent_replay: bool,
}

/// Input to [`ApprovalStore::emit_approval_vote_atomic`].
#[derive(Debug, Clone)]
pub struct EmitApprovalVoteInput {
    pub owner: Owner,
    pub payload: ApprovalVoteV1,
    pub policy_memory_id: MemoryId,
    pub edge_authorship: EdgeAuthorshipKind,
    pub authorship_owner: Option<MemoryId>,
}

/// Outcome of [`ApprovalStore::emit_approval_vote_atomic`].
#[derive(Debug, Clone)]
pub struct ApprovalVoteEmitOutcome {
    pub memory_id: MemoryId,
    pub vote_edge_id: Option<uuid::Uuid>,
    pub idempotent_replay: bool,
}

/// Input to [`ApprovalStore::emit_approval_decision_atomic`].
#[derive(Debug, Clone)]
pub struct EmitApprovalDecisionInput {
    pub owner: Owner,
    pub payload: ApprovalDecisionV1,
    pub policy_memory_id: MemoryId,
    pub authorship_owner: Option<MemoryId>,
}

/// Outcome of [`ApprovalStore::emit_approval_decision_atomic`].
#[derive(Debug, Clone)]
pub struct ApprovalDecisionEmitOutcome {
    pub memory_id: MemoryId,
    pub edge_ids: Vec<uuid::Uuid>,
    pub idempotent_replay: bool,
}

/// Build the `EventDraft` for an approval-gate Fact: CBOR-encode the
/// typed payload, content-address it, and wire the content-addressing
/// object/whole schema ids. Pure (no I/O) — the storage verbs call this
/// before opening their transaction.
///
/// # Errors
///
/// Returns `StorageError::Internal` if `payload` fails to CBOR-encode
/// or carries a non-approval `SCHEMA_ID`.
pub fn approval_fact_event_draft<F: FactPayload + Serialize>(
    owner: &Owner,
    payload: &F,
) -> Result<EventDraft, StorageError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes)
        .map_err(|err| StorageError::Internal(format!("serialize approval payload: {err}")))?;
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
        other => {
            return Err(StorageError::Internal(format!(
                "unsupported approval payload schema {other}"
            )));
        }
    };
    Ok(EventDraft {
        source_id: SourceId::new(APPROVAL_SOURCE_ID),
        source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
        principal: owner.principal.clone(),
        org_id: Some(owner.org_id),
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
    })
}

fn approval_store_unimplemented(verb: &str) -> StorageError {
    StorageError::Internal(format!(
        "storage backend does not implement ApprovalStore::{verb}"
    ))
}

#[async_trait]
pub trait ApprovalStore: Send + Sync {
    /// Canonical kind string of an approval target, or `None` when the
    /// target is not visible to `owner`. A goal target resolves to
    /// `"goal"` when the goal row exists; a memory target resolves to
    /// its actual kind (`"fact"`, `"abstraction"`, `"perspective"`).
    async fn approval_target_kind(
        &self,
        _owner: &Owner,
        _target: &ApprovalTargetRef,
    ) -> Result<Option<String>, StorageError> {
        Ok(None)
    }

    /// Load an approval policy payload by its Fact memory id.
    async fn load_approval_policy(
        &self,
        _owner: &Owner,
        _policy_memory_id: MemoryId,
    ) -> Result<Option<ApprovalPolicyV1>, StorageError> {
        Ok(None)
    }

    /// Load the latest vote per voter for a policy.
    async fn load_approval_votes(
        &self,
        _owner: &Owner,
        _policy_memory_id: MemoryId,
    ) -> Result<Vec<ApprovalVoteRecord>, StorageError> {
        Ok(Vec::new())
    }

    /// Atomically materialize an approval-policy Fact, its sidecar row,
    /// and the `core/has-approval-policy` edge to the gated target.
    async fn emit_approval_policy_atomic(
        &self,
        _registry: &FlavorRegistryFrozen,
        _input: &EmitApprovalPolicyInput,
    ) -> Result<ApprovalPolicyEmitOutcome, StorageError> {
        Err(approval_store_unimplemented("emit_approval_policy_atomic"))
    }

    /// Atomically materialize an approval-vote Fact, its sidecar row,
    /// and the `core/votes-on` edge to the policy.
    async fn emit_approval_vote_atomic(
        &self,
        _registry: &FlavorRegistryFrozen,
        _input: &EmitApprovalVoteInput,
    ) -> Result<ApprovalVoteEmitOutcome, StorageError> {
        Err(approval_store_unimplemented("emit_approval_vote_atomic"))
    }

    /// Atomically materialize an approval-decision Fact, its sidecar
    /// row, and the decision provenance edges (`core/has-approval-decision`
    /// to the target, `core/derived-from` to the policy and each
    /// counted vote).
    async fn emit_approval_decision_atomic(
        &self,
        _registry: &FlavorRegistryFrozen,
        _input: &EmitApprovalDecisionInput,
    ) -> Result<ApprovalDecisionEmitOutcome, StorageError> {
        Err(approval_store_unimplemented(
            "emit_approval_decision_atomic",
        ))
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
            let target = resolve_target(&ctx, args.target_kind, &args.target)?;
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
            let outcome = require_storage(&ctx)?
                .emit_approval_policy_atomic(
                    &ctx.registry,
                    &EmitApprovalPolicyInput {
                        owner: ctx.owner.clone(),
                        payload,
                        edge_authorship: edge_authorship_for_ctx(&ctx),
                        authorship_owner: ctx.caller_self_perspective,
                    },
                )
                .await?;
            Ok(EmitApprovalPolicyOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                target_edge_handle: outcome
                    .target_edge_id
                    .map(|id| ctx.format_edge(EdgeId::new(id))),
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
                .eligible_voters
                .iter()
                .find(|candidate| candidate.voter_key == voter_key)
                .ok_or_else(|| McpToolError::InvalidInput("voter_key is not eligible".into()))?;
            validate_caller_for_voter(&ctx, voter)?;
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
            let outcome = require_storage(&ctx)?
                .emit_approval_vote_atomic(
                    &ctx.registry,
                    &EmitApprovalVoteInput {
                        owner: ctx.owner.clone(),
                        payload,
                        policy_memory_id,
                        edge_authorship: edge_authorship_for_ctx(&ctx),
                        authorship_owner: ctx.caller_self_perspective,
                    },
                )
                .await?;
            Ok(EmitApprovalVoteOutput {
                handle: ctx.format_fact_memory(outcome.memory_id),
                vote_edge_handle: outcome
                    .vote_edge_id
                    .map(|id| ctx.format_edge(EdgeId::new(id))),
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
            let evaluation = evaluate_policy(&policy, &votes)?;
            let Some(decision) = evaluation.decision else {
                return Ok(TryEmitApprovalDecisionOutput::NotReady {
                    reason: evaluation.reason,
                });
            };
            let payload = ApprovalDecisionV1 {
                policy_memory_id: policy_memory_id.into_inner(),
                target: policy.target.clone(),
                decision,
                reason: evaluation.reason.clone(),
                counted_votes: evaluation.counted_votes,
                idempotency_key,
                decided_at: OffsetDateTime::now_utc(),
            };
            let outcome = require_storage(&ctx)?
                .emit_approval_decision_atomic(
                    &ctx.registry,
                    &EmitApprovalDecisionInput {
                        owner: ctx.owner.clone(),
                        payload,
                        policy_memory_id,
                        authorship_owner: ctx.caller_self_perspective,
                    },
                )
                .await?;
            Ok(TryEmitApprovalDecisionOutput::Written {
                handle: ctx.format_fact_memory(outcome.memory_id),
                decision,
                reason: evaluation.reason,
                edge_handles: outcome
                    .edge_ids
                    .into_iter()
                    .map(|id| ctx.format_edge(EdgeId::new(id)))
                    .collect(),
                idempotent_replay: outcome.idempotent_replay,
            })
        })
    }
}

struct Evaluation {
    decision: Option<ApprovalDecision>,
    reason: String,
    counted_votes: Vec<ApprovalCountedVote>,
}

fn evaluate_policy(
    policy: &ApprovalPolicyV1,
    votes: &[ApprovalVoteRecord],
) -> Result<Evaluation, McpToolError> {
    let eligible: HashMap<&str, &ApprovalEligibleVoter> = policy
        .eligible_voters
        .iter()
        .map(|voter| (voter.voter_key.as_str(), voter))
        .collect();
    let latest_by_key: HashMap<&str, &ApprovalVoteRecord> = votes
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

fn counted_votes(latest_by_key: &HashMap<&str, &ApprovalVoteRecord>) -> Vec<ApprovalCountedVote> {
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

fn validate_caller_for_voter(
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

fn resolve_target(
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
    let actual = require_storage(ctx)?
        .approval_target_kind(&ctx.owner, target)
        .await?;
    let Some(actual_kind) = actual else {
        return Err(McpToolError::InvalidInput(match target.kind {
            ApprovalTargetKind::Goal => "target goal is not visible".into(),
            _ => "target memory is not visible".into(),
        }));
    };
    if actual_kind.to_ascii_lowercase() != target.kind.as_str() {
        return Err(McpToolError::InvalidInput(format!(
            "target kind mismatch: expected {}, got {actual_kind}",
            target.kind.as_str()
        )));
    }
    Ok(())
}

async fn load_policy(
    ctx: &McpToolCtx,
    policy_memory_id: MemoryId,
) -> Result<ApprovalPolicyV1, McpToolError> {
    require_storage(ctx)?
        .load_approval_policy(&ctx.owner, policy_memory_id)
        .await?
        .ok_or_else(|| McpToolError::InvalidInput("approval policy is not visible".into()))
}

async fn load_latest_votes(
    ctx: &McpToolCtx,
    policy_memory_id: MemoryId,
) -> Result<Vec<ApprovalVoteRecord>, McpToolError> {
    Ok(require_storage(ctx)?
        .load_approval_votes(&ctx.owner, policy_memory_id)
        .await?)
}

/// Borrow the engine-backed `Storage` handle the approval tools need.
///
/// `McpToolCtx::engine` is `None` only in test scaffolds without a wired
/// engine; the approval tools always require one.
fn require_storage(ctx: &McpToolCtx) -> Result<&dyn Storage, McpToolError> {
    ctx.storage()
        .ok_or_else(|| McpToolError::Other("approval tools require an attached engine".into()))
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
            .map(|key| ApprovalVoteRecord {
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
