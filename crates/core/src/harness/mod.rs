//! HarnessAdapter: the seam between `fire_wake_entry` and the
//! in-process LLM loop that owns model calls plus tool dispatch.
//!
//! This module defines trait and value types only. The concrete loop
//! driver, provider clients, and workspace tools live in `crates/harness/`.
//! Keeping the trait in `proxima-core` lets wake dispatch depend on
//! `&dyn HarnessAdapter` without pulling provider implementations into core.

pub mod outcome;
pub mod tool_projection;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use uuid::Uuid;

use crate::personality::PersonalityInstanceId;
use crate::{MemoryId, Owner};

pub use outcome::{
    ErrorClass, FinishReason, HarnessOutcome, HarnessOutcomeKind, classify_outcome, duration_ms,
};
pub use tool_projection::{
    HarnessToolDispatch, HarnessToolProjection, ToolProjectionError, build_wake_tool_projection,
};

/// Reinject fulfillment instructions every N unfulfilled rounds.
pub const FULFILLMENT_REMINDER_INTERVAL_ROUNDS: u32 = 4;
/// Stop run-until wakes after this many unfulfilled rounds.
pub const FULFILLMENT_STALL_ROUND_LIMIT: u32 = 16;
/// Stop after the same tool returns the same recoverable error this many times.
pub const TOOL_ERROR_STREAK_LIMIT: u32 = 3;

/// Everything the dispatcher hands the harness for one wake invocation.
#[derive(Debug, Clone)]
pub struct HarnessProgram {
    /// System-prompt body sourced from the firing Root Perspective.
    pub system_prompt: String,
    /// Per-wake instruction body sourced from `WakeEntry.instructions`.
    pub instructions: String,
    /// Rendered wake context: root perspective, active goals, trigger
    /// event, triggering memory, plus workspace context when applicable.
    pub context_params: HashMap<String, serde_json::Value>,
    /// Provider-facing tool projection derived from the wake's raw palette.
    pub tool_projection: Vec<HarnessToolProjection>,
    /// Explicit schema ids that satisfy run-until fulfillment. Empty means
    /// any producing tool in `tool_projection` can satisfy fulfillment.
    pub required_fulfillment_schema_ids: Vec<String>,
    /// Substrate + flavor tool ids from `WakeEntry.substrate_tool_palette`.
    pub substrate_tool_palette: Vec<String>,
    /// Workspace-mode jail root. `None` for substrate-only wakes.
    pub workspace_root: Option<PathBuf>,
    /// Workspace tool ids allowed for this wake. Empty means no workspace
    /// tools, even when a workspace root is present.
    pub workspace_tool_palette: Vec<String>,
    /// Per-wake observation-sandbox spec. `Some` puts the whole wake inside a
    /// disposable Docker container; `None` runs workspace tools on the host
    /// (the no-Docker dev escape hatch). Always `None` for substrate-only
    /// wakes — only set when `workspace_root` is `Some`.
    pub workspace_sandbox: Option<WorkspaceSandboxSpec>,
    /// Hard round cap. `0` means no model-imposed cap.
    pub max_rounds: u32,
    /// Resolved provider configuration.
    pub provider: ProviderTarget,
}

/// Per-wake observation-sandbox parameters.
///
/// The sandbox is an *observation instrument*, not an adversarial jail: it
/// gives the personality maximum freedom inside one disposable container,
/// contains the mess, and discards it. The container runs as the host
/// uid/gid so bind-mounted clone files stay host-owned and host-side
/// finalize operates on them without an ownership split.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceSandboxSpec {
    /// Sandbox container image (carries build/test tooling).
    pub image: String,
    /// Logging forward-proxy image for the per-wake egress network.
    pub proxy_image: String,
    /// Host uid the container runs as.
    pub uid: u32,
    /// Host gid the container runs as.
    pub gid: u32,
    /// Named volume mounted at `/cache` for persistent build caches.
    pub cache_volume: String,
    /// Container memory limit, e.g. `"4g"`. `None` leaves it unbounded.
    pub memory: Option<String>,
    /// Docker label `proxima.wake=<invocation_id>` for orphan reaping.
    pub label: String,
}

/// Resolved provider configuration after inference-target lookup.
#[derive(Debug, Clone)]
pub enum ProviderTarget {
    MistralChat {
        base_url: String,
        model_id: String,
        api_key: String,
        temperature: Option<f32>,
        max_completion_tokens: Option<u32>,
        reasoning_effort: Option<String>,
        context_window_tokens: Option<u32>,
    },
    OpenAIChat {
        base_url: String,
        model_id: String,
        api_key: String,
        temperature: Option<f32>,
        max_completion_tokens: Option<u32>,
        context_window_tokens: Option<u32>,
    },
    OpenAIResponses {
        base_url: String,
        model_id: String,
        api_key: String,
        reasoning_effort: Option<String>,
        context_window_tokens: Option<u32>,
    },
    ChatGPTCodex {
        base_url: String,
        model_id: String,
        reasoning_effort: Option<String>,
        context_window_tokens: Option<u32>,
        /// `~/.codex/auth.json` location. The client constructs a fresh
        /// `CodexAuthResolver` per `tool_round` and pays the cost of a
        /// JSON read; refresh remains stateful in the file itself.
        auth_json: std::path::PathBuf,
    },
}

/// Tool descriptor resolved in `crates/harness` through the substrate bridge.
#[derive(Clone)]
pub struct SubstrateToolBinding {
    pub canonical_name: String,
    pub description: String,
    pub args_schema: serde_json::Value,
}

impl std::fmt::Debug for SubstrateToolBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubstrateToolBinding")
            .field("canonical_name", &self.canonical_name)
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

/// Per-invocation context the harness needs for substrate dispatch and trace
/// persistence.
#[derive(Debug, Clone)]
pub struct HarnessContext {
    pub owner: Owner,
    pub invocation_id: Uuid,
    pub wake_entry_id: Uuid,
    pub personality_instance_id: PersonalityInstanceId,
    pub change_event_seq: Uuid,
    /// Calling personality's Root/Self Perspective. Substrate MCP authoring
    /// tools receive this as `McpToolCtx.caller_self_perspective`.
    pub root_perspective_memory_id: MemoryId,
    pub wake_token: Uuid,
    pub invocation_timeout: Duration,
}

/// Trait implemented by the concrete `HarnessLoop` in `crates/harness`.
#[async_trait]
pub trait HarnessAdapter: Send + Sync {
    async fn run(
        &self,
        program: HarnessProgram,
        ctx: HarnessContext,
    ) -> Result<HarnessOutcome, HarnessError>;
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("provider configuration invalid: {0}")]
    InvalidProvider(String),
    #[error("provider transport failed: {0}")]
    Transport(String),
    #[error("invocation timed out after {timeout:?}")]
    Timeout { timeout: Duration },
    #[error("internal: {0}")]
    Internal(String),
}
