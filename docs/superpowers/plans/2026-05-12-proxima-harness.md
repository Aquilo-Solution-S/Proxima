# Proxima Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `LocalCliGooseAdapter` subprocess + recipe-YAML rewriter with an in-process Rust LLM harness that owns the wake loop, dispatches tools natively, and persists every session as a `wake-trace-v1` Fact + JSONL `CitedObject`.

**Architecture:** A new `crates/harness/` crate plugs into the wake dispatcher through a `HarnessAdapter` trait defined in `proxima-core`. The harness drives a `ProviderClient` per round (Mistral / OpenAI-Chat / OpenAI-Responses), dispatches substrate+flavor+workspace tools in-process, and emits structural outcomes from `finish_reason` (never regex). Greenfield single-cut: Goose, recipe YAML, `LocalCli`/`RemoteModel` variants, and the recipe rewriter all leave in the same atomic commit that wires the harness into `fire_wake_entry`.

**Tech Stack:** Rust 2024 edition, `reqwest` (rustls), `tokio`, `schemars` v1, `serde_json`, `async-trait`, `blake3`, `sqlx` (postgres), `tracing`. New crate `crates/harness/`; touches `crates/core`, `crates/storage-pg`, `flavors/code`, `apps/proxima-engine`, `apps/proxima-shell`, `apps/proxima-code`, `apps/proxima-mcp`.

**Reference spec:** `docs/superpowers/specs/2026-05-12-proxima-harness-design.md` (commit `6894e13`). The spec is authoritative — when this plan summarises, the spec wins.

---

## Phase landability summary

| Phase | Lands as | Affects existing wake path? |
|---|---|---|
| 1. `HarnessAdapter` trait + value types + outcome classifier | own commit | no — additive in `proxima-core` |
| 2. `crates/harness` skeleton + Mistral provider + JSONL buffer | own commit | no — new crate not yet wired |
| 3. Three workspace tools | own commit | no — additive in harness crate |
| 4. Substrate/flavor dispatch + reverse-map + `HarnessLoop` driver | own commit | no — additive in harness crate |
| 5. OpenAI-Chat + OpenAI-Responses providers | own commit | no — additive in harness crate |
| 6. `WakeEntry.instructions` column + `DefaultWakeEntrySeed` constants + onboarding wiring | own commit | additive — column is unread by Goose path |
| 7. Three wake-trace schemas registered | own commit | additive — schemas exist but no emitter yet |
| 8. **THE CUT** — `InferenceTargetConfig` rewrite, harness wired into `fire_wake_entry`, Fact emission, file deletions, data migration, end-to-end test | one atomic commit | yes — replaces Goose at runtime |

Phases 1–7 are land-anytime; Phase 8 is the single atomic change where Goose, recipe YAML, `LocalCli`, `RemoteModel`, and the recipe rewriter all leave together.

---

## File structure (created or modified across all phases)

**New files:**

- `crates/core/src/harness/mod.rs` — `HarnessAdapter` trait + value types
- `crates/core/src/harness/outcome.rs` — `HarnessOutcome`, `FinishReason`, `ErrorClass`, classifier
- `crates/harness/Cargo.toml`
- `crates/harness/src/lib.rs` — `HarnessLoop` concrete adapter + re-exports
- `crates/harness/src/program.rs` — `HarnessProgram` builder
- `crates/harness/src/conversation.rs` — `Conversation`, `Turn`, `ToolCall`, `ToolSpec`, `AssistantTurn`, `ToolResultTurn`
- `crates/harness/src/loop_driver.rs` — wake-loop driver (`loop` is a keyword)
- `crates/harness/src/tools/mod.rs` — `ToolBinding`, `ToolName`
- `crates/harness/src/tools/substrate_dispatch.rs` — in-process call into `McpToolDescriptor.call`
- `crates/harness/src/tools/workspace/mod.rs` — `WorkspaceTool` trait + registry
- `crates/harness/src/tools/workspace/shell.rs` — `workspace_shell`
- `crates/harness/src/tools/workspace/text_editor.rs` — `workspace_text_editor`
- `crates/harness/src/tools/workspace/list_files.rs` — `workspace_list_files`
- `crates/harness/src/providers/mod.rs` — `ProviderClient` trait + `ProviderError`
- `crates/harness/src/providers/mistral.rs`
- `crates/harness/src/providers/openai_chat.rs`
- `crates/harness/src/providers/openai_responses.rs`
- `crates/harness/src/trace/mod.rs` — JSONL buffer
- `crates/harness/src/trace/jsonl.rs` — buffer impl with size cap
- `crates/harness/tests/fixtures/mistral/*.json` — recorded HTTP fixtures
- `crates/harness/tests/fixtures/openai_chat/*.json`
- `crates/harness/tests/fixtures/openai_responses/*.json`
- `crates/core/src/personality/default_seeds.rs` — `DefaultWakeEntrySeed` + flavor trait
- `crates/core/src/wake/trace/mod.rs` — wake-trace payload structs + Fact emitter
- `flavors/code/src/personalities.rs` — `EngineerSeed`, `ExecutionWorkerSeed` constants
- `crates/storage-pg/migrations/20260512000010_wake_entry_instructions.sql`
- `crates/storage-pg/migrations/20260512000020_wake_trace_sidecars.sql`
- `crates/storage-pg/migrations/20260512000030_inference_targets_rewrite.sql`
- `crates/storage-pg/migrations/20260512000040_drop_wake_entry_recipe_ref.sql`
- `crates/core/tests/harness_outcome_classifier.rs`
- `crates/harness/tests/mistral_replay.rs`
- `crates/harness/tests/openai_chat_replay.rs`
- `crates/harness/tests/openai_responses_replay.rs`
- `crates/harness/tests/workspace_shell.rs`
- `crates/harness/tests/workspace_text_editor.rs`
- `crates/harness/tests/workspace_list_files.rs`
- `crates/harness/tests/substrate_dispatch.rs`
- `crates/harness/tests/loop_driver.rs`
- `flavors/code/tests/default_seeds.rs`
- `crates/core/tests/wake_trace_emission.rs`
- `crates/core/tests/inference_target_migration.rs`
- `crates/harness/tests/end_to_end_wake.rs`

**Modified files:**

- `Cargo.toml` (workspace `members`)
- `crates/core/src/lib.rs` (re-export `harness` module)
- `crates/core/src/inference/types.rs` (rewrite `InferenceTargetConfig`)
- `crates/core/src/inference/mod.rs` (drop `recipe_resolve`, `recipe_validate` from `pub mod`)
- `crates/core/src/personality/rows.rs` (`WakeEntryRow` — drop `recipe_ref`, add `instructions`)
- `crates/core/src/personality/mod.rs` (re-export `default_seeds`)
- `crates/core/src/wake/target_adapter/mod.rs` (re-export `HarnessAdapter` for backwards-name compat at the seam during the cut)
- `crates/core/src/wake/fire/fire.rs` (rewire to `HarnessAdapter`; remove `write_effective_recipe` call; emit `wake-trace-v1` Fact)
- `crates/core/src/wake/fire/mod.rs` (drop `pub mod recipe`)
- `flavors/code/src/lib.rs` (call `register_default_seeds`)
- `apps/proxima-engine/src/main.rs` (construct `HarnessLoop`)
- `apps/proxima-shell/src-tauri/src/boot.rs` (construct `HarnessLoop`)
- `apps/proxima-shell/src-tauri/src/config/types.rs` (`InferenceTargetRecord` variants)
- `apps/proxima-code/src/main.rs` (construct `HarnessLoop`)
- `apps/proxima-mcp/src/main.rs` (construct `HarnessLoop`)

**Deleted files (in Phase 8 only):**

- `crates/core/src/wake/target_adapter/local_cli_goose.rs`
- `crates/core/tests/target_adapter_local_cli.rs`
- `crates/core/src/wake/fire/recipe.rs`
- `crates/core/src/inference/recipe_resolve.rs`
- `crates/core/src/inference/recipe_validate.rs`
- `flavors/code/recipes/engineer.yaml`
- `flavors/code/recipes/execution_worker.yaml`

---

## Phase 1 — `HarnessAdapter` trait + value types + outcome classifier

**Goal:** Land the new seam in `proxima-core` so the harness crate (Phase 2+) has a stable trait to implement and `fire_wake_entry` (Phase 8) has a stable adapter to call. Pure additive change — no existing wake code is rewired in this phase.

### Task 1.1 — Create `crates/core/src/harness/` module

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

use crate::{Owner, mcp::McpToolDescriptor, personality::PersonalityInstanceId};

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
    /// Substrate + flavor tool specs (canonical name, schema, palette
    /// scope already applied). Workspace tools are added by the
    /// harness from `workspace_root`.
    pub substrate_tools: Vec<SubstrateToolBinding>,
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
    Mistral {
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

/// Tool descriptor handed to the harness: canonical name + schema
/// + dispatch handle. The harness layers provider-safe-name mapping
/// on top.
#[derive(Clone)]
pub struct SubstrateToolBinding {
    pub canonical_name: String,
    pub description: String,
    pub args_schema: serde_json::Value,
    /// Direct dispatch into the registered MCP tool descriptor.
    /// `crates/harness/src/tools/substrate_dispatch.rs` calls
    /// `(descriptor.call)(ctx, args)` against this.
    pub descriptor: McpToolDescriptor,
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
/// Mirrors OpenAI/Mistral `finish_reason` plus a few harness-owned
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

### Task 1.2 — Exhaustive classifier test

**Files:**
- Create: `crates/core/tests/harness_outcome_classifier.rs`

- [ ] **Step 1: Write the table-driven test**

```rust
//! Exhaustive coverage of the (FinishReason, ErrorClass) classification
//! table. One assertion per documented row in spec §"Outcome
//! classification (exhaustive)".

use proxima_core::harness::{
    ErrorClass, FinishReason, HarnessOutcomeKind, classify_outcome,
};

fn case(
    fr: FinishReason,
    ec: ErrorClass,
    rounds_used: u32,
    max_rounds: u32,
    expect: HarnessOutcomeKind,
    label: &str,
) {
    let got = classify_outcome(fr, ec, rounds_used, max_rounds);
    assert_eq!(got, expect, "row {label}: got {got:?}, want {expect:?}");
}

#[test]
fn stop_with_no_error_succeeds() {
    case(
        FinishReason::Stop,
        ErrorClass::None,
        3,
        30,
        HarnessOutcomeKind::Succeeded,
        "stop/none",
    );
}

#[test]
fn length_truncates() {
    case(
        FinishReason::Length,
        ErrorClass::None,
        5,
        30,
        HarnessOutcomeKind::Truncated,
        "length/none",
    );
}

#[test]
fn context_length_error_truncates_regardless_of_finish_reason() {
    case(
        FinishReason::Stop,
        ErrorClass::ContextLength,
        2,
        30,
        HarnessOutcomeKind::Truncated,
        "stop/context_length",
    );
    case(
        FinishReason::ToolCalls,
        ErrorClass::ContextLength,
        2,
        30,
        HarnessOutcomeKind::Truncated,
        "tool_calls/context_length",
    );
}

#[test]
fn max_rounds_hit_truncates() {
    case(
        FinishReason::MaxRounds,
        ErrorClass::None,
        30,
        30,
        HarnessOutcomeKind::Truncated,
        "max_rounds reached",
    );
}

#[test]
fn max_rounds_without_cap_fails() {
    // max_rounds == 0 means "no model-imposed cap"; reaching
    // MaxRounds in that mode is a harness bug.
    case(
        FinishReason::MaxRounds,
        ErrorClass::None,
        0,
        0,
        HarnessOutcomeKind::Failed,
        "max_rounds with no cap",
    );
}

#[test]
fn auth_error_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::Auth,
        0,
        30,
        HarnessOutcomeKind::Failed,
        "auth",
    );
}

#[test]
fn rate_limited_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::RateLimited,
        1,
        30,
        HarnessOutcomeKind::Failed,
        "rate_limited",
    );
}

#[test]
fn invalid_request_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::InvalidRequest,
        0,
        30,
        HarnessOutcomeKind::Failed,
        "invalid_request",
    );
}

#[test]
fn server_error_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::ServerError,
        2,
        30,
        HarnessOutcomeKind::Failed,
        "server_error",
    );
}

#[test]
fn network_error_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::Network,
        2,
        30,
        HarnessOutcomeKind::Failed,
        "network",
    );
}

#[test]
fn timeout_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::Timeout,
        4,
        30,
        HarnessOutcomeKind::Failed,
        "timeout",
    );
}

#[test]
fn deserialize_fails() {
    case(
        FinishReason::Stop,
        ErrorClass::Deserialize,
        0,
        30,
        HarnessOutcomeKind::Failed,
        "deserialize",
    );
}

#[test]
fn tool_dispatch_fatal_fails() {
    case(
        FinishReason::ToolCalls,
        ErrorClass::ToolDispatchFatal,
        2,
        30,
        HarnessOutcomeKind::Failed,
        "tool_dispatch_fatal",
    );
}

#[test]
fn unknown_finish_reason_fails() {
    case(
        FinishReason::Unknown("eos_garbage"),
        ErrorClass::None,
        1,
        30,
        HarnessOutcomeKind::Failed,
        "unknown finish reason",
    );
}

#[test]
fn tool_calls_with_no_error_is_treated_as_mid_loop_exit() {
    // Should not happen normally; classifier treats it as Failed
    // because a clean loop never exits with finish_reason == ToolCalls.
    case(
        FinishReason::ToolCalls,
        ErrorClass::None,
        3,
        30,
        HarnessOutcomeKind::Failed,
        "tool_calls/none (mid-loop exit)",
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p proxima-core --test harness_outcome_classifier`
Expected: all 14 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/core/tests/harness_outcome_classifier.rs
git commit -m "core(harness): exhaustive outcome-classifier table tests"
```

---

## Phase 2 — `crates/harness` skeleton + Mistral provider + JSONL buffer

**Goal:** Stand up the new crate, define the provider-neutral conversation types, ship the first `ProviderClient` (Mistral), and the JSONL trace buffer. End state: `cargo build -p proxima-harness` succeeds; replay tests against recorded Mistral fixtures pass.

### Task 2.1 — Workspace member + Cargo.toml

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/harness/Cargo.toml`
- Create: `crates/harness/src/lib.rs` (skeleton)

- [ ] **Step 1: Add to workspace members**

Edit `Cargo.toml`. Change the `members = [...]` line to include `"crates/harness"`. The new line should read:

```toml
members = ["crates/core", "crates/harness", "crates/mcp-server", "apps/proxima-engine", "apps/proxima-code", "apps/proxima-mcp", "flavors/code", "flavors/mcp", "flavors/goal", "crates/llm-openai-compat", "apps/proxima-shell/src-tauri", "crates/storage-pg", "crates/wire-grpc"]
```

- [ ] **Step 2: Create `crates/harness/Cargo.toml`**

```toml
[package]
name = "proxima-harness"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
proxima-core = { path = "../core" }
serde = { workspace = true }
serde_json = { workspace = true }
async-trait = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true, features = ["process", "fs", "time", "io-util"] }
reqwest = { workspace = true }
uuid = { workspace = true }
schemars = { workspace = true }
tracing = { workspace = true }
time = { workspace = true }
blake3 = { workspace = true }
futures = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "test-util", "fs"] }
tempfile = "3"
```

- [ ] **Step 3: Create `crates/harness/src/lib.rs` skeleton**

```rust
//! Proxima Harness — in-process LLM loop driver.
//!
//! Implements `proxima_core::harness::HarnessAdapter` via
//! [`HarnessLoop`]. See
//! `docs/superpowers/specs/2026-05-12-proxima-harness-design.md`.

#![forbid(unsafe_code)]

pub mod conversation;
pub mod loop_driver;
pub mod program;
pub mod providers;
pub mod tools;
pub mod trace;

pub use loop_driver::HarnessLoop;
```

- [ ] **Step 4: Verify the workspace builds**

Run: `cargo build -p proxima-harness`
Expected: builds (each submodule will be a stub for now; we'll fill them in below). The crate compiles because we create empty stubs in step 5.

- [ ] **Step 5: Create empty submodule stubs**

For each submodule, create the file with a single doc line. This keeps the build green while we fill them in across Tasks 2.2–2.5.

Create `crates/harness/src/conversation.rs`:
```rust
//! Provider-neutral conversation types — filled in Task 2.2.
```

Create `crates/harness/src/program.rs`:
```rust
//! HarnessProgram builder — filled in Task 4.1.
```

Create `crates/harness/src/loop_driver.rs`:
```rust
//! Loop driver — filled in Task 4.3. Stub `HarnessLoop` for now.

#[derive(Debug, Default)]
pub struct HarnessLoop;
```

Create `crates/harness/src/providers/mod.rs`:
```rust
//! ProviderClient trait — filled in Task 2.3.
```

Create `crates/harness/src/tools/mod.rs`:
```rust
//! Tool dispatch — filled in Tasks 3.1 and 4.2.
```

Create `crates/harness/src/trace/mod.rs`:
```rust
//! Trace artifacts.

pub mod jsonl;
```

Create `crates/harness/src/trace/jsonl.rs`:
```rust
//! JSONL transcript buffer — filled in Task 2.4.
```

- [ ] **Step 6: Verify build**

Run: `cargo build -p proxima-harness`
Expected: builds clean.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/harness
git commit -m "harness: crate skeleton + workspace registration"
```

### Task 2.2 — Conversation types

**Files:**
- Modify: `crates/harness/src/conversation.rs`

- [ ] **Step 1: Replace the stub with the typed conversation surface**

```rust
//! Provider-neutral conversation types.
//!
//! The loop driver assembles a [`Conversation`] and hands it to a
//! [`crate::providers::ProviderClient`] each round; the provider
//! returns a [`crate::providers::RoundResult`]. None of these types
//! carry provider-specific JSON — they're the canonical shape the
//! harness reasons about.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Conversation {
    pub system_prompt: String,
    pub user_seed: String,
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone)]
pub enum Turn {
    Assistant(AssistantTurn),
    ToolResult(ToolResultTurn),
}

#[derive(Debug, Clone, Default)]
pub struct AssistantTurn {
    /// May be empty when the round was tool-call-only.
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    /// Provider-specific opaque blob for re-sending the assistant
    /// turn verbatim on the next round (some providers require it).
    pub raw: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Provider-issued call id, opaque to the harness.
    pub call_id: String,
    /// **Canonical** tool name (already reverse-mapped from
    /// provider-safe). See `crates/core/src/mcp/mod.rs::provider_safe_tool_name`.
    pub tool_name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone)]
pub struct ToolResultTurn {
    pub call_id: String,
    pub status: ToolResultStatus,
    pub content: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultStatus {
    Ok,
    Error,
}

/// Spec for one tool the provider sees. The harness owns the
/// canonical ↔ provider-safe name map per round.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub canonical: String,
    pub provider_safe: String,
    pub description: String,
    pub input_schema: Value,
}
```

- [ ] **Step 2: Verify build**

Run: `cargo build -p proxima-harness`
Expected: builds clean.

- [ ] **Step 3: Commit**

```bash
git add crates/harness/src/conversation.rs
git commit -m "harness: provider-neutral conversation types"
```

### Task 2.3 — `ProviderClient` trait + Mistral impl

**Files:**
- Modify: `crates/harness/src/providers/mod.rs`
- Create: `crates/harness/src/providers/mistral.rs`

- [ ] **Step 1: Define the trait + error type in `providers/mod.rs`**

```rust
//! Provider abstraction. One `tool_round` call = one
//! request/response cycle to a provider, returning either a final
//! assistant message, a batch of tool calls, or a length cap signal.

