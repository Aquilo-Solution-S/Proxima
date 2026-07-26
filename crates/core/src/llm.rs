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

/// Smallest chunk (in bytes) the chunked-embedding rescue will bisect down
/// to before treating a rejection as genuinely invalid input rather than an
/// over-limit one.
pub const CHUNKED_EMBED_MIN_BYTES: usize = 2048;

/// Split an input the provider rejected as over-limit into pieces it will
/// accept, so the memory lands as one chunked embedding version rather than
/// going terminally un-embedded.
///
/// The provider's rejection does not say *why* the input is invalid, and
/// this never needs to know: an over-limit input starts embedding once its
/// pieces are short enough, while an input rejected for any other reason
/// keeps failing all the way down and the caller sends the job terminal
/// exactly as before. A piece still rejected below
/// [`CHUNKED_EMBED_MIN_BYTES`] is treated as genuinely invalid and aborts
/// the rescue — partial coverage would mask the poison input.
///
/// This lives here, rather than on `Engine`, because there are two drains:
/// the in-process engine worker and `maintain-embeddings --drain`. When only
/// the engine could rescue, whichever drain reached a job first decided
/// whether an over-limit memory was recoverable or permanently dead.
///
/// Returns `Ok(Some(vectors))` (in text order) on rescue, `Ok(None)` when
/// some piece is rejected at every length, and `Err` on the first transient
/// provider error so the caller records an ordinary retryable attempt.
///
/// # Errors
///
/// Returns the first non-`EmbedPermanent` provider error encountered.
pub async fn embed_in_chunks(
    client: &dyn EmbeddingClient,
    text: &str,
) -> Result<Option<Vec<Vec<f32>>>, LlmError> {
    // Depth-first, left-to-right bisection keeps chunk vectors in text
    // order without recursion (async fns don't recurse).
    let mut pending: Vec<&str> = vec![text];
    let mut vectors: Vec<Vec<f32>> = Vec::new();
    while let Some(segment) = pending.pop() {
        match client.embed(segment).await {
            Ok(vector) => vectors.push(vector),
            Err(LlmError::EmbedPermanent(_)) => {
                let mut cut = segment.len() / 2;
                while cut > 0 && !segment.is_char_boundary(cut) {
                    cut -= 1;
                }
                if cut < CHUNKED_EMBED_MIN_BYTES {
                    return Ok(None);
                }
                // Pop order: push right half first so the left half embeds
                // (or splits) next.
                pending.push(&segment[cut..]);
                pending.push(&segment[..cut]);
            }
            Err(err) => return Err(err),
        }
    }
    Ok(Some(vectors))
}
