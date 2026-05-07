//! Anthropic structured tool-call loop driving a single personality wake.
//!
//! Per spec §Decisions: tool calls, never JSON parsing. The loop bounds
//! drift with three caps — `max_turns`, `max_cost_usd`, and
//! `max_wall_clock`. Tool errors (authorization, decode failures, tool
//! body errors) come back as `tool_result { is_error: true }` blocks
//! and the loop continues. LLM-level errors mark the wake `failed`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::ProtocolError;
use crate::llm::{
    AnthropicClient, ContentBlock, Message, MessageRole, MessagesRequest, ToolDefinition, Usage,
    pricing,
};
use crate::personality::{
    PersonalityFlavor, PersonalityTool, PersonalityToolContext, WakeInvocationStatus,
    authorization::{AuthorizationError, authorize_tool_call},
};

/// Tunable upper bounds on a single wake. Substrate constants per spec.
#[derive(Debug, Clone, Copy)]
pub struct StopConditions {
    pub max_turns: u16,
    pub max_cost_usd: f64,
    pub max_wall_clock: Duration,
}

impl Default for StopConditions {
    fn default() -> Self {
        Self {
            max_turns: 5,
            max_cost_usd: 0.10,
            max_wall_clock: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentLoopOutcome {
    pub status: WakeInvocationStatus,
    pub turn_count: u16,
    pub cost_usd: f64,
    pub elapsed: Duration,
    /// Final assistant text after the loop ended, if any. Diagnostic
    /// only — the substrate-shipped tools are the side-effect channel.
    pub final_text: Option<String>,
}

/// Tool-call loop. The first user turn is the personality's typed wake
/// context as JSON; the system prompt comes from the personality. The
/// loop ends when the model emits no `tool_use`, when any cap trips,
/// or on transport-level error.
///
/// `palette` is the *effective* tool palette (substrate pack + flavor
/// tools) — building the union is the dispatcher's job, not this
/// function's.
pub async fn run_agent_loop(
    anthropic: &dyn AnthropicClient,
    personality: &dyn PersonalityFlavor,
    wake_context_json: serde_json::Value,
    palette: &[Arc<dyn PersonalityTool>],
    tool_ctx: &PersonalityToolContext<'_>,
    stop: StopConditions,
) -> Result<AgentLoopOutcome, ProtocolError> {
    let started = Instant::now();
    let model_id = anthropic.model_id_for(personality.tier()).to_string();
    let tier_pricing = pricing(personality.tier());
    let tools = palette
        .iter()
        .map(|tool| ToolDefinition {
            name: tool.tool_id().to_string(),
            description: tool.description().to_string(),
            input_schema: tool.args_schema(),
        })
        .collect::<Vec<_>>();

    let mut messages: Vec<Message> = vec![Message {
        role: MessageRole::User,
        content: vec![ContentBlock::Text {
            text: wake_context_json.to_string(),
        }],
    }];
    let mut total_cost = 0.0_f64;
    let mut turn_count: u16 = 0;
    let mut final_text: Option<String> = None;
    let max_wall = stop.max_wall_clock;

    loop {
        if turn_count >= stop.max_turns {
            return Ok(AgentLoopOutcome {
                status: WakeInvocationStatus::Truncated,
                turn_count,
                cost_usd: total_cost,
                elapsed: started.elapsed(),
                final_text,
            });
        }
        if started.elapsed() >= max_wall {
            return Ok(AgentLoopOutcome {
                status: WakeInvocationStatus::Truncated,
                turn_count,
                cost_usd: total_cost,
                elapsed: started.elapsed(),
                final_text,
            });
        }
        let request = MessagesRequest {
            model: model_id.clone(),
            system: Some(personality.system_prompt().to_string()),
            messages: messages.clone(),
            tools: tools.clone(),
            tool_choice: None,
            max_tokens: Some(2048),
        };
        let response = match anthropic.messages_create(request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "anthropic messages_create failed");
                return Ok(AgentLoopOutcome {
                    status: WakeInvocationStatus::Failed,
                    turn_count,
                    cost_usd: total_cost,
                    elapsed: started.elapsed(),
                    final_text,
                });
            }
        };
        turn_count = turn_count.saturating_add(1);
        total_cost += tier_pricing.cost_usd(response.usage);
        if total_cost > stop.max_cost_usd {
            return Ok(AgentLoopOutcome {
                status: WakeInvocationStatus::Truncated,
                turn_count,
                cost_usd: total_cost,
                elapsed: started.elapsed(),
                final_text,
            });
        }

        let mut tool_uses = Vec::new();
        let mut text_for_turn: Option<String> = None;
        for block in &response.content {
            match block {
                ContentBlock::Text { text } => {
                    text_for_turn = Some(text.clone());
                }
                ContentBlock::ToolUse { id, name, input } => {
                    tool_uses.push((id.clone(), name.clone(), input.clone()));
                }
                ContentBlock::ToolResult { .. } => {}
            }
        }
        if tool_uses.is_empty() {
            final_text = text_for_turn;
            return Ok(AgentLoopOutcome {
                status: WakeInvocationStatus::Succeeded,
                turn_count,
                cost_usd: total_cost,
                elapsed: started.elapsed(),
                final_text,
            });
        }

        messages.push(Message {
            role: MessageRole::Assistant,
            content: response.content.clone(),
        });

        let mut tool_results: Vec<ContentBlock> = Vec::with_capacity(tool_uses.len());
        for (tool_use_id, tool_name, input) in tool_uses {
            let block = invoke_one_tool(palette, tool_ctx, &tool_use_id, &tool_name, input).await?;
            tool_results.push(block);
        }
        messages.push(Message {
            role: MessageRole::User,
            content: tool_results,
        });
    }
}

async fn invoke_one_tool(
    palette: &[Arc<dyn PersonalityTool>],
    tool_ctx: &PersonalityToolContext<'_>,
    tool_use_id: &str,
    tool_name: &str,
    input: serde_json::Value,
) -> Result<ContentBlock, ProtocolError> {
    if let Err(err) = authorize_tool_call(tool_name, palette) {
        let content = match err {
            AuthorizationError::OutsidePalette { tool_id } => serde_json::json!({
                "error": format!("tool {tool_id} is not in this personality's palette"),
            }),
            other => serde_json::json!({ "error": other.to_string() }),
        };
        return Ok(ContentBlock::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content,
            is_error: true,
        });
    }
    let Some(tool) = palette.iter().find(|t| t.tool_id() == tool_name) else {
        return Ok(ContentBlock::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: serde_json::json!({
                "error": format!("tool {tool_name} not found"),
            }),
            is_error: true,
        });
    };
    match tool.invoke(tool_ctx, input).await {
        Ok(result) => Ok(ContentBlock::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: result.content,
            is_error: result.is_error,
        }),
        Err(e) => Ok(ContentBlock::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: serde_json::json!({
                "error": e.message,
            }),
            is_error: true,
        }),
    }
}

