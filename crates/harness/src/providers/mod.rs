//! `ProviderClient` trait and provider adapters.

use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::conversation::{AssistantTurn, Conversation, ToolCall, ToolSpec};

// Module visibility is the mechanical enforcement of the "no public
// compat surface" boundary: `chat_completions_wire` is declared with
// crate-only visibility, so nothing it exports — regardless of `pub`
// markers inside — is reachable from outside `proxima-harness`.
// Vendor adapters in this same `providers/` directory access it via
// `super::chat_completions_wire`. Do NOT change this to `pub mod`.
mod chat_completions_wire;
pub mod chatgpt_codex;
pub mod mistral_chat;
pub mod openai_chat;
pub mod openai_responses;
mod responses_wire;

#[doc(hidden)]
pub async fn classify_chat_completions_fixture(
    resp: reqwest::Response,
) -> Result<RoundResult, ProviderError> {
    chat_completions_wire::classify_and_parse(resp).await
}

#[async_trait]
pub trait ProviderClient: Send + Sync {
    async fn tool_round(
        &self,
        conversation: &Conversation,
        tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError>;
}

#[derive(Debug, Clone)]
pub enum RoundResult {
    /// Model wants to call N tools, then continue.
    ToolCalls {
        calls: Vec<ToolCall>,
        raw_assistant: AssistantTurn,
    },
    /// Model finished the turn with text and no tool calls.
    Final {
        text: String,
        raw_assistant: AssistantTurn,
    },
    /// Provider returned a "length"-style `finish_reason` mid-stream.
    LengthCap {
        partial_text: Option<String>,
        raw_assistant: AssistantTurn,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider authentication failed")]
    Auth,
    #[error("provider rate limited request")]
    RateLimited { retry_after: Option<Duration> },
    #[error("provider context length exceeded")]
    ContextLength,
    #[error("provider rejected request: {0}")]
    InvalidRequest(String),
    #[error("provider server error: {0}")]
    ServerError(String),
    #[error("provider network error: {0}")]
    Network(String),
    #[error("provider timed out")]
    Timeout,
    #[error("provider response deserialize error: {0}")]
    Deserialize(String),
}
