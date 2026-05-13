# ChatGPT-Codex Harness Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the existing `ChatGPTCodex` inference-target config into the harness's `ProviderTarget` enum and add a `ChatGPTCodexClient` implementing `ProviderClient`, so wake invocations whose tier resolves to a Codex-backed target stop failing with `provider_not_yet_supported:ChatGPTCodex`.

**Architecture:** Codex's `/responses` endpoint shares the same wire shape as OpenAI's `/v1/responses`, with three deltas: (1) the system prompt lives in a top-level `instructions` field, not in `input[0]`; (2) auth is Codex OAuth (`~/.codex/auth.json` + refresh) instead of a bearer API key; (3) two extra request headers (`chatgpt-account-id`, `originator`). We factor the shared Responses-API wire helpers into `crates/harness/src/providers/responses_wire.rs`, then the Codex client composes auth + headers + body + parsing on top of that helper. The `~/.codex/auth.json` path is carried into the harness via a new `ProviderTarget::ChatGPTCodex` variant; per-`tool_round` the client builds a `CodexAuthResolver`, resolves credentials, POSTs, and on 401 retries once after `invalidate_and_refresh`.

**Tech Stack:** Rust (workspace 1.85, edition 2024), `reqwest` for HTTP, `proxima-codex-auth` crate for OAuth resolution, hand-rolled `tokio::net::TcpListener`-based mock server for replay tests (matches existing convention in `crates/harness/tests/openai_responses_replay.rs`).

---

## Confirmed via probe (2026-05-13)

The Codex endpoint at `https://chatgpt.com/backend-api/codex/responses` **requires** `stream: true`. A POST with `stream: false` returns `400 {"detail":"Stream must be set to true"}`. SSE accumulation is mandatory.

The SSE event sequence for a simple text completion (captured live):

```
 0: response.created            → response.output: []           — initial
 1: response.in_progress        → response.output: []
 2: response.output_item.added  → item: {id, type:"reasoning", summary:[]}
 3: response.output_item.done   → reasoning done
 4: response.output_item.added  → item: {type:"message", role:"assistant", content:[]}
 5: response.content_part.added → part: {type:"output_text", text:""}
 6: response.output_text.delta  → delta:"pong"
 7: response.output_text.done   → text:"pong"
 8: response.content_part.done  → part: {type:"output_text", text:"pong"}
 9: response.output_item.done   → item: {type:"message", content:[{type:"output_text", text:"pong"}], status:"completed"}
10: response.completed          → response: {status:"completed", output: []  ← empty!}
```

**Critical:** `response.completed.response.output` is `[]` — the final canonical output does **not** live on the wrap-up event. The accumulator must collect the `item` field from every `response.output_item.done` event to reconstruct the output array.

**Accumulator strategy:**
1. Read frames split by `\n\n`.
2. For each frame, parse `event:` name and `data:` JSON.
3. On `response.output_item.done`: push `data.item` into a `Vec<Value>`.
4. On `response.completed`: record `data.response.status` and `data.response.incomplete_details`.
5. After stream close (or `response.completed`): synthesize `{output: collected_items, status, incomplete_details}` JSON matching `responses_wire::ResponsesBody`'s shape and hand to the existing `parse_success` — zero changes to parsing.

Function calls go through the same path: the item's `type` is `"function_call"` with populated `arguments` (final concatenated value present on `response.output_item.done`); `responses_wire::extract_tool_calls` already filters by that kind.

Probe binaries used: `crates/codex-auth/examples/probe_responses_stream_false.rs` and `probe_responses_stream_true.rs`. They're disposable — delete after fixtures land (Task 4 step 4).

## File Structure

**Modified:**
- `crates/core/src/harness/mod.rs` — add `ProviderTarget::ChatGPTCodex` variant
- `crates/core/src/wake/trace/emit.rs` — `provider_target_from_config` constructs the new variant; drop the `NotYetSupported` arm; `model_id` arm already exists
- `crates/harness/Cargo.toml` — add `proxima-codex-auth` dep
- `crates/harness/src/providers/mod.rs` — `pub mod chatgpt_codex; mod responses_wire;`
- `crates/harness/src/providers/openai_responses.rs` — refactor to use the extracted `responses_wire` helpers; behaviour unchanged
- `crates/harness/src/loop_driver.rs` — `build_provider` and `model_id_for_log` gain `ChatGPTCodex` arms

**Created:**
- `crates/harness/src/providers/responses_wire.rs` — shared Responses-API body/parse helpers
- `crates/harness/src/providers/chatgpt_codex.rs` — `ChatGPTCodexClient` impl
- `crates/harness/tests/chatgpt_codex_replay.rs` — wire-replay tests with hand-rolled mock server
- `crates/harness/tests/fixtures/chatgpt_codex_*.sse` — recorded SSE streams; `chatgpt_codex_auth.json` — synthetic auth fixture for mock-server tests

**Out of scope (separate plan if needed):**
- `crates/wire-grpc/src/convert/inference.rs:175-180` keeps its "no gRPC proto variant yet" stub. Codex isn't exposed over gRPC yet by design; v1 desktop runs the engine in-process per `[[project_embedded_engine_v1]]`.

---

### Task 1: Wire `proxima-codex-auth` into `proxima-harness`

**Files:**
- Modify: `crates/harness/Cargo.toml`

- [ ] **Step 1: Add the dependency**

Edit `crates/harness/Cargo.toml`. In the `[dependencies]` table, add a line right after `proxima-core`:

```toml
proxima-codex-auth = { path = "../codex-auth" }
```

- [ ] **Step 2: Verify the workspace builds**

Run: `cargo check -p proxima-harness`
Expected: clean compile.

- [ ] **Step 3: Commit**

```bash
git add crates/harness/Cargo.toml
git commit -m "build(harness): depend on proxima-codex-auth for Codex provider"
```

---

### Task 2: Extract shared Responses-API wire helpers

This step does no behaviour change. We move the OpenAI Responses body-building and response-parsing into a sibling module that both `openai_responses` and `chatgpt_codex` will share. Keeps the next two tasks small.

**Files:**
- Create: `crates/harness/src/providers/responses_wire.rs`
- Modify: `crates/harness/src/providers/mod.rs`
- Modify: `crates/harness/src/providers/openai_responses.rs`

- [ ] **Step 1: Re-run the existing OpenAI Responses replay tests to capture baseline**

