# Task 5.2 — OpenAI-Responses (Codex tier) impl

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

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

    // OpenAI Responses API status contract:
    //   "completed"   — terminal success; emit Final or ToolCalls
    //   "incomplete"  — capped (max_output_tokens / context); emit LengthCap
    //   "failed"      — model-side failure; emit ProviderError::ServerError
    //   "in_progress" — should never reach us synchronously
    //   anything else — contract violation; emit ProviderError::Deserialize
    //
    // Do NOT fall through to RoundResult::Final for unknown statuses —
    // that silently reports success on unrecognized terminal states
    // (the same pitfall fixed in task-05's MistralChat impl).
    match parsed.status.as_deref() {
        Some("incomplete") => Ok(RoundResult::LengthCap {
            partial_text: if text.is_empty() { None } else { Some(text) },
            assistant,
            prompt_tokens: prompt,
            completion_tokens: completion,
        }),
        Some("completed") | None if !tool_calls.is_empty() => Ok(RoundResult::ToolCalls {
            calls: tool_calls,
            assistant,
            prompt_tokens: prompt,
            completion_tokens: completion,
        }),
        Some("completed") | None => Ok(RoundResult::Final {
            text,
            assistant,
            prompt_tokens: prompt,
            completion_tokens: completion,
        }),
        Some("failed") => Err(ProviderError::ServerError(
            "OpenAI Responses returned status=failed".into(),
        )),
        Some(other) => Err(ProviderError::Deserialize(format!(
            "unsupported OpenAI Responses status: {other:?}"
        ))),
    }
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

Create `crates/harness/tests/openai_responses_replay.rs`. Copy the `spawn_mock` helper from `mistral_chat_replay.rs` into this file; do not `include!` the whole test file. Assertions cover `Final` / `ToolCalls` / `LengthCap`, plus 401 and 400-context-length.

```rust
use proxima_harness::conversation::{Conversation, ToolSpec};
use proxima_harness::providers::{ProviderClient, RoundResult};
use proxima_harness::providers::openai_responses::OpenAIResponsesClient;
use serde_json::json;
use tokio_util::sync::CancellationToken;

// Copy `spawn_mock` from mistral_chat_replay.rs here.

#[tokio::test]
async fn responses_stop_returns_final() {
    let body = std::fs::read("tests/fixtures/openai_responses/stop.json").unwrap();
    let url = spawn_mock(body, "200 OK").await;
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
    let url = spawn_mock(body, "200 OK").await;
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
    let url = spawn_mock(body, "200 OK").await;
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

Run: `cargo test -p proxima-harness --test openai_responses_replay`
Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/harness/src/providers/openai_responses.rs crates/harness/src/providers/mod.rs crates/harness/src/loop_driver.rs crates/harness/tests/fixtures/openai_responses crates/harness/tests/openai_responses_replay.rs
git commit -m "harness: OpenAI /v1/responses (Codex) provider with replay tests"
```
