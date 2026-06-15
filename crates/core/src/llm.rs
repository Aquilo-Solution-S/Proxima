//! Model-client contracts used by substrate provenance.

use async_trait::async_trait;

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

pub trait AnthropicClient: Send + Sync + std::fmt::Debug {
    fn model_id(&self) -> &str;
}

pub const EMBEDDING_DIM: usize = 1024;
pub const EMBEDDING_JOB_MAX_ATTEMPTS: i32 = 5;

/// Embedding client surface. Concrete impls live outside core.
#[async_trait]
pub trait EmbeddingClient: Send + Sync + std::fmt::Debug {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError>;

    fn model_id(&self) -> &str;
    fn dim(&self) -> usize;
}
