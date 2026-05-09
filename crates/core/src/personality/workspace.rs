//! Flavor-supplied workspace runners.
//!
//! See `docs/superpowers/specs/2026-05-09-workspace-mode-design.md` for
//! the full design. Runners are registered per-flavor via the
//! `proxima_flavor!` macro and looked up by `flavor_id` in
//! `wake/fire.rs` when a workspace-mode wake fires. The trait is
//! intentionally minimal in Phase 1 — `WorkspaceScope` lands in
//! Phase 2.

use std::path::PathBuf;

use thiserror::Error;
use uuid::Uuid;

use crate::{MemoryId, Owner};

/// Errors a runner can return from `prepare` or `finalize`.
///
/// `Unimplemented` is the v1-Phase-1 sentinel: the Code flavor's
/// runner returns it so existing behaviour (failure_reason =
/// "workspace_mode_not_yet_implemented") is preserved while the seam
/// lands.
#[derive(Debug, Error)]
pub enum WorkspaceRunnerError {
    #[error("workspace runner not implemented for this flavor")]
    Unimplemented,
    #[error("workspace runner internal error: {0}")]
    Internal(String),
}

/// Everything the dispatcher hands a runner before the adapter runs.
///
/// Lifetimes are short — only valid for the duration of one
/// `prepare` call. Owners hold no state across calls.
pub struct WorkspacePrepareInput<'a> {
    pub invocation_id: Uuid,
    pub owner: &'a Owner,
    /// The wake_token the dispatcher has minted for this invocation.
    /// Forwarded to the runner so workspace-side tools can authenticate
    /// to the substrate MCP listener with the same token the goose
    /// subprocess receives via `PROXIMA_WAKE_TOKEN`.
    pub wake_token: Uuid,
    pub mcp_url: &'a str,
    /// Snapshotted Root Perspective at wake-context assembly time.
    pub root_perspective_memory_id: MemoryId,
    /// The memory whose insertion triggered this wake.
    pub triggering_memory_id: MemoryId,
    /// Schema id of the triggering memory (e.g.
    /// `"proxima-code/commit-summary-v1"`). Phase 2 uses this with
    /// the `workspace_triggers` registry to resolve scope; Phase 1
    /// passes it through unused.
    pub triggering_memory_schema_id: &'a str,
    /// Provider-neutral capability allowlist for workspace-side
    /// tools. Phase 3 maps these to goose extension/tool names.
    pub workspace_tool_palette: &'a [String],
    /// Bytes of the bundled or user recipe selected by `recipe_ref`.
    pub recipe_bytes: &'a [u8],
    /// Pre-computed sha256 hex of `recipe_bytes`. Stored on the
    /// wake_invocation row.
    pub recipe_sha256: &'a str,
}

/// Everything the runner produces from `prepare` so the dispatcher
/// can invoke the adapter against the right cwd.
pub struct WorkspacePreparedRun {
    pub worktree_path: PathBuf,
    pub branch_name: String,
    pub parent_sha: String,
    /// On-disk path of the rendered effective recipe with workspace
    /// extensions injected. Distinct from the bundled recipe path.
    pub effective_recipe_path: PathBuf,
}

/// Adapter-level outcome the dispatcher hands back to `finalize`.
/// This is a subset of `TargetOutcome` — runners only need exit
/// shape, tails, and duration.
pub struct WorkspaceOutcome {
    pub exit_code: Option<i32>,
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub duration_ms: Option<u64>,
}

/// What `finalize` returns once the run Fact is on disk.
pub struct WorkspaceRunRecord {
    pub run_memory_id: MemoryId,
    pub head_sha: String,
}

#[async_trait::async_trait]
pub trait WorkspaceRunner: Send + Sync + std::fmt::Debug {
    async fn prepare(
        &self,
        input: WorkspacePrepareInput<'_>,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError>;

    async fn finalize(
        &self,
        prepared: WorkspacePreparedRun,
        outcome: WorkspaceOutcome,
    ) -> Result<WorkspaceRunRecord, WorkspaceRunnerError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke-test the error type compiles and renders as expected.
    /// More substantive tests live with the runner impl in Phase 3.
    #[test]
    fn unimplemented_error_renders() {
        let e = WorkspaceRunnerError::Unimplemented;
        assert_eq!(e.to_string(), "workspace runner not implemented for this flavor");
    }

    #[test]
    fn internal_error_renders() {
        let e = WorkspaceRunnerError::Internal("boom".into());
        assert_eq!(e.to_string(), "workspace runner internal error: boom");
    }
}