Run: `cargo test -p proxima-harness --test openai_responses_replay`
Expected: all pass. Note the count — must still pass after refactor.

- [ ] **Step 2: Create the new wire helper file**

Create `crates/harness/src/providers/responses_wire.rs` with the body-builder and parser lifted verbatim from `openai_responses.rs`. The only signature change: `build_input_array` takes `system_role_in_input: bool` so callers can choose whether to put the system prompt in `input[0]` (OpenAI) or skip it (Codex, which uses top-level `instructions`).

```rust
//! Shared wire helpers for the OpenAI Responses API shape.
//!
//! Both `openai_responses.rs` (against api.openai.com) and
//! `chatgpt_codex.rs` (against chatgpt.com/backend-api/codex) build
//! requests in this shape; the only top-level differences are the
//! `instructions` field placement and the auth/headers (handled by the
//! caller).

use std::time::Duration;

use reqwest::header::RETRY_AFTER;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::conversation::{
    AssistantTurn, Conversation, ToolCall, ToolResultStatus, ToolSpec, Turn,
};

use super::{ProviderError, RoundResult};

/// Build the `input` array. When `system_role_in_input` is true, the
/// system prompt is prepended as a `role: system` item (OpenAI shape).
/// When false, the caller is responsible for placing the system prompt
/// in a top-level `instructions` field (Codex shape).
pub(super) fn build_input_array(conv: &Conversation, system_role_in_input: bool) -> Vec<Value> {
    let mut input = Vec::new();
    if system_role_in_input {
        input.push(json!({
            "role": "system",
            "content": [{"type": "input_text", "text": conv.system_prompt}],
        }));
    }
    input.push(json!({
        "role": "user",
        "content": [{"type": "input_text", "text": conv.user_seed}],
    }));

    for turn in &conv.turns {
        match turn {
            Turn::Assistant(assistant) => {
                input.extend(assistant_output_items(assistant));
            }
            Turn::ToolResult(result) => {
                let content = match result.status {
                    ToolResultStatus::Ok => result.content.clone(),
                    ToolResultStatus::Error => json!({ "error": result.content }),
                };
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": result.call_id,
                    "output": content.to_string(),
                }));
            }
        }
    }
    input
}

pub(super) fn tools_array(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.provider_safe,
                "description": tool.description,
                "parameters": tool.input_schema,
            })
        })
        .collect()
}

fn assistant_output_items(assistant: &AssistantTurn) -> Vec<Value> {
    if let Some(raw) = &assistant.raw
        && let Ok(items) = serde_json::from_value::<Vec<Value>>(raw.clone())
    {
        return items;
    }

    let mut items = Vec::new();
    if !assistant.text.is_empty() {
        items.push(json!({
            "role": "assistant",
            "content": [{"type": "output_text", "text": assistant.text}],
        }));
    }
    items.extend(assistant.tool_calls.iter().map(|call| {
        json!({
            "type": "function_call",
            "call_id": call.call_id,
            "name": call.tool_name,
            "arguments": call.arguments.to_string(),
        })
    }));
    items
}

pub(super) async fn classify(resp: reqwest::Response) -> Result<RoundResult, ProviderError> {
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(ProviderError::Auth);
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = resp
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs);
        return Err(ProviderError::RateLimited { retry_after });
    }

    let body = resp
        .text()
        .await
        .map_err(|err| ProviderError::Network(err.to_string()))?;

    if !status.is_success() {
        if status == reqwest::StatusCode::BAD_REQUEST && looks_like_context_length(&body) {
            return Err(ProviderError::ContextLength);
        }
        if status.is_server_error() {
            return Err(ProviderError::ServerError(body));
        }
        return Err(ProviderError::InvalidRequest(body));
    }

    let raw_body: Value =
        serde_json::from_str(&body).map_err(|err| ProviderError::Deserialize(err.to_string()))?;
    let raw_output = raw_body
        .get("output")
        .cloned()
        .ok_or_else(|| ProviderError::Deserialize("missing output".to_string()))?;
    if !raw_output.is_array() {
        return Err(ProviderError::Deserialize(
            "output must be an array".to_string(),
        ));
    }
    let parsed: ResponsesBody = serde_json::from_value(raw_body)
        .map_err(|err| ProviderError::Deserialize(err.to_string()))?;
    parse_success(&parsed, raw_output)
}

/// Classifier for the 401 case specifically — Codex needs to detect this
/// to decide whether to refresh and retry. Returns `true` iff the
/// response is `401 Unauthorized`.
pub(super) fn is_unauthorized(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::UNAUTHORIZED
}

fn looks_like_context_length(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("context_length")
        || lower.contains("context length")
        || lower.contains("maximum context")
        || lower.contains("too many tokens")
}

fn parse_success(parsed: &ResponsesBody, raw_output: Value) -> Result<RoundResult, ProviderError> {
    let text = extract_text(&parsed.output);
    let tool_calls = extract_tool_calls(&parsed.output)?;
    let raw_assistant = AssistantTurn {
        text: text.clone(),
        tool_calls: tool_calls.clone(),
        raw: Some(raw_output),
    };

    match parsed.status.as_deref() {
        Some("incomplete") if parsed.incomplete_reason() == Some("max_output_tokens") => {
            Ok(RoundResult::LengthCap {
                partial_text: if text.is_empty() { None } else { Some(text) },
                raw_assistant,
            })
        }
        Some("incomplete") => Err(ProviderError::Deserialize(format!(
            "unsupported incomplete reason: {:?}",
            parsed.incomplete_reason()
        ))),
        Some("completed") | None => match parsed.output.last().map(|item| item.kind.as_str()) {
            Some("function_call") => Ok(RoundResult::ToolCalls {
                calls: tool_calls,
                raw_assistant,
            }),
            Some("message") => Ok(RoundResult::Final {
                text,
                raw_assistant,
            }),
            Some(other) => Err(ProviderError::Deserialize(format!(
                "unsupported final output type: {other}"
            ))),
            None => Err(ProviderError::Deserialize(
                "missing output item".to_string(),
            )),
        },
        Some("failed") => Err(ProviderError::ServerError(
            "Responses endpoint returned status=failed".to_string(),
        )),
        Some(other) => Err(ProviderError::Deserialize(format!(
            "unsupported Responses status: {other}"
        ))),
    }
}

fn extract_text(output: &[OutputItem]) -> String {
    output
        .iter()
        .filter(|item| item.kind == "message")
        .filter_map(|item| item.content.as_ref())
        .flat_map(|content| content.iter())
        .filter(|content| content.kind == "output_text")
        .filter_map(|content| content.text.as_deref())
        .collect()
}

fn extract_tool_calls(output: &[OutputItem]) -> Result<Vec<ToolCall>, ProviderError> {
    output
        .iter()
        .filter(|item| item.kind == "function_call")
        .map(|item| {
            let call_id = item.call_id.clone().ok_or_else(|| {
                ProviderError::Deserialize("function_call missing call_id".to_string())
            })?;
            let name = item.name.clone().ok_or_else(|| {
                ProviderError::Deserialize("function_call missing name".to_string())
            })?;
            let arguments = match item.arguments.as_deref() {
                Some(arguments) if !arguments.trim().is_empty() => serde_json::from_str(arguments)
                    .map_err(|err| {
                        ProviderError::Deserialize(format!("invalid tool arguments JSON: {err}"))
                    })?,
                _ => Value::Object(serde_json::Map::new()),
            };
            Ok(ToolCall {
                call_id,
                tool_name: name,
                arguments,
            })
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct ResponsesBody {
    output: Vec<OutputItem>,
    status: Option<String>,
    #[serde(default)]
    incomplete_details: Option<IncompleteDetails>,
}

impl ResponsesBody {
    fn incomplete_reason(&self) -> Option<&str> {
        self.incomplete_details
            .as_ref()
            .and_then(|details| details.reason.as_deref())
    }
}

#[derive(Debug, Deserialize)]
struct IncompleteDetails {
    reason: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
struct OutputContent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}
```

