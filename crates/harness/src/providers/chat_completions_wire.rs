//! Private Chat Completions request/response helpers.

use std::time::Duration;

use reqwest::header::RETRY_AFTER;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::conversation::{AssistantTurn, Conversation, ToolCall, ToolSpec, Turn};
use crate::tools::strict_schema::StrictToolSchema;

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
    pub reasoning_effort: Option<&'a str>,
    pub token_limit_field: TokenLimitField,
    pub tool_policy: ChatCompletionsToolPolicy<'a>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ChatCompletionsToolPolicy<'a> {
    pub strict_tools: bool,
    pub tool_choice: Option<&'a str>,
    pub parallel_tool_calls: Option<bool>,
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
            let strict_tool = opts.tool_policy.strict_tools && supports_strict_tool_schema(tool);
            let mut function = json!({
                "name": tool.provider_safe,
                "description": tool.description,
                "parameters": tool_input_schema(tool, strict_tool),
            });
            if strict_tool {
                function["strict"] = Value::Bool(true);
            }
            json!({
                "type": "function",
                "function": function,
            })
        })
        .collect();

    let mut request = json!({
        "model": opts.model_id,
        "messages": messages,
    });
    if !tool_values.is_empty() {
        request["tools"] = Value::Array(tool_values);
        if let Some(tool_choice) = opts.tool_policy.tool_choice {
            request["tool_choice"] = json!(tool_choice);
        }
        if let Some(parallel_tool_calls) = opts.tool_policy.parallel_tool_calls {
            request["parallel_tool_calls"] = json!(parallel_tool_calls);
        }
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
    if let Some(reasoning_effort) = opts.reasoning_effort {
        request["reasoning_effort"] = json!(reasoning_effort);
    }
    request
}

fn tool_input_schema(tool: &ToolSpec, strict_tools: bool) -> Value {
    if strict_tools {
        StrictToolSchema::from_schema(&tool.input_schema)
            .expect("supports_strict_tool_schema must accept the schema")
            .value
    } else {
        tool.input_schema.clone()
    }
}

fn supports_strict_tool_schema(tool: &ToolSpec) -> bool {
    StrictToolSchema::from_schema(&tool.input_schema).is_ok()
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
    let text = message_content_text(choice.message.content.as_ref())?;
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
            let arguments = parse_tool_arguments(call.function.arguments)?;
            Ok(ToolCall {
                call_id: call.id,
                tool_name: call.function.name,
                arguments,
            })
        })
        .collect()
}

fn parse_tool_arguments(arguments: Value) -> Result<Value, ProviderError> {
    match arguments {
        Value::String(s) if s.trim().is_empty() => Ok(Value::Object(serde_json::Map::new())),
        Value::String(s) => serde_json::from_str(&s).map_err(|err| {
            ProviderError::Deserialize(format!("invalid tool arguments JSON: {err}"))
        }),
        Value::Object(_) => Ok(arguments),
        other => Err(ProviderError::Deserialize(format!(
            "tool arguments must be object or JSON string, got {other}"
        ))),
    }
}

