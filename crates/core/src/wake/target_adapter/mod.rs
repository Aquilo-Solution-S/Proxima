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