- [ ] **Step 3: Wire the new module**

Edit `crates/harness/src/providers/mod.rs`. Add the line `mod responses_wire;` right under the existing `mod chat_completions_wire;` line (line 16):

```rust
mod chat_completions_wire;
mod responses_wire;
pub mod mistral_chat;
pub mod openai_chat;
pub mod openai_responses;
```

The `mod` is private (no `pub`) — same rationale as `chat_completions_wire`: it's a crate-internal wire helper, not a public surface.

- [ ] **Step 4: Refactor `openai_responses.rs` to call the helper**

Replace the body of `crates/harness/src/providers/openai_responses.rs` with the version below. All wire types and parsing functions are removed from this file (they now live in `responses_wire`); only the public `OpenAIResponsesClient` struct and its `ProviderClient` impl remain.

```rust
//! OpenAI `/v1/responses` provider adapter.

use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::conversation::{Conversation, ToolSpec};

use super::responses_wire;
use super::{ProviderClient, ProviderError, RoundResult};

#[derive(Debug, Clone)]
pub struct OpenAIResponsesClient {
    pub http: reqwest::Client,
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
            http: reqwest::Client::new(),
            base_url,
            model_id,
            api_key,
            reasoning_effort: None,
            request_timeout: Duration::from_mins(3),
        }
    }
}

#[async_trait::async_trait]
impl ProviderClient for OpenAIResponsesClient {
    async fn tool_round(
        &self,
        conversation: &Conversation,
        tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError> {
        let mut body = json!({
            "model": self.model_id,
            "input": responses_wire::build_input_array(conversation, true),
            "tools": responses_wire::tools_array(tools),
            "tool_choice": "auto",
        });
        if let Some(reasoning_effort) = &self.reasoning_effort {
            body["reasoning"] = json!({ "effort": reasoning_effort });
        }

        let url = format!("{}/v1/responses", self.base_url.trim_end_matches('/'));
        let request = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .timeout(self.request_timeout)
            .json(&body)
            .send();

        let resp = tokio::select! {
            result = request => result.map_err(|err| ProviderError::Network(err.to_string()))?,
            () = cancel.cancelled() => return Err(ProviderError::Timeout),
        };

        responses_wire::classify(resp).await
    }
}
```

- [ ] **Step 5: Re-run the OpenAI Responses replay tests**

Run: `cargo test -p proxima-harness --test openai_responses_replay`
Expected: same count as Step 1, all pass.

- [ ] **Step 6: Workspace compile check**

Run: `cargo check --workspace --all-targets`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/harness/src/providers/mod.rs \
        crates/harness/src/providers/responses_wire.rs \
        crates/harness/src/providers/openai_responses.rs
git commit -m "refactor(harness): extract responses_wire shared by both Responses-API providers"
```

---

### Task 3: Add `ProviderTarget::ChatGPTCodex` variant

**Files:**
- Modify: `crates/core/src/harness/mod.rs:47-68`
- Modify: `crates/core/src/wake/trace/emit.rs:195-234`
- Modify: `crates/harness/src/loop_driver.rs:86-136`

- [ ] **Step 1: Extend the enum**

In `crates/core/src/harness/mod.rs`, append a fourth variant inside `pub enum ProviderTarget` (the enum that currently ends at line 68):

```rust
    ChatGPTCodex {
        base_url: String,
        model_id: String,
        reasoning_effort: Option<String>,
        /// `~/.codex/auth.json` location. The client constructs a fresh
        /// `CodexAuthResolver` per `tool_round` and pays the cost of a
        /// JSON read; refresh remains stateful in the file itself.
        auth_json: std::path::PathBuf,
    },
```

- [ ] **Step 2: Update `provider_target_from_config`**

In `crates/core/src/wake/trace/emit.rs`, replace the `ChatGPTCodex` arm (currently lines 228-232 that return `NotYetSupported`) with the construction:

```rust
        crate::InferenceTargetConfig::ChatGPTCodex(cfg) => {
            let home = std::env::var_os("HOME").ok_or_else(|| {
                ProviderTargetBuildError::MissingCredentials {
                    env: "HOME".to_string(),
                }
            })?;
            let auth_json = std::path::PathBuf::from(home).join(".codex/auth.json");
            Ok(ProviderTarget::ChatGPTCodex {
                base_url: cfg.base_url.clone(),
                model_id: cfg.model_id.clone(),
                reasoning_effort: cfg.reasoning_effort.clone(),
                auth_json,
            })
        }
