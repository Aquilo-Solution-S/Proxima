# Task 1.1 — Create `crates/core/src/harness/` module

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Create: `crates/core/src/harness/mod.rs`
- Create: `crates/core/src/harness/outcome.rs`
- Modify: `crates/core/src/lib.rs` (add `pub mod harness;`)

- [ ] **Step 1: Add module to `lib.rs`**

Read `crates/core/src/lib.rs`. Find the existing `pub mod inference;` line. Insert immediately after:

```rust
pub mod harness;
```

- [ ] **Step 2: Create `crates/core/src/harness/mod.rs`**

```rust
//! HarnessAdapter — the seam between `fire_wake_entry` and the
//! in-process LLM loop that owns model calls + tool dispatch.
//!
//! This module defines the **trait and value types only**. The
//! concrete loop driver, provider clients, and workspace tools live
//! in `crates/harness/`. Keeping the trait in `proxima-core` lets
//! `fire_wake_entry` depend on `&dyn HarnessAdapter` without
//! `proxima-core` pulling in network/runtime crates.
//!
//! See `docs/superpowers/specs/2026-05-12-proxima-harness-design.md`
//! §"Crate layout" and §"Core traits".

pub mod outcome;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use uuid::Uuid;

use crate::{MemoryId, Owner, personality::PersonalityInstanceId};

pub use outcome::{
    ErrorClass, FinishReason, HarnessOutcome, HarnessOutcomeKind, classify_outcome,
};

/// Everything the dispatcher hands the harness for one wake invocation.
///
/// Mirrors the previous `TargetInvocation` shape (see
/// `crates/core/src/wake/target_adapter/mod.rs`) but is typed for the
/// harness's needs: no `recipe_path`, `params` are the typed
/// four-param wake context, `tool_specs` are pre-resolved.
#[derive(Debug, Clone)]
pub struct HarnessProgram {
    /// System-prompt body — sourced from
    /// `RootPersonalityPerspectiveV1::system_prompt` at wake time.
    pub system_prompt: String,
    /// Per-wake instruction body — sourced from
    /// `WakeEntry.instructions` (Phase 6 adds the column).
    pub instructions: String,
    /// Rendered four-param context: keys
    /// `root_perspective` / `active_goals` / `trigger_event` /
    /// `triggering_memory`, plus `workspace_context` for
    /// workspace-mode wakes.
    pub context_params: HashMap<String, serde_json::Value>,
    /// Substrate + flavor tool ids from `WakeEntry.substrate_tool_palette`.
    /// The concrete harness resolves these through `HarnessSubstrateBridge`
    /// so discovery matches the live MCP surface: registry MCP tools plus
    /// wake-scoped personality substrate-pack tools.
    pub substrate_tool_palette: Vec<String>,
    /// `Some` when the wake is workspace-mode; the worktree path the
    /// workspace tools jail their cwd to.
    pub workspace_root: Option<PathBuf>,
    /// Hard upper bound on rounds. `0` means "no model-imposed cap";
    /// the harness still terminates on `finish_reason == "stop"`.
    pub max_rounds: u32,
    /// Inference-target-resolved provider configuration. The harness
    /// uses this to pick a `ProviderClient` impl.
    pub provider: ProviderTarget,
}

/// Resolved provider configuration after `InferenceTargetConfig`
/// lookup. Concrete HTTP clients live in `crates/harness/src/providers/`.
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
}

/// Tool descriptor resolved inside `crates/harness` from
/// `HarnessSubstrateBridge::list_harness_tools`.
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

/// Per-invocation context the harness needs to construct an
/// `McpToolCtx` for substrate dispatch and to write trace artifacts.
#[derive(Debug, Clone)]
pub struct HarnessContext {
    pub owner: Owner,
    pub invocation_id: Uuid,
    pub wake_entry_id: Uuid,
    pub personality_instance_id: PersonalityInstanceId,
    pub change_event_seq: Uuid,
    /// Calling personality's Root/Self Perspective. Substrate MCP
    /// authoring tools receive this as `McpToolCtx.caller_self_perspective`.
    pub root_perspective_memory_id: MemoryId,
    pub wake_token: Uuid,
    pub invocation_timeout: Duration,
}

/// Trait implemented by the concrete `HarnessLoop` in
/// `crates/harness/src/lib.rs`. `fire_wake_entry` consumes
/// `&dyn HarnessAdapter` — the same shape the prior `TargetAdapter`
/// trait used.
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
```

