#![allow(dead_code)]

use async_trait::async_trait;
use proxima_core::llm::{EMBEDDING_DIM, EmbeddingClient, LlmError};
use proxima_core::{Owner, Principal, UserId};

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

#[must_use]
pub fn owner_fixture() -> Owner {
    Principal::User(UserId::new(uuid::Uuid::nil()))
}