```

Rationale for reusing `MissingCredentials` for a missing `HOME`: keeps the existing error taxonomy (treated by `provider_target_failure_reason` as `credentials_missing:HOME`) without adding a new variant for an edge case that never fires on macOS/Linux dev boxes.

- [ ] **Step 3: Make the loop driver acknowledge the variant (stub)**

In `crates/harness/src/loop_driver.rs`, extend `model_id_for_log` (currently lines 86-92) to include the new variant in the same OR-pattern:

```rust
fn model_id_for_log(target: &ProviderTarget) -> String {
    match target {
        ProviderTarget::MistralChat { model_id, .. }
        | ProviderTarget::OpenAIChat { model_id, .. }
        | ProviderTarget::OpenAIResponses { model_id, .. }
        | ProviderTarget::ChatGPTCodex { model_id, .. } => model_id.clone(),
    }
}
```

Then add a `todo!()` arm to `build_provider` so the workspace compiles, knowing Task 5 will replace it:

```rust
        ProviderTarget::ChatGPTCodex { .. } => {
            todo!("ChatGPTCodexClient wired in task 5")
        }
```

- [ ] **Step 4: Workspace compile check**

Run: `cargo check --workspace --all-targets`
Expected: clean. (The `todo!()` arm is fine at compile time.)

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/harness/mod.rs \
        crates/core/src/wake/trace/emit.rs \
        crates/harness/src/loop_driver.rs
git commit -m "feat(core,harness): add ProviderTarget::ChatGPTCodex variant"
```

---

### Task 4: SSE → `ResponsesBody` accumulator

The Codex endpoint streams SSE; we collect items and synthesize a single JSON body that `responses_wire::parse_success` can already parse. Lives in `responses_wire.rs` next to `classify` so both helpers share the parsing path.

**Files:**
- Modify: `crates/harness/src/providers/responses_wire.rs` — add `classify_sse`
- Create: `crates/harness/tests/fixtures/chatgpt_codex_final_text.sse` — captured stream from live probe
- Create: `crates/harness/tests/fixtures/chatgpt_codex_tool_call.sse` — captured tool-call stream (recorded in Task 4.5 below)

- [ ] **Step 1: Capture the final-text fixture from the live endpoint**

Create a one-shot probe binary that uses the audited `proxima_codex_auth::CodexAuthResolver` so the access token stays opaque. Write to `crates/codex-auth/examples/probe_capture_sse.rs`:

```rust
//! One-shot fixture capture for the Codex /responses SSE stream.
//! Deleted after Task 4 step 7. Do not commit.

use std::time::Duration;

use proxima_codex_auth::{AuthDotJsonPath, CodexAuthResolver};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION};
use serde_json::json;

const ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const MODEL: &str = "gpt-5.5";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var_os("HOME").ok_or("HOME unset")?;
    let resolver =
        CodexAuthResolver::new(AuthDotJsonPath::from_home(std::path::Path::new(&home)))?;
    let creds = resolver.resolve().await?;

    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", creds.access_token))?,
    );
    headers.insert(
        HeaderName::from_static("chatgpt-account-id"),
        HeaderValue::from_str(&creds.account_id)?,
    );
    headers.insert(
        HeaderName::from_static("originator"),
        HeaderValue::from_static("proxima"),
    );
    headers.insert(
        reqwest::header::ACCEPT,
        HeaderValue::from_static("text/event-stream"),
    );

    let body = json!({
        "model": MODEL,
        "instructions": "Reply with the single word: pong.",
        "input": [{"role":"user","content":[{"type":"input_text","text":"ping"}]}],
        "store": false,
        "stream": true,
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let text = client
        .post(ENDPOINT)
        .headers(headers)
        .json(&body)
        .send()
        .await?
        .text()
        .await?;
    std::fs::write("/tmp/codex-final-text.sse", &text)?;
    println!("wrote {} bytes to /tmp/codex-final-text.sse", text.len());
    Ok(())
}
```

Then:

```bash
mkdir -p crates/codex-auth/examples crates/harness/tests/fixtures
cargo run -p proxima-codex-auth --example probe_capture_sse
cp /tmp/codex-final-text.sse crates/harness/tests/fixtures/chatgpt_codex_final_text.sse
head -3 crates/harness/tests/fixtures/chatgpt_codex_final_text.sse
```

Expected: the file's first line is `event: response.created`. If `cargo run` fails with `proxima-codex-auth has no example named …`, Cargo auto-discovers files under `examples/` so no Cargo.toml change is needed — just confirm the file exists.

- [ ] **Step 2: Write failing accumulator test**

Append to `crates/harness/src/providers/responses_wire.rs` (this is a unit test, not in the integration test dir, so the helpers can stay `pub(super)`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const FINAL_TEXT_SSE: &str = include_str!(
        "../../tests/fixtures/chatgpt_codex_final_text.sse"
    );

    #[test]
    fn accumulate_sse_extracts_final_text() {
        let body = accumulate_sse(FINAL_TEXT_SSE).expect("accumulate");
        let parsed: ResponsesBody =
            serde_json::from_value(body.clone()).expect("parsed");
        assert_eq!(parsed.status.as_deref(), Some("completed"));
        // Output array must include the message item with text "pong".
        let output: Vec<OutputItem> =
            serde_json::from_value(body["output"].clone()).expect("output");
        let text = extract_text(&output);
        assert_eq!(text, "pong");
    }
}
```

- [ ] **Step 3: Run it; confirm it fails**

Run: `cargo test -p proxima-harness --lib responses_wire::tests`
Expected: compile error — `accumulate_sse` not defined.

- [ ] **Step 4: Implement `accumulate_sse`**

Add to `crates/harness/src/providers/responses_wire.rs`:

```rust
/// Accumulate an SSE response body into a `ResponsesBody`-shaped JSON
/// `Value` that `parse_success` can consume.
///
/// Iterates over `event:`/`data:` frame pairs (split by blank lines).
/// Collects every `data.item` from `response.output_item.done` events
/// into the output array; reads `data.response.status` and
/// `data.response.incomplete_details` from the terminating
/// `response.completed` event. If the stream ends before
/// `response.completed`, returns a `ProviderError::Deserialize` —
/// callers treat that as a transport failure.
pub(super) fn accumulate_sse(body: &str) -> Result<Value, ProviderError> {
    let mut output_items: Vec<Value> = Vec::new();
    let mut status: Option<String> = None;
    let mut incomplete_details: Option<Value> = None;
    let mut saw_completed = false;

    for block in body.split("\n\n") {
        if block.trim().is_empty() {
            continue;
        }
        let mut event_name: Option<&str> = None;
        let mut data_line: Option<&str> = None;
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("event: ") {
                event_name = Some(rest);
            } else if let Some(rest) = line.strip_prefix("data: ") {
                data_line = Some(rest);
            }
        }
        let (Some(name), Some(data)) = (event_name, data_line) else {
            continue;
        };
        let payload: Value = serde_json::from_str(data).map_err(|err| {
            ProviderError::Deserialize(format!("SSE frame data not JSON: {err}"))
        })?;
        match name {
            "response.output_item.done" => {
                if let Some(item) = payload.get("item").cloned() {
                    output_items.push(item);
                }
            }
            "response.completed" => {
                saw_completed = true;
                if let Some(response_obj) = payload.get("response") {
                    if let Some(s) = response_obj.get("status").and_then(|v| v.as_str())
                    {
                        status = Some(s.to_string());
                    }
                    incomplete_details = response_obj
                        .get("incomplete_details")
                        .cloned()
                        .filter(|v| !v.is_null());
                }
            }
            _ => {}
        }
    }

    if !saw_completed {
        return Err(ProviderError::Deserialize(
            "Codex SSE stream ended before response.completed".to_string(),
        ));
    }

    let mut body_obj = serde_json::Map::new();
    body_obj.insert("output".to_string(), Value::Array(output_items));
    if let Some(s) = status {
        body_obj.insert("status".to_string(), Value::String(s));
    }
    if let Some(d) = incomplete_details {
        body_obj.insert("incomplete_details".to_string(), d);
    }
    Ok(Value::Object(body_obj))
}

