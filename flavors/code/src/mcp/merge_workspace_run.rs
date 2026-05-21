use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::{FactPayload, MemoryId, StorageError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::payloads::WorkspaceDecisionV1;
use crate::repos::WorkspaceMergeOutcome;
use crate::workspace_flow::{WorkspaceFlowError, merge_workspace_run};

#[derive(Debug)]
pub struct CodeMergeWorkspaceRunTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeMergeWorkspaceRunArgs {
    /// Handle (F...) for the proxima-core/workspace-run-v1 Fact to merge.
    #[schemars(
        description = "`F...` memory handle for the proxima-core/workspace-run-v1 Fact to merge after approval. Model wakes should not call this."
    )]
    pub workspace_run_memory: String,
}

#[derive(Debug, Serialize)]
pub struct CodeMergeWorkspaceRunOutput {
    pub workspace_run_memory: String,
    pub workspace_decision_memory: String,
    pub repo_id: String,
    pub target_branch: String,
    pub old_target_sha: String,
    pub new_target_sha: String,
}

impl McpTool for CodeMergeWorkspaceRunTool {
    const NAME: &'static str = "proxima-code/code_merge_workspace_run";
    const DESCRIPTION: &'static str = "Fast-forward the repo target branch to the workspace run's head SHA after an Approved \
         latest workspace-review-v1 and emit a proxima-code/workspace-decision-v1 Fact with \
         decision=Merged. Rejects if the latest review is not Approved or a later workspace \
         decision already exists. Args: `{workspace_run_memory: \"W…\"}`.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[WorkspaceDecisionV1::SCHEMA_ID];

    type Args = CodeMergeWorkspaceRunArgs;
    type Output = CodeMergeWorkspaceRunOutput;

    fn call(
        ctx: McpToolCtx,
        args: CodeMergeWorkspaceRunArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeMergeWorkspaceRunOutput, McpToolError>>
    {
        Box::pin(async move {
            // User-confirmation gate: this tool fast-forwards a git target
            // branch — an irreversible action. Only master-token callers
            // (the shell on behalf of the user) may invoke it. Model wake
            // dispatches carry no master_token_id.
            if ctx.master_token_id.is_none() {
                return Err(McpToolError::InvalidInput(
                    "code_merge_workspace_run requires a master-token caller (user action); \
                     models cannot trigger merges directly"
                        .into(),
                ));
            }
            let run_memory_id = ctx.resolve_fact_memory(&args.workspace_run_memory)?;
            let outcome = merge_workspace_run(&ctx.pool, &ctx.owner, run_memory_id)
                .await
                .map_err(map_workspace_flow_err)?;
            let WorkspaceMergeOutcome {
                run_memory_id,
                decision_memory_id,
                repo_id,
                target_branch,
                old_target_sha,
                new_target_sha,
            } = outcome;
            Ok(CodeMergeWorkspaceRunOutput {
                workspace_run_memory: ctx.format_fact_memory(MemoryId::new(run_memory_id)),
                workspace_decision_memory: ctx
                    .format_fact_memory(MemoryId::new(decision_memory_id)),
                repo_id: repo_id.to_string(),
                target_branch,
                old_target_sha,
                new_target_sha,
            })
        })
    }
}

fn map_workspace_flow_err(err: WorkspaceFlowError) -> McpToolError {
    match err {
        WorkspaceFlowError::RunNotFound { memory_id } => {
            McpToolError::InvalidInput(format!("workspace run not found: {memory_id}"))
        }
        WorkspaceFlowError::ApprovedReviewRequired { memory_id } => McpToolError::InvalidInput(
            format!("workspace run requires an Approved latest review before merge: {memory_id}"),
        ),
        WorkspaceFlowError::LaterWorkspaceDecision { memory_id } => McpToolError::InvalidInput(
            format!("workspace run already has a later workspace decision: {memory_id}"),
        ),
        WorkspaceFlowError::RepoNotFound { repo_id } => {
            McpToolError::InvalidInput(format!("repo not found: {repo_id}"))
        }
        WorkspaceFlowError::MissingTargetBranch { repo_id } => {
            McpToolError::InvalidInput(format!("repo has no target branch configured: {repo_id}"))
        }
        WorkspaceFlowError::Git { command, stderr } => McpToolError::Other(format!(
            "git command failed during merge: {command}: {stderr}"
        )),
        WorkspaceFlowError::InvalidSidecar { message } => {
            McpToolError::Storage(StorageError::Internal(message))
        }
        WorkspaceFlowError::Database(err) => {
            McpToolError::Storage(StorageError::Internal(err.to_string()))
        }
        WorkspaceFlowError::Storage(err) => McpToolError::Storage(err),
        WorkspaceFlowError::InvalidReviewVerdict { value } => McpToolError::InvalidInput(format!(
            "invalid workspace review verdict in sidecar: {value}"
        )),
        WorkspaceFlowError::InvalidDecision { value } => {
            McpToolError::InvalidInput(format!("invalid workspace decision in sidecar: {value}"))
        }
    }
}
