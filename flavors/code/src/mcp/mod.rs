mod sql;

pub(crate) const REPO_HANDLE_KIND: &str = "proxima-code/repo";
pub(crate) const REPO_HANDLE_PREFIX: char = 'R';

pub mod emit_execution_request;
pub mod goal_completion_status;
pub mod merge_workspace_run;
pub mod open_file_revision;
pub mod repos;
pub mod search_chunks;
pub mod search_commits;
pub mod workspace_review;

pub use emit_execution_request::{
    CODE_HAS_ACCEPTANCE_CRITERIA_RELATION, CODE_TARGETS_EXECUTION_REQUEST_RELATION,
    CodeEmitExecutionPlanTool, CodeEmitExecutionRequestTool, CodeRetryExecutionRequestTool,
};
pub use goal_completion_status::CodeGoalCompletionStatusTool;
pub use merge_workspace_run::CodeMergeWorkspaceRunTool;
pub use open_file_revision::CodeOpenFileRevisionTool;
pub use repos::{CodeListReposTool, CodeRegisterRepoTool};
pub use search_chunks::CodeSearchChunksTool;
pub use search_commits::CodeSearchCommitsTool;
pub use workspace_review::{
    CODE_REVIEWS_RELATION, CodeEmitCorrectionExecutionRequestTool,
    CodeEmitVerificationEvidenceTool, CodeEmitWorkspaceReviewTool, MAX_WORKSPACE_VETO_ROUNDS,
    WORKSPACE_REVIEW_OBJECT_SCHEMA, WORKSPACE_REVIEW_SOURCE_ID, WORKSPACE_REVIEW_WHOLE_SCHEMA,
};
