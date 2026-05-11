//! Phase 1d: TargetAdapter trait — the seam between the wake dispatcher
//! and whatever runs the LLM loop. v1 ships [`LocalCliGooseAdapter`]
//! (subprocess `goose run --recipe ...`); a future RemoteModelAdapter
//! (Phase 2) implements this same trait without changing the dispatcher.

pub mod local_cli_goose;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

/// Everything the dispatcher hands an adapter for one wake invocation.
///
/// `params` are recipe parameters (the four-param wake context — trigger
/// event, triggering memory, root perspective, active goals — serialised
/// to JSON values). `env` carries the always-injected `PROXIMA_WAKE_TOKEN`
/// + `PROXIMA_MCP_URL` plus any target-resolved credentials.
#[derive(Debug, Clone)]
pub struct TargetInvocation {
    pub recipe_path: PathBuf,
    pub params: HashMap<String, serde_json::Value>,
    pub max_rounds: u32,
    pub env: HashMap<String, String>,
    pub timeout: Duration,
    /// Enables Goose's built-in developer extension through the CLI.
    ///
    /// Current Goose exposes write-capable workspace tools when the
    /// builtin is supplied with `--with-builtin developer`; declaring
    /// the same builtin only inside a rendered recipe is not sufficient.
    pub enable_developer_builtin: bool,
    /// Working directory for the subprocess. `None` keeps the
    /// adapter's default (inherited cwd). Workspace-mode wakes set
    /// this to the disposable worktree path; substrate-only wakes
    /// leave it `None`.
    pub cwd: Option<PathBuf>,
    /// Optional local JSONL artifact for the full worker session.
    /// The adapter treats logging as best-effort: failures to open or
    /// write this file must not fail the wake invocation.
    pub session_log_path: Option<PathBuf>,
    pub invocation_id: Option<uuid::Uuid>,
    pub personality_instance_id: Option<uuid::Uuid>,
    pub wake_entry_id: Option<uuid::Uuid>,
    pub change_event_seq: Option<uuid::Uuid>,
}

/// Adapter-classified outcome of a single wake.
///
/// `turn_count` is best-effort (parsed from the subprocess output where
/// available); stdout/stderr tails are bounded diagnostics.
#[derive(Debug, Clone)]
pub struct TargetOutcome {
    pub kind: TargetOutcomeKind,
    pub turn_count: Option<i32>,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub session_log_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOutcomeKind {
    Succeeded,
    Truncated,
    Failed,
}

#[derive(Debug, Error)]
pub enum TargetAdapterError {
    #[error("failed to spawn target binary: {source}")]
    SpawnFailed {
        #[source]
        source: std::io::Error,
    },
    #[error("subprocess timed out after {timeout:?}")]
    Timeout { timeout: Duration },
    #[error("subprocess io error: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
}

#[async_trait]
pub trait TargetAdapter: Send + Sync {
    async fn run(&self, invocation: TargetInvocation) -> Result<TargetOutcome, TargetAdapterError>;
}