/// Classifier for an SSE-streamed Codex response. Mirrors `classify`'s
/// status-code triage, then collects the body via `text()` and pipes
/// through `accumulate_sse` + `parse_success`.
pub(super) async fn classify_sse(
    resp: reqwest::Response,
) -> Result<RoundResult, ProviderError> {
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN
    {
        return Err(ProviderError::Auth);
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs);
        return Err(ProviderError::RateLimited { retry_after });
    }

    let body = resp
        .text()
        .await
        .map_err(|err| ProviderError::Network(err.to_string()))?;

    if !status.is_success() {
        if status == reqwest::StatusCode::BAD_REQUEST && looks_like_context_length(&body) {
            return Err(ProviderError::ContextLength);
        }
        if status.is_server_error() {
            return Err(ProviderError::ServerError(body));
        }
        return Err(ProviderError::InvalidRequest(body));
    }

    let assembled = accumulate_sse(&body)?;
    let raw_output = assembled
        .get("output")
        .cloned()
        .ok_or_else(|| ProviderError::Deserialize("missing output".to_string()))?;
    let parsed: ResponsesBody = serde_json::from_value(assembled)
        .map_err(|err| ProviderError::Deserialize(err.to_string()))?;
    parse_success(&parsed, raw_output)
}
```

- [ ] **Step 5: Run the test, confirm it passes**

Run: `cargo test -p proxima-harness --lib responses_wire`
Expected: pass.

- [ ] **Step 6: Capture the tool-call SSE fixture**

Edit `crates/codex-auth/examples/probe_capture_sse.rs`: replace the request body with the tool-call variant and change the output path:

```rust
    let body = json!({
        "model": MODEL,
        "instructions": "When the user asks for the time, call the tool `get_time`.",
        "input": [{"role":"user","content":[{"type":"input_text","text":"What time is it?"}]}],
        "tools": [{
            "type": "function",
            "name": "get_time",
            "description": "Return the current time.",
            "parameters": {"type":"object","properties":{},"additionalProperties":false},
        }],
        "tool_choice": "auto",
        "store": false,
        "stream": true,
    });
    // ... unchanged through send ...
    std::fs::write("/tmp/codex-tool-call.sse", &text)?;
```

Then:

```bash
cargo run -p proxima-codex-auth --example probe_capture_sse
cp /tmp/codex-tool-call.sse crates/harness/tests/fixtures/chatgpt_codex_tool_call.sse
```

Verify the fixture contains a `response.output_item.done` frame whose `item.type == "function_call"`:

```bash
grep -o 'function_call' crates/harness/tests/fixtures/chatgpt_codex_tool_call.sse | head -1
```
Expected: prints `function_call`. If empty, the model didn't call the tool — adjust the `instructions` to be more forcing (e.g. add "You MUST call `get_time`.") and re-capture.

Add a second test asserting that `accumulate_sse` on the tool-call fixture yields `output` containing a `{type:"function_call", name:"get_time", arguments:"{}"}` item that `extract_tool_calls` can decode:

```rust
const TOOL_CALL_SSE: &str = include_str!(
    "../../tests/fixtures/chatgpt_codex_tool_call.sse"
);

#[test]
fn accumulate_sse_extracts_tool_call() {
    let body = accumulate_sse(TOOL_CALL_SSE).expect("accumulate");
    let output: Vec<OutputItem> =
        serde_json::from_value(body["output"].clone()).expect("output");
    let calls = extract_tool_calls(&output).expect("calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].tool_name, "get_time");
}
```

Run: `cargo test -p proxima-harness --lib responses_wire`
Expected: both tests pass.

- [ ] **Step 7: Delete the probe binary**

It served its purpose; the fixture files carry the wire reality forward. The probe was never committed, so plain `rm` suffices.

```bash
rm -rf crates/codex-auth/examples
```

- [ ] **Step 8: Commit**

```bash
git add crates/harness/src/providers/responses_wire.rs \
        crates/harness/tests/fixtures/chatgpt_codex_final_text.sse \
        crates/harness/tests/fixtures/chatgpt_codex_tool_call.sse
git commit -m "feat(harness): SSE accumulator for Codex /responses streaming"
```

---

### Task 5: Implement `ChatGPTCodexClient` (happy path)

**Files:**
- Create: `crates/harness/src/providers/chatgpt_codex.rs`
- Modify: `crates/harness/src/providers/mod.rs` — `pub mod chatgpt_codex;`
- Create: `crates/harness/tests/chatgpt_codex_replay.rs`
- Reuse: `crates/harness/tests/fixtures/chatgpt_codex_final_text.sse` (created in Task 4)

- [ ] **Step 1: Write the failing replay test (final-text round)**

Create `crates/harness/tests/chatgpt_codex_replay.rs`:

```rust
use std::path::PathBuf;
use std::time::Duration;

use proxima_codex_auth::auth_json::AuthDotJsonPath;
use proxima_harness::conversation::{Conversation, ToolSpec};
use proxima_harness::providers::chatgpt_codex::ChatGPTCodexClient;
use proxima_harness::providers::{ProviderClient, RoundResult};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const FINAL_TEXT_SSE: &str = include_str!("fixtures/chatgpt_codex_final_text.sse");

async fn write_auth_json(tmp: &tempfile::TempDir) -> PathBuf {
    let auth_path = tmp.path().join(".codex/auth.json");
    tokio::fs::create_dir_all(auth_path.parent().unwrap()).await.unwrap();
    // Minimal stub that the resolver can read; access_token is a JWT
    // whose `chatgpt_account_id` claim parses out to "acct-test".
    // Pre-baked fixture (no real secrets).
    let body = include_str!("fixtures/chatgpt_codex_auth.json");
    tokio::fs::write(&auth_path, body).await.unwrap();
    auth_path
}

async fn spawn_mock(body: &'static str, status: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await.unwrap();
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(resp.as_bytes()).await.unwrap();
    });
    format!("http://{}", addr)
}

