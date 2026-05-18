// Workspace review types and constants
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use proxima_core::MemoryId;

use crate::payloads::{
    VerificationEvidenceStatus, WorkspaceDecisionV1, WorkspaceReviewFinding, WorkspaceReviewV1,
    WorkspaceReviewVerdict,
};

pub const WORKSPACE_REVIEW_SOURCE_ID: &str = "proxima-code/workspace-review";
pub const WORKSPACE_REVIEW_OBJECT_SCHEMA: &str = "proxima-code/workspace-review-object-v1";
pub const WORKSPACE_REVIEW_WHOLE_SCHEMA: &str = "proxima-code/workspace-review-whole-v1";
pub const MAX_WORKSPACE_VETO_ROUNDS: i64 = 2;

/// Arguments for emitting a workspace review
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeEmitWorkspaceReviewArgs {
    pub workspace_run_memory: String,
    pub verdict: WorkspaceReviewVerdict,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<WorkspaceReviewFinding>,
    #[serde(default)]
    pub correction_instructions: Option<String>,
    #[serde(default)]
    pub verification_summary: Option<String>,
    pub idempotency_key: String,
}

/// Output for emitting a workspace review
#[derive(Debug, Serialize)]
pub struct CodeEmitWorkspaceReviewOutput {
    pub handle: String,
    pub authored_edge_handle: Option<String>,
    pub derived_edge_handles: Vec<String>,
    pub verdict: WorkspaceReviewVerdict,
    pub round_index: u32,
    pub idempotent_replay: bool,
}

/// Arguments for emitting verifier evidence
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeEmitVerificationEvidenceArgs {
    pub workspace_run_memory: String,
    pub criterion_key: String,
    pub status: VerificationEvidenceStatus,
    pub summary: String,
    #[serde(default)]
    pub artifact_refs_json: serde_json::Value,
    pub idempotency_key: String,
}

/// Output for emitting verifier evidence
#[derive(Debug, Serialize)]
pub struct CodeEmitVerificationEvidenceOutput {
    pub handle: String,
    pub authored_edge_handle: Option<String>,
    pub derived_edge_handles: Vec<String>,
    pub idempotent_replay: bool,
}

/// Arguments for emitting a correction execution request
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeEmitCorrectionExecutionRequestArgs {
    #[serde(default)]
    pub workspace_review_memory: Option<String>,
    #[serde(default)]
    pub workspace_decision_memory: Option<String>,
    pub target_personality: String,
    pub idempotency_key: String,
}

/// Output for emitting a correction execution request
#[derive(Debug, Serialize)]
pub struct CodeEmitCorrectionExecutionRequestOutput {
    pub handle: String,
    pub authored_edge_handle: Option<String>,
    pub target_edge_handle: Option<String>,
    pub derived_edge_handles: Vec<String>,
    pub idempotent_replay: bool,
}

/// Trigger for correction execution requests
#[derive(Debug)]
pub enum CorrectionTrigger {
    RejectedReview(LoadedWorkspaceReview),
    RetryDecision {
        decision: LoadedWorkspaceDecision,
        execution_request_memory_id: MemoryId,
        latest_rejected_review: Option<LoadedWorkspaceReview>,
    },
}

impl CorrectionTrigger {
    #[must_use]
    pub fn execution_request_memory_id(&self) -> MemoryId {
        match self {
            Self::RejectedReview(review) => review.execution_request_memory_id,
            Self::RetryDecision {
                execution_request_memory_id,
                ..
            } => *execution_request_memory_id,
        }
    }

    #[must_use]
    pub fn workspace_run_memory_id(&self) -> MemoryId {
        match self {
            Self::RejectedReview(review) => MemoryId::new(review.payload.workspace_run_memory_id),
            Self::RetryDecision { decision, .. } => {
                MemoryId::new(decision.payload.workspace_run_memory_id)
            }
        }
    }

    #[must_use]
    pub fn rejected_review(&self) -> Option<&LoadedWorkspaceReview> {
        match self {
            Self::RejectedReview(review) => Some(review),
            Self::RetryDecision {
                latest_rejected_review,
                ..
            } => latest_rejected_review.as_ref(),
        }
    }

    #[must_use]
    pub fn retry_decision(&self) -> Option<&LoadedWorkspaceDecision> {
        match self {
            Self::RejectedReview(_) => None,
            Self::RetryDecision { decision, .. } => Some(decision),
        }
    }
}

/// Loaded workspace review with memory context
#[derive(Debug)]
pub struct LoadedWorkspaceReview {
    pub memory_id: MemoryId,
    pub execution_request_memory_id: MemoryId,
    pub payload: WorkspaceReviewV1,
}

/// Loaded workspace decision with memory context
#[derive(Debug)]
pub struct LoadedWorkspaceDecision {
    pub memory_id: MemoryId,
    pub payload: WorkspaceDecisionV1,
}
