//! Private Chat Completions request/response helpers.

use std::time::Duration;

use reqwest::header::RETRY_AFTER;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::conversation::{AssistantTurn, Conversation, ToolCall, ToolSpec, Turn};

use super::{ProviderError, RoundResult};

#[derive(Debug, Clone, Copy)]
pub(crate) enum TokenLimitField {
    MaxTokens,
    MaxCompletionTokens,
}

#[derive(Debug, Clone, Copy)]
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
) -> Value {
    let mut messages = vec![
        json!({
            "role": "system",
            "content": conversation.system_prompt,
        }),
        json!({
            "role": "user",
            "content": conversation.user_seed,
        }),
    ];

    for turn in &conversation.turns {
        match turn {
            Turn::Assistant(assistant) => {
                messages.push(assistant_message(assistant));
            }
            Turn::ToolResult(result) => {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": result.call_id,
                    "content": result.content.to_string(),
                }));
            }
        }
    }

    let tool_values: Vec<Value> = tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.provider_safe,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                },
            })
        })
        .collect();

    let mut request = json!({
        "model": opts.model_id,
        "messages": messages,
    });
    if !tool_values.is_empty() {
        request["tools"] = Value::Array(tool_values);
    }
    if let Some(temperature) = opts.temperature {
        request["temperature"] = json!(temperature);
    }
    if let Some(max_completion_tokens) = opts.max_completion_tokens {
        let field = match opts.token_limit_field {
            TokenLimitField::MaxTokens => "max_tokens",
            TokenLimitField::MaxCompletionTokens => "max_completion_tokens",
        };
        request[field] = json!(max_completion_tokens);
    }
    request
}

fn assistant_message(assistant: &AssistantTurn) -> Value {
    let mut message = json!({
        "role": "assistant",
        "content": assistant.text,
    });
    if !assistant.tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(
            assistant
                .tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.call_id,
                        "type": "function",
                        "function": {
                            "name": call.tool_name,
                            "arguments": call.arguments.to_string(),
                        },
                    })
                })
                .collect(),
        );
    }
    message
}

pub(crate) async fn classify_and_parse(
    resp: reqwest::Response,
) -> Result<RoundResult, ProviderError> {
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

    let text = resp
        .text()
        .await
        .map_err(|err| ProviderError::Network(err.to_string()))?;

    if !status.is_success() {
        if status == reqwest::StatusCode::BAD_REQUEST && looks_like_context_length(&text) {
            return Err(ProviderError::ContextLength);
        }
        if status.is_server_error() {
            return Err(ProviderError::ServerError(text));
        }
        return Err(ProviderError::InvalidRequest(text));
    }

    let parsed: ChatCompletionResponse =
        serde_json::from_str(&text).map_err(|err| ProviderError::Deserialize(err.to_string()))?;
    parse_success(parsed)
}

fn looks_like_context_length(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("context_length")
        || lower.contains("context length")
        || lower.contains("maximum context")
        || lower.contains("too many tokens")
}

fn parse_success(response: ChatCompletionResponse) -> Result<RoundResult, ProviderError> {
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| ProviderError::Deserialize("missing choices[0]".to_string()))?;
    let finish_reason = choice
        .finish_reason
        .ok_or_else(|| ProviderError::Deserialize("missing finish_reason".to_string()))?;
    let text = choice.message.content.clone().unwrap_or_default();
    let raw_value = serde_json::to_value(&choice.message)
        .map_err(|err| ProviderError::Deserialize(err.to_string()))?;

    match finish_reason.as_str() {
        "stop" => {
            let raw_assistant = AssistantTurn {
                text: text.clone(),
                tool_calls: Vec::new(),
                raw: Some(raw_value),
            };
            Ok(RoundResult::Final {
                text,
                raw_assistant,
            })
        }
        "tool_calls" => {
            let calls = extract_tool_calls(choice.message.tool_calls)?;
            let raw_assistant = AssistantTurn {
                text,
                tool_calls: calls.clone(),
                raw: Some(raw_value),
            };
            Ok(RoundResult::ToolCalls {
                calls,
                raw_assistant,
            })
        }
        "length" => {
            let raw_assistant = AssistantTurn {
                text: text.clone(),
                tool_calls: Vec::new(),
                raw: Some(raw_value),
            };
            Ok(RoundResult::LengthCap {
                partial_text: Some(text),
                raw_assistant,
            })
        }
        other => Err(ProviderError::Deserialize(format!(
            "unsupported finish_reason: {other}"
        ))),
    }
}

fn extract_tool_calls(calls: Option<Vec<ChatToolCall>>) -> Result<Vec<ToolCall>, ProviderError> {
    let calls = calls.ok_or_else(|| {
        ProviderError::Deserialize("finish_reason tool_calls without tool_calls".to_string())
    })?;
    if calls.is_empty() {
        return Err(ProviderError::Deserialize(
            "finish_reason tool_calls with empty tool_calls".to_string(),
        ));
    }

    calls
        .into_iter()
        .map(|call| {
            let arguments = if call.function.arguments.trim().is_empty() {
                Value::Object(serde_json::Map::new())
            } else {
                serde_json::from_str(&call.function.arguments).map_err(|err| {
                    ProviderError::Deserialize(format!("invalid tool arguments JSON: {err}"))
                })?
            };
            Ok(ToolCall {
                call_id: call.id,
                tool_name: call.function.name,
                arguments,
            })
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChatMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatToolCall>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChatToolCall {
    id: String,
    function: ChatToolFunction,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChatToolFunction {
    name: String,
    arguments: String,
}