- [ ] **Step 3: Create `crates/core/src/harness/outcome.rs`**

```rust
//! Outcome classifier — derives `HarnessOutcomeKind` from explicit
//! signals (HTTP status, `finish_reason`, round counter, tool-dispatch
//! results, exception class). No regex, no string-matching of model
//! prose.
//!
//! See spec §"Outcome classification (exhaustive)" for the table this
//! function implements.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessOutcomeKind {
    Succeeded,
    Truncated,
    Failed,
}

/// What the provider told us the round terminated for.
/// Mirrors provider structural finish signals plus a few harness-owned
/// values for cases the provider doesn't model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    /// Model emitted a final assistant message with no tool calls.
    Stop,
    /// Model wants to call one or more tools.
    ToolCalls,
    /// Provider returned `length` (context or completion-token cap).
    Length,
    /// Harness ran out of `max_rounds` before model emitted `Stop`.
    MaxRounds,
    /// Provider returned a finish reason we don't recognise.
    Unknown(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    None,
    Auth,
    RateLimited,
    ContextLength,
    InvalidRequest,
    ServerError,
    Network,
    Timeout,
    Deserialize,
    ToolDispatchFatal,
}

#[derive(Debug, Clone)]
pub struct HarnessOutcome {
    pub kind: HarnessOutcomeKind,
    pub finish_reason: FinishReason,
    pub error_class: ErrorClass,
    pub failure_reason: Option<String>,
    pub rounds_used: u32,
    pub duration_ms: u64,
    pub total_prompt_tokens: Option<u64>,
    pub total_completion_tokens: Option<u64>,
    pub tool_call_count: u32,
    /// In-memory JSONL bytes (capped) — Phase 8 hands these to the
    /// CitedObject emitter.
    pub jsonl_bytes: Vec<u8>,
    pub jsonl_truncated: bool,
}

/// Classifier — exhaustive over (finish_reason, error_class). The
/// table is the source of truth in the spec; this function is the
/// code mirror.
#[must_use]
pub fn classify_outcome(
    finish_reason: FinishReason,
    error_class: ErrorClass,
    rounds_used: u32,
    max_rounds: u32,
) -> HarnessOutcomeKind {
    use ErrorClass as E;
    use FinishReason as F;
    match (finish_reason, error_class) {
        (_, E::Auth | E::InvalidRequest | E::Deserialize | E::ToolDispatchFatal) => {
            HarnessOutcomeKind::Failed
        }
        (_, E::Network | E::ServerError | E::Timeout) => HarnessOutcomeKind::Failed,
        (_, E::RateLimited) => HarnessOutcomeKind::Failed,
        (_, E::ContextLength) => HarnessOutcomeKind::Truncated,
        (F::Stop, E::None) => HarnessOutcomeKind::Succeeded,
        (F::ToolCalls, E::None) => {
            // Round emitted tool calls but the loop ended — this can
            // only happen when the harness exited mid-dispatch (caller
            // cancelled). Treat as failed.
            HarnessOutcomeKind::Failed
        }
        (F::Length, E::None) => HarnessOutcomeKind::Truncated,
        (F::MaxRounds, E::None) => {
            if max_rounds > 0 && rounds_used >= max_rounds {
                HarnessOutcomeKind::Truncated
            } else {
                HarnessOutcomeKind::Failed
            }
        }
        (F::Unknown(_), E::None) => HarnessOutcomeKind::Failed,
    }
}

#[must_use]
pub fn duration_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build -p proxima-core`
Expected: builds clean, no new warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/harness crates/core/src/lib.rs
git commit -m "$(cat <<'EOF'
core(harness): add HarnessAdapter trait + outcome classifier

Defines the seam between fire_wake_entry and the upcoming
crates/harness/ loop driver. Trait + value types only — no
network or runtime deps land in proxima-core.

Phase 1 of docs/superpowers/specs/2026-05-12-proxima-harness-design.md.
EOF
)"
```