#[tokio::test]
async fn final_text_round_returns_final() {
    let tmp = tempfile::tempdir().unwrap();
    let auth_path = write_auth_json(&tmp).await;
    let base_url = spawn_mock(FINAL_TEXT_SSE, "200 OK").await;

    let client = ChatGPTCodexClient::new(
        base_url,
        "gpt-5.5".into(),
        AuthDotJsonPath::from_explicit(auth_path),
    );
    let conv = Conversation {
        system_prompt: "you are a helpful assistant".into(),
        user_seed: "hello".into(),
        turns: vec![],
    };
    let result = client
        .tool_round(&conv, &[], CancellationToken::new())
        .await
        .expect("round ok");

    match result {
        RoundResult::Final { text, .. } => assert!(!text.is_empty()),
        other => panic!("expected Final, got {other:?}"),
    }
}
```

The `AuthDotJsonPath::from_explicit` constructor may not exist yet — if it doesn't, add it in `crates/codex-auth/src/auth_json.rs` alongside the existing `from_home`. It should just wrap a `PathBuf` directly without re-deriving from `$HOME`.

- [ ] **Step 2: Run it to confirm it fails to compile**

Run: `cargo test -p proxima-harness --test chatgpt_codex_replay`
Expected: compile error — `ChatGPTCodexClient` and `chatgpt_codex` module don't exist yet.

- [ ] **Step 3: Create the auth fixture**

Write a minimal `auth.json` stub at `crates/harness/tests/fixtures/chatgpt_codex_auth.json`. The access_token is a JWT with the structure `header.payload.sig` where `payload` base64-decodes to a JSON object containing at minimum `chatgpt_account_id` and an `exp` set well in the future. Use any pre-generated JWT-shaped string from existing `proxima-codex-auth` tests (`crates/codex-auth/tests/` or fixtures) — do not hand-roll. If none exists, generate one in step 3a.

3a (if needed): generate a fixture JWT. Run:
```bash
python3 - <<'PY'
import base64, json, time
header = base64.urlsafe_b64encode(json.dumps({"alg":"none","typ":"JWT"}).encode()).decode().rstrip("=")
payload = base64.urlsafe_b64encode(json.dumps({
    "chatgpt_account_id":"acct-test",
    "exp": int(time.time()) + 3600,
}).encode()).decode().rstrip("=")
print(f"{header}.{payload}.sig")
PY
```

Then write the fixture (replace `<JWT>` with the output):
```json
{"tokens":{"id_token":"unused","access_token":"<JWT>","refresh_token":"unused-refresh"}}
```

- [ ] **Step 4: Implement `ChatGPTCodexClient`**

Create `crates/harness/src/providers/chatgpt_codex.rs`:

```rust
//! ChatGPT (subscription) `/responses` provider adapter against
//! `chatgpt.com/backend-api/codex`.

use std::time::Duration;

use proxima_codex_auth::{AuthDotJsonPath, CodexAuthResolver, CodexCredentials};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::conversation::{Conversation, ToolSpec};

use super::responses_wire;
use super::{ProviderClient, ProviderError, RoundResult};

#[derive(Debug, Clone)]
pub struct ChatGPTCodexClient {
    pub http: reqwest::Client,
    pub base_url: String,
    pub model_id: String,
    pub reasoning_effort: Option<String>,
    pub auth_json: AuthDotJsonPath,
    pub request_timeout: Duration,
}

impl ChatGPTCodexClient {
    #[must_use]
    pub fn new(base_url: String, model_id: String, auth_json: AuthDotJsonPath) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            model_id,
            reasoning_effort: None,
            auth_json,
            request_timeout: Duration::from_mins(3),
        }
    }

    fn build_headers(&self, creds: &CodexCredentials) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", creds.access_token))
                .map_err(|e| ProviderError::Network(format!("invalid access_token: {e}")))?,
        );
        headers.insert(
            HeaderName::from_static("chatgpt-account-id"),
            HeaderValue::from_str(&creds.account_id)
                .map_err(|e| ProviderError::Network(format!("invalid account_id: {e}")))?,
        );
        headers.insert(
            HeaderName::from_static("originator"),
            HeaderValue::from_static("proxima"),
        );
        Ok(headers)
    }

    fn build_body(&self, conv: &Conversation, tools: &[ToolSpec]) -> Value {
        let mut body = json!({
            "model": self.model_id,
            "instructions": conv.system_prompt,
            "input": responses_wire::build_input_array(conv, /* system_role_in_input = */ false),
            "tools": responses_wire::tools_array(tools),
            "tool_choice": "auto",
            "store": false,
            "stream": true, // mandatory — endpoint rejects stream:false
        });
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning"] = json!({ "effort": effort });
        }
        body
    }
}

#[async_trait::async_trait]
impl ProviderClient for ChatGPTCodexClient {
    async fn tool_round(
        &self,
        conversation: &Conversation,
        tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError> {
        let resolver = CodexAuthResolver::new(self.auth_json.clone())
            .map_err(|e| ProviderError::Auth)
            .map_err(|_| ProviderError::Network("codex resolver init".into()))?;
        let creds = resolver
            .resolve()
            .await
            .map_err(|_| ProviderError::Auth)?;
        let body = self.build_body(conversation, tools);
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));

