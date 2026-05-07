//! Model-client contracts used by personality dispatch.
//!
//! The substrate owns the typed Anthropic message vocabulary for
//! wake tool loops. Runtime model implementations live outside core,
//! except for the thin Anthropic HTTP client in [`anthropic_http`].

use async_trait::async_trait;

use crate::ModelTier;

pub mod anthropic_http;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod scripted;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("LLM call failed: {0}")]
    Llm(String),
    #[error("embedding call failed: {0}")]
    Embed(String),
    #[error("output validation failed: {0}")]
    OutputValidation(String),
    #[error("internal: {0}")]
    Internal(String),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MessagesResponse {
    pub id: String,
    pub model: String,
    pub role: MessageRole,
    pub stop_reason: Option<String>,
    pub content: Vec<ContentBlock>,
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: serde_json::Value,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Any,
    Tool { name: String },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
}

#[async_trait]
pub trait AnthropicClient: Send + Sync + std::fmt::Debug {
    async fn messages_create(&self, request: MessagesRequest)
    -> Result<MessagesResponse, LlmError>;

    fn model_id_for(&self, tier: ModelTier) -> &str;
}

/// Embedding client surface. Concrete impls live outside core.
#[async_trait]
pub trait EmbeddingClient: Send + Sync + std::fmt::Debug {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError>;

    fn model_id(&self) -> &str;
    fn dim(&self) -> usize;
}

#[must_use]
pub const fn pricing(tier: ModelTier) -> TokenPricing {
    match tier {
        ModelTier::Fast => TokenPricing {
            input_per_million_usd: 0.25,
            output_per_million_usd: 1.25,
        },
        ModelTier::Standard => TokenPricing {
            input_per_million_usd: 3.0,
            output_per_million_usd: 15.0,
        },
        ModelTier::Deep => TokenPricing {
            input_per_million_usd: 15.0,
            output_per_million_usd: 75.0,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenPricing {
    pub input_per_million_usd: f64,
    pub output_per_million_usd: f64,
}

impl TokenPricing {
    #[must_use]
    pub fn cost_usd(self, usage: Usage) -> f64 {
        (f64::from(usage.input_tokens) * self.input_per_million_usd
            + f64::from(usage.output_tokens) * self.output_per_million_usd)
            / 1_000_000.0
    }
}
