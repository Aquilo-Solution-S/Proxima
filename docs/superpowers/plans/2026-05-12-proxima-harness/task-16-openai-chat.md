# Task 5.1 — OpenAIChat impl

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Create: `crates/harness/src/providers/openai_chat.rs`
- Modify: `crates/harness/src/providers/mod.rs`
- Create: `crates/harness/tests/fixtures/openai_chat/{stop,tool_calls,length,auth_401,rate_limit_429,context_length_400,unsupported_finish}.json` (seven files — one per assertion in Step 3)
- Create: `crates/harness/tests/openai_chat_replay.rs`

- [ ] **Step 1: Implement `OpenAIChatClient`**

OpenAI Chat uses `/v1/chat/completions`, but it is a separate adapter from MistralChat. It shares the private `chat_completions_wire` helpers from Task 2.3; it does not expose a public compat flag.

```rust
#[derive(Debug, Clone)]
pub struct OpenAIChatClient {
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
- passes `TokenLimitField::MaxCompletionTokens`
- calls `chat_completions_wire::classify_and_parse(resp)`

- [ ] **Step 2: Add module + build-provider branch**

`crates/harness/src/providers/mod.rs`:

```rust
pub mod openai_chat;
```

In `loop_driver.rs::build_provider`, add the `ProviderTarget::OpenAIChat` branch. It constructs `OpenAIChatClient`, copies `temperature` and `max_completion_tokens`, and returns it as `Box<dyn ProviderClient>`.

- [ ] **Step 3: Replay tests**

Create `crates/harness/tests/openai_chat_replay.rs`. Reuse the same mock-server shape as `mistral_chat_replay.rs`; copy the helper into the test file rather than using `include!`.

Assertions:
- stop fixture returns `RoundResult::Final`
- tool_calls fixture returns `RoundResult::ToolCalls`
- length fixture returns `RoundResult::LengthCap`
- 401/403 returns `ProviderError::Auth`
- 429 returns `ProviderError::RateLimited`
- context-length 400 returns `ProviderError::ContextLength`
- unsupported/missing `finish_reason` returns `ProviderError::Deserialize`

- [ ] **Step 4: Run tests**

```bash
cargo test -p proxima-harness --test openai_chat_replay
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/harness/src/providers/openai_chat.rs crates/harness/src/providers/mod.rs \
        crates/harness/src/loop_driver.rs crates/harness/tests/fixtures/openai_chat \
        crates/harness/tests/openai_chat_replay.rs
git commit -m "harness: OpenAI chat adapter with replay tests"
```
