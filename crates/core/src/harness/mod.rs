//! HarnessAdapter: the seam between `fire_wake_entry` and the
//! in-process LLM loop that owns model calls plus tool dispatch.
//!
//! This module defines trait and value types only. The concrete loop
//! driver, provider clients, and workspace tools live in `crates/harness/`.
//! Keeping the trait in `proxima-core` lets wake dispatch depend on
//! `&dyn HarnessAdapter` without pulling provider implementations into core.

pub mod outcome;

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
    /// Substrate + flavor tool ids from `WakeEntry.substrate_tool_palette`.
    pub substrate_tool_palette: Vec<String>,
    /// Workspace-mode jail root. `None` for substrate-only wakes.
    pub workspace_root: Option<PathBuf>,
    /// Hard round cap. `0` means no model-imposed cap.
    pub max_rounds: u32,
    /// Resolved provider configuration.
    pub provider: ProviderTarget,
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
    },
    OpenAIChat {
        base_url: String,
        model_id: String,
        api_key: String,
        temperature: Option<f32>,
        max_completion_tokens: Option<u32>,
    },
    OpenAIResponses {
        base_url: String,
        model_id: String,
        api_key: String,
        reasoning_effort: Option<String>,
    },
    ChatGPTCodex {
        base_url: String,
        model_id: String,
        reasoning_effort: Option<String>,
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
