# Task 2.2 — Conversation types

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

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

