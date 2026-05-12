# Task 2.5 — MistralChat replay test against recorded fixtures

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Create: `crates/harness/tests/fixtures/mistral_chat/{stop,tool_calls,length,auth_401,rate_limit_429,context_length_400,unsupported_finish}.json` (seven files — one per assertion in Step 2)
- Create: `crates/harness/tests/mistral_chat_replay.rs`

- [ ] **Step 1: Record fixtures**

Create Mistral-shaped `/v1/chat/completions` response fixtures for:
- final assistant message with `finish_reason = "stop"`
- function-call response with `finish_reason = "tool_calls"`
- length cap with `finish_reason = "length"`
- 401 body sample
- 429 body sample
- 400 context-length body sample
- unsupported finish reason, e.g. `finish_reason = "content_filter"`

Fixtures are response bodies only. The test's in-process mock server controls the HTTP status line.

- [ ] **Step 2: Write replay test**

Create `crates/harness/tests/mistral_chat_replay.rs`. Use a tiny in-process HTTP server returning one fixture for `POST /v1/chat/completions`.

The test imports:

```rust
use proxima_harness::providers::mistral_chat::MistralChatClient;
use proxima_harness::providers::{ProviderClient, ProviderError, RoundResult};
```

Assertions:
- stop fixture returns `RoundResult::Final`
- tool_calls fixture returns `RoundResult::ToolCalls` with provider-safe tool name preserved
- length fixture returns `RoundResult::LengthCap`
- 401/403 returns `ProviderError::Auth`
- 429 returns `ProviderError::RateLimited`
- context-length 400 returns `ProviderError::ContextLength`
- unsupported/missing `finish_reason` returns `ProviderError::Deserialize`

- [ ] **Step 3: Run tests**

```bash
cargo test -p proxima-harness --test mistral_chat_replay
```

Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/harness/tests/fixtures/mistral_chat crates/harness/tests/mistral_chat_replay.rs
git commit -m "harness: Mistral chat replay tests"
```