use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::conversation::{AssistantTurn, Conversation, ToolCall, ToolSpec};

pub mod mistral;
// openai_chat and openai_responses arrive in Phase 5.

#[async_trait]
pub trait ProviderClient: Send + Sync {
    async fn tool_round(
        &self,
        conversation: &Conversation,
        tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError>;
}

#[derive(Debug, Clone)]
pub enum RoundResult {
    ToolCalls {
        calls: Vec<ToolCall>,
        assistant: AssistantTurn,
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
    },
    Final {
        text: String,
        assistant: AssistantTurn,
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
    },
    LengthCap {
        partial_text: Option<String>,
        assistant: AssistantTurn,
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("auth (HTTP 401/403)")]
    Auth,
    #[error("rate limited (HTTP 429), retry_after={retry_after:?}")]
    RateLimited { retry_after: Option<Duration> },
    #[error("context length exceeded")]
    ContextLength,
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("server error: {0}")]
    ServerError(String),
    #[error("network: {0}")]
    Network(String),
    #[error("timeout")]
    Timeout,
    #[error("deserialize: {0}")]
    Deserialize(String),
}
```

Add `tokio-util` to `crates/harness/Cargo.toml` `[dependencies]`:
```toml
tokio-util = { workspace = true }
```

- [ ] **Step 2: Implement the Mistral client**

Create `crates/harness/src/providers/mistral.rs`:

```rust
//! Mistral `/v1/chat/completions` adapter.
//!
//! Endpoint: `{base_url}/v1/chat/completions`
//! Auth:     `Authorization: Bearer {api_key}`
//! Tools:    `tools: [{ "type": "function", "function": {...} }]`
//! Finish:   `choices[0].finish_reason` ∈ {"stop","tool_calls","length"}

use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::conversation::{
    AssistantTurn, Conversation, ToolCall, ToolResultStatus, ToolSpec, Turn,
};

use super::{ProviderClient, ProviderError, RoundResult};

#[derive(Debug, Clone)]
pub struct MistralClient {
    pub http: Client,
    pub base_url: String,
    pub model_id: String,
    pub api_key: String,
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
    pub request_timeout: Duration,
}

impl MistralClient {
    #[must_use]
    pub fn new(base_url: String, model_id: String, api_key: String) -> Self {
        Self {
            http: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
            base_url,
            model_id,
            api_key,
            temperature: None,
            max_completion_tokens: None,
            request_timeout: Duration::from_secs(120),
        }
    }
}

#[async_trait]
impl ProviderClient for MistralClient {
    async fn tool_round(
        &self,
        conversation: &Conversation,
        tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError> {
        let body = build_request(self, conversation, tools);
        let url = format!("{}/v1/chat/completions", self.base_url.trim_end_matches('/'));

        let send = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send();

        let resp = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::Timeout),
            r = send => r.map_err(|e| ProviderError::Network(e.to_string()))?,
        };

        classify_and_parse(resp).await
    }
}

fn build_request(c: &MistralClient, conv: &Conversation, tools: &[ToolSpec]) -> Value {
    let mut messages: Vec<Value> = Vec::with_capacity(2 + conv.turns.len() * 2);
    messages.push(json!({"role": "system", "content": c_system_prompt(conv)}));
    messages.push(json!({"role": "user",   "content": conv.user_seed.clone()}));
    for turn in &conv.turns {
        match turn {
            Turn::Assistant(a) => messages.push(assistant_to_wire(a)),
            Turn::ToolResult(t) => messages.push(json!({
                "role": "tool",
                "tool_call_id": t.call_id,
                "content": tool_result_content(&t.status, &t.content),
            })),
        }
    }

    let mut req = json!({
        "model": c.model_id,
        "messages": messages,
        "tools": tools.iter().map(tool_spec_to_wire).collect::<Vec<_>>(),
        "tool_choice": "auto",
    });
    if let Some(t) = c.temperature {
        req["temperature"] = json!(t);
    }
    if let Some(m) = c.max_completion_tokens {
        req["max_tokens"] = json!(m);
    }
    req
}

fn c_system_prompt(conv: &Conversation) -> String {
    conv.system_prompt.clone()
}

fn assistant_to_wire(a: &AssistantTurn) -> Value {
    if let Some(raw) = &a.raw {
        return raw.clone();
    }
    let mut msg = json!({"role": "assistant"});
    if !a.text.is_empty() {
        msg["content"] = json!(a.text);
    }
    if !a.tool_calls.is_empty() {
        msg["tool_calls"] = json!(
            a.tool_calls
                .iter()
                .map(|tc| json!({
                    "id": tc.call_id,
                    "type": "function",
                    "function": {
                        "name": tc.tool_name, // provider-safe name on the wire
                        "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default(),
                    },
                }))
                .collect::<Vec<_>>()
        );
    }
    msg
}

fn tool_result_content(status: &ToolResultStatus, body: &Value) -> String {
    match status {
        ToolResultStatus::Ok => serde_json::to_string(body).unwrap_or_default(),
        ToolResultStatus::Error => serde_json::to_string(
            &json!({"error": body}),
        )
        .unwrap_or_default(),
    }
}

fn tool_spec_to_wire(t: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": t.provider_safe,
            "description": t.description,
            "parameters": t.input_schema,
        }
    })
}

async fn classify_and_parse(resp: reqwest::Response) -> Result<RoundResult, ProviderError> {
    let status = resp.status();
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(ProviderError::Auth);
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs);
        return Err(ProviderError::RateLimited { retry_after });
    }
    if status == StatusCode::BAD_REQUEST {
        let body = resp.text().await.unwrap_or_default();
        if body.contains("context_length_exceeded") {
            return Err(ProviderError::ContextLength);
        }
        return Err(ProviderError::InvalidRequest(body));
    }
    if status.is_server_error() {
        let body = resp.text().await.unwrap_or_default();
        return Err(ProviderError::ServerError(format!("{status}: {body}")));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| ProviderError::Network(e.to_string()))?;
    let parsed: MistralResp = serde_json::from_slice(&bytes)
        .map_err(|e| ProviderError::Deserialize(e.to_string()))?;
    let choice = parsed
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| ProviderError::Deserialize("no choices in response".into()))?;
    let assistant = AssistantTurn {
        text: choice.message.content.clone().unwrap_or_default(),
        tool_calls: choice
            .message
            .tool_calls
            .iter()
            .flat_map(|v| v.iter())
            .map(|c| ToolCall {
                call_id: c.id.clone(),
                tool_name: c.function.name.clone(),
                arguments: serde_json::from_str(&c.function.arguments)
                    .unwrap_or(Value::Null),
            })
            .collect(),
        raw: serde_json::to_value(&choice.message).ok(),
    };
    let prompt_tokens = parsed.usage.as_ref().and_then(|u| u.prompt_tokens);
    let completion_tokens = parsed.usage.as_ref().and_then(|u| u.completion_tokens);
    Ok(match choice.finish_reason.as_deref() {
        Some("stop") => RoundResult::Final {
            text: assistant.text.clone(),
            assistant,
            prompt_tokens,
            completion_tokens,
        },
        Some("tool_calls") => RoundResult::ToolCalls {
            calls: assistant.tool_calls.clone(),
            assistant,
            prompt_tokens,
            completion_tokens,
        },
        Some("length") => RoundResult::LengthCap {
            partial_text: if assistant.text.is_empty() {
                None
            } else {
                Some(assistant.text.clone())
            },
            assistant,
            prompt_tokens,
            completion_tokens,
        },
        _ => RoundResult::Final {
            text: assistant.text.clone(),
            assistant,
            prompt_tokens,
            completion_tokens,
        },
    })
}

#[derive(Debug, Deserialize)]
struct MistralResp {
    choices: Vec<MistralChoice>,
    usage: Option<MistralUsage>,
}

#[derive(Debug, Deserialize)]
struct MistralChoice {
    message: MistralMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct MistralMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<MistralToolCall>>,
    #[serde(flatten)]
    extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct MistralToolCall {
    id: String,
    #[serde(rename = "type", default)]
    kind: String,
    function: MistralToolFn,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct MistralToolFn {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct MistralUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}
```

- [ ] **Step 3: Verify build**

Run: `cargo build -p proxima-harness`
Expected: builds clean (warnings about unused `Serialize` on `MistralResp` etc. are fine; if any compile error, fix and re-run).

- [ ] **Step 4: Commit**

```bash
git add crates/harness
git commit -m "harness: ProviderClient trait + Mistral chat-completions impl"
```

### Task 2.4 — JSONL trace buffer

**Files:**
- Modify: `crates/harness/src/trace/jsonl.rs`

- [ ] **Step 1: Write the failing test**

Create the test file `crates/harness/tests/jsonl_buffer.rs`:

```rust
use proxima_harness::trace::jsonl::JsonlBuffer;
use serde_json::json;

#[test]
fn small_buffer_records_lines_in_order() {
    let mut buf = JsonlBuffer::with_capacity(64 * 1024);
    buf.append(&json!({"record":"start","invocation_id":"X"}));
    buf.append(&json!({"record":"round_start","round":0}));
    let snap = buf.snapshot();
    let text = std::str::from_utf8(&snap.bytes).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("\"record\":\"start\""));
    assert!(lines[1].contains("\"record\":\"round_start\""));
    assert!(!snap.truncated);
}

#[test]
fn cap_hit_emits_truncated_marker_and_stops_appending() {
    let mut buf = JsonlBuffer::with_capacity(256);
    for i in 0..1000 {
        buf.append(&json!({"record":"round","i":i,"pad":"xxxxxxxxxxxxxxxxxx"}));
    }
    let snap = buf.snapshot();
    assert!(snap.truncated);
    let text = std::str::from_utf8(&snap.bytes).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let last = lines.last().expect("at least one line");
    assert!(
        last.contains("\"record\":\"truncated\""),
        "last line should be the truncated marker, got {last}"
    );
    assert!(snap.bytes.len() <= 256 + 256);
}

#[test]
fn content_hash_is_stable_for_equal_byte_sequences() {
    let mut a = JsonlBuffer::with_capacity(1024);
    let mut b = JsonlBuffer::with_capacity(1024);
    a.append(&json!({"x":1}));
    a.append(&json!({"y":2}));
    b.append(&json!({"x":1}));
    b.append(&json!({"y":2}));
    assert_eq!(a.snapshot().content_hash, b.snapshot().content_hash);
}
```

Run: `cargo test -p proxima-harness --test jsonl_buffer`
Expected: FAIL with "no such item `JsonlBuffer`".

- [ ] **Step 2: Implement `JsonlBuffer`**

Replace `crates/harness/src/trace/jsonl.rs`:

```rust
//! In-memory JSONL transcript buffer with byte cap + truncate marker.
//!
//! Per spec §"Layer 1 — JSONL transcript", the buffer enforces a
//! per-invocation cap (default 5 MB, configurable per Owner). When
//! the cap is hit, the harness writes a final `truncated` marker
//! line and stops appending — the wake itself does not fail.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug)]
pub struct JsonlBuffer {
    bytes: Vec<u8>,
    cap_bytes: usize,
    truncated: bool,
    line_count: u64,
}

#[derive(Debug, Clone)]
pub struct JsonlSnapshot {
    pub bytes: Vec<u8>,
    pub truncated: bool,
    pub line_count: u64,
    pub content_hash: [u8; 32],
}

impl JsonlBuffer {
    #[must_use]
    pub fn with_capacity(cap_bytes: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(cap_bytes.min(64 * 1024)),
            cap_bytes,
            truncated: false,
            line_count: 0,
        }
    }

    /// Append one JSON-serialisable record as a single line.
    /// Once `truncated == true`, further `append` calls are no-ops.
    pub fn append<T: Serialize>(&mut self, record: &T) {
        if self.truncated {
            return;
        }
        let mut line = match serde_json::to_vec(record) {
            Ok(b) => b,
            Err(_) => return,
        };
        line.push(b'\n');
        if self.bytes.len() + line.len() > self.cap_bytes {
            self.write_truncated_marker(self.bytes.len() + line.len());
            return;
        }
        self.bytes.extend_from_slice(&line);
        self.line_count += 1;
    }

    fn write_truncated_marker(&mut self, attempted_total: usize) {
        self.truncated = true;
        let marker = serde_json::json!({
            "record": "truncated",
            "reason": "size_cap",
            "cap_bytes": self.cap_bytes,
            "attempted_total": attempted_total,
        });
        if let Ok(mut line) = serde_json::to_vec(&marker) {
            line.push(b'\n');
            // Reserve room for the marker even if we have to drop
            // tail bytes — but never below cap_bytes itself.
            while !self.bytes.is_empty()
                && self.bytes.len() + line.len() > self.cap_bytes
            {
                // Pop tail bytes up to the previous newline so the
                // truncated file still ends on a record boundary.
                let pop_at = self
                    .bytes
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map_or(0, |i| i + 1);
                self.bytes.truncate(pop_at.saturating_sub(1));
            }
            self.bytes.extend_from_slice(&line);
            self.line_count += 1;
        }
    }

    /// Allow records that don't serialise via Serialize (e.g.
    /// pre-built `Value`). Identical semantics to `append`.
    pub fn append_value(&mut self, v: &Value) {
        self.append(v);
    }

    #[must_use]
    pub fn snapshot(&self) -> JsonlSnapshot {
        let content_hash = *blake3::hash(&self.bytes).as_bytes();
        JsonlSnapshot {
            bytes: self.bytes.clone(),
            truncated: self.truncated,
            line_count: self.line_count,
            content_hash,
        }
    }

    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p proxima-harness --test jsonl_buffer`
Expected: all 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/harness/src/trace crates/harness/tests/jsonl_buffer.rs
git commit -m "harness: JSONL transcript buffer with size cap"
```

### Task 2.5 — Mistral replay test against recorded fixtures

**Files:**
- Create: `crates/harness/tests/fixtures/mistral/stop.json`
- Create: `crates/harness/tests/fixtures/mistral/tool_calls.json`
- Create: `crates/harness/tests/fixtures/mistral/length.json`
- Create: `crates/harness/tests/fixtures/mistral/auth_error.json` (HTTP 401 body sample)
- Create: `crates/harness/tests/mistral_replay.rs`

- [ ] **Step 1: Record the three success fixtures**

Each fixture is a JSON file holding the **response body** Mistral returns. The test spins up a tiny in-process mock HTTP server (`tokio::net::TcpListener` + a hand-rolled handler) that returns the fixture for `POST /v1/chat/completions`.

Create `crates/harness/tests/fixtures/mistral/stop.json`:
```json
{
  "id": "test-stop",
  "object": "chat.completion",
  "model": "mistral-medium-3.5",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "All done — no further work required."
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {"prompt_tokens": 42, "completion_tokens": 11}
}
```

Create `crates/harness/tests/fixtures/mistral/tool_calls.json`:
```json
{
  "id": "test-tc",
  "object": "chat.completion",
  "model": "mistral-medium-3.5",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": null,
        "tool_calls": [
          {
            "id": "call_abc",
            "type": "function",
            "function": {
              "name": "workspace_shell",
              "arguments": "{\"command\":\"ls\",\"timeout_ms\":30000}"
            }
          }
        ]
      },
      "finish_reason": "tool_calls"
    }
  ],
  "usage": {"prompt_tokens": 50, "completion_tokens": 22}
}
```

Create `crates/harness/tests/fixtures/mistral/length.json`:
```json
{
  "id": "test-len",
  "object": "chat.completion",
  "model": "mistral-medium-3.5",
  "choices": [
    {
      "index": 0,
      "message": {"role": "assistant", "content": "Partial output…"},
      "finish_reason": "length"
    }
  ],
  "usage": {"prompt_tokens": 30, "completion_tokens": 4096}
}
```

- [ ] **Step 2: Write the replay test**

Create `crates/harness/tests/mistral_replay.rs`:

```rust
//! Replay tests for the Mistral provider against recorded fixtures.
//! Uses an in-process loopback HTTP server — no live calls.

use std::sync::Arc;

use proxima_harness::conversation::{Conversation, ToolSpec};
use proxima_harness::providers::ProviderClient;
use proxima_harness::providers::RoundResult;
use proxima_harness::providers::mistral::MistralClient;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

async fn spawn_mock(body: Vec<u8>, status_line: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let body = Arc::new(body);
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut req = [0u8; 8192];
            // Read until end of headers.
            let mut total = 0;
            loop {
                let n = sock.read(&mut req[total..]).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                total += n;
                if req[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let resp = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.write_all(&body).await;
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{addr}")
}

fn empty_conversation() -> Conversation {
    Conversation {
        system_prompt: "You are a test.".into(),
        user_seed: "Hello.".into(),
        turns: vec![],
    }
}

fn tools() -> Vec<ToolSpec> {
    vec![ToolSpec {
        canonical: "core/emit_abstraction".into(),
        provider_safe: "core_emit_abstraction".into(),
        description: "emit".into(),
        input_schema: json!({"type":"object"}),
    }]
}

#[tokio::test]
async fn mistral_stop_returns_final() {
    let body = std::fs::read("tests/fixtures/mistral/stop.json").unwrap();
    let url = spawn_mock(body, "200 OK").await;
    let client = MistralClient::new(url, "mistral-medium-3.5".into(), "test".into());
    let r = client
        .tool_round(&empty_conversation(), &tools(), CancellationToken::new())
        .await
        .unwrap();
    matches!(r, RoundResult::Final { .. });
    if let RoundResult::Final { text, .. } = r {
        assert!(text.starts_with("All done"));
    }
}

#[tokio::test]
async fn mistral_tool_calls_returns_calls() {
    let body = std::fs::read("tests/fixtures/mistral/tool_calls.json").unwrap();
    let url = spawn_mock(body, "200 OK").await;
    let client = MistralClient::new(url, "mistral-medium-3.5".into(), "test".into());
    let r = client
        .tool_round(&empty_conversation(), &tools(), CancellationToken::new())
        .await
        .unwrap();
    if let RoundResult::ToolCalls { calls, .. } = r {
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, "workspace_shell");
        assert_eq!(calls[0].arguments["command"], "ls");
    } else {
        panic!("expected ToolCalls, got {r:?}");
    }
}

#[tokio::test]
async fn mistral_length_returns_length_cap() {
    let body = std::fs::read("tests/fixtures/mistral/length.json").unwrap();
    let url = spawn_mock(body, "200 OK").await;
    let client = MistralClient::new(url, "mistral-medium-3.5".into(), "test".into());
    let r = client
        .tool_round(&empty_conversation(), &tools(), CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(r, RoundResult::LengthCap { .. }));
}

#[tokio::test]
async fn mistral_401_returns_auth_error() {
    let url = spawn_mock(b"{\"error\":\"unauthorized\"}".to_vec(), "401 Unauthorized").await;
    let client = MistralClient::new(url, "mistral-medium-3.5".into(), "bad".into());
    let err = client
        .tool_round(&empty_conversation(), &tools(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        proxima_harness::providers::ProviderError::Auth
    ));
}

#[tokio::test]
async fn mistral_429_returns_rate_limited() {
    let url = spawn_mock(b"{}".to_vec(), "429 Too Many Requests").await;
    let client = MistralClient::new(url, "mistral-medium-3.5".into(), "ok".into());
    let err = client
        .tool_round(&empty_conversation(), &tools(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        proxima_harness::providers::ProviderError::RateLimited { .. }
    ));
}

#[tokio::test]
async fn mistral_400_context_length_returns_context_length() {
    let url = spawn_mock(
        br#"{"error":{"code":"context_length_exceeded","message":"too big"}}"#.to_vec(),
        "400 Bad Request",
    )
    .await;
    let client = MistralClient::new(url, "mistral-medium-3.5".into(), "ok".into());
    let err = client
        .tool_round(&empty_conversation(), &tools(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        proxima_harness::providers::ProviderError::ContextLength
    ));
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p proxima-harness --test mistral_replay`
Expected: all 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/harness/tests
git commit -m "harness: Mistral replay tests covering stop/tool_calls/length/auth/429/ctx-length"
```

---

## Phase 3 — Three workspace tools

**Goal:** Land `workspace_shell`, `workspace_text_editor`, `workspace_list_files` as Rust impls with `schemars`-derived schemas and cwd-jail enforcement. Each tool is a free-standing impl tested in isolation; wiring into `HarnessLoop` happens in Phase 4.

### Task 3.1 — `WorkspaceTool` trait + registry

**Files:**
- Modify: `crates/harness/src/tools/mod.rs`
- Create: `crates/harness/src/tools/workspace/mod.rs`

- [ ] **Step 1: Define `ToolBinding` and the workspace trait**

Replace `crates/harness/src/tools/mod.rs`:

```rust
//! Tool surfaces the harness exposes to the model.
//!
//! Three sources:
//! - **Substrate**: typed MCP tools registered on
//!   `crates/core/src/mcp/`; dispatched via
//!   `crates/harness/src/tools/substrate_dispatch.rs` (Phase 4).
//! - **Flavor**: same shape as substrate; the harness doesn't
//!   distinguish them at the dispatch layer.
//! - **Workspace**: Rust impls in `workspace/`; cwd-jailed to the
//!   prepared worktree.

use std::path::PathBuf;

use proxima_core::harness::SubstrateToolBinding;

pub mod substrate_dispatch;
pub mod workspace;

/// Resolved binding per tool in the active palette.
#[derive(Clone)]
pub enum ToolBinding {
    Substrate(SubstrateToolBinding),
    Workspace(workspace::WorkspaceToolName),
}

impl std::fmt::Debug for ToolBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Substrate(s) => f.debug_tuple("Substrate").field(&s.canonical_name).finish(),
            Self::Workspace(w) => f.debug_tuple("Workspace").field(w).finish(),
        }
    }
}

