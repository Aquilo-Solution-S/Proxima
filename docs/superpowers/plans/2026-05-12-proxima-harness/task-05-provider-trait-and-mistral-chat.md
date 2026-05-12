# Task 2.3 — `ProviderClient` trait + MistralChat impl

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: `crates/harness/src/providers/mod.rs`
- Create: `crates/harness/src/providers/chat_completions_wire.rs`
- Create: `crates/harness/src/providers/mistral_chat.rs`

- [ ] **Step 1: Define the trait + error type in `providers/mod.rs`**

Define `ProviderClient`, `RoundResult`, and `ProviderError` exactly once. The harness loop only depends on these types; it never branches on vendor-specific response shapes.

```rust
// Module visibility is the mechanical enforcement of the "no public
// compat surface" boundary: `chat_completions_wire` is declared with
// crate-only visibility, so nothing it exports — regardless of `pub`
// markers inside — is reachable from outside `proxima-harness`.
// Vendor adapters in this same `providers/` directory access it via
// `super::chat_completions_wire`. Do NOT change this to `pub mod`.
mod chat_completions_wire;
pub mod mistral_chat;
// openai_chat and openai_responses arrive in Phase 5.

#[async_trait::async_trait]
pub trait ProviderClient: Send + Sync {
    async fn tool_round(
        &self,
        conversation: &Conversation,
        tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError>;
}
```

Add `tokio-util` to `crates/harness/Cargo.toml` if it is not already present.

- [ ] **Step 2: Add private Chat Completions wire helpers**

Create `crates/harness/src/providers/chat_completions_wire.rs`. This module is private shared implementation, not a public compatibility surface.

It owns:
- request message construction from `Conversation` + `ToolSpec`
- response DTO structs for `/v1/chat/completions`
- tool-call extraction
- HTTP status classification
- finish-reason parsing where only `stop`, `tool_calls`, and `length` are accepted

Expose small helper functions used by vendor adapters:

```rust
pub(crate) enum TokenLimitField {
    MaxTokens,
    MaxCompletionTokens,
}

pub(crate) struct ChatCompletionsRequestOptions<'a> {
    pub model_id: &'a str,
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
    pub token_limit_field: TokenLimitField,
}

pub(crate) fn build_request(
    opts: ChatCompletionsRequestOptions<'_>,
    conversation: &Conversation,
    tools: &[ToolSpec],
) -> serde_json::Value;

pub(crate) async fn classify_and_parse(
    resp: reqwest::Response,
) -> Result<RoundResult, ProviderError>;
```

Unknown or missing `finish_reason` must return `ProviderError::Deserialize`; never fall through to `RoundResult::Final`.

- [ ] **Step 3: Implement `MistralChatClient`**

Create `crates/harness/src/providers/mistral_chat.rs`.

```rust
#[derive(Debug, Clone)]
pub struct MistralChatClient {
    pub http: reqwest::Client,
    pub base_url: String,
    pub model_id: String,
    pub api_key: String,
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
}
```

`tool_round`:
- POSTs to `{base_url}/v1/chat/completions`
- uses bearer auth
- calls `chat_completions_wire::build_request(...)`
- passes `TokenLimitField::MaxTokens`
- calls `chat_completions_wire::classify_and_parse(resp)`

Do not expose any public `compat` struct. Mistral's `max_tokens` field name is an implementation detail of `MistralChatClient`.

- [ ] **Step 4: Verify build**

Run:

```bash
cargo build -p proxima-harness
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/harness
git commit -m "harness: ProviderClient trait and Mistral chat adapter"
```
