use proxima_core::models::EmbedCaps;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Top-level config parsed from `proxima.config.toml` or loaded from settings tables.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    #[serde(default)]
    pub inference: InferenceConfig,
    #[serde(default)]
    pub embedding: EmbeddingConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(deny_unknown_fields)]
pub struct InferenceConfig {
    #[serde(default)]
    pub targets: Vec<InferenceTargetRecord>,
    #[serde(default)]
    pub inference_tier_bindings: InferenceTierBindings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(deny_unknown_fields)]
pub struct InferenceTierBindings {
    #[serde(default)]
    pub fast: Option<String>,
    #[serde(default)]
    pub standard: Option<String>,
    #[serde(default)]
    pub deep: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, specta::Type)]
#[serde(deny_unknown_fields)]
pub struct InferenceTargetRecord {
    pub target_ref: String,
    pub config: proxima_core::InferenceTargetConfig,
}

impl From<proxima_core::InferenceTargetRow> for InferenceTargetRecord {
    fn from(row: proxima_core::InferenceTargetRow) -> Self {
        Self {
            target_ref: row.target_ref,
            config: row.config,
        }
    }
}

/// Embedding model configuration section.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingConfig {
    #[serde(default)]
    pub models: Vec<EmbeddingModelRecord>,
    /// Globally-active embedding model. v1 is single-global per
    /// docs/10 §Composite embedding selection.
    #[serde(default)]
    pub active: Option<EmbeddingModelRef>,
}

/// A single embedding model entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, specta::Type)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingModelRecord {
    pub vendor: String,
    pub model_id: String,
    pub base_url: String,
    pub caps: EmbedCaps,
    #[serde(default)]
    pub secret_ref: Option<String>,
}

/// Reference to an embedding model by `(vendor, model_id)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, specta::Type)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingModelRef {
    pub vendor: String,
    pub model_id: String,
}

/// Errors from config loading and validation.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Config file could not be read.
    #[error("config IO failed at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// Config file could not be written.
    #[error("config save failed at {path}: {source}")]
    IoSave {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// Config TOML parse failed.
    #[error("config TOML parse failed: {0}")]
    Parse(#[from] toml::de::Error),

    /// TOML serialization failed (writeback path).
    #[error("config TOML serialize failed: {0}")]
    Serialize(#[from] toml::ser::Error),

    /// `[embedding.active]` references a `(vendor, model_id)` not in
    /// `[[embedding.models]]`.
    #[error("active embedding {0:?} not in registered embedding models")]
    UnknownEmbeddingActive(EmbeddingModelRef),

    /// Two `[[embedding.models]]` rows share the same `(vendor, model_id)`.
    #[error("duplicate embedding model {0:?}")]
    DuplicateEmbeddingModel(EmbeddingModelRef),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::models::EmbedCaps;

    fn sample_embedding_model(vendor: &str, model_id: &str, dim: u32) -> EmbeddingModelRecord {
        EmbeddingModelRecord {
            vendor: vendor.to_string(),
            model_id: model_id.to_string(),
            base_url: "https://embed.example.com".to_string(),
            caps: EmbedCaps {
                dim,
                matryoshka: false,
            },
            secret_ref: None,
        }
    }

    #[test]
    fn roundtrip_full_config() {
        let cfg = AppConfig {
            inference: InferenceConfig {
                targets: vec![InferenceTargetRecord {
                    target_ref: "default-chat".to_string(),
                    config: proxima_core::InferenceTargetConfig::MistralChat(
                        proxima_core::MistralChatConfig {
                            base_url: "https://api.mistral.ai".to_string(),
                            model_id: "mistral-medium-3.5".to_string(),
                            api_key_env: "MISTRAL_API_KEY".to_string(),
                            temperature: None,
                            max_completion_tokens: None,
                        },
                    ),
                }],
                inference_tier_bindings: InferenceTierBindings {
                    fast: Some("default-chat".to_string()),
                    standard: None,
                    deep: None,
                },
            },
            embedding: EmbeddingConfig {
                models: vec![sample_embedding_model(
                    "openai",
                    "text-embedding-3-small",
                    1536,
                )],
                active: Some(EmbeddingModelRef {
                    vendor: "openai".to_string(),
                    model_id: "text-embedding-3-small".to_string(),
                }),
            },
        };
        let s = toml::to_string(&cfg).expect("to_string");
        let back: AppConfig = toml::from_str(&s).expect("from_str");
        assert_eq!(cfg, back);
    }

    fn roundtrip_target(config: proxima_core::InferenceTargetConfig, expected_kind: &str) {
        let cfg = AppConfig {
            inference: InferenceConfig {
                targets: vec![InferenceTargetRecord {
                    target_ref: expected_kind.to_string(),
                    config,
                }],
                inference_tier_bindings: InferenceTierBindings::default(),
            },
            embedding: EmbeddingConfig::default(),
        };
        let s = toml::to_string(&cfg).expect("to_string");
        assert!(s.contains(&format!("kind = \"{expected_kind}\"")));
        let back: AppConfig = toml::from_str(&s).expect("from_str");
        assert_eq!(cfg, back);
    }

    #[test]
    fn roundtrip_inference_target_variants() {
        roundtrip_target(
            proxima_core::InferenceTargetConfig::MistralChat(proxima_core::MistralChatConfig {
                base_url: "https://api.mistral.ai".to_string(),
                model_id: "mistral-medium-3.5".to_string(),
                api_key_env: "MISTRAL_API_KEY".to_string(),
                temperature: Some(0.2),
                max_completion_tokens: Some(2048),
            }),
            "mistral_chat",
        );
        roundtrip_target(
            proxima_core::InferenceTargetConfig::OpenAIChat(proxima_core::OpenAIChatConfig {
                base_url: "https://api.openai.com".to_string(),
                model_id: "gpt-4.1".to_string(),
                api_key_env: "OPENAI_API_KEY".to_string(),
                temperature: Some(0.1),
                max_completion_tokens: Some(4096),
            }),
            "openai_chat",
        );
        roundtrip_target(
            proxima_core::InferenceTargetConfig::OpenAIResponses(
                proxima_core::OpenAIResponsesConfig {
                    base_url: "https://api.openai.com".to_string(),
                    model_id: "codex-mini-latest".to_string(),
                    api_key_env: "OPENAI_API_KEY".to_string(),
                    reasoning_effort: Some("medium".to_string()),
                },
            ),
            "openai_responses",
        );
    }

    #[test]
    fn empty_file_parses_to_default() {
        let cfg: AppConfig = toml::from_str("").expect("empty parses");
        assert_eq!(cfg, AppConfig::default());
    }

    #[test]
    fn deny_unknown_fields() {
        let result: Result<AppConfig, _> = toml::from_str(r#"unknown_key = "bad""#);
        assert!(result.is_err());
    }
}
