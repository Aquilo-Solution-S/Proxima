//! Shared wire helpers for the `OpenAI` Responses API shape.
//!
//! Both `openai_responses.rs` (against `api.openai.com`) and
//! `chatgpt_codex.rs` (against `chatgpt.com/backend-api/codex`) build
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
/// system prompt is prepended as a `role: system` item (`OpenAI` shape).
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

#[cfg(feature = "chatgpt-codex")]
/// Accumulate an SSE response body into a `ResponsesBody`-shaped JSON
/// `Value` that `parse_success` can consume.
///
/// Iterates over `event:`/`data:` frame pairs (split by blank lines).
/// Collects every `data.item` from `response.output_item.done` events
/// into the output array; reads `data.response.status` and
/// `data.response.incomplete_details` from the terminating
/// `response.completed` event. If the stream ends before
/// `response.completed`, returns a `ProviderError::Deserialize` -
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
        let payload: Value = serde_json::from_str(data)
            .map_err(|err| ProviderError::Deserialize(format!("SSE frame data not JSON: {err}")))?;
        match name {
            "response.output_item.done" => {
                if let Some(item) = payload.get("item").cloned() {
                    output_items.push(item);
                }
            }
            "response.completed" => {
                saw_completed = true;
                if let Some(response_obj) = payload.get("response") {
                    if let Some(s) = response_obj.get("status").and_then(|v| v.as_str()) {
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

#[cfg(feature = "chatgpt-codex")]
/// Classifier for an SSE-streamed Codex response. Mirrors `classify`'s
/// status-code triage, then collects the body via `text()` and pipes
/// through `accumulate_sse` + `parse_success`.
pub(super) async fn classify_sse(resp: reqwest::Response) -> Result<RoundResult, ProviderError> {
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
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

#[cfg(all(test, feature = "chatgpt-codex"))]
mod tests {
    use super::*;

    const FINAL_TEXT_SSE: &str = include_str!("../../tests/fixtures/chatgpt_codex_final_text.sse");
    const TOOL_CALL_SSE: &str = include_str!("../../tests/fixtures/chatgpt_codex_tool_call.sse");

    #[test]
    fn accumulate_sse_extracts_final_text() {
        let body = accumulate_sse(FINAL_TEXT_SSE).expect("accumulate");
        let parsed: ResponsesBody = serde_json::from_value(body.clone()).expect("parsed");
        assert_eq!(parsed.status.as_deref(), Some("completed"));
        // Output array must include the message item with text "pong".
        let output: Vec<OutputItem> =
            serde_json::from_value(body["output"].clone()).expect("output");
        let text = extract_text(&output);
        assert_eq!(text, "pong");
    }

    #[test]
    fn accumulate_sse_extracts_tool_call() {
        let body = accumulate_sse(TOOL_CALL_SSE).expect("accumulate");
        let output: Vec<OutputItem> =
            serde_json::from_value(body["output"].clone()).expect("output");
        let calls = extract_tool_calls(&output).expect("calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, "get_time");
    }
}
