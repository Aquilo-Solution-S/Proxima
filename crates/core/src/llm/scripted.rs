//! Deterministic Anthropic client fixture for dispatcher/tool-loop tests.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::llm::{
    AnthropicClient, ContentBlock, LlmError, MessageRole, MessagesRequest, MessagesResponse, Usage,
};

#[derive(Debug)]
pub struct ScriptedAnthropicClient {
    turns: Mutex<VecDeque<ScriptedTurn>>,
    model_id: String,
}

#[derive(Debug)]
pub enum ScriptedTurn {
    ToolUse {
        tool_id: String,
        args: serde_json::Value,
    },
    EndTurn,
    Error(LlmError),
}

impl ScriptedAnthropicClient {
    #[must_use]
    pub fn new(turns: Vec<ScriptedTurn>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            model_id: "scripted-anthropic".to_string(),
        }
    }

    #[must_use]
    pub fn with_model_id(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = model_id.into();
        self
    }
}

impl ScriptedTurn {
    #[must_use]
    pub fn tool_use(tool_id: impl Into<String>, args: serde_json::Value) -> Self {
        Self::ToolUse {
            tool_id: tool_id.into(),
            args,
        }
    }

    #[must_use]
    pub const fn end_turn() -> Self {
        Self::EndTurn
    }

    #[must_use]
    pub fn error(error: LlmError) -> Self {
        Self::Error(error)
    }
}

#[async_trait]
impl AnthropicClient for ScriptedAnthropicClient {
    async fn messages_create(
        &self,
        _request: MessagesRequest,
    ) -> Result<MessagesResponse, LlmError> {
        let turn = self
            .turns
            .lock()
            .map_err(|_| LlmError::Internal("scripted client mutex poisoned".to_string()))?
            .pop_front()
            .ok_or_else(|| LlmError::Llm("scripted client exhausted".to_string()))?;
        match turn {
            ScriptedTurn::ToolUse { tool_id, args } => Ok(MessagesResponse {
                id: format!("msg_{}", uuid::Uuid::now_v7().simple()),
                model: self.model_id.clone(),
                role: MessageRole::Assistant,
                stop_reason: Some("tool_use".to_string()),
                content: vec![ContentBlock::ToolUse {
                    id: format!("toolu_{}", uuid::Uuid::now_v7().simple()),
                    name: tool_id,
                    input: args,
                }],
                usage: Usage::default(),
            }),
            ScriptedTurn::EndTurn => Ok(MessagesResponse {
                id: format!("msg_{}", uuid::Uuid::now_v7().simple()),
                model: self.model_id.clone(),
                role: MessageRole::Assistant,
                stop_reason: Some("end_turn".to_string()),
                content: vec![ContentBlock::Text {
                    text: "end_turn".to_string(),
                }],
                usage: Usage::default(),
            }),
            ScriptedTurn::Error(error) => Err(error),
        }
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}
