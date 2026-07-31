#![allow(dead_code)]

use async_trait::async_trait;
use proxima_core::llm::{EMBEDDING_DIM, EmbeddingClient, LlmError};
use proxima_core::{Owner, OwnerRef, UserId};

#[derive(Debug, Clone)]
pub struct ConstantEmbedding {
    model_id: String,
    vector: Vec<f32>,
}

impl ConstantEmbedding {
    #[must_use]
    pub fn zero(model_id: impl Into<String>) -> Self {
        Self::filled(model_id, 0.0)
    }

    #[must_use]
    pub fn filled(model_id: impl Into<String>, value: f32) -> Self {
        Self {
            model_id: model_id.into(),
            vector: vec![value; EMBEDDING_DIM],
        }
    }

    #[must_use]
    pub fn prefixed(model_id: impl Into<String>, prefix: &[f32]) -> Self {
        let mut vector = vec![0.0; EMBEDDING_DIM];
        let prefix_len = prefix.len().min(EMBEDDING_DIM);
        vector[..prefix_len].copy_from_slice(&prefix[..prefix_len]);
        Self {
            model_id: model_id.into(),
            vector,
        }
    }
}

#[async_trait]
impl EmbeddingClient for ConstantEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(self.vector.clone())
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.vector.len()
    }
}

/// How a [`RefusingEmbedding`] says no.
///
/// The distinction is the whole point of the fixture: only one of these
/// two errors is self-evidently about the input.
#[derive(Debug, Clone, Copy)]
pub enum EmbedRefusal {
    /// [`LlmError::EmbedPermanent`] — the provider read the input and
    /// rejected it (HTTP 400/413/422 on a live provider, or a client-side
    /// `max_input_chars` guard).
    Permanent,
    /// [`LlmError::Embed`] — the ambiguous one. A local runner that an
    /// over-long input *kills* answers `400 {"error": "… EOF"}`, which is
    /// indistinguishable from a runner that was already down, so it is
    /// classified transient. This is the error production actually hit.
    Transient,
}

/// An embedding client that refuses inputs longer than
/// `embeds_up_to_chars` and embeds everything shorter.
///
/// The threshold is what makes the two failure worlds separable, because a
/// liveness probe is a very short input. A non-zero threshold is a provider
/// that is **up** and cannot take one particular text; zero refuses the
/// probe too and is a provider that is **down**. A client that only ever
/// returned errors could not tell those apart, which is exactly the
/// confusion under test.
#[derive(Debug, Clone)]
pub struct RefusingEmbedding {
    model_id: String,
    embeds_up_to_chars: usize,
    refusal: EmbedRefusal,
    vector: Vec<f32>,
}

impl RefusingEmbedding {
    /// A provider that is up but refuses anything longer than
    /// `embeds_up_to_chars`.
    #[must_use]
    pub fn provider_up(
        model_id: impl Into<String>,
        embeds_up_to_chars: usize,
        refusal: EmbedRefusal,
    ) -> Self {
        Self {
            model_id: model_id.into(),
            embeds_up_to_chars,
            refusal,
            vector: vec![0.0; EMBEDDING_DIM],
        }
    }

    /// A provider that refuses every input, liveness probe included.
    #[must_use]
    pub fn provider_down(model_id: impl Into<String>, refusal: EmbedRefusal) -> Self {
        Self::provider_up(model_id, 0, refusal)
    }
}

#[async_trait]
impl EmbeddingClient for RefusingEmbedding {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        let chars = text.chars().count();
        if chars <= self.embeds_up_to_chars {
            return Ok(self.vector.clone());
        }
        let message = format!("refusing {chars} chars");
        Err(match self.refusal {
            EmbedRefusal::Permanent => LlmError::EmbedPermanent(message),
            EmbedRefusal::Transient => LlmError::Embed(message),
        })
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.vector.len()
    }
}

#[must_use]
pub fn owner_fixture() -> Owner {
    OwnerRef::Personal(UserId::new(uuid::Uuid::nil()))
}
