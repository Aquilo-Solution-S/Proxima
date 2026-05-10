//! Flavor-supplied workspace runners.
//!
//! See `docs/superpowers/specs/2026-05-09-workspace-mode-design.md` for
//! the full design. Runners are registered per-flavor via the
//! `proxima_flavor!` macro and looked up by `flavor_id` in
//! `wake/fire.rs` when a workspace-mode wake fires.

use std::path::{Path, PathBuf};

use thiserror::Error;
use uuid::Uuid;

use crate::{MemoryId, Owner, RegisteredRelation};

/// Errors a runner can return from `prepare` or `finalize`.
#[derive(Debug, Error)]
pub enum WorkspaceRunnerError {
    #[error("workspace runner not implemented for this flavor")]
    Unimplemented,
    #[error("workspace trigger is not eligible for workspace mode: {0}")]
    TriggerNotEligible(String),
    #[error("workspace prepare failed: {0}")]
    PrepareFailed(String),
    #[error("workspace finalize failed: {0}")]
    FinalizeFailed(String),
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
    /// Substrate MCP URL the runner forwards to its inner subprocess
    /// (typically as `PROXIMA_MCP_URL`). Phase 1's wake/fire dispatch
    /// passes `""` when `engine.mcp_url()` is `None` because the
    /// Code-flavor stub returns `Unimplemented` regardless. Phase 3's
    /// real runners must validate this is non-empty before launching
    /// any subprocess that will call back into the substrate.
    pub mcp_url: &'a str,
    /// Snapshotted Root Perspective at wake-context assembly time.
    pub root_perspective_memory_id: MemoryId,
    /// The memory whose insertion triggered this wake.
    pub triggering_memory_id: MemoryId,
    /// Schema id of the triggering memory (e.g.
    /// `"proxima-code/commit-v1"`). Core treats it as an opaque
    /// flavor-qualified schema id.
    pub triggering_memory_schema_id: &'a str,
    /// Typed payload for the triggering memory. Core passes it through
    /// unchanged; the flavor runner interprets its own fields.
    pub triggering_memory_payload: &'a serde_json::Value,
    /// Provider-neutral capability allowlist for workspace-side
    /// tools. Phase 3 maps these to goose extension/tool names.
    pub workspace_tool_palette: &'a [String],
    pub effective_recipe_path: &'a Path,
    /// Bytes of the bundled or user recipe selected by `recipe_ref`.
    pub recipe_bytes: &'a [u8],
    /// Pre-computed sha256 hex of `recipe_bytes`. Stored on the
    /// wake_invocation row.
    pub recipe_sha256: &'a str,
}

/// Everything the runner produces from `prepare` so the dispatcher
/// can invoke the adapter against the right cwd.
pub struct WorkspacePreparedRun {
    pub work_dir: PathBuf,
    /// On-disk path of the rendered effective recipe with workspace
    /// extensions injected. Distinct from the bundled recipe path.
    pub effective_recipe_path: PathBuf,
    /// Runner-owned state. Core treats this as opaque and hands it
    /// back to the same runner during finalize.
    pub runner_state: serde_json::Value,
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
    pub primary_memory_id: Option<MemoryId>,
}

pub struct WorkspaceFinalizeInput<'a> {
    pub owner: &'a Owner,
    pub invocation_id: Uuid,
    pub root_perspective_memory_id: MemoryId,
    pub triggering_memory_id: MemoryId,
    pub authored_relation: RegisteredRelation<'a>,
    pub derived_from_relation: RegisteredRelation<'a>,
    pub prepared: WorkspacePreparedRun,
    pub outcome: WorkspaceOutcome,
}

#[async_trait::async_trait]
pub trait WorkspaceRunner: Send + Sync + std::fmt::Debug {
    async fn prepare(
        &self,
        input: WorkspacePrepareInput<'_>,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError>;

    async fn finalize(
        &self,
        input: WorkspaceFinalizeInput<'_>,
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
        assert_eq!(
            e.to_string(),
            "workspace runner not implemented for this flavor"
        );
    }

    #[test]
    fn internal_error_renders() {
        let e = WorkspaceRunnerError::Internal("boom".into());
        assert_eq!(e.to_string(), "workspace runner internal error: boom");
    }
}