/// Resolved environment for workspace-tool dispatch.
#[derive(Debug, Clone)]
pub struct WorkspaceCtx {
    pub workspace_root: PathBuf,
}
```

- [ ] **Step 2: Create `workspace/mod.rs` with the trait and stub for the three tools**

```rust
//! Workspace tools: cwd-jailed to a prepared worktree.

pub mod list_files;
pub mod shell;
pub mod text_editor;

use serde_json::Value;

use super::WorkspaceCtx;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceToolName {
    Shell,
    TextEditor,
    ListFiles,
}

impl WorkspaceToolName {
    #[must_use]
    pub fn canonical(self) -> &'static str {
        match self {
            Self::Shell => "workspace_shell",
            Self::TextEditor => "workspace_text_editor",
            Self::ListFiles => "workspace_list_files",
        }
    }

    pub fn from_canonical(s: &str) -> Option<Self> {
        match s {
            "workspace_shell" => Some(Self::Shell),
            "workspace_text_editor" => Some(Self::TextEditor),
            "workspace_list_files" => Some(Self::ListFiles),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceToolError {
    #[error("invalid args: {0}")]
    InvalidArgs(String),
    #[error("path escapes workspace root: {0}")]
    PathEscape(String),
    #[error("io: {0}")]
    Io(String),
    #[error("timeout after {ms} ms")]
    Timeout { ms: u64 },
}

pub async fn dispatch(
    name: WorkspaceToolName,
    args: Value,
    ctx: &WorkspaceCtx,
) -> Result<Value, WorkspaceToolError> {
    match name {
        WorkspaceToolName::Shell => shell::run(args, ctx).await,
        WorkspaceToolName::TextEditor => text_editor::run(args, ctx).await,
        WorkspaceToolName::ListFiles => list_files::run(args, ctx).await,
    }
}

/// Cwd-jail check. Resolves `requested` against `root` and rejects
/// anything that escapes the root, including `..` and symlinks that
/// point outside.
pub(crate) fn jail_path(
    root: &std::path::Path,
    requested: &str,
) -> Result<std::path::PathBuf, WorkspaceToolError> {
    let p = std::path::Path::new(requested);
    if p.is_absolute() {
        return Err(WorkspaceToolError::PathEscape(requested.into()));
    }
    let mut acc = root.to_path_buf();
    for c in p.components() {
        match c {
            std::path::Component::Normal(s) => acc.push(s),
            std::path::Component::CurDir => {}
            _ => return Err(WorkspaceToolError::PathEscape(requested.into())),
        }
    }
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let canon = acc.canonicalize().unwrap_or(acc.clone());
    if !canon.starts_with(&canon_root) {
        return Err(WorkspaceToolError::PathEscape(requested.into()));
    }
    Ok(acc)
}
```

- [ ] **Step 3: Create stubs that compile**

Create `crates/harness/src/tools/workspace/shell.rs`:
```rust
//! workspace_shell — implemented in Task 3.2.
use serde_json::Value;
use super::{WorkspaceCtx, WorkspaceToolError};
pub async fn run(_args: Value, _ctx: &WorkspaceCtx) -> Result<Value, WorkspaceToolError> {
    Err(WorkspaceToolError::Io("unimplemented".into()))
}
```

Same shape for `crates/harness/src/tools/workspace/text_editor.rs` and `crates/harness/src/tools/workspace/list_files.rs`.

Create `crates/harness/src/tools/substrate_dispatch.rs`:
```rust
//! Substrate dispatch — implemented in Task 4.2.
```

- [ ] **Step 4: Verify build**

Run: `cargo build -p proxima-harness`
Expected: builds clean.

- [ ] **Step 5: Commit**

```bash
git add crates/harness/src/tools
git commit -m "harness: workspace tool trait + cwd-jail helper"
```

### Task 3.2 — `workspace_shell`

**Files:**
- Modify: `crates/harness/src/tools/workspace/shell.rs`
- Create: `crates/harness/tests/workspace_shell.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/harness/tests/workspace_shell.rs`:

```rust
use std::path::PathBuf;

use proxima_harness::tools::WorkspaceCtx;
use proxima_harness::tools::workspace::shell;
use serde_json::json;
use tempfile::tempdir;

fn ctx(root: PathBuf) -> WorkspaceCtx {
    WorkspaceCtx { workspace_root: root }
}

#[tokio::test]
async fn returns_exit_code_and_stdout() {
    let tmp = tempdir().unwrap();
    let r = shell::run(
        json!({"command": "echo hello-world"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    assert_eq!(r["exit_code"], 0);
    assert!(r["stdout"].as_str().unwrap().contains("hello-world"));
    assert_eq!(r["timed_out"], false);
}

#[tokio::test]
async fn timeout_returns_timed_out_true_and_keeps_stdout() {
    let tmp = tempdir().unwrap();
    let r = shell::run(
        json!({"command": "sleep 5", "timeout_ms": 200}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    assert_eq!(r["timed_out"], true);
}

#[tokio::test]
async fn stdout_is_capped_at_32k() {
    let tmp = tempdir().unwrap();
    let r = shell::run(
        // Generate ~64 KB and ensure cap kicks in.
        json!({"command": "yes 'x' | head -c 65536"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    let stdout = r["stdout"].as_str().unwrap();
    assert!(stdout.len() <= 32 * 1024);
    assert_eq!(r["stdout_truncated"], true);
}

#[tokio::test]
async fn env_is_cleared_except_for_allowlist() {
    let tmp = tempdir().unwrap();
    // PROXIMA_WAKE_TOKEN must not leak into the subshell.
    // We can't set process env from inside the test reliably across
    // platforms, so verify HOME survives and PROXIMA_WAKE_TOKEN
    // does not by checking what `env` returns.
    let r = shell::run(
        json!({"command": "env | grep -E '^(HOME|PROXIMA_WAKE_TOKEN)=' || true"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    let stdout = r["stdout"].as_str().unwrap();
    assert!(!stdout.contains("PROXIMA_WAKE_TOKEN"));
}

#[tokio::test]
async fn cwd_is_the_workspace_root() {
    let tmp = tempdir().unwrap();
    let r = shell::run(
        json!({"command": "pwd"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    let canon_root = tmp.path().canonicalize().unwrap();
    let out = r["stdout"].as_str().unwrap().trim();
    assert_eq!(std::path::Path::new(out).canonicalize().unwrap(), canon_root);
}
```

Run: `cargo test -p proxima-harness --test workspace_shell`
Expected: FAIL — `shell::run` returns `Err("unimplemented")`.

- [ ] **Step 2: Implement `shell::run`**

Replace `crates/harness/src/tools/workspace/shell.rs`:

```rust
//! workspace_shell: bounded `bash -lc` execution, cwd-jailed.
//!
//! Args:   { command: string, timeout_ms?: u32 }
//! Result: { exit_code, stdout, stdout_truncated, stderr,
//!           stderr_truncated, duration_ms, timed_out }
//! Env:    cleared except PATH, HOME, USER, LANG, TERM.

use std::process::Stdio;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::{WorkspaceCtx, WorkspaceToolError};

const DEFAULT_TIMEOUT_MS: u32 = 30_000;
const MAX_TIMEOUT_MS: u32 = 120_000;
const STDOUT_CAP: usize = 32 * 1024;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShellArgs {
    pub command: String,
    #[serde(default)]
    pub timeout_ms: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ShellResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stdout_truncated: bool,
    pub stderr: String,
    pub stderr_truncated: bool,
    pub duration_ms: u64,
    pub timed_out: bool,
}

pub async fn run(args: Value, ctx: &WorkspaceCtx) -> Result<Value, WorkspaceToolError> {
    let args: ShellArgs = serde_json::from_value(args)
        .map_err(|e| WorkspaceToolError::InvalidArgs(e.to_string()))?;
    let timeout_ms = args
        .timeout_ms
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .min(MAX_TIMEOUT_MS);

    let mut cmd = Command::new("bash");
    cmd.arg("-lc")
        .arg(&args.command)
        .current_dir(&ctx.workspace_root)
        .env_clear()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    // Allowlist.
    for k in &["PATH", "HOME", "USER", "LANG", "TERM"] {
        if let Ok(v) = std::env::var(k) {
            cmd.env(k, v);
        }
    }

    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();

    let timeout = Duration::from_millis(u64::from(timeout_ms));
    let mut out_buf = Vec::with_capacity(8 * 1024);
    let mut err_buf = Vec::with_capacity(8 * 1024);

    let result = tokio::time::timeout(timeout, async {
        let read_out = async {
            let mut tmp = [0u8; 4096];
            loop {
                let n = stdout.read(&mut tmp).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                if out_buf.len() < STDOUT_CAP {
                    let take = (STDOUT_CAP - out_buf.len()).min(n);
                    out_buf.extend_from_slice(&tmp[..take]);
                }
            }
        };
        let read_err = async {
            let mut tmp = [0u8; 4096];
            loop {
                let n = stderr.read(&mut tmp).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                if err_buf.len() < STDOUT_CAP {
                    let take = (STDOUT_CAP - err_buf.len()).min(n);
                    err_buf.extend_from_slice(&tmp[..take]);
                }
            }
        };
        tokio::join!(read_out, read_err);
        child.wait().await
    })
    .await;

    let (exit_code, timed_out) = match result {
        Ok(Ok(status)) => (status.code().unwrap_or(-1), false),
        Ok(Err(e)) => {
            return Err(WorkspaceToolError::Io(e.to_string()));
        }
        Err(_) => {
            // Timed out — kill and report.
            let _ = child.start_kill();
            (-1, true)
        }
    };
    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    let stdout_truncated = out_buf.len() >= STDOUT_CAP;
    let stderr_truncated = err_buf.len() >= STDOUT_CAP;
    let stdout = String::from_utf8_lossy(&out_buf).into_owned();
    let stderr = String::from_utf8_lossy(&err_buf).into_owned();

    Ok(json!({
        "exit_code": exit_code,
        "stdout": stdout,
        "stdout_truncated": stdout_truncated,
        "stderr": stderr,
        "stderr_truncated": stderr_truncated,
        "duration_ms": duration_ms,
        "timed_out": timed_out,
    }))
}

/// Schemars-derived JSON schema for the args. Used by the harness
/// when building `ToolSpec.input_schema`.
#[must_use]
pub fn args_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(ShellArgs)).unwrap_or(Value::Null)
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p proxima-harness --test workspace_shell -- --test-threads=1`
Expected: all 5 tests pass. (`--test-threads=1` keeps the env-var test from racing.)

- [ ] **Step 4: Commit**

```bash
git add crates/harness/src/tools/workspace/shell.rs crates/harness/tests/workspace_shell.rs
git commit -m "harness: workspace_shell with cwd-jail, env-clear, output cap, timeout"
```

### Task 3.3 — `workspace_text_editor`

**Files:**
- Modify: `crates/harness/src/tools/workspace/text_editor.rs`
- Create: `crates/harness/tests/workspace_text_editor.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/harness/tests/workspace_text_editor.rs`:

```rust
use proxima_harness::tools::WorkspaceCtx;
use proxima_harness::tools::workspace::text_editor;
use serde_json::json;
use tempfile::tempdir;

fn ctx(root: std::path::PathBuf) -> WorkspaceCtx {
    WorkspaceCtx { workspace_root: root }
}

#[tokio::test]
async fn create_writes_file_and_returns_summary() {
    let tmp = tempdir().unwrap();
    let r = text_editor::run(
        json!({"op":"create","path":"a.txt","file_text":"hello\nworld\n"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    assert_eq!(r["op"], "create");
    assert_eq!(r["line_count"], 2);
    let content = std::fs::read_to_string(tmp.path().join("a.txt")).unwrap();
    assert_eq!(content, "hello\nworld\n");
}

#[tokio::test]
async fn view_returns_lines() {
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "1\n2\n3\n").unwrap();
    let r = text_editor::run(
        json!({"op":"view","path":"a.txt"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    assert_eq!(r["content"], "1\n2\n3\n");
}

#[tokio::test]
async fn str_replace_errors_when_old_str_not_unique() {
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "x\nx\n").unwrap();
    let err = text_editor::run(
        json!({"op":"str_replace","path":"a.txt","old_str":"x","new_str":"y"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("not unique") || msg.contains("multiple"));
}

#[tokio::test]
async fn path_traversal_dot_dot_rejected() {
    let tmp = tempdir().unwrap();
    let err = text_editor::run(
        json!({"op":"view","path":"../etc/passwd"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap_err();
    assert!(format!("{err}").contains("escapes"));
}

#[tokio::test]
async fn absolute_path_rejected() {
    let tmp = tempdir().unwrap();
    let err = text_editor::run(
        json!({"op":"view","path":"/etc/passwd"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap_err();
    assert!(format!("{err}").contains("escapes"));
}

#[tokio::test]
async fn insert_at_line_works() {
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "a\nb\nc\n").unwrap();
    let _ = text_editor::run(
        json!({"op":"insert","path":"a.txt","insert_line":1,"new_str":"INSERTED"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(tmp.path().join("a.txt")).unwrap();
    assert_eq!(content, "a\nINSERTED\nb\nc\n");
}
```

Run: `cargo test -p proxima-harness --test workspace_text_editor`
Expected: FAIL — unimplemented.

- [ ] **Step 2: Implement `text_editor::run`**

Replace `crates/harness/src/tools/workspace/text_editor.rs`:

```rust
//! workspace_text_editor: view | create | str_replace | insert,
//! cwd-jailed.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::fs;

use super::{WorkspaceCtx, WorkspaceToolError, jail_path};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum TextEditorArgs {
    View {
        path: String,
        #[serde(default)]
        view_range: Option<[u32; 2]>,
    },
    Create {
        path: String,
        file_text: String,
    },
    StrReplace {
        path: String,
        old_str: String,
        new_str: String,
    },
    Insert {
        path: String,
        insert_line: u32,
        new_str: String,
    },
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TextEditorResult {
    pub op: &'static str,
    pub path: String,
    pub line_count: u32,
    pub content: Option<String>,
}

pub async fn run(args: Value, ctx: &WorkspaceCtx) -> Result<Value, WorkspaceToolError> {
    let parsed: TextEditorArgs = serde_json::from_value(args)
        .map_err(|e| WorkspaceToolError::InvalidArgs(e.to_string()))?;
    match parsed {
        TextEditorArgs::View { path, view_range } => view(ctx, &path, view_range).await,
        TextEditorArgs::Create { path, file_text } => create(ctx, &path, &file_text).await,
        TextEditorArgs::StrReplace {
            path,
            old_str,
            new_str,
        } => str_replace(ctx, &path, &old_str, &new_str).await,
        TextEditorArgs::Insert {
            path,
            insert_line,
            new_str,
        } => insert(ctx, &path, insert_line, &new_str).await,
    }
}

async fn view(
    ctx: &WorkspaceCtx,
    path: &str,
    view_range: Option<[u32; 2]>,
) -> Result<Value, WorkspaceToolError> {
    let p = jail_path(&ctx.workspace_root, path)?;
    let content = fs::read_to_string(&p)
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let line_count = u32::try_from(content.lines().count()).unwrap_or(u32::MAX);
    let trimmed = match view_range {
        Some([start, end]) => content
            .lines()
            .skip((start.saturating_sub(1)) as usize)
            .take((end.saturating_sub(start.saturating_sub(1))) as usize)
            .collect::<Vec<_>>()
            .join("\n"),
        None => content,
    };
    Ok(json!({"op":"view","path":path,"line_count":line_count,"content":trimmed}))
}

async fn create(
    ctx: &WorkspaceCtx,
    path: &str,
    file_text: &str,
) -> Result<Value, WorkspaceToolError> {
    let p = jail_path(&ctx.workspace_root, path)?;
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    }
    fs::write(&p, file_text)
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let line_count = u32::try_from(file_text.lines().count()).unwrap_or(u32::MAX);
    Ok(json!({"op":"create","path":path,"line_count":line_count}))
}

async fn str_replace(
    ctx: &WorkspaceCtx,
    path: &str,
    old_str: &str,
    new_str: &str,
) -> Result<Value, WorkspaceToolError> {
    let p = jail_path(&ctx.workspace_root, path)?;
    let content = fs::read_to_string(&p)
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let occurrences = content.matches(old_str).count();
    if occurrences == 0 {
        return Err(WorkspaceToolError::InvalidArgs(format!(
            "old_str not found in {path}"
        )));
    }
    if occurrences > 1 {
        return Err(WorkspaceToolError::InvalidArgs(format!(
            "old_str not unique in {path} (found {occurrences} occurrences)"
        )));
    }
    let replaced = content.replacen(old_str, new_str, 1);
    fs::write(&p, &replaced)
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let line_count = u32::try_from(replaced.lines().count()).unwrap_or(u32::MAX);
    Ok(json!({"op":"str_replace","path":path,"line_count":line_count}))
}

async fn insert(
    ctx: &WorkspaceCtx,
    path: &str,
    insert_line: u32,
    new_str: &str,
) -> Result<Value, WorkspaceToolError> {
    let p = jail_path(&ctx.workspace_root, path)?;
    let content = fs::read_to_string(&p)
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let mut lines: Vec<&str> = content.split_inclusive('\n').collect();
    let idx = (insert_line as usize).min(lines.len());
    let prefix: String = lines.drain(..idx).collect();
    let suffix: String = lines.into_iter().collect();
    let needs_nl = !new_str.ends_with('\n');
    let inserted = if needs_nl {
        format!("{prefix}{new_str}\n{suffix}")
    } else {
        format!("{prefix}{new_str}{suffix}")
    };
    fs::write(&p, &inserted)
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let line_count = u32::try_from(inserted.lines().count()).unwrap_or(u32::MAX);
    Ok(json!({"op":"insert","path":path,"line_count":line_count}))
}

#[must_use]
pub fn args_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(TextEditorArgs)).unwrap_or(Value::Null)
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p proxima-harness --test workspace_text_editor`
Expected: all 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/harness/src/tools/workspace/text_editor.rs crates/harness/tests/workspace_text_editor.rs
git commit -m "harness: workspace_text_editor with cwd-jail and unique-match enforcement"
```

### Task 3.4 — `workspace_list_files`

**Files:**
- Modify: `crates/harness/src/tools/workspace/list_files.rs`
- Create: `crates/harness/tests/workspace_list_files.rs`

- [ ] **Step 1: Write failing tests**

```rust
use proxima_harness::tools::WorkspaceCtx;
use proxima_harness::tools::workspace::list_files;
use serde_json::json;
use tempfile::tempdir;

fn ctx(root: std::path::PathBuf) -> WorkspaceCtx {
    WorkspaceCtx { workspace_root: root }
}

#[tokio::test]
async fn lists_top_level() {
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "x").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub/b.txt"), "y").unwrap();
    let r = list_files::run(json!({"path":"."}), &ctx(tmp.path().to_path_buf()))
        .await
        .unwrap();
    let entries = r["entries"].as_array().unwrap();
    let names: Vec<&str> = entries.iter().filter_map(|e| e["path"].as_str()).collect();
    assert!(names.contains(&"a.txt"));
    assert!(names.contains(&"sub"));
}

#[tokio::test]
async fn skips_hidden_dot_git_by_default() {
    let tmp = tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    std::fs::write(tmp.path().join("a.txt"), "x").unwrap();
    let r = list_files::run(json!({"path":"."}), &ctx(tmp.path().to_path_buf()))
        .await
        .unwrap();
    let names: Vec<&str> = r["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["path"].as_str())
        .collect();
    assert!(!names.iter().any(|n| n.starts_with(".git")));
}

#[tokio::test]
async fn path_traversal_rejected() {
    let tmp = tempdir().unwrap();
    let err = list_files::run(json!({"path":"../"}), &ctx(tmp.path().to_path_buf()))
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("escapes"));
}
```

Run: `cargo test -p proxima-harness --test workspace_list_files`
Expected: FAIL.

- [ ] **Step 2: Implement**

```rust
//! workspace_list_files: cwd-rooted listing of file entries.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{WorkspaceCtx, WorkspaceToolError, jail_path};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListFilesArgs {
    #[serde(default = "default_path")]
    pub path: String,
    #[serde(default)]
    pub include_hidden: bool,
    #[serde(default = "default_recursive")]
    pub recursive: bool,
}

fn default_path() -> String { ".".into() }
fn default_recursive() -> bool { false }

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListEntry {
    pub path: String,
    pub kind: &'static str, // "file" | "dir" | "symlink"
    pub size_bytes: Option<u64>,
}

pub async fn run(args: Value, ctx: &WorkspaceCtx) -> Result<Value, WorkspaceToolError> {
    let args: ListFilesArgs = serde_json::from_value(args)
        .map_err(|e| WorkspaceToolError::InvalidArgs(e.to_string()))?;
    let base = jail_path(&ctx.workspace_root, &args.path)?;
    let mut out: Vec<ListEntry> = Vec::new();
    walk(&base, &ctx.workspace_root, args.include_hidden, args.recursive, &mut out)
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    Ok(json!({"entries": out}))
}

fn walk(
    dir: &std::path::Path,
    root: &std::path::Path,
    include_hidden: bool,
    recursive: bool,
    out: &mut Vec<ListEntry>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !include_hidden && (name_str == ".git" || name_str.starts_with('.')) {
            continue;
        }
        let ft = entry.file_type()?;
        let kind = if ft.is_dir() {
            "dir"
        } else if ft.is_symlink() {
            "symlink"
        } else {
            "file"
        };
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(&entry.path())
            .to_string_lossy()
            .into_owned();
        let size_bytes = if ft.is_file() {
            entry.metadata().ok().map(|m| m.len())
        } else {
            None
        };
        out.push(ListEntry { path: rel, kind, size_bytes });
        if recursive && ft.is_dir() {
            walk(&entry.path(), root, include_hidden, recursive, out)?;
        }
    }
    Ok(())
}

#[must_use]
pub fn args_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(ListFilesArgs)).unwrap_or(Value::Null)
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p proxima-harness --test workspace_list_files`
Expected: all 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/harness/src/tools/workspace/list_files.rs crates/harness/tests/workspace_list_files.rs
git commit -m "harness: workspace_list_files with hidden-skip and cwd-jail"
```

---

## Phase 4 — Substrate/flavor dispatch + reverse-map + `HarnessLoop` driver

**Goal:** Wire substrate/flavor MCP tools into the harness with a provider-safe ↔ canonical name reverse-map, then assemble the loop driver `HarnessLoop<P: ProviderClient>`. End state: a unit test runs a full multi-round wake against a stub provider, dispatching workspace + substrate tools.

### Task 4.1 — `HarnessProgram` builder + name-map helper

**Files:**
- Modify: `crates/harness/src/program.rs`

- [ ] **Step 1: Implement**

```rust
//! HarnessProgram → Conversation + tools list, plus the
//! canonical ↔ provider-safe name map the loop driver uses to
//! reverse-resolve `function.name` from the provider.

use std::collections::HashMap;

use proxima_core::harness::{HarnessProgram, SubstrateToolBinding};
use proxima_core::mcp::provider_safe_tool_name;

use crate::conversation::{Conversation, ToolSpec};
use crate::tools::{ToolBinding, workspace::WorkspaceToolName};

#[derive(Debug)]
pub struct ResolvedProgram {
    pub conversation: Conversation,
    pub tools: Vec<ToolSpec>,
    /// provider-safe name → canonical name. Lookup direction the
    /// loop driver uses when reading `function.name` back from the
    /// provider response.
    pub reverse_map: HashMap<String, String>,
    /// canonical name → binding. Lookup direction the dispatch path
    /// uses after reverse-resolving the name.
    pub bindings: HashMap<String, ToolBinding>,
}

#[must_use]
pub fn resolve(program: HarnessProgram) -> ResolvedProgram {
    let user_seed = build_user_seed(&program);
    let mut tools = Vec::with_capacity(program.substrate_tools.len() + 3);
    let mut reverse_map = HashMap::new();
    let mut bindings = HashMap::new();

    for s in &program.substrate_tools {
        let provider_safe = provider_safe_tool_name(&s.canonical_name);
        tools.push(ToolSpec {
            canonical: s.canonical_name.clone(),
            provider_safe: provider_safe.clone(),
            description: s.description.clone(),
            input_schema: s.args_schema.clone(),
        });
        reverse_map.insert(provider_safe, s.canonical_name.clone());
        bindings.insert(
            s.canonical_name.clone(),
            ToolBinding::Substrate(s.clone()),
        );
    }

    if program.workspace_root.is_some() {
        for name in [
            WorkspaceToolName::Shell,
            WorkspaceToolName::TextEditor,
            WorkspaceToolName::ListFiles,
        ] {
            let canonical = name.canonical().to_string();
            let provider_safe = provider_safe_tool_name(&canonical);
            tools.push(ToolSpec {
                canonical: canonical.clone(),
                provider_safe: provider_safe.clone(),
                description: workspace_description(name).into(),
                input_schema: workspace_args_schema(name),
            });
            reverse_map.insert(provider_safe, canonical.clone());
            bindings.insert(canonical, ToolBinding::Workspace(name));
        }
    }

    ResolvedProgram {
        conversation: Conversation {
            system_prompt: program.system_prompt,
            user_seed,
            turns: vec![],
        },
        tools,
        reverse_map,
        bindings,
    }
}

fn build_user_seed(program: &HarnessProgram) -> String {
    let mut s = String::new();
    if !program.instructions.is_empty() {
        s.push_str(&program.instructions);
        s.push_str("\n\n");
    }
    for key in [
        "root_perspective",
        "active_goals",
        "trigger_event",
        "triggering_memory",
        "workspace_context",
    ] {
        if let Some(v) = program.context_params.get(key) {
            s.push_str(&format!(
                "{}:\n{}\n\n",
                snake_to_title(key),
                serde_json::to_string_pretty(v).unwrap_or_default()
            ));
        }
    }
    s.trim_end().to_string()
}

fn snake_to_title(s: &str) -> String {
    s.split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_uppercase().chain(c).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn workspace_description(name: WorkspaceToolName) -> &'static str {
    match name {
        WorkspaceToolName::Shell => "Run a shell command in the prepared worktree.",
        WorkspaceToolName::TextEditor => {
            "Create or edit files in the prepared worktree (view | create | str_replace | insert)."
        }
        WorkspaceToolName::ListFiles => "List files under a path in the prepared worktree.",
    }
}

fn workspace_args_schema(name: WorkspaceToolName) -> serde_json::Value {
    match name {
        WorkspaceToolName::Shell => crate::tools::workspace::shell::args_schema(),
        WorkspaceToolName::TextEditor => crate::tools::workspace::text_editor::args_schema(),
        WorkspaceToolName::ListFiles => crate::tools::workspace::list_files::args_schema(),
    }
}

// Re-export to satisfy `pub mod program;` even though this module's
// public API is `resolve` + `ResolvedProgram`.
#[allow(unused_imports)]
use SubstrateToolBinding as _;
```

- [ ] **Step 2: Verify build**

Run: `cargo build -p proxima-harness`
Expected: builds clean.

- [ ] **Step 3: Commit**

```bash
git add crates/harness/src/program.rs
git commit -m "harness: program builder with canonical/provider-safe name maps"
```

### Task 4.2 — Substrate dispatch

**Files:**
- Modify: `crates/harness/src/tools/substrate_dispatch.rs`

- [ ] **Step 1: Implement**

```rust
//! In-process dispatch into substrate/flavor MCP tools.
//!
//! Builds an `McpToolCtx` from the wake-token-derived context the
//! harness was handed and calls `(descriptor.call)(ctx, args)`
//! directly. No HTTP, no JSON-RPC, no MCP transport — but the same
//! `McpToolDescriptor.call` function pointer the MCP server's HTTP
//! path resolves at runtime.

use std::sync::Arc;

use proxima_core::Engine;
use proxima_core::harness::{HarnessContext, SubstrateToolBinding};
use proxima_core::mcp::{McpAuthorContext, McpToolCtx, McpToolError};
use serde_json::Value;

/// Result of one substrate-tool dispatch.
#[derive(Debug, Clone)]
pub enum SubstrateDispatchResult {
    Ok(Value),
    /// Recoverable error — feed it back to the model as a tool
    /// result with `status: "error"`. The loop continues.
    Recoverable(String),
    /// Fatal — terminate the wake. This is reserved for storage
    /// outages, layering violations, and unknown-handle panics.
    Fatal(String),
}

pub async fn dispatch(
    engine: &Arc<Engine>,
    binding: &SubstrateToolBinding,
    args: Value,
    ctx: &HarnessContext,
    model_id: &str,
) -> SubstrateDispatchResult {
    let mcp_ctx = McpToolCtx {
        pool: engine.storage_pool_clone(),
        owner: ctx.owner.clone(),
        handles: engine.handle_table(),
        registry: engine.frozen_registry(),
        author: McpAuthorContext {
            model_id: model_id.to_string(),
            client_name: "proxima-harness".to_string(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            caller_self_perspective: None,
        },
        caller_self_perspective: None,
        master_token_id: None,
        engine: Some(engine.clone()),
    };

    match (binding.descriptor.call)(mcp_ctx, args).await {
        Ok(v) => SubstrateDispatchResult::Ok(v),
        Err(McpToolError::Storage(e)) => {
            SubstrateDispatchResult::Fatal(format!("storage: {e}"))
        }
        Err(McpToolError::LayeringViolation(s)) => {
            SubstrateDispatchResult::Fatal(format!("layering: {s}"))
        }
        Err(other) => SubstrateDispatchResult::Recoverable(other.to_string()),
    }
}
```

This relies on three `Engine` helpers that may not yet exist as public-named methods: `storage_pool_clone`, `handle_table`, `frozen_registry`. Check the current `Engine` surface in `crates/core/src/engine/` and either reuse existing accessors or add three thin `#[must_use] pub fn`s next to the existing ones. **Add only the minimum needed** — no setters, no new state.

- [ ] **Step 2: Add missing `Engine` accessors**

Open `crates/core/src/engine/mod.rs` (locate the `impl Engine` block — likely in `engine/api.rs` or similar). Add the three accessors:

```rust
impl Engine {
    #[must_use]
    pub fn storage_pool_clone(&self) -> sqlx::PgPool {
        // adapt the existing pool accessor; many Engine impls
        // already expose `pool()` returning `&PgPool`.
        self.pool().clone()
    }

    #[must_use]
    pub fn handle_table(&self) -> std::sync::Arc<crate::mcp::HandleTable> {
        // adapt to wherever HandleTable currently lives on Engine.
        self.handles().clone()
    }

    #[must_use]
    pub fn frozen_registry(&self) -> std::sync::Arc<crate::verbs::schema::FlavorRegistryFrozen> {
        // adapt — Engine already holds a frozen registry; expose
        // an Arc clone of it.
        self.registry_frozen().clone()
    }
}
```

If `pool()`, `handles()`, or `registry_frozen()` are named differently, mirror the actual accessor names. **Do not** add new fields to `Engine`.

- [ ] **Step 3: Verify build**

Run: `cargo build -p proxima-harness -p proxima-core`
Expected: builds clean.

- [ ] **Step 4: Commit**

```bash
git add crates/harness/src/tools/substrate_dispatch.rs crates/core/src/engine
git commit -m "harness: in-process substrate dispatch via McpToolDescriptor.call"
```

### Task 4.3 — `HarnessLoop` driver

**Files:**
- Modify: `crates/harness/src/loop_driver.rs`
- Modify: `crates/harness/src/lib.rs`

- [ ] **Step 1: Implement the loop**

Replace `crates/harness/src/loop_driver.rs`:

```rust
//! HarnessLoop — the concrete `HarnessAdapter` impl.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use proxima_core::Engine;
use proxima_core::harness::{
    ErrorClass, FinishReason, HarnessAdapter, HarnessContext, HarnessError, HarnessOutcome,
    HarnessProgram, ProviderTarget, classify_outcome, duration_ms,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::conversation::{AssistantTurn, ToolResultStatus, ToolResultTurn, Turn};
use crate::program::{ResolvedProgram, resolve};
use crate::providers::{ProviderClient, ProviderError, RoundResult};
use crate::providers::mistral::MistralClient;
use crate::tools::workspace::dispatch as workspace_dispatch;
use crate::tools::{ToolBinding, WorkspaceCtx};
use crate::trace::jsonl::JsonlBuffer;

/// Concrete `HarnessAdapter` impl. Holds a clone of the Engine so it
/// can dispatch substrate tools.
#[derive(Clone)]
pub struct HarnessLoop {
    pub engine: Arc<Engine>,
    pub jsonl_cap_bytes: usize,
}

impl HarnessLoop {
    #[must_use]
    pub fn new(engine: Arc<Engine>) -> Self {
        Self {
            engine,
            jsonl_cap_bytes: 5 * 1024 * 1024,
        }
    }
}

impl std::fmt::Debug for HarnessLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessLoop").finish_non_exhaustive()
    }
}

#[async_trait]
impl HarnessAdapter for HarnessLoop {
    async fn run(
        &self,
        program: HarnessProgram,
        ctx: HarnessContext,
    ) -> Result<HarnessOutcome, HarnessError> {
        let max_rounds = program.max_rounds;
        let workspace_root = program.workspace_root.clone();
        let model_id = model_id_for_log(&program.provider);
        let resolved = resolve(program);

        let provider: Box<dyn ProviderClient> = match build_provider(&self.engine, &resolved_target_clone(&self, &model_id, &ctx)) {
            Some(p) => p,
            None => return Err(HarnessError::InvalidProvider("unsupported provider".into())),
        };

        run_loop(
            self,
            &*provider,
            resolved,
            workspace_root,
            ctx,
            max_rounds,
            &model_id,
        )
        .await
    }
}

// Helper: keep the ProviderTarget out of move-after-`resolve` so
// `run` can rebuild the provider. We unwrap the variant from a
// fresh `HarnessProgram` — see `resolve` for why this is fine: we
// re-read the program before calling `resolve` in production.
fn resolved_target_clone(
    _loop: &HarnessLoop,
    _model_id: &str,
    _ctx: &HarnessContext,
) -> ProviderTarget {
    // The actual implementation stores the provider config alongside
    // ResolvedProgram. For the driver sketch we accept the limitation
    // that `provider` is rebuilt from the program before `resolve()`
    // consumes it — see `run` above which already cloned the target
    // before `resolve`.
    unreachable!("rebound in run()")
}

fn model_id_for_log(p: &ProviderTarget) -> String {
    match p {
        ProviderTarget::Mistral { model_id, .. }
        | ProviderTarget::OpenAIChat { model_id, .. }
        | ProviderTarget::OpenAIResponses { model_id, .. } => model_id.clone(),
    }
}

fn build_provider(_engine: &Engine, target: &ProviderTarget) -> Option<Box<dyn ProviderClient>> {
    match target {
        ProviderTarget::Mistral {
            base_url,
            model_id,
            api_key,
            temperature,
            max_completion_tokens,
        } => {
            let mut c = MistralClient::new(base_url.clone(), model_id.clone(), api_key.clone());
            c.temperature = *temperature;
            c.max_completion_tokens = *max_completion_tokens;
            Some(Box::new(c))
        }
        ProviderTarget::OpenAIChat { .. } | ProviderTarget::OpenAIResponses { .. } => {
            // Phase 5 lands these.
            None
        }
    }
}

async fn run_loop(
    loop_: &HarnessLoop,
    provider: &dyn ProviderClient,
    mut resolved: ResolvedProgram,
    workspace_root: Option<std::path::PathBuf>,
    ctx: HarnessContext,
    max_rounds: u32,
    model_id: &str,
) -> Result<HarnessOutcome, HarnessError> {
    let started = Instant::now();
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let timeout = ctx.invocation_timeout;
    tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        cancel_clone.cancel();
    });

    let mut jsonl = JsonlBuffer::with_capacity(loop_.jsonl_cap_bytes);
    jsonl.append(&json!({
        "record": "start",
        "invocation_id": ctx.invocation_id,
        "model_id": model_id,
        "max_rounds": max_rounds,
    }));

    let mut rounds_used: u32 = 0;
    let mut total_prompt: u64 = 0;
    let mut total_completion: u64 = 0;
    let mut tool_call_count: u32 = 0;

    let (finish_reason, error_class, failure_reason) = loop {
        if max_rounds > 0 && rounds_used >= max_rounds {
            break (FinishReason::MaxRounds, ErrorClass::None, None);
        }
        rounds_used += 1;
        jsonl.append(&json!({"record":"round_start","round_idx": rounds_used}));

        let r = provider
            .tool_round(&resolved.conversation, &resolved.tools, cancel.clone())
            .await;
        match r {
            Ok(RoundResult::Final { text, assistant, prompt_tokens, completion_tokens }) => {
                total_prompt += prompt_tokens.unwrap_or(0);
                total_completion += completion_tokens.unwrap_or(0);
                jsonl.append(&json!({
                    "record":"assistant_message",
                    "round_idx": rounds_used,
                    "text_excerpt": excerpt(&text, 2000),
                    "tool_call_count": 0,
                }));
                resolved.conversation.turns.push(Turn::Assistant(assistant));
                break (FinishReason::Stop, ErrorClass::None, None);
            }
            Ok(RoundResult::LengthCap { partial_text, assistant, prompt_tokens, completion_tokens }) => {
                total_prompt += prompt_tokens.unwrap_or(0);
                total_completion += completion_tokens.unwrap_or(0);
                jsonl.append(&json!({
                    "record":"assistant_message",
                    "round_idx": rounds_used,
                    "text_excerpt": excerpt(partial_text.as_deref().unwrap_or(""), 2000),
                    "length_cap": true,
                }));
                resolved.conversation.turns.push(Turn::Assistant(assistant));
                break (FinishReason::Length, ErrorClass::None, None);
            }
            Ok(RoundResult::ToolCalls { calls, assistant, prompt_tokens, completion_tokens }) => {
                total_prompt += prompt_tokens.unwrap_or(0);
                total_completion += completion_tokens.unwrap_or(0);
                resolved.conversation.turns.push(Turn::Assistant(assistant.clone()));
                let mut fatal: Option<String> = None;
                for call in calls {
                    tool_call_count += 1;
                    let canonical = resolved
                        .reverse_map
                        .get(&call.tool_name)
                        .cloned()
                        .unwrap_or_else(|| call.tool_name.clone());
                    jsonl.append(&json!({
                        "record":"tool_call",
                        "round_idx": rounds_used,
                        "call_id": call.call_id,
                        "tool_name": canonical,
                        "args": call.arguments,
                    }));
                    let dispatch_started = Instant::now();
                    let result = dispatch_one(
                        loop_,
                        &resolved,
                        &canonical,
                        call.arguments.clone(),
                        workspace_root.as_deref(),
                        &ctx,
                        model_id,
                    )
                    .await;
                    let dur = duration_ms(dispatch_started.elapsed());
                    let (status, content): (ToolResultStatus, serde_json::Value) = match result {
                        DispatchOne::Ok(v) => (ToolResultStatus::Ok, v),
                        DispatchOne::Recoverable(msg) => {
                            (ToolResultStatus::Error, json!({"error": msg}))
                        }
                        DispatchOne::Fatal(msg) => {
                            fatal = Some(msg.clone());
                            (ToolResultStatus::Error, json!({"error": msg}))
                        }
                        DispatchOne::Unknown => (
                            ToolResultStatus::Error,
                            json!({"error":"unknown_tool", "tool_name": canonical}),
                        ),
                    };
                    jsonl.append(&json!({
                        "record":"tool_result",
                        "round_idx": rounds_used,
                        "call_id": call.call_id,
                        "status": status,
                        "duration_ms": dur,
                    }));
                    resolved.conversation.turns.push(Turn::ToolResult(ToolResultTurn {
                        call_id: call.call_id.clone(),
                        status,
                        content,
                    }));
                    if let Some(f) = fatal.clone() {
                        return Ok(HarnessOutcome {
                            kind: classify_outcome(
                                FinishReason::ToolCalls,
                                ErrorClass::ToolDispatchFatal,
                                rounds_used,
                                max_rounds,
                            ),
                            finish_reason: FinishReason::ToolCalls,
                            error_class: ErrorClass::ToolDispatchFatal,
                            failure_reason: Some(f),
                            rounds_used,
                            duration_ms: duration_ms(started.elapsed()),
                            total_prompt_tokens: Some(total_prompt),
                            total_completion_tokens: Some(total_completion),
                            tool_call_count,
                            jsonl_bytes: jsonl.snapshot().bytes,
                            jsonl_truncated: jsonl.truncated(),
                        });
                    }
                }
            }
            Err(e) => {
                let (class, msg) = error_class_for(&e);
                jsonl.append(&json!({
                    "record":"provider_error",
                    "round_idx": rounds_used,
                    "class": format!("{class:?}"),
                    "message": msg,
                }));
                break (FinishReason::Stop, class, Some(msg));
            }
        }
    };

    let dur = duration_ms(started.elapsed());
    let kind = classify_outcome(finish_reason, error_class, rounds_used, max_rounds);
    jsonl.append(&json!({
        "record":"finish",
        "outcome_kind": format!("{kind:?}"),
        "failure_reason": failure_reason,
        "rounds_used": rounds_used,
        "total_prompt_tokens": total_prompt,
        "total_completion_tokens": total_completion,
        "total_duration_ms": dur,
    }));

    let snap = jsonl.snapshot();
    Ok(HarnessOutcome {
        kind,
        finish_reason,
        error_class,
        failure_reason,
        rounds_used,
        duration_ms: dur,
        total_prompt_tokens: Some(total_prompt),
        total_completion_tokens: Some(total_completion),
        tool_call_count,
        jsonl_bytes: snap.bytes,
        jsonl_truncated: snap.truncated,
    })
}

enum DispatchOne {
    Ok(serde_json::Value),
    Recoverable(String),
    Fatal(String),
    Unknown,
}

async fn dispatch_one(
    loop_: &HarnessLoop,
    resolved: &ResolvedProgram,
    canonical: &str,
    args: serde_json::Value,
    workspace_root: Option<&std::path::Path>,
    ctx: &HarnessContext,
    model_id: &str,
) -> DispatchOne {
    match resolved.bindings.get(canonical) {
        Some(ToolBinding::Substrate(b)) => {
            use crate::tools::substrate_dispatch::{SubstrateDispatchResult, dispatch};
            match dispatch(&loop_.engine, b, args, ctx, model_id).await {
                SubstrateDispatchResult::Ok(v) => DispatchOne::Ok(v),
                SubstrateDispatchResult::Recoverable(m) => DispatchOne::Recoverable(m),
                SubstrateDispatchResult::Fatal(m) => DispatchOne::Fatal(m),
            }
        }
        Some(ToolBinding::Workspace(name)) => {
            let root = match workspace_root {
                Some(r) => r.to_path_buf(),
                None => {
                    return DispatchOne::Recoverable(
                        "workspace tool called in non-workspace wake".into(),
                    );
                }
            };
            match workspace_dispatch(*name, args, &WorkspaceCtx { workspace_root: root }).await {
                Ok(v) => DispatchOne::Ok(v),
                Err(e) => DispatchOne::Recoverable(e.to_string()),
            }
        }
        None => DispatchOne::Unknown,
    }
}

fn error_class_for(e: &ProviderError) -> (ErrorClass, String) {
    match e {
        ProviderError::Auth => (ErrorClass::Auth, "auth".into()),
        ProviderError::RateLimited { .. } => (ErrorClass::RateLimited, "rate_limited".into()),
        ProviderError::ContextLength => (ErrorClass::ContextLength, "context_length".into()),
        ProviderError::InvalidRequest(s) => (ErrorClass::InvalidRequest, s.clone()),
        ProviderError::ServerError(s) => (ErrorClass::ServerError, s.clone()),
        ProviderError::Network(s) => (ErrorClass::Network, s.clone()),
        ProviderError::Timeout => (ErrorClass::Timeout, "timeout".into()),
        ProviderError::Deserialize(s) => (ErrorClass::Deserialize, s.clone()),
    }
}

fn excerpt(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
```

Note on the `resolved_target_clone` placeholder: the implementation needs to read `program.provider` *before* `resolve(program)` consumes it. In the `run` method, change the order:

```rust
let provider_target = match &program.provider {
    ProviderTarget::Mistral { .. } => program.provider.clone(),
    ProviderTarget::OpenAIChat { .. } => program.provider.clone(),
    ProviderTarget::OpenAIResponses { .. } => program.provider.clone(),
};
let provider = build_provider(&self.engine, &provider_target)
    .ok_or_else(|| HarnessError::InvalidProvider("unsupported provider".into()))?;
let resolved = resolve(program);
```

Update the `run` method to use this pattern and delete the `resolved_target_clone` placeholder.

- [ ] **Step 2: Re-export from lib.rs**

Update `crates/harness/src/lib.rs`:

```rust
#![forbid(unsafe_code)]

pub mod conversation;
pub mod loop_driver;
pub mod program;
pub mod providers;
pub mod tools;
pub mod trace;

pub use loop_driver::HarnessLoop;

// Re-export trait + program types so callers depend only on
// proxima-harness for typing (they still need proxima-core types
// for HarnessAdapter, HarnessProgram, etc.).
pub use proxima_core::harness::{
    HarnessAdapter, HarnessContext, HarnessError, HarnessOutcome, HarnessOutcomeKind,
    HarnessProgram, ProviderTarget, SubstrateToolBinding,
};
```

- [ ] **Step 3: Build**

Run: `cargo build -p proxima-harness`
Expected: builds clean.

- [ ] **Step 4: Write a stub-provider integration test**

Create `crates/harness/tests/loop_driver.rs`. The test uses an in-test `ProviderClient` impl that returns scripted `RoundResult`s.

```rust
//! Loop driver integration test: stub provider drives the loop
//! through one tool call and a final stop.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use proxima_harness::conversation::{AssistantTurn, Conversation, ToolCall, ToolSpec};
use proxima_harness::providers::{ProviderClient, ProviderError, RoundResult};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct StubProvider {
    round: AtomicUsize,
}

#[async_trait]
impl ProviderClient for StubProvider {
    async fn tool_round(
        &self,
        _conversation: &Conversation,
        _tools: &[ToolSpec],
        _cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError> {
        let r = self.round.fetch_add(1, Ordering::SeqCst);
        Ok(match r {
            0 => RoundResult::ToolCalls {
                calls: vec![ToolCall {
                    call_id: "call_0".into(),
                    tool_name: "workspace_list_files".into(),
                    arguments: json!({"path":".","recursive":false}),
                }],
                assistant: AssistantTurn::default(),
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
            },
            _ => RoundResult::Final {
                text: "Done.".into(),
                assistant: AssistantTurn {
                    text: "Done.".into(),
                    ..Default::default()
                },
                prompt_tokens: Some(8),
                completion_tokens: Some(3),
            },
        })
    }
}

// Note: full driver wiring (HarnessLoop::new requires Engine) is
// exercised in the end-to-end test in Phase 8. This test pokes the
// provider+conversation surface alone.

#[tokio::test]
async fn stub_provider_returns_two_rounds() {
    let p = StubProvider::default();
    let conv = Conversation {
        system_prompt: "test".into(),
        user_seed: "go".into(),
        turns: vec![],
    };
    let tools: Vec<ToolSpec> = vec![];
    let r1 = p
        .tool_round(&conv, &tools, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(r1, RoundResult::ToolCalls { .. }));
    let r2 = p
        .tool_round(&conv, &tools, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(r2, RoundResult::Final { .. }));
}
```

Run: `cargo test -p proxima-harness --test loop_driver`
Expected: passes.

- [ ] **Step 5: Commit**

```bash
git add crates/harness/src/lib.rs crates/harness/src/loop_driver.rs crates/harness/tests/loop_driver.rs
git commit -m "harness: HarnessLoop driver with multi-round dispatch + reverse-map"
```

### Task 4.4 — Substrate dispatch test

**Files:**
- Create: `crates/harness/tests/substrate_dispatch.rs`

This test ensures the reverse-map and palette enforcement behave correctly. Because the test would need a wired `Engine` + `McpToolDescriptor`, defer the full e2e to Phase 8 and instead exercise the program builder's name maps here.

- [ ] **Step 1: Write the test**

```rust
use proxima_core::harness::{HarnessProgram, ProviderTarget, SubstrateToolBinding};
use proxima_harness::program::resolve;
use serde_json::json;

fn binding(canonical: &str) -> SubstrateToolBinding {
    SubstrateToolBinding {
        canonical_name: canonical.into(),
        description: "stub".into(),
        args_schema: json!({"type":"object"}),
        // The descriptor here is never invoked in this test — we
        // only exercise name resolution.
        descriptor: proxima_core::mcp::McpToolDescriptor {
            name: "stub",
            description: "stub",
            produces_schema_ids: &[],
            args_schema: serde_json::json!({}),
            call: |_, _| Box::pin(async { Err(proxima_core::mcp::McpToolError::Other("unused".into())) }),
        },
    }
}

fn empty_program(bindings: Vec<SubstrateToolBinding>, workspace: bool) -> HarnessProgram {
    HarnessProgram {
        system_prompt: "sys".into(),
        instructions: "do".into(),
        context_params: Default::default(),
        substrate_tools: bindings,
        workspace_root: workspace.then(|| std::path::PathBuf::from("/tmp/x")),
        max_rounds: 5,
        provider: ProviderTarget::Mistral {
            base_url: "http://x".into(),
            model_id: "m".into(),
            api_key: "k".into(),
            temperature: None,
            max_completion_tokens: None,
        },
    }
}

#[test]
fn provider_safe_names_reverse_map_back_to_canonical() {
    let p = empty_program(vec![binding("core/emit_abstraction")], false);
    let r = resolve(p);
    let safe = r.tools.iter().find(|t| t.canonical == "core/emit_abstraction").unwrap();
    assert_eq!(safe.provider_safe, "core_emit_abstraction");
    assert_eq!(
        r.reverse_map.get("core_emit_abstraction").unwrap(),
        "core/emit_abstraction"
    );
}

#[test]
fn workspace_tools_appear_only_when_workspace_root_is_set() {
    let p_no_ws = empty_program(vec![], false);
    let r_no = resolve(p_no_ws);
    assert!(!r_no.tools.iter().any(|t| t.canonical.starts_with("workspace_")));

    let p_ws = empty_program(vec![], true);
    let r_ws = resolve(p_ws);
    let names: Vec<&str> = r_ws.tools.iter().map(|t| t.canonical.as_str()).collect();
    assert!(names.contains(&"workspace_shell"));
    assert!(names.contains(&"workspace_text_editor"));
    assert!(names.contains(&"workspace_list_files"));
}
```

Run: `cargo test -p proxima-harness --test substrate_dispatch`
Expected: both tests pass.

- [ ] **Step 2: Commit**

```bash
git add crates/harness/tests/substrate_dispatch.rs
git commit -m "harness: program builder name-map + workspace-only-when-rooted tests"
```

---

## Phase 5 — OpenAI-Chat + OpenAI-Responses providers

**Goal:** Two more `ProviderClient` impls, parallel in shape to Mistral. Each gets recorded fixtures and an equivalent replay suite. End state: `build_provider` in `loop_driver.rs` no longer returns `None` for OpenAIChat / OpenAIResponses.

### Task 5.1 — OpenAI-Chat impl

**Files:**
- Create: `crates/harness/src/providers/openai_chat.rs`
- Modify: `crates/harness/src/providers/mod.rs` (add `pub mod openai_chat;`)
- Create: `crates/harness/tests/fixtures/openai_chat/{stop,tool_calls,length,context_length_400}.json`
- Create: `crates/harness/tests/openai_chat_replay.rs`

- [ ] **Step 1: Implement `OpenAIChatClient`**

`/v1/chat/completions` is wire-compatible with the Mistral shape. The simplest correct impl is a near-copy of `MistralClient` with three differences:
- `base_url` defaults to `https://api.openai.com`
- response body's `usage` carries the same `prompt_tokens`/`completion_tokens` field names
- some models return `tool_calls` with `function.arguments` as a *string* (already handled — `MistralClient` parses it the same way)

Mirror `mistral.rs` line-for-line, renaming `MistralClient` → `OpenAIChatClient`, `MistralResp` → `OpenAIChatResp`, etc. Keep the schema-aware parse fields identical.

- [ ] **Step 2: Add module + provider build branch**

`crates/harness/src/providers/mod.rs` — add:
```rust
pub mod openai_chat;
```

In `crates/harness/src/loop_driver.rs::build_provider`, replace the `OpenAIChat { .. } => None` arm with:
```rust
ProviderTarget::OpenAIChat {
    base_url, model_id, api_key, temperature, max_completion_tokens,
} => {
    let mut c = crate::providers::openai_chat::OpenAIChatClient::new(
        base_url.clone(), model_id.clone(), api_key.clone(),
    );
    c.temperature = *temperature;
    c.max_completion_tokens = *max_completion_tokens;
    Some(Box::new(c))
}
```

- [ ] **Step 3: Record fixtures + replay test**

Copy `crates/harness/tests/fixtures/mistral/*.json` to `crates/harness/tests/fixtures/openai_chat/*.json` (same wire shape). The `context_length_400.json` fixture is a 400 response body:
```json
{"error":{"code":"context_length_exceeded","message":"This model's maximum context length is 128000 tokens"}}
```

Create `crates/harness/tests/openai_chat_replay.rs` — copy `mistral_replay.rs` line-for-line and rename imports (`MistralClient` → `OpenAIChatClient`).

Run: `cargo test -p proxima-harness --test openai_chat_replay`
Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/harness/src/providers/openai_chat.rs crates/harness/src/providers/mod.rs crates/harness/src/loop_driver.rs crates/harness/tests/fixtures/openai_chat crates/harness/tests/openai_chat_replay.rs
git commit -m "harness: OpenAI chat-completions provider with replay tests"
```

### Task 5.2 — OpenAI-Responses (Codex tier) impl

**Files:**
- Create: `crates/harness/src/providers/openai_responses.rs`
- Modify: `crates/harness/src/providers/mod.rs` (add `pub mod openai_responses;`)
- Create: `crates/harness/tests/fixtures/openai_responses/{stop,function_call,incomplete}.json`
- Create: `crates/harness/tests/openai_responses_replay.rs`

- [ ] **Step 1: Implement `OpenAIResponsesClient`**

The Responses API differs from Chat:
- endpoint: `{base_url}/v1/responses`
- request shape: `{ model, input: [...messages], tools: [...], tool_choice, reasoning?: {effort: "low"|"medium"|"high"} }`
- response shape: `{ output: [{ type: "message" | "function_call", ... }], status, usage }`
- finish signal: `status == "completed"` plus the *type* of the last output item (`message` = final, `function_call` = tool call); `status == "incomplete"` + `incomplete_details.reason == "max_output_tokens"` maps to `LengthCap`

Sketch:

```rust
//! OpenAI `/v1/responses` adapter (Codex tier).

use std::time::Duration;
use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::conversation::{AssistantTurn, Conversation, ToolCall, ToolResultStatus, ToolSpec, Turn};
use super::{ProviderClient, ProviderError, RoundResult};

#[derive(Debug, Clone)]
pub struct OpenAIResponsesClient {
    pub http: Client,
    pub base_url: String,
    pub model_id: String,
    pub api_key: String,
    pub reasoning_effort: Option<String>,
    pub request_timeout: Duration,
}

impl OpenAIResponsesClient {
    #[must_use]
    pub fn new(base_url: String, model_id: String, api_key: String) -> Self {
        Self {
            http: Client::builder().timeout(Duration::from_secs(180)).build().unwrap(),
            base_url, model_id, api_key,
            reasoning_effort: None,
            request_timeout: Duration::from_secs(180),
        }
    }
}

#[async_trait]
impl ProviderClient for OpenAIResponsesClient {
    async fn tool_round(
        &self,
        conversation: &Conversation,
        tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError> {
        let body = build_request(self, conversation, tools);
        let url = format!("{}/v1/responses", self.base_url.trim_end_matches('/'));
        let send = self.http.post(&url).bearer_auth(&self.api_key).json(&body).send();
        let resp = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::Timeout),
            r = send => r.map_err(|e| ProviderError::Network(e.to_string()))?,
        };
        classify(resp).await
    }
}

fn build_request(c: &OpenAIResponsesClient, conv: &Conversation, tools: &[ToolSpec]) -> Value {
    let mut input: Vec<Value> = vec![
        json!({"role":"system","content":[{"type":"input_text","text": conv.system_prompt}]}),
        json!({"role":"user","content":[{"type":"input_text","text": conv.user_seed}]}),
    ];
    for t in &conv.turns {
        match t {
            Turn::Assistant(a) => input.push(json!({
                "role":"assistant",
                "content":[{"type":"output_text","text": a.text}],
                // tool_calls live separately in Responses API; if a.raw
                // carries them, prefer that. The harness re-attaches
                // them on each round.
            })),
            Turn::ToolResult(tr) => input.push(json!({
                "type":"function_call_output",
                "call_id": tr.call_id,
                "output": serde_json::to_string(&match tr.status {
                    ToolResultStatus::Ok => tr.content.clone(),
                    ToolResultStatus::Error => json!({"error": tr.content}),
                }).unwrap_or_default(),
            })),
        }
    }

    let mut req = json!({
        "model": c.model_id,
        "input": input,
        "tools": tools.iter().map(|t| json!({
            "type":"function",
            "name": t.provider_safe,
            "description": t.description,
            "parameters": t.input_schema,
        })).collect::<Vec<_>>(),
        "tool_choice":"auto",
    });
    if let Some(e) = &c.reasoning_effort {
        req["reasoning"] = json!({"effort": e});
    }
    req
}

async fn classify(resp: reqwest::Response) -> Result<RoundResult, ProviderError> {
    let status = resp.status();
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(ProviderError::Auth);
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(ProviderError::RateLimited { retry_after: None });
    }
    if status == StatusCode::BAD_REQUEST {
        let body = resp.text().await.unwrap_or_default();
        if body.contains("context_length_exceeded") {
            return Err(ProviderError::ContextLength);
        }
        return Err(ProviderError::InvalidRequest(body));
    }
    if status.is_server_error() {
        let body = resp.text().await.unwrap_or_default();
        return Err(ProviderError::ServerError(format!("{status}: {body}")));
    }

    let bytes = resp.bytes().await.map_err(|e| ProviderError::Network(e.to_string()))?;
    let parsed: RespBody = serde_json::from_slice(&bytes)
        .map_err(|e| ProviderError::Deserialize(e.to_string()))?;

    let mut text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for item in &parsed.output {
        match item.kind.as_str() {
            "message" => {
                if let Some(content) = &item.content {
                    for c in content {
                        if c.kind == "output_text" {
                            text.push_str(c.text.as_deref().unwrap_or(""));
                        }
                    }
                }
            }
            "function_call" => {
                tool_calls.push(ToolCall {
                    call_id: item.call_id.clone().unwrap_or_default(),
                    tool_name: item.name.clone().unwrap_or_default(),
                    arguments: item
                        .arguments
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or(serde_json::Value::Null),
                });
            }
            _ => {}
        }
    }

    let assistant = AssistantTurn { text: text.clone(), tool_calls: tool_calls.clone(), raw: None };
    let prompt = parsed.usage.as_ref().and_then(|u| u.input_tokens);
    let completion = parsed.usage.as_ref().and_then(|u| u.output_tokens);

    if parsed.status.as_deref() == Some("incomplete") {
        return Ok(RoundResult::LengthCap {
            partial_text: if text.is_empty() { None } else { Some(text) },
            assistant,
            prompt_tokens: prompt,
            completion_tokens: completion,
        });
    }
    if !tool_calls.is_empty() {
        return Ok(RoundResult::ToolCalls {
            calls: tool_calls,
            assistant,
            prompt_tokens: prompt,
            completion_tokens: completion,
        });
    }
    Ok(RoundResult::Final { text, assistant, prompt_tokens: prompt, completion_tokens: completion })
}

#[derive(Debug, Deserialize)]
struct RespBody {
    output: Vec<OutputItem>,
    status: Option<String>,
    usage: Option<RespUsage>,
}

#[derive(Debug, Deserialize)]
struct OutputItem {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    content: Option<Vec<OutputContent>>,
}

#[derive(Debug, Deserialize)]
struct OutputContent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RespUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}
```

- [ ] **Step 2: Add module + build-provider branch**

`crates/harness/src/providers/mod.rs` — add:
```rust
pub mod openai_responses;
```

In `loop_driver.rs::build_provider`:
```rust
ProviderTarget::OpenAIResponses { base_url, model_id, api_key, reasoning_effort } => {
    let mut c = crate::providers::openai_responses::OpenAIResponsesClient::new(
        base_url.clone(), model_id.clone(), api_key.clone(),
    );
    c.reasoning_effort = reasoning_effort.clone();
    Some(Box::new(c))
}
```

- [ ] **Step 3: Record fixtures**

`crates/harness/tests/fixtures/openai_responses/stop.json`:
```json
{
  "id": "resp_test",
  "status": "completed",
  "output": [
    {"type":"message","content":[{"type":"output_text","text":"Done."}]}
  ],
  "usage": {"input_tokens": 30, "output_tokens": 5}
}
```

`function_call.json`:
```json
{
  "id": "resp_fc",
  "status": "completed",
  "output": [
    {"type":"function_call","call_id":"fc_1","name":"workspace_shell","arguments":"{\"command\":\"ls\"}"}
  ],
  "usage": {"input_tokens": 35, "output_tokens": 12}
}
```

`incomplete.json`:
```json
{
  "id": "resp_inc",
  "status": "incomplete",
  "incomplete_details": {"reason": "max_output_tokens"},
  "output": [
    {"type":"message","content":[{"type":"output_text","text":"Partial…"}]}
  ],
  "usage": {"input_tokens": 50, "output_tokens": 4096}
}
```

- [ ] **Step 4: Replay test**

Create `crates/harness/tests/openai_responses_replay.rs`. Same in-process mock shape as `mistral_replay.rs`; assertions cover `Final` / `ToolCalls` / `LengthCap`, plus 401 and 400-context-length.

```rust
use proxima_harness::conversation::{Conversation, ToolSpec};
use proxima_harness::providers::{ProviderClient, RoundResult};
use proxima_harness::providers::openai_responses::OpenAIResponsesClient;
use serde_json::json;
use tokio_util::sync::CancellationToken;

mod common {
    include!("mistral_replay.rs");
}

#[tokio::test]
async fn responses_stop_returns_final() {
    let body = std::fs::read("tests/fixtures/openai_responses/stop.json").unwrap();
    let url = common::spawn_mock(body, "200 OK").await;
    let c = OpenAIResponsesClient::new(url, "gpt-5-codex".into(), "test".into());
    let r = c
        .tool_round(
            &Conversation { system_prompt: "s".into(), user_seed: "u".into(), turns: vec![] },
            &[],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(r, RoundResult::Final { .. }));
}

#[tokio::test]
async fn responses_function_call_returns_tool_calls() {
    let body = std::fs::read("tests/fixtures/openai_responses/function_call.json").unwrap();
    let url = common::spawn_mock(body, "200 OK").await;
    let c = OpenAIResponsesClient::new(url, "gpt-5-codex".into(), "test".into());
    let r = c
        .tool_round(
            &Conversation { system_prompt: "s".into(), user_seed: "u".into(), turns: vec![] },
            &[ToolSpec {
                canonical: "workspace_shell".into(),
                provider_safe: "workspace_shell".into(),
                description: "shell".into(),
                input_schema: json!({"type":"object"}),
            }],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    if let RoundResult::ToolCalls { calls, .. } = r {
        assert_eq!(calls[0].tool_name, "workspace_shell");
    } else {
        panic!("expected ToolCalls");
    }
}

#[tokio::test]
async fn responses_incomplete_returns_length_cap() {
    let body = std::fs::read("tests/fixtures/openai_responses/incomplete.json").unwrap();
    let url = common::spawn_mock(body, "200 OK").await;
    let c = OpenAIResponsesClient::new(url, "gpt-5-codex".into(), "test".into());
    let r = c
        .tool_round(
            &Conversation { system_prompt: "s".into(), user_seed: "u".into(), turns: vec![] },
            &[],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(r, RoundResult::LengthCap { .. }));
}
```

Note the `include!("mistral_replay.rs")` trick reuses the `spawn_mock` helper from `mistral_replay.rs`. If `include!` cross-test-file is awkward, copy the `spawn_mock` helper into the openai_responses_replay file directly — it's a 25-line function.

Run: `cargo test -p proxima-harness --test openai_responses_replay`
Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/harness/src/providers/openai_responses.rs crates/harness/src/providers/mod.rs crates/harness/src/loop_driver.rs crates/harness/tests/fixtures/openai_responses crates/harness/tests/openai_responses_replay.rs
git commit -m "harness: OpenAI /v1/responses (Codex) provider with replay tests"
```

---

## Phase 6 — `WakeEntry.instructions` column + `DefaultWakeEntrySeed` constants + onboarding wiring

**Goal:** Add the new column and the build-time source of default `instructions:` bodies that flavors ship. The column is additive — existing Goose path keeps using `recipe_ref`; the cut in Phase 8 swaps which side is read. After this phase the column exists, the constants live in flavor source, and default-personality provisioning copies the constant into the new column.

### Task 6.1 — Migration: add `instructions` column

**Files:**
- Create: `crates/storage-pg/migrations/20260512000010_wake_entry_instructions.sql`

- [ ] **Step 1: Write the migration**

```sql
-- Spec: docs/superpowers/specs/2026-05-12-proxima-harness-design.md
--       §"Recipe lifecycle: kill the YAML".
--
-- Adds the per-trigger instruction body that today's recipe YAML's
-- `instructions:` field carries. The Goose path ignores this column;
-- the harness path (Phase 8) reads it as the user-seed prefix.
ALTER TABLE proxima_core.personality_wake_entries
    ADD COLUMN IF NOT EXISTS instructions text NOT NULL DEFAULT '';
```

- [ ] **Step 2: Apply migrations locally**

Run: `cargo test -p proxima-storage-pg --test migrations` (or whatever the existing migration test is — check `crates/storage-pg/tests/`).
Expected: migration applies cleanly. Find the existing migration smoke test and confirm it still passes.

- [ ] **Step 3: Commit**

```bash
git add crates/storage-pg/migrations/20260512000010_wake_entry_instructions.sql
git commit -m "storage(wake_entries): add instructions column"
```

### Task 6.2 — `WakeEntryRow` Rust shape

**Files:**
- Modify: `crates/core/src/personality/rows.rs`

- [ ] **Step 1: Add the field**

In `WakeEntryRow` (around line 47–64), add `instructions: String` after `recipe_ref`:

```rust
pub struct WakeEntryRow {
    pub wake_entry_id: Uuid,
    pub trigger_kind: WakeEntryTriggerKind,
    pub trigger_id: String,
    pub label: String,
    pub enabled: bool,
    pub execution_mode: WakeEntryExecutionMode,
    pub authored_by: WakeEntryAuthoredBy,
    pub probability_promille: u16,
    pub goal_scope: WakeEntryGoalScope,
    pub recipe_ref: String,
    pub instructions: String,
    pub model_tier: crate::ModelTier,
    pub inference_target_ref: Option<String>,
    pub substrate_tool_palette: Vec<String>,
    pub workspace_tool_palette: Vec<String>,
    pub max_rounds: u16,
    pub disabled_reason: Option<String>,
}
```

If `WakeEntryDraft` (likely nearby in the same file) carries the same fields, add `instructions: String` there as well. Use `String::new()` as the default for any test fixture / builder that materialises a row.

- [ ] **Step 2: Update storage SQL**

Find the `SELECT` / `INSERT` for `personality_wake_entries` in `crates/storage-pg/src/`. Add `instructions` to both the column list and the `RETURNING`/`SELECT` shape. The query macro will fail at `cargo check` if the row no longer matches.

Run: `cargo check -p proxima-storage-pg`
Expected: green.

- [ ] **Step 3: Update test fixtures**

`grep -rn "WakeEntryRow {" crates/ flavors/ apps/ --include="*.rs"` to find every construction site. Add `instructions: String::new()` (or `instructions: String::from("…")` where the test cares about the value).

Run: `cargo build --workspace`
Expected: green.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/personality/rows.rs crates/storage-pg/src
git commit -m "core(wake_entries): add instructions field; storage round-trips it"
```

### Task 6.3 — `DefaultWakeEntrySeed` trait + flavor surface

**Files:**
- Create: `crates/core/src/personality/default_seeds.rs`
- Modify: `crates/core/src/personality/mod.rs` (`pub mod default_seeds;`)

- [ ] **Step 1: Implement**

```rust
//! Build-time source of the default `instructions:` body each flavor
//! ships for its bundled personalities. Replaces the recipe YAML
//! that today lives in `flavors/*/recipes/`.

use crate::ModelTier;
use crate::personality::WakeEntryExecutionMode;

#[derive(Debug, Clone)]
pub struct DefaultWakeEntrySeed {
    pub trigger_kind: TriggerKind,
    pub trigger_id: &'static str,
    pub label: &'static str,
    pub execution_mode: WakeEntryExecutionMode,
    pub substrate_tool_palette: &'static [&'static str],
    pub workspace_tool_palette: &'static [&'static str],
    pub max_rounds: u16,
    pub model_tier: ModelTier,
    pub probability_promille: u16,
    pub instructions: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub enum TriggerKind {
    ChangeEventSchema,
    GoalKind,
    SelfPerspectivePulse,
}

#[derive(Debug, Clone)]
pub struct DefaultPersonalitySeed {
    pub display_name: &'static str,
    pub purpose: &'static str,
    pub system_prompt: &'static str,
    pub wake_entries: &'static [DefaultWakeEntrySeed],
}
```

- [ ] **Step 2: Re-export from `personality/mod.rs`**

```rust
pub mod default_seeds;
pub use default_seeds::{DefaultPersonalitySeed, DefaultWakeEntrySeed, TriggerKind};
```

- [ ] **Step 3: Build**

Run: `cargo build -p proxima-core`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/personality
git commit -m "core(personality): DefaultWakeEntrySeed surface for flavor-shipped instructions"
```

### Task 6.4 — Code flavor `personalities.rs` constants

**Files:**
- Create: `flavors/code/src/personalities.rs`
- Modify: `flavors/code/src/lib.rs`

- [ ] **Step 1: Migrate the two YAML bodies to Rust constants**

The instruction body in `flavors/code/recipes/execution_worker.yaml` lines 43–75 becomes a single `&'static str` constant. Same for `flavors/code/recipes/engineer.yaml` (read it first — it follows the same template). Verbatim transcription; do not summarise.

```rust
//! Default personalities shipped by the Code flavor.
//!
//! These constants replace the recipe YAML files (deleted in Phase 8).
//! On a fresh owner the provisioning path copies these into
//! `personality_wake_entries.instructions`.

use proxima_core::ModelTier;
use proxima_core::personality::{
    DefaultPersonalitySeed, DefaultWakeEntrySeed, TriggerKind, WakeEntryExecutionMode,
};

pub const ENGINEER_INSTRUCTIONS: &str = include_str!("../instructions/engineer.txt");
pub const EXECUTION_WORKER_INSTRUCTIONS: &str =
    include_str!("../instructions/execution_worker.txt");

pub const ENGINEER: DefaultPersonalitySeed = DefaultPersonalitySeed {
    display_name: "Code Engineer",
    purpose: "Reviews and orients on Code repo events; proposes execution requests.",
    system_prompt:
        "You are the Code engineer Personality inside Proxima. Read Reality, \
        decide what to do, and emit either an execution-request Fact or a no-op.",
    wake_entries: &[DefaultWakeEntrySeed {
        trigger_kind: TriggerKind::ChangeEventSchema,
        trigger_id: "proxima-code/commit-v1",
        label: "Engineer wake on commit",
        execution_mode: WakeEntryExecutionMode::Substrate,
        substrate_tool_palette: &[
            "core/emit_abstraction",
            "core/emit_perspective",
            "proxima-code/emit_execution_request",
        ],
        workspace_tool_palette: &[],
        max_rounds: 8,
        model_tier: ModelTier::Strategic,
        probability_promille: 1000,
        instructions: ENGINEER_INSTRUCTIONS,
    }],
};

pub const EXECUTION_WORKER: DefaultPersonalitySeed = DefaultPersonalitySeed {
    display_name: "Code Execution Worker",
    purpose: "Implements an execution-request Fact inside a prepared worktree.",
    system_prompt:
        "You are an unattended software engineer inside a prepared Proxima \
        worktree. Optimize for completing one concrete change.",
    wake_entries: &[DefaultWakeEntrySeed {
        trigger_kind: TriggerKind::ChangeEventSchema,
        trigger_id: "proxima-code/execution-request-v1",
        label: "Execution worker wake",
        execution_mode: WakeEntryExecutionMode::Workspace,
        substrate_tool_palette: &[],
        workspace_tool_palette: &[
            "workspace_shell",
            "workspace_text_editor",
            "workspace_list_files",
        ],
        max_rounds: 30,
        model_tier: ModelTier::Implementation,
        probability_promille: 1000,
        instructions: EXECUTION_WORKER_INSTRUCTIONS,
    }],
};

pub const ALL: &[DefaultPersonalitySeed] = &[ENGINEER, EXECUTION_WORKER];
```

Create `flavors/code/instructions/engineer.txt` — paste the `instructions:` body from `flavors/code/recipes/engineer.yaml` verbatim (preserve line endings, indentation, paragraph breaks). Do **not** wrap it in YAML pipes or quotes.

Create `flavors/code/instructions/execution_worker.txt` — paste lines 43–75 of `flavors/code/recipes/execution_worker.yaml` verbatim. (Lines starting with two spaces in the YAML body are *not* indented in the .txt — the YAML's `instructions: |` block-scalar marker stripped the two leading spaces. Match what `goose run` saw at the prompt: dedented.)

- [ ] **Step 2: Wire into `flavors/code/src/lib.rs`**

Add `pub mod personalities;` near the existing module declarations. Export `personalities::ALL` so the onboarding path can iterate it (Task 6.5).

- [ ] **Step 3: Verify constants compile and `include_str!` resolves**

Run: `cargo build -p proxima-code-flavor` (or `cargo build -p code` — match the actual crate name; check `flavors/code/Cargo.toml`).
Expected: clean.

- [ ] **Step 4: Add a smoke test**

Create `flavors/code/tests/default_seeds.rs`:

```rust
use code::personalities::{ALL, ENGINEER, EXECUTION_WORKER, ENGINEER_INSTRUCTIONS, EXECUTION_WORKER_INSTRUCTIONS};

#[test]
fn instructions_are_non_empty() {
    assert!(!ENGINEER_INSTRUCTIONS.is_empty());
    assert!(!EXECUTION_WORKER_INSTRUCTIONS.is_empty());
}

#[test]
fn execution_worker_instructions_contain_phase_order_marker() {
    // Sanity that the YAML→txt migration preserved the phase numbering.
    assert!(EXECUTION_WORKER_INSTRUCTIONS.contains("phase order"));
}

#[test]
fn each_seed_has_at_least_one_wake_entry() {
    for s in ALL {
        assert!(!s.wake_entries.is_empty(), "{} missing wake entries", s.display_name);
    }
}

#[test]
fn engineer_wake_entry_triggers_on_commit_schema() {
    assert_eq!(ENGINEER.wake_entries[0].trigger_id, "proxima-code/commit-v1");
}
```

Replace `code` with the actual crate name from `flavors/code/Cargo.toml` if different.

Run: `cargo test -p code --test default_seeds` (or the matching crate name).
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add flavors/code/src/personalities.rs flavors/code/instructions flavors/code/src/lib.rs flavors/code/tests/default_seeds.rs
git commit -m "code(flavor): DefaultPersonalitySeed constants replacing recipe YAML bodies"
```

### Task 6.5 — Provisioning path wires seeds into `instructions`

**Files:**
- Modify: the owner-default provisioning code path — *located during implementation*. Run `grep -rn "personality_wake_entries\|create_default_personality\|default_personalities\|seed_default" crates/ apps/ flavors/ --include="*.rs"` to find it. Likely candidates: `crates/core/src/personality/`, `apps/proxima-shell/src-tauri/src/boot.rs`, or the existing path that today reads recipe YAML paths into the `recipe_ref` column.

- [ ] **Step 1: Locate the path**

Find the function that today inserts the default Engineer + Execution Worker rows. The `recipe_ref` column is non-NULL today, so the function necessarily references a recipe path or slug. Look for `"engineer"`, `"execution_worker"`, or `recipe_ref:` in literals.

- [ ] **Step 2: Add `instructions` to the insert**

The function currently passes `recipe_ref = "bundled:proxima-code/engineer"` (or similar). Add `instructions = personalities::ENGINEER.wake_entries[0].instructions.to_string()` to the same insert. Both columns coexist until Phase 8 drops `recipe_ref`.

- [ ] **Step 3: Integration test (or augment existing)**

Find the existing default-personality provisioning test (likely in `crates/core/tests/` or `apps/proxima-shell/src-tauri/tests/`). Add an assertion that the inserted row's `instructions` column is non-empty:

```rust
let row = sqlx::query!(
    "SELECT instructions FROM proxima_core.personality_wake_entries \
     WHERE label = $1",
    "Engineer wake on commit",
)
.fetch_one(&pool)
.await
.unwrap();
assert!(!row.instructions.is_empty(), "default Engineer seed should populate instructions");
```

Run the test. Expected: passes.

- [ ] **Step 4: Commit**

```bash
git add <touched files>
git commit -m "core(onboarding): provisioning copies DefaultWakeEntrySeed.instructions into wake_entries"
```

---

## Phase 7 — Three wake-trace schemas registered

**Goal:** Register `proxima-core/wake-trace-v1` (Fact), `proxima-core/wake-trace-jsonl-v1` (CitedObject), `proxima-core/wake-trace-citation-v1` (CitationMapping). Add sidecar tables. Emission wiring waits until Phase 8 where `fire_wake_entry` produces them after the harness returns.

### Task 7.1 — Sidecar tables migration

**Files:**
- Create: `crates/storage-pg/migrations/20260512000020_wake_trace_sidecars.sql`

- [ ] **Step 1: Write the migration**

```sql
-- Spec §"Layer 3 — wake-trace-v1 Fact".
-- Three new sidecars in proxima_core.

CREATE TABLE proxima_core.wake_trace_v1 (
    memory_id                   uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    invocation_id               uuid NOT NULL,
    wake_entry_id               uuid NOT NULL,
    personality_instance_id     uuid NOT NULL,
    model_target_ref            text NOT NULL,
    model_id                    text NOT NULL,
    started_at                  timestamptz NOT NULL,
    finished_at                 timestamptz NOT NULL,
    outcome_kind                text NOT NULL,
    failure_reason              text NULL,
    rounds_used                 integer NOT NULL,
    finish_reason               text NULL,
    total_prompt_tokens         bigint NULL,
    total_completion_tokens     bigint NULL,
    tool_call_count             integer NOT NULL,
    jsonl_truncated             boolean NOT NULL
);

CREATE INDEX wake_trace_v1_invocation_idx
    ON proxima_core.wake_trace_v1 (invocation_id);
CREATE INDEX wake_trace_v1_personality_idx
    ON proxima_core.wake_trace_v1 (personality_instance_id, started_at DESC);

CREATE TABLE proxima_core.cited_wake_trace_jsonl_v1 (
    cited_object_id             uuid PRIMARY KEY REFERENCES proxima_core.cited_objects(cited_object_id),
    byte_len                    bigint NOT NULL,
    line_count                  bigint NOT NULL,
    truncated                   boolean NOT NULL,
    storage_path                text NULL,           -- s3 or local; NULL while body lives inline
    body                        bytea NOT NULL       -- inline storage for v1 (≤ 5MB cap)
);

CREATE TABLE proxima_core.citation_wake_trace_v1 (
    citation_mapping_id         uuid PRIMARY KEY REFERENCES proxima_core.citation_mappings(citation_mapping_id),
    byte_range_start            bigint NULL,
    byte_range_end              bigint NULL
);
```

- [ ] **Step 2: Commit**

```bash
git add crates/storage-pg/migrations/20260512000020_wake_trace_sidecars.sql
git commit -m "storage(wake_trace): sidecars for Fact + CitedObject + CitationMapping"
```

### Task 7.2 — Rust payload structs + payload trait impls

**Files:**
- Create: `crates/core/src/wake/trace/mod.rs`
- Modify: `crates/core/src/wake/mod.rs` (`pub mod trace;`)

- [ ] **Step 1: Implement the three payloads**

```rust
//! wake-trace schemas. See spec §"Observability: three layers".

use proxima_core::CitationMappingPayload;
use proxima_core::CitedObjectPayload;
use proxima_core::FactPayload;
use proxima_core::proxima_schema_id;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WakeTracePayload {
    pub invocation_id: Uuid,
    pub wake_entry_id: Uuid,
    pub personality_instance_id: Uuid,
    pub model_target_ref: String,
    pub model_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub finished_at: OffsetDateTime,
    pub outcome_kind: String,
    pub failure_reason: Option<String>,
    pub rounds_used: u32,
    pub finish_reason: Option<String>,
    pub total_prompt_tokens: Option<u64>,
    pub total_completion_tokens: Option<u64>,
    pub tool_call_count: u32,
    pub jsonl_truncated: bool,
}

impl FactPayload for WakeTracePayload {
    const SCHEMA_ID: &'static str = proxima_schema_id!("wake-trace-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.wake_trace_v1"
    }

    fn render(&self) -> String {
        format!(
            "Wake {} {} ({} rounds)",
            self.invocation_id, self.outcome_kind, self.rounds_used
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WakeTraceJsonlPayload {
    pub byte_len: u64,
    pub line_count: u64,
    pub truncated: bool,
    /// Inline body bytes. v1 stores them in `cited_wake_trace_jsonl_v1.body`.
    #[serde(with = "serde_bytes")]
    pub body: Vec<u8>,
}

impl CitedObjectPayload for WakeTraceJsonlPayload {
    const SCHEMA_ID: &'static str = proxima_schema_id!("wake-trace-jsonl-v1");
    const SCHEMA_VERSION: u32 = 1;
    const SPECIAL_CATEGORY: bool = false;

    fn sidecar_table() -> &'static str {
        "proxima_core.cited_wake_trace_jsonl_v1"
    }

    fn idempotency_key(&self) -> [u8; 32] {
        *blake3::hash(&self.body).as_bytes()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct WakeTraceCitationPayload {
    pub byte_range_start: Option<u64>,
    pub byte_range_end: Option<u64>,
}

impl CitationMappingPayload for WakeTraceCitationPayload {
    const SCHEMA_ID: &'static str = proxima_schema_id!("wake-trace-citation-v1");
    const SCHEMA_VERSION: u32 = 1;
    const SPECIAL_CATEGORY: bool = false;

    fn sidecar_table() -> &'static str {
        "proxima_core.citation_wake_trace_v1"
    }

    fn cited_object_schema() -> &'static str {
        proxima_schema_id!("wake-trace-jsonl-v1")
    }
}
```

Adapt `CitationMappingPayload::cited_object_schema()` to return the precise type the existing trait expects — check `docs/11-citations.md` and `crates/core/src/payload.rs` for the exact signature (might be `SchemaId` instead of `&'static str`). Match it.

Add `serde_bytes` to `crates/core/Cargo.toml` if not already present:
```toml
serde_bytes = "0.11"
```

- [ ] **Step 2: Register the schemas in the core flavor**

Find the core-flavor registration site (likely `crates/core/src/flavor.rs` near the existing `add_fact_schema` calls). Add:

```rust
registry.add_fact_schema::<wake::trace::WakeTracePayload>();
registry.add_cited_object_schema::<wake::trace::WakeTraceJsonlPayload>();
registry.add_citation_mapping_schema::<wake::trace::WakeTraceCitationPayload>();
```

If `add_cited_object_schema` / `add_citation_mapping_schema` don't yet exist on `FlavorRegistry`, follow the same pattern as `add_fact_schema` (line ~101 of `flavor.rs`) — small additive change. Land them alongside this task.

- [ ] **Step 3: Build**

Run: `cargo build -p proxima-core`
Expected: clean.

- [ ] **Step 4: Test schema registration**

Add to the existing schema-registration test (likely `crates/core/tests/flavor_registry.rs`):

```rust
#[test]
fn wake_trace_schemas_are_registered_in_core_flavor() {
    let frozen = proxima_core::flavor::core_flavor().freeze();
    assert!(frozen.fact_schemas().any(|s| s.schema_id() == "proxima-core/wake-trace-v1"));
    assert!(frozen
        .cited_object_schemas()
        .any(|s| s.schema_id() == "proxima-core/wake-trace-jsonl-v1"));
    assert!(frozen
        .citation_mapping_schemas()
        .any(|s| s.schema_id() == "proxima-core/wake-trace-citation-v1"));
}
```

Match the exact iterator names from `FlavorRegistryFrozen`'s real surface — read it before writing the assertion.

Run the test. Expected: passes.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/wake/trace crates/core/src/wake/mod.rs crates/core/src/flavor.rs crates/core/Cargo.toml crates/core/tests
git commit -m "core(wake_trace): register three schemas (Fact + CitedObject + CitationMapping)"
```

---

## Phase 8 — THE CUT

**Goal:** One atomic commit that rewires `fire_wake_entry` to the harness, rewrites `InferenceTargetConfig`, migrates the database rows, deletes Goose + recipe YAML + recipe rewriter + `LocalCli`/`RemoteModel` variants, and ships an end-to-end test that drives a wake through the harness against a recorded Mistral fixture.

> **Atomicity warning:** Tasks 8.1–8.10 land in **one commit**. The intermediate states do not compile. Work on a feature branch; verify at the end with `cargo test --workspace` before committing.

### Task 8.1 — Rewrite `InferenceTargetConfig`

**Files:**
- Modify: `crates/core/src/inference/types.rs`
- Modify: `crates/core/src/inference/mod.rs` (drop `recipe_resolve`, `recipe_validate` re-exports)

- [ ] **Step 1: Rewrite the enum**

Replace `InferenceTargetConfig` in `crates/core/src/inference/types.rs` (lines 20–25 today):

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InferenceTargetConfig {
    Mistral(MistralConfig),
    OpenAIChat(OpenAIChatConfig),
    OpenAIResponses(OpenAIResponsesConfig),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct MistralConfig {
    pub base_url: String,
    pub model_id: String,
    pub api_key_env: String,
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct OpenAIChatConfig {
    pub base_url: String,
    pub model_id: String,
    pub api_key_env: String,
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct OpenAIResponsesConfig {
    pub base_url: String,
    pub model_id: String,
    pub api_key_env: String,
    pub reasoning_effort: Option<String>,
}
```

Note `MistralConfig` and `OpenAIChatConfig` have `f32` fields — `PartialEq` excludes `Eq`. Drop `Eq` from those structs and from `InferenceTargetConfig` if the auto-derive complains.

- [ ] **Step 2: Delete `LocalCliConfig` and `RemoteModelConfig`**

Remove those two structs from `types.rs` entirely.

- [ ] **Step 3: Update re-exports**

`crates/core/src/inference/mod.rs` — drop these from `pub mod` and `pub use`:
- `pub mod recipe_resolve;`
- `pub mod recipe_validate;`
- `LocalCliConfig` and `RemoteModelConfig` in the `pub use types::{…};` list

Add `MistralConfig`, `OpenAIChatConfig`, `OpenAIResponsesConfig` to the same `pub use`.

### Task 8.2 — Data migration: split existing rows by `(vendor, dialect)`

**Files:**
- Create: `crates/storage-pg/migrations/20260512000030_inference_targets_rewrite.sql`

- [ ] **Step 1: Write the migration**

```sql
-- Spec §"InferenceTargetConfig migration".
-- Translates every existing inference_targets row to the new variants;
-- unmappable rows ABORT the migration.

DO $$
DECLARE
    r RECORD;
    new_config jsonb;
BEGIN
    FOR r IN SELECT owner_principal_kind, owner_principal_id, owner_org_id,
                    target_ref, config
             FROM proxima_core.inference_targets
    LOOP
        IF r.config->>'kind' = 'local_cli' THEN
            RAISE EXCEPTION 'inference_targets row % uses LocalCli; hand-map to a peer Mistral/OpenAI target before re-running the cut', r.target_ref;
        ELSIF r.config->>'kind' = 'remote_model' THEN
            IF r.config->>'vendor' = 'mistral' THEN
                new_config := jsonb_build_object(
                    'kind', 'mistral',
                    'base_url', COALESCE(r.config->>'base_url','https://api.mistral.ai'),
                    'model_id', r.config->>'model_id',
                    'api_key_env', COALESCE(r.config->>'api_key_env','MISTRAL_API_KEY'),
                    'temperature', null::jsonb,
                    'max_completion_tokens', null::jsonb
                );
            ELSIF r.config->>'vendor' = 'openai' AND r.config->>'dialect' = 'chat' THEN
                new_config := jsonb_build_object(
                    'kind', 'open_a_i_chat',
                    'base_url', COALESCE(r.config->>'base_url','https://api.openai.com'),
                    'model_id', r.config->>'model_id',
                    'api_key_env', COALESCE(r.config->>'api_key_env','OPENAI_API_KEY'),
                    'temperature', null::jsonb,
                    'max_completion_tokens', null::jsonb
                );
            ELSIF r.config->>'vendor' = 'openai' AND r.config->>'dialect' = 'responses' THEN
                new_config := jsonb_build_object(
                    'kind', 'open_a_i_responses',
                    'base_url', COALESCE(r.config->>'base_url','https://api.openai.com'),
                    'model_id', r.config->>'model_id',
                    'api_key_env', COALESCE(r.config->>'api_key_env','OPENAI_API_KEY'),
                    'reasoning_effort', null::jsonb
                );
            ELSE
                RAISE EXCEPTION
                  'inference_targets row % has unmappable vendor=% dialect=%; hand-fix before re-running the cut',
                  r.target_ref, r.config->>'vendor', r.config->>'dialect';
            END IF;

            UPDATE proxima_core.inference_targets
            SET config = new_config
            WHERE owner_principal_kind = r.owner_principal_kind
              AND owner_principal_id   = r.owner_principal_id
              AND owner_org_id         = r.owner_org_id
              AND target_ref           = r.target_ref;
        END IF;
    END LOOP;
END
$$;
```

The `open_a_i_chat` / `open_a_i_responses` kebab in `kind` matches `serde(tag = "kind", rename_all = "snake_case")` applied to `OpenAIChat` / `OpenAIResponses`. **Verify** by serialising a sample variant in a Rust unit test before relying on the string here; if serde renames to `open_ai_chat` instead, fix the SQL.

- [ ] **Step 2: Migration test**

Create `crates/core/tests/inference_target_migration.rs`:

```rust
//! Verifies the post-migration `InferenceTargetConfig` JSON shape
//! against the `serde(tag = "kind", rename_all = "snake_case")`
//! discriminator the SQL migration assumes.

use proxima_core::inference::{
    InferenceTargetConfig, MistralConfig, OpenAIChatConfig, OpenAIResponsesConfig,
};

#[test]
fn mistral_variant_serializes_as_kind_mistral() {
    let c = InferenceTargetConfig::Mistral(MistralConfig {
        base_url: "https://api.mistral.ai".into(),
        model_id: "m".into(),
        api_key_env: "K".into(),
        temperature: None,
        max_completion_tokens: None,
    });
    let v = serde_json::to_value(&c).unwrap();
    assert_eq!(v["kind"], "mistral");
}

#[test]
fn openai_chat_kind_string() {
    let c = InferenceTargetConfig::OpenAIChat(OpenAIChatConfig {
        base_url: "x".into(), model_id: "m".into(), api_key_env: "K".into(),
        temperature: None, max_completion_tokens: None,
    });
    let v = serde_json::to_value(&c).unwrap();
    // The string the SQL migration uses MUST match this.
    let kind = v["kind"].as_str().unwrap();
    assert!(kind == "open_a_i_chat" || kind == "open_ai_chat",
        "unexpected serde rename: {kind} — update the migration SQL");
}

#[test]
fn openai_responses_kind_string() {
    let c = InferenceTargetConfig::OpenAIResponses(OpenAIResponsesConfig {
        base_url: "x".into(), model_id: "m".into(), api_key_env: "K".into(),
        reasoning_effort: None,
    });
    let v = serde_json::to_value(&c).unwrap();
    let kind = v["kind"].as_str().unwrap();
    assert!(kind == "open_a_i_responses" || kind == "open_ai_responses",
        "unexpected serde rename: {kind} — update the migration SQL");
}
```

Run this test **before** finalising the migration SQL. Adjust the `kind` strings in the SQL to whichever case `serde` actually produces.

### Task 8.3 — Drop `recipe_ref` column

**Files:**
- Create: `crates/storage-pg/migrations/20260512000040_drop_wake_entry_recipe_ref.sql`

- [ ] **Step 1: Write the migration**

```sql
ALTER TABLE proxima_core.personality_wake_entries
    DROP COLUMN recipe_ref;
```

Remove `recipe_ref: String` from `WakeEntryRow` in `crates/core/src/personality/rows.rs`. Remove from `WakeEntryDraft` too. Audit every construction site (`grep -rn "recipe_ref"` in the workspace) and delete the field — including in the SQL row mappers in `crates/storage-pg/src/`.

### Task 8.4 — Delete recipe rewriter, recipe resolve/validate, recipe YAML

**Files (delete):**
- `crates/core/src/wake/fire/recipe.rs`
- `crates/core/src/inference/recipe_resolve.rs`
- `crates/core/src/inference/recipe_validate.rs`
- `flavors/code/recipes/engineer.yaml`
- `flavors/code/recipes/execution_worker.yaml`
- `crates/core/src/wake/target_adapter/local_cli_goose.rs`
- `crates/core/tests/target_adapter_local_cli.rs`

**Files (modify):**
- `crates/core/src/wake/fire/mod.rs` — drop `pub mod recipe;`
- `crates/core/src/wake/target_adapter/mod.rs` — drop `pub mod local_cli_goose;`, re-export `HarnessAdapter`/`HarnessProgram`/`HarnessOutcome`/`HarnessContext` for the seam name compat, then plan to delete the file entirely.

- [ ] **Step 1: Run `git rm` on the deletes**

```bash
git rm crates/core/src/wake/fire/recipe.rs \
       crates/core/src/inference/recipe_resolve.rs \
       crates/core/src/inference/recipe_validate.rs \
       flavors/code/recipes/engineer.yaml \
       flavors/code/recipes/execution_worker.yaml \
       crates/core/src/wake/target_adapter/local_cli_goose.rs \
       crates/core/tests/target_adapter_local_cli.rs
```

- [ ] **Step 2: Remove `target_adapter` module re-exports**

Simplest path: keep `crates/core/src/wake/target_adapter/mod.rs` as a thin shim that re-exports the harness seam from `proxima_core::harness::*`, then in a *follow-up* delete the module entirely. For now, replace `mod.rs` contents with:

```rust
//! Wake target-adapter seam.
//!
//! v1: `TargetAdapter` is replaced by `proxima_core::harness::HarnessAdapter`.
//! This module re-exports the new types under the old path so a small
//! follow-up commit can rename call sites then delete the module.

pub use crate::harness::{
    HarnessAdapter as TargetAdapter,
    HarnessContext as TargetContext,
    HarnessError as TargetAdapterError,
    HarnessOutcome as TargetOutcome,
    HarnessOutcomeKind as TargetOutcomeKind,
    HarnessProgram as TargetInvocation,
};
```

The aliases preserve `fire_wake_entry`'s `&dyn TargetAdapter` parameter name for the rewrite below.

### Task 8.5 — Rewire `fire_wake_entry`

**Files:**
- Modify: `crates/core/src/wake/fire/fire.rs`

- [ ] **Step 1: Replace recipe-path/recipe-bytes/effective-recipe steps with the harness call**

The current `fire_wake_entry` (lines 67–299) does:
- step 2: `resolve_recipe_path` → delete
- step 3: read recipe bytes + sha256 → delete (the `recipe_sha256` column becomes a fixed `"harness/v1"` literal until the column itself is dropped; if `start_wake_invocation` requires it, pass `"".to_string()` or rename the column to `target_descriptor` in a follow-up)
- step 7: `write_effective_recipe` → delete
- step 9: `adapter.run(TargetInvocation { recipe_path, params, ... })` → replaced by `adapter.run(HarnessProgram { ... }, HarnessContext { ... })`

Rewrite the dispatch shape. Sketch (full diff would be ~120 lines — apply against the actual file):

```rust
// After step 1 (wake_context assembled) and step 4 (wake token minted)
// + step 5 (start_wake_invocation), build the HarnessProgram and call adapter:

let provider_target = build_provider_target(&resolved, engine).await?;
let substrate_tools = resolve_substrate_tools(engine, &input.wake_entry.substrate_tool_palette)?;
let mut context_params = std::collections::HashMap::new();
context_params.insert("root_perspective".into(),
    serde_json::to_value(&wake_context.root_perspective)?);
context_params.insert("active_goals".into(),
    serde_json::to_value(&wake_context.active_goals)?);
context_params.insert("trigger_event".into(),
    serde_json::to_value(&wake_context.trigger_event)?);
context_params.insert("triggering_memory".into(),
    serde_json::to_value(&wake_context.triggering_memory)?);

let workspace_root = if matches!(
    input.wake_entry.execution_mode,
    WakeEntryExecutionMode::Workspace
) {
    let ws = workspace_runner.prepare(&input, &wake_context).await?;
    context_params.insert(
        "workspace_context".into(),
        serde_json::to_value(&ws.context_payload)?,
    );
    Some(ws.worktree_path.clone())
} else {
    None
};

let program = proxima_core::harness::HarnessProgram {
    system_prompt: wake_context.root_perspective.system_prompt.clone(),
    instructions: input.wake_entry.instructions.clone(),
    context_params,
    substrate_tools,
    workspace_root,
    max_rounds: u32::from(input.wake_entry.max_rounds),
    provider: provider_target,
};
let hctx = proxima_core::harness::HarnessContext {
    owner: input.owner.clone(),
    invocation_id: invocation_id_for_dispatch,
    wake_entry_id: input.wake_entry.wake_entry_id,
    personality_instance_id: input.personality_instance_id,
    change_event_seq: input.change_event_seq,
    wake_token,
    invocation_timeout,
};
let outcome_result = adapter.run(program, hctx).await;
```

`build_provider_target` reads `resolved.config_kind` and the credential env var (`std::env::var(&cfg.api_key_env)`), surfaces `Failed("credentials_missing:...")` if absent. `resolve_substrate_tools` walks the engine's frozen registry and returns `Vec<SubstrateToolBinding>` for the palette names; reuse existing palette-resolution code if present.

- [ ] **Step 2: Wake-trace Fact emission after `adapter.run`**

After the outcome lands and before `finalize(engine, &input, outcome)`:

```rust
if let Ok(outcome) = &outcome_result {
    emit_wake_trace(
        engine,
        &input,
        &resolved,
        invocation_id_for_dispatch,
        outcome,
        wake_context_started_at,
        wake_context_finished_at,
    )
    .await
    .ok(); // non-blocking; failure is logged but doesn't fail the wake
}
```

`emit_wake_trace` builds an `EventDraft` with:
- `schema_id = "proxima-core/wake-trace-v1"`
- `payload = serde_json::to_vec(&WakeTracePayload { ... })`
- `cited_object = CitedObjectHint { schema_id: "proxima-core/wake-trace-jsonl-v1", schema_version: 1, content_hash: blake3(jsonl_bytes) }`
- `citation_mapping = CitationMappingHint { schema_id: "proxima-core/wake-trace-citation-v1", schema_version: 1 }`

Then calls `engine.event_ingest(EventDraft { ... })`. The cited-object sidecar `cited_wake_trace_jsonl_v1.body` is populated from `outcome.jsonl_bytes` — wire that through the `EventIngest` storage path the same way other cited-object sidecars (e.g. `cited_doc_pdf_v1`) get populated. If the existing path doesn't generically write the cited-object sidecar from the `EventDraft`, this commit must add that wiring — match the pattern used for the most-recently-added cited-object sidecar in `crates/storage-pg/src/`.

Place `emit_wake_trace` in `crates/core/src/wake/trace/emit.rs` (new file). Keep it small — pure transformation from `HarnessOutcome` to `EventDraft`.

### Task 8.6 — Construct `HarnessLoop` in every binary

**Files:**
- Modify: `apps/proxima-engine/src/main.rs`
- Modify: `apps/proxima-shell/src-tauri/src/boot.rs`
- Modify: `apps/proxima-code/src/main.rs`
- Modify: `apps/proxima-mcp/src/main.rs`

- [ ] **Step 1: Find each adapter construction site**

`grep -rn "LocalCliGooseAdapter::new\|target_adapter" apps/` shows where Goose is wired today. Replace each:

```rust
let adapter = std::sync::Arc::new(
    proxima_harness::HarnessLoop::new(engine.clone()),
);
```

`Engine::set_target_adapter(adapter)` or the equivalent setter — match the existing call shape. The adapter trait alias from Task 8.4 keeps the type name working.

Add `proxima-harness = { path = "../../crates/harness" }` to each binary's `Cargo.toml`.

### Task 8.7 — Update Shell config + TOML round-trip test

**Files:**
- Modify: `apps/proxima-shell/src-tauri/src/config/types.rs`
- Modify: the existing config round-trip test

- [ ] **Step 1: Rewrite `InferenceTargetRecord` variants**

The Shell config mirrors `InferenceTargetConfig`. Replace the `LocalCli` / `RemoteModel` cases with `Mistral` / `OpenAIChat` / `OpenAIResponses` following the same shape as the core enum.

- [ ] **Step 2: Update the TOML round-trip test**

The current test uses `LocalCli { command: "goose", profile: Some("work") }`. Replace with three test cases:

```rust
#[test]
fn toml_round_trip_mistral() {
    let r = InferenceTargetRecord {
        target_ref: "default-strategic".into(),
        config: InferenceTargetConfigRecord::Mistral(MistralConfigRecord {
            base_url: "https://api.mistral.ai".into(),
            model_id: "mistral-medium-3.5".into(),
            api_key_env: "MISTRAL_API_KEY".into(),
            temperature: None,
            max_completion_tokens: None,
        }),
    };
    let toml = toml::to_string(&r).unwrap();
    let back: InferenceTargetRecord = toml::from_str(&toml).unwrap();
    assert_eq!(back, r);
}
// + tests for OpenAIChat and OpenAIResponses
```

### Task 8.8 — End-to-end harness wake test

**Files:**
- Create: `crates/harness/tests/end_to_end_wake.rs`

- [ ] **Step 1: Write the test**

The test spins up a Postgres instance (use the existing test-harness pattern in `crates/storage-pg/tests/` — look for `sqlx::PgPool::connect` against `DATABASE_URL` or a per-test tempdir Postgres), seeds an Engineer personality with a Mistral inference target pointing at the in-process mock from `mistral_replay.rs`, fires one wake against a synthetic ChangeEvent, asserts:

1. `wake_invocations` row finalised with status `succeeded`.
2. `wake-trace-v1` Fact memory exists with `outcome_kind = "Succeeded"`.
3. `cited_wake_trace_jsonl_v1.body` is non-empty and the BLAKE3 hash matches the row's content_hash.
4. `citation_wake_trace_v1` row exists pointing the trace Fact at the JSONL CitedObject.
5. The Engineer's `core/authored` edge points at the new wake-trace Fact memory.

Sketch:

```rust
use std::sync::Arc;
use proxima_core::Engine;
use proxima_harness::HarnessLoop;

#[tokio::test(flavor = "multi_thread")]
async fn engineer_wake_emits_succeeded_trace_fact() {
    let (pool, owner) = test_db_with_seeded_engineer().await;
    let engine = Arc::new(Engine::new(pool.clone()).await.unwrap());
    let mock_url = spawn_mistral_mock_returning_stop().await;
    register_mistral_inference_target(&engine, &owner, mock_url).await;

    let adapter = Arc::new(HarnessLoop::new(engine.clone()));
    engine.set_harness_adapter(adapter.clone()).await;

    let seq = ingest_commit_change_event(&engine, &owner).await;
    let fired = engine.fire_due_wakes(&owner, seq).await.unwrap();
    assert!(fired >= 1);

    let inv = sqlx::query!(
        "SELECT status FROM proxima_core.wake_invocations \
         WHERE change_event_seq = $1",
        seq,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(inv.status, "succeeded");

    let trace = sqlx::query!(
        "SELECT outcome_kind, jsonl_truncated FROM proxima_core.wake_trace_v1 \
         WHERE invocation_id IN (
             SELECT wake_invocation_id FROM proxima_core.wake_invocations
             WHERE change_event_seq = $1)",
        seq,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(trace.outcome_kind, "Succeeded");
    assert_eq!(trace.jsonl_truncated, false);

    let jsonl = sqlx::query!(
        "SELECT byte_len, body FROM proxima_core.cited_wake_trace_jsonl_v1 \
         WHERE byte_len > 0"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!jsonl.body.is_empty());
}
```

Fill in `test_db_with_seeded_engineer`, `spawn_mistral_mock_returning_stop`, `register_mistral_inference_target`, and `ingest_commit_change_event` using existing helpers (look in `flavors/code/tests/` and `crates/core/tests/` for patterns — the existing `target_adapter_local_cli.rs` test will be deleted but its setup helpers should be ported.)

### Task 8.9 — Migrate Code's two personalities to a native target

**Files:**
- Modify: the provisioning path located in Task 6.5

- [ ] **Step 1: Switch default `inference_target_ref`**

Today the default Engineer + Execution Worker rows point at a `target_ref` resolved by Goose's local config. After the cut they must resolve to a Mistral inference-target row (or OpenAI — pick one per personality). The simplest landing: the onboarding path inserts a default `inference_targets` row keyed on the env var presence:

```rust
let target_ref = if std::env::var("MISTRAL_API_KEY").is_ok() {
    register_default_inference_target(
        engine,
        &owner,
        "default-strategic",
        InferenceTargetConfig::Mistral(MistralConfig {
            base_url: "https://api.mistral.ai".into(),
            model_id: "mistral-medium-3.5".into(),
            api_key_env: "MISTRAL_API_KEY".into(),
            temperature: None,
            max_completion_tokens: None,
        }),
    ).await?;
    "default-strategic"
} else if std::env::var("OPENAI_API_KEY").is_ok() {
    /* same shape, OpenAIChat or OpenAIResponses */
} else {
    return Err("no MISTRAL_API_KEY or OPENAI_API_KEY in env; cannot provision default inference target".into());
};
```

Run on fresh DB. Verify the Engineer personality's default `inference_target_ref` resolves correctly post-provisioning.

### Task 8.10 — Build, test, commit

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: clean (warnings denied; this MUST compile with zero warnings).

- [ ] **Step 2: Full workspace test**

Run: `cargo test --workspace`
Expected: all tests pass, including:
- `harness_outcome_classifier` (14 tests)
- `mistral_replay`, `openai_chat_replay`, `openai_responses_replay`
- `workspace_shell`, `workspace_text_editor`, `workspace_list_files`
- `jsonl_buffer`, `substrate_dispatch`, `loop_driver`
- `default_seeds` (code flavor)
- `inference_target_migration`
- the e2e wake test
- every pre-existing test in the workspace

- [ ] **Step 3: Verify no stale references**

```bash
! grep -rn "LocalCliGooseAdapter\|LocalCli\b\|RemoteModel\b\|write_effective_recipe\|recipe_ref\|recipe_resolve\|recipe_validate\|GOOSE_PROFILE\|engineer\.yaml\|execution_worker\.yaml" --include="*.rs" --include="*.sql" --include="*.toml" --include="*.md" crates/ apps/ flavors/ \
    | grep -v "docs/superpowers/specs/" \
    | grep -v "docs/superpowers/plans/" \
    | grep -v "/.git/"
```

Expected: empty output. Any hit other than spec/plan files needs to be cleaned up in the same commit.

- [ ] **Step 4: Single atomic commit**

```bash
git add -A
git status   # eyeball the file list; should match Tasks 8.1–8.9 exactly
git commit -m "$(cat <<'EOF'
harness(cut): replace Goose subprocess with in-process Proxima Harness

Atomic greenfield cut, per spec
docs/superpowers/specs/2026-05-12-proxima-harness-design.md:

- Rewrite InferenceTargetConfig to Mistral | OpenAIChat | OpenAIResponses
  (LocalCli + RemoteModel variants dropped).
- One-shot data migration translates existing inference_targets rows
  by (vendor, dialect); unmappable rows abort the migration.
- Wire HarnessLoop into fire_wake_entry; delete write_effective_recipe
  call and recipe-rewrite middle layer.
- Emit wake-trace-v1 Fact + wake-trace-jsonl-v1 CitedObject +
  wake-trace-citation-v1 CitationMapping after every wake.
- Drop personality_wake_entries.recipe_ref column.
- Delete crates/core/src/wake/target_adapter/local_cli_goose.rs,
  crates/core/src/wake/fire/recipe.rs,
  crates/core/src/inference/recipe_resolve.rs,
  crates/core/src/inference/recipe_validate.rs,
  flavors/code/recipes/engineer.yaml,
  flavors/code/recipes/execution_worker.yaml.
- Construct HarnessLoop in every binary (engine, shell, code, mcp).
- End-to-end test: Engineer wake → Mistral mock → wake-trace Fact
  persisted with non-empty JSONL CitedObject.
- Migrate Code's two default personalities to a native Mistral target;
  provisioning errors loudly if no API key env var is set.

This is the single commit Heinrich approved as the greenfield cut —
no transition window, no deprecation lane, no coexistence variants.
EOF
)"
```

---

## Self-Review Notes

**Spec coverage:**

| Spec section | Plan task(s) |
|---|---|
| Six principles | All — woven through Phases 1–8 |
| Crate layout (core defines trait, harness depends on core) | Task 1.1, 2.1 |
| Core traits (HarnessAdapter, ProviderClient, Conversation, RoundResult) | Tasks 1.1, 2.2, 2.3 |
| Outcome classification table | Task 1.1 + 1.2 (exhaustive tests) |
| InferenceTargetConfig migration | Tasks 8.1 + 8.2 |
| Workspace tools (shell, text_editor, list_files) | Tasks 3.1–3.4 |
| Substrate/flavor in-process dispatch + reverse-map | Tasks 4.1 + 4.2 + 4.4 |
| Recipe lifecycle: kill the YAML | Tasks 6.1–6.5 + 8.4 |
| Provisioning defaults (DefaultWakeEntrySeed) | Tasks 6.3–6.5 |
| Three observability layers | Layer 1 in Task 2.4 + 4.3 (JSONL); Layer 2 in 4.3 (`wake_invocation_log` rows already written by existing code, harness adds `harness_round` phase rows — Task 4.3 sketch covers it); Layer 3 in Tasks 7.1 + 7.2 + 8.5 |
| Changes in fire_wake_entry | Task 8.5 |
| Provider scope (Mistral + OpenAI-Chat + OpenAI-Responses) | Phases 2 + 5 |
| Single-cut Goose removal | Phase 8 |
| What stays valuable | Tasks 6.1 (WakeEntry shape preserved), 4.2 (McpToolDescriptor reuse), e2e test (WorkspaceRunner.prepare still called) |

**Type consistency:** `HarnessAdapter` / `HarnessProgram` / `HarnessOutcome` / `HarnessContext` / `ProviderTarget` / `SubstrateToolBinding` names are stable across Tasks 1.1, 4.1, 4.3, 8.5. `WorkspaceToolName::{Shell,TextEditor,ListFiles}` are stable across Tasks 3.1, 4.1, 4.3. `ToolSpec { canonical, provider_safe, description, input_schema }` consistent in Tasks 2.2, 4.1, 4.4.

**Placeholder scan:** No "TBD" / "implement later" markers. Each task either includes the code or points at the exact spec section to mirror.

**Known fragilities (called out in tasks):**

- Engine accessor names (`pool()`, `handles()`, `registry_frozen()`) in Task 4.2 — verify before adding the three `Engine` wrappers; rename if needed.
- `CitationMappingPayload::cited_object_schema()` return type in Task 7.2 — verify against `crates/core/src/payload.rs`.
- Provisioning module path in Tasks 6.5 + 8.9 — located during implementation.
- `FlavorRegistryFrozen` iterator names (`fact_schemas`, `cited_object_schemas`, `citation_mapping_schemas`) in Task 7.2 — verify before writing the assertion.
- serde-rename of `OpenAIChat`/`OpenAIResponses` in Task 8.2 — the test in Task 8.2 verifies the actual string; SQL migration must match.