        let send = |creds: &CodexCredentials| {
            let headers = self.build_headers(creds);
            let req = headers.map(|h| {
                self.http
                    .post(&url)
                    .headers(h)
                    .timeout(self.request_timeout)
                    .json(&body)
                    .send()
            });
            req
        };

        let first = match send(&creds) {
            Ok(fut) => fut,
            Err(e) => return Err(e),
        };
        let resp = tokio::select! {
            result = first => result.map_err(|err| ProviderError::Network(err.to_string()))?,
            () = cancel.cancelled() => return Err(ProviderError::Timeout),
        };

        // 401 retry path lives in Task 6. Task 5 ships the happy path only.
        // SSE-only — endpoint rejects stream:false (probed 2026-05-13).
        responses_wire::classify_sse(resp).await
    }
}
```

- [ ] **Step 5: Wire the new module**

In `crates/harness/src/providers/mod.rs`, add the `pub mod chatgpt_codex;` line:

```rust
mod chat_completions_wire;
mod responses_wire;
pub mod chatgpt_codex;
pub mod mistral_chat;
pub mod openai_chat;
pub mod openai_responses;
```

- [ ] **Step 6: Run the replay test**

Run: `cargo test -p proxima-harness --test chatgpt_codex_replay`
Expected: passes.

- [ ] **Step 7: Commit**

```bash
git add crates/harness/src/providers/mod.rs \
        crates/harness/src/providers/chatgpt_codex.rs \
        crates/harness/tests/chatgpt_codex_replay.rs \
        crates/harness/tests/fixtures/chatgpt_codex_auth.json
git commit -m "feat(harness): ChatGPTCodexClient happy-path implementation"
```

---

### Task 6: Add 401 invalidate-and-retry path

**Files:**
- Modify: `crates/harness/src/providers/chatgpt_codex.rs`
- Modify: `crates/harness/tests/chatgpt_codex_replay.rs` — new test
- Optional: add `crates/codex-auth/` test-only constructor if `CodexAuthResolver` doesn't already accept a stubbed refresh client wired to a mock URL

- [ ] **Step 1: Write failing test for 401 → refresh → 200**

Append to `crates/harness/tests/chatgpt_codex_replay.rs`:

```rust
async fn spawn_seq_mock(responses: Vec<(&'static str, &'static str)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for (status, body) in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await.unwrap();
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(resp.as_bytes()).await.unwrap();
        }
    });
    format!("http://{}", addr)
}

#[tokio::test]
async fn unauthorized_then_success_retries_after_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    let auth_path = write_auth_json(&tmp).await;
    let base_url = spawn_seq_mock(vec![
        ("401 Unauthorized", "{\"error\":\"expired\"}"),
        ("200 OK", FINAL_TEXT_SSE),
    ]).await;

    let client = ChatGPTCodexClient::new(
        base_url,
        "gpt-5.5".into(),
        AuthDotJsonPath::from_explicit(auth_path),
    );
    let conv = Conversation {
        system_prompt: "system".into(),
        user_seed: "hello".into(),
        turns: vec![],
    };
    let result = client
        .tool_round(&conv, &[], CancellationToken::new())
        .await
        .expect("round ok after retry");
    matches!(result, RoundResult::Final { .. });
}
```

The mocked refresh endpoint inside `proxima-codex-auth` is the bit that needs care: `CodexAuthResolver::invalidate_and_refresh` POSTs to `auth.openai.com/oauth/token` by default. For the test to avoid a real network call, use `CodexAuthResolver::with_refresh_client` (constructor already exists per `crates/codex-auth/src/lib.rs:55`) wired to `RefreshClient::with_endpoint(mock_url)` if that helper exists; if not, add `RefreshClient::with_endpoint` as a thin constructor that takes the OAuth token URL. The Codex client needs a small surface change to accept an injected resolver for tests — add `ChatGPTCodexClient::with_resolver_factory` (returns `CodexAuthResolver`) for test override:

```rust
impl ChatGPTCodexClient {
    pub fn with_resolver_factory<F>(mut self, factory: F) -> Self
    where
        F: Fn() -> Result<CodexAuthResolver, proxima_codex_auth::CodexAuthError>
            + Send + Sync + 'static,
    {
        self.resolver_factory = Some(std::sync::Arc::new(factory));
        self
    }
}
```

Add an `Option<Arc<…>>` field on the struct that, when present, replaces the default `CodexAuthResolver::new(self.auth_json.clone())` call inside `tool_round`. Default path (no factory set) keeps current behaviour.

- [ ] **Step 2: Implement the retry path in `tool_round`**

Inside `ChatGPTCodexClient::tool_round`, after the first `classify_sse` call, branch on the auth error path. Easiest factoring: don't go through `classify_sse` for the first call — peek at the response status before consuming the body, and only retry on 401. Adjust the function:

```rust
    async fn tool_round(
        &self,
        conversation: &Conversation,
        tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError> {
        let resolver = self.make_resolver()?;
        let creds = resolver
            .resolve()
            .await
            .map_err(|_| ProviderError::Auth)?;
        let body = self.build_body(conversation, tools);
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));

        let resp = self.send(&url, &creds, &body, cancel.clone()).await?;
        if responses_wire::is_unauthorized(resp.status()) {
            let refreshed = resolver
                .invalidate_and_refresh()
                .await
                .map_err(|_| ProviderError::Auth)?;
            let resp2 = self.send(&url, &refreshed, &body, cancel).await?;
            return responses_wire::classify_sse(resp2).await;
        }
        responses_wire::classify_sse(resp).await
    }

    async fn send(
        &self,
        url: &str,
        creds: &CodexCredentials,
        body: &Value,
        cancel: CancellationToken,
    ) -> Result<reqwest::Response, ProviderError> {
        let headers = self.build_headers(creds)?;
        let fut = self
            .http
            .post(url)
            .headers(headers)
            .timeout(self.request_timeout)
            .json(body)
            .send();
        tokio::select! {
            result = fut => result.map_err(|err| ProviderError::Network(err.to_string())),
            () = cancel.cancelled() => Err(ProviderError::Timeout),
        }
    }

    fn make_resolver(&self) -> Result<CodexAuthResolver, ProviderError> {
        if let Some(factory) = &self.resolver_factory {
            return factory().map_err(|_| ProviderError::Auth);
        }
        CodexAuthResolver::new(self.auth_json.clone()).map_err(|_| ProviderError::Auth)
    }