fn message_content_text(content: Option<&Value>) -> Result<String, ProviderError> {
    let Some(content) = content else {
        return Ok(String::new());
    };
    match content {
        Value::Null => Ok(String::new()),
        Value::String(text) => Ok(text.clone()),
        Value::Array(chunks) => {
            let mut text = String::new();
            for chunk in chunks {
                let Some(chunk_type) = chunk.get("type").and_then(Value::as_str) else {
                    continue;
                };
                if chunk_type == "text"
                    && let Some(chunk_text) = chunk.get("text").and_then(Value::as_str)
                {
                    text.push_str(chunk_text);
                }
            }
            Ok(text)
        }
        other => Err(ProviderError::Deserialize(format!(
            "message content must be string, array, or null, got {other}"
        ))),
    }
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
    content: Option<Value>,
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
    arguments: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::Conversation;
    use serde_json::json;

    #[test]
    fn strict_tool_policy_sets_mistral_tool_controls_and_schema() {
        let request = build_request(
            ChatCompletionsRequestOptions {
                model_id: "mistral-medium-latest",
                temperature: Some(0.2),
                max_completion_tokens: Some(128),
                reasoning_effort: Some("high"),
                token_limit_field: TokenLimitField::MaxTokens,
                tool_policy: ChatCompletionsToolPolicy {
                    strict_tools: true,
                    tool_choice: Some("auto"),
                    parallel_tool_calls: Some(false),
                },
            },
            &Conversation {
                system_prompt: "system".into(),
                user_seed: "user".into(),
                turns: Vec::new(),
            },
            &[ToolSpec {
                canonical: "workspace_shell".into(),
                provider_safe: "workspace_shell".into(),
                description: "Run a bounded command.".into(),
                input_schema: json!({
                    "$schema": "https://json-schema.org/draft/2020-12/schema",
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "timeout_ms": { "type": "integer" }
                    },
                    "required": ["command"]
                }),
            }],
        );

        assert_eq!(request["tool_choice"], "auto");
        assert_eq!(request["reasoning_effort"], "high");
        assert_eq!(request["parallel_tool_calls"], false);
        assert_eq!(request["tools"][0]["function"]["strict"], true);
        assert_eq!(
            request["tools"][0]["function"]["parameters"]["additionalProperties"],
            false
        );
        assert_eq!(
            request["tools"][0]["function"]["parameters"]["required"],
            json!(["command", "timeout_ms"])
        );
        assert!(request["tools"][0]["function"]["parameters"]["$schema"].is_null());
    }

    #[test]
    fn strict_tool_policy_skips_unbounded_value_schemas() {
        let request = build_request(
            ChatCompletionsRequestOptions {
                model_id: "mistral-medium-latest",
                temperature: None,
                max_completion_tokens: None,
                reasoning_effort: None,
                token_limit_field: TokenLimitField::MaxTokens,
                tool_policy: ChatCompletionsToolPolicy {
                    strict_tools: true,
                    tool_choice: Some("auto"),
                    parallel_tool_calls: Some(false),
                },
            },
            &Conversation {
                system_prompt: "system".into(),
                user_seed: "user".into(),
                turns: Vec::new(),
            },
            &[ToolSpec {
                canonical: "core/emit_abstraction".into(),
                provider_safe: "core_emit_abstraction".into(),
                description: "Emit one abstraction.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "schema_id": { "type": "string" },
                        "payload": true
                    },
                    "required": ["schema_id", "payload"]
                }),
            }],
        );

        assert!(request["tools"][0]["function"]["strict"].is_null());
        assert_eq!(
            request["tools"][0]["function"]["parameters"]["properties"]["payload"],
            true
        );
        assert_eq!(request["tool_choice"], "auto");
        assert_eq!(request["parallel_tool_calls"], false);
    }

    #[test]
    fn passthrough_tool_policy_leaves_schema_and_controls_absent() {
        let request = build_request(
            ChatCompletionsRequestOptions {
                model_id: "gpt-4.1",
                temperature: None,
                max_completion_tokens: None,
                reasoning_effort: None,
                token_limit_field: TokenLimitField::MaxCompletionTokens,
                tool_policy: ChatCompletionsToolPolicy::default(),
            },
            &Conversation {
                system_prompt: "system".into(),
                user_seed: "user".into(),
                turns: Vec::new(),
            },
            &[ToolSpec {
                canonical: "workspace_shell".into(),
                provider_safe: "workspace_shell".into(),
                description: "Run a bounded command.".into(),
                input_schema: json!({
                    "$schema": "https://json-schema.org/draft/2020-12/schema",
                    "type": "object"
                }),
            }],
        );

        assert!(request["tool_choice"].is_null());
        assert!(request["parallel_tool_calls"].is_null());
        assert!(request["tools"][0]["function"]["strict"].is_null());
        assert_eq!(
            request["tools"][0]["function"]["parameters"]["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
    }

    #[test]
    fn tool_arguments_accept_string_or_object() {
        assert_eq!(
            parse_tool_arguments(json!("{\"command\":\"ls\"}")).unwrap(),
            json!({"command": "ls"})
        );
        assert_eq!(
            parse_tool_arguments(json!({"command": "ls"})).unwrap(),
            json!({"command": "ls"})
        );
    }

    #[test]
    fn parses_mistral_reasoning_chunks_as_final_text() {
        let response: ChatCompletionResponse = serde_json::from_value(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": [
                        {
                            "type": "thinking",
                            "thinking": [{
                                "type": "text",
                                "text": "scratch"
                            }]
                        },
                        {
                            "type": "text",
                            "text": "OK"
                        }
                    ]
                }
            }]
        }))
        .unwrap();

        let result = parse_success(response).unwrap();
        let RoundResult::Final { text, .. } = result else {
            panic!("expected final");
        };
        assert_eq!(text, "OK");
    }
}