#[allow(dead_code)]
fn _usage_unused_marker(_u: Usage) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::NoAuth;
    use crate::ids::{OrgId, UserId};
    use crate::llm::{LlmError, MessageRole, MessagesResponse, Usage};
    use crate::personality::{
        PersonalityFlavor, PersonalityInstanceId, PersonalitySelfDraft, WakeChainDepth, WakeFilter,
    };
    use crate::verbs::query::MemoryStore;
    use crate::{
        Engine, FlavorRegistry, MemoryId, ModelTier, Owner, Principal, SchemaId, SchemaVersion,
    };
    use async_trait::async_trait;
    use std::sync::Mutex;
    use uuid::Uuid;

    #[derive(Debug)]
    struct ScriptedAnthropic {
        responses: Mutex<Vec<Result<MessagesResponse, LlmError>>>,
    }

    impl ScriptedAnthropic {
        fn new(responses: Vec<Result<MessagesResponse, LlmError>>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl AnthropicClient for ScriptedAnthropic {
        async fn messages_create(
            &self,
            _request: MessagesRequest,
        ) -> Result<MessagesResponse, LlmError> {
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                return Err(LlmError::Llm("scripted client exhausted".into()));
            }
            q.remove(0)
        }

        fn model_id_for(&self, _tier: ModelTier) -> &str {
            "test-model"
        }
    }

    fn fake_response(content: Vec<ContentBlock>, stop_reason: &str) -> MessagesResponse {
        MessagesResponse {
            id: "msg_test".into(),
            model: "test-model".into(),
            role: MessageRole::Assistant,
            stop_reason: Some(stop_reason.into()),
            content,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 20,
            },
        }
    }

    #[derive(Debug, Default)]
    struct DummyPersonality;

    #[async_trait]
    impl PersonalityFlavor for DummyPersonality {
        fn personality_type_id(&self) -> &'static str {
            "test/dummy"
        }
        fn self_schema(&self) -> SchemaId {
            SchemaId::new("test/self".into())
        }
        fn default_self_payload(
            &self,
            _owner: &Owner,
            _payload_overrides: Option<&serde_json::Value>,
        ) -> Result<PersonalitySelfDraft, ProtocolError> {
            Ok(PersonalitySelfDraft {
                schema_id: self.self_schema(),
                schema_version: SchemaVersion::new(1),
                text: "self".into(),
                typed_payload: serde_json::json!({}),
            })
        }
        fn system_prompt(&self) -> &'static str {
            "test"
        }
        fn writeable_schemas(&self) -> &'static [&'static str] {
            &[]
        }
        fn writeable_relations(&self) -> &'static [&'static str] {
            &[]
        }
        fn default_wake_filters(&self) -> Vec<WakeFilter> {
            Vec::new()
        }
    }

    fn engine() -> Engine {
        let principal = Principal::User(UserId::new(Uuid::now_v7()));
        let owner = Owner {
            principal: principal.clone(),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        Engine::new(
            FlavorRegistry::new().freeze(),
            MemoryStore::new(),
            Box::new(NoAuth::new(principal, owner)),
        )
    }

    fn ctx<'a>(
        engine: &'a Engine,
        owner: &'a Owner,
        palette: &'a [Arc<dyn PersonalityTool>],
    ) -> PersonalityToolContext<'a> {
        PersonalityToolContext::new(
            engine,
            owner,
            "test/dummy",
            PersonalityInstanceId::new(Uuid::now_v7()),
            MemoryId::new(Uuid::now_v7()),
            MemoryId::new(Uuid::now_v7()),
            WakeChainDepth::zero(),
            &[],
            &[],
            palette,
        )
    }

    #[tokio::test]
    async fn ends_when_assistant_emits_no_tool_use() {
        let anthropic = ScriptedAnthropic::new(vec![Ok(fake_response(
            vec![ContentBlock::Text {
                text: "all done".into(),
            }],
            "end_turn",
        ))]);
        let engine = engine();
        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let palette: Vec<Arc<dyn PersonalityTool>> = Vec::new();
        let ctx = ctx(&engine, &owner, &palette);
        let outcome = run_agent_loop(
            &anthropic,
            &DummyPersonality,
            serde_json::json!({}),
            &palette,
            &ctx,
            StopConditions::default(),
        )
        .await
        .expect("loop runs");
        assert_eq!(outcome.status, WakeInvocationStatus::Succeeded);
        assert_eq!(outcome.turn_count, 1);
        assert_eq!(outcome.final_text.as_deref(), Some("all done"));
    }

    #[tokio::test]
    async fn truncates_at_max_turns() {
        let resp = || {
            Ok(fake_response(
                vec![ContentBlock::ToolUse {
                    id: "u1".into(),
                    name: "missing/tool".into(),
                    input: serde_json::json!({}),
                }],
                "tool_use",
            ))
        };
        let anthropic = ScriptedAnthropic::new(vec![resp(), resp(), resp()]);
        let engine = engine();
        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let palette: Vec<Arc<dyn PersonalityTool>> = Vec::new();
        let ctx = ctx(&engine, &owner, &palette);
        let outcome = run_agent_loop(
            &anthropic,
            &DummyPersonality,
            serde_json::json!({}),
            &palette,
            &ctx,
            StopConditions {
                max_turns: 2,
                ..StopConditions::default()
            },
        )
        .await
        .expect("loop runs");
        assert_eq!(outcome.status, WakeInvocationStatus::Truncated);
        assert_eq!(outcome.turn_count, 2);
    }

    #[tokio::test]
    async fn fails_on_anthropic_error() {
        let anthropic =
            ScriptedAnthropic::new(vec![Err(LlmError::Llm("rate limited".into()))]);
        let engine = engine();
        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let palette: Vec<Arc<dyn PersonalityTool>> = Vec::new();
        let ctx = ctx(&engine, &owner, &palette);
        let outcome = run_agent_loop(
            &anthropic,
            &DummyPersonality,
            serde_json::json!({}),
            &palette,
            &ctx,
            StopConditions::default(),
        )
        .await
        .expect("loop runs");
        assert_eq!(outcome.status, WakeInvocationStatus::Failed);
    }

    #[tokio::test]
    async fn unknown_tool_returns_tool_error_then_loop_ends() {
        let anthropic = ScriptedAnthropic::new(vec![
            Ok(fake_response(
                vec![ContentBlock::ToolUse {
                    id: "u1".into(),
                    name: "missing/tool".into(),
                    input: serde_json::json!({}),
                }],
                "tool_use",
            )),
            Ok(fake_response(
                vec![ContentBlock::Text {
                    text: "giving up".into(),
                }],
                "end_turn",
            )),
        ]);
        let engine = engine();
        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let palette: Vec<Arc<dyn PersonalityTool>> = Vec::new();
        let ctx = ctx(&engine, &owner, &palette);
        let outcome = run_agent_loop(
            &anthropic,
            &DummyPersonality,
            serde_json::json!({}),
            &palette,
            &ctx,
            StopConditions::default(),
        )
        .await
        .expect("loop runs");
        assert_eq!(outcome.status, WakeInvocationStatus::Succeeded);
        assert_eq!(outcome.turn_count, 2);
    }
}