```

- [ ] **Step 3: Run the new test**

Run: `cargo test -p proxima-harness --test chatgpt_codex_replay`
Expected: both tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/harness/src/providers/chatgpt_codex.rs \
        crates/harness/tests/chatgpt_codex_replay.rs \
        crates/codex-auth/src/auth_json.rs crates/codex-auth/src/refresh.rs
git commit -m "feat(harness): retry once on 401 via codex-auth invalidate_and_refresh"
```

---

### Task 7: Wire `ChatGPTCodexClient` into the loop driver dispatch

**Files:**
- Modify: `crates/harness/src/loop_driver.rs:94-136`

- [ ] **Step 1: Replace the `todo!()` arm**

In `crates/harness/src/loop_driver.rs`, replace the stub from Task 3 step 3:

```rust
        ProviderTarget::ChatGPTCodex {
            base_url,
            model_id,
            reasoning_effort,
            auth_json,
        } => {
            let mut client = ChatGPTCodexClient::new(
                base_url.clone(),
                model_id.clone(),
                proxima_codex_auth::AuthDotJsonPath::from_explicit(auth_json.clone()),
            );
            client.reasoning_effort.clone_from(reasoning_effort);
            Box::new(client)
        }
```

And add the import at the top:

```rust
use crate::providers::chatgpt_codex::ChatGPTCodexClient;
```

- [ ] **Step 2: Workspace compile + replay tests**

Run: `cargo check --workspace --all-targets && cargo test -p proxima-harness`
Expected: clean compile, all tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/harness/src/loop_driver.rs
git commit -m "feat(harness): dispatch ChatGPTCodex variant to ChatGPTCodexClient"
```

---

### Task 8: End-to-end wake firing against mocked Codex endpoint

**Files:**
- Create: `crates/harness/tests/chatgpt_codex_wake_e2e.rs`

This test exercises the full `fire_wake_entry` → `build_provider` → `tool_round` → trace-emit chain to confirm the wake invocation no longer fails with `provider_not_yet_supported:ChatGPTCodex`. Pattern matches `crates/harness/tests/loop_driver.rs`.

- [ ] **Step 1: Write the failing E2E test**

Create `crates/harness/tests/chatgpt_codex_wake_e2e.rs`. The structure follows the existing `loop_driver.rs` pattern — refer to it for the exact harness setup. Key assertions:

```rust
// (skeleton — fill in by mirroring loop_driver.rs)

#[tokio::test]
async fn codex_wake_round_completes_without_provider_not_yet_supported() {
    // Spin up mock /responses endpoint (use spawn_mock from chatgpt_codex_replay).
    // Wire ProviderTarget::ChatGPTCodex into a HarnessProgram.
    // Run HarnessLoop::run.
    // Assert outcome is HarnessOutcomeKind::Completed and not Failed with
    // failure_reason containing "provider_not_yet_supported".
}
```

If `loop_driver.rs` already exercises a generic provider arm and the only delta would be the `ProviderTarget` variant, just add a parameterized variant of the existing test instead of duplicating fixture setup.

- [ ] **Step 2: Run the E2E**

Run: `cargo test -p proxima-harness --test chatgpt_codex_wake_e2e`
Expected: passes.

- [ ] **Step 3: Commit**

```bash
git add crates/harness/tests/chatgpt_codex_wake_e2e.rs
git commit -m "test(harness): E2E wake firing against mocked Codex endpoint"
```

---

### Task 9: Verify with the running Proxima Shell

**Files:** none — runtime verification only.

- [ ] **Step 1: Rebuild and restart the Shell**

Run: `cargo build -p proxima-shell` then restart the Tauri app (kill + relaunch). The previous-session Shell binary still serves the pre-rename MCP field shape from `eb03da7`, and it lacks this Codex provider; a full rebuild is required.

- [ ] **Step 2: Trigger a wake**

From the running Shell UI: trigger the personality whose W1 (Planner) uses `model_tier = standard` (which resolves to `GPT-5.5` via the existing tier binding). Watch the wake-run log directory under `~/.proxima/wake-runs/<owner>/<invocation_id>/`.

Expected: `worker-session.jsonl` shows a non-empty assistant turn; the run status is `completed` (not `failed`); the previous failure mode `provider_not_yet_supported:ChatGPTCodex` is gone.

If the wake fails for a different reason (e.g. `credentials_missing:HOME`, parse errors against the real response shape, or rate-limit), iterate — those are real follow-ups but out of this plan's scope.

- [ ] **Step 3: Final commit (optional manifest update)**

If `Cargo.lock` changed in earlier tasks but wasn't committed, fold it in now:

```bash
git add Cargo.lock
git commit -m "chore: refresh Cargo.lock for codex-auth → harness dep"
```

---

## Self-Review

**Spec coverage:**
- ChatGPTCodex variant added → Task 3
- `provider_target_from_config` constructs it → Task 3
- New `ChatGPTCodexClient` impls `ProviderClient` → Tasks 5–6
- Auth via `proxima-codex-auth` with 401 retry → Task 6
- Shared Responses-wire helpers → Task 2
- Loop-driver dispatch → Task 7
- E2E wake firing → Task 8
- Runtime verification → Task 9
- Streaming open question → flagged at top with explicit branch in Task 4

**Placeholder scan:** Task 8 has a deliberate skeleton ("fill in by mirroring loop_driver.rs"). That's the only one — the implementing engineer needs to read `loop_driver.rs` to match its setup style; specifying every line would duplicate that file here. Acceptable.

**Type consistency:**
- `ProviderTarget::ChatGPTCodex { base_url, model_id, reasoning_effort, auth_json }` is consistent across Tasks 3, 5, 6, 7.
- `ChatGPTCodexClient::new(base_url, model_id, auth_json)` is consistent in Tasks 5, 6, 7.
- `AuthDotJsonPath::from_explicit` is referenced in Tasks 5–7; Task 5 step 1 spec'd to add it to `crates/codex-auth/src/auth_json.rs` if absent — first use defines it.
- `responses_wire::{build_input_array, tools_array, classify, classify_sse, accumulate_sse, is_unauthorized}` consistent across Tasks 2, 4, 5, 6. `classify` (non-streamed) stays for `OpenAIResponsesClient`; `classify_sse` is Codex-only.

**Out-of-scope confirmation:** gRPC convert stub (`crates/wire-grpc/src/convert/inference.rs:175-180`) deliberately untouched; v1 desktop runs the engine in-process per the embedded-engine memory.
