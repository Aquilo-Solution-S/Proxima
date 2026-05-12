//! `OpenAI` `/v1/responses` provider adapter.

use std::time::Duration;

use reqwest::header::RETRY_AFTER;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::conversation::{
    AssistantTurn, Conversation, ToolCall, ToolResultStatus, ToolSpec, Turn,
};

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
        let body = build_request(self, conversation, tools);
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

        classify(resp).await
    }
}

fn build_request(c: &OpenAIResponsesClient, conv: &Conversation, tools: &[ToolSpec]) -> Value {
    let mut input = vec![
        json!({
            "role": "system",
            "content": [{"type": "input_text", "text": conv.system_prompt}],
        }),
        json!({
            "role": "user",
            "content": [{"type": "input_text", "text": conv.user_seed}],
        }),
    ];

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

    let mut request = json!({
        "model": c.model_id,
        "input": input,
        "tools": tools.iter().map(|tool| json!({
            "type": "function",
            "name": tool.provider_safe,
            "description": tool.description,
            "parameters": tool.input_schema,
        })).collect::<Vec<_>>(),
        "tool_choice": "auto",
    });
    if let Some(reasoning_effort) = &c.reasoning_effort {
        request["reasoning"] = json!({ "effort": reasoning_effort });
    }
    request
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

async fn classify(resp: reqwest::Response) -> Result<RoundResult, ProviderError> {
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
            "OpenAI Responses returned status=failed".to_string(),
        )),
        Some(other) => Err(ProviderError::Deserialize(format!(
            "unsupported OpenAI Responses status: {other}"
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
