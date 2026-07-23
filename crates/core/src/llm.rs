//! Model-client contracts used by substrate provenance.

use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("LLM call failed: {0}")]
    Llm(String),
    #[error("embedding call failed: {0}")]
    Embed(String),
    /// Embedding rejected for a cause retries cannot fix (e.g. input over
    /// the model's token limit — HTTP 400/413/422). Jobs hitting this must
    /// fail terminally instead of burning retry attempts forever.
    #[error("embedding permanently rejected: {0}")]
    EmbedPermanent(String),
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
/// How many texts the drain worker embeds per provider call. Providers'
/// `/embeddings` endpoints accept arrays; batching divides request count
/// (and therefore request-rate-limit pressure) by this factor.
pub const EMBEDDING_BATCH_SIZE: usize = 32;

/// Embedding client surface. Concrete impls live outside core.
#[async_trait]
pub trait EmbeddingClient: Send + Sync + std::fmt::Debug {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError>;

    /// Embed several texts in one provider call, one vector per input, in
    /// input order. The default falls back to sequential single embeds so
    /// existing impls stay correct; batching impls should override.
    async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        let mut out = Vec::with_capacity(texts.len());
        for text in texts {
            out.push(self.embed(text).await?);
        }
        Ok(out)
    }

    fn model_id(&self) -> &str;
    fn dim(&self) -> usize;
}
