use proxima_core::models::{Dialect, EmbedCaps, LlmCaps, ModelTier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Top-level config parsed from `proxima.config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub embedding: EmbeddingConfig,
    #[serde(default)]
    pub tiers: TierBindings,
}

/// LLM model configuration section.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    #[serde(default)]
    pub models: Vec<LlmModelRecord>,
}

/// A single LLM model entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, specta::Type)]
#[serde(deny_unknown_fields)]
pub struct LlmModelRecord {
    pub vendor: String,
    pub model_id: String,
    pub dialect: Dialect,
    pub base_url: String,
    pub caps: LlmCaps,
    #[serde(default)]
    pub secret_ref: Option<String>,
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
    pub active: Option<ModelRef>,
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

/// Tier-to-model bindings. Each tier may be unbound (None).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(deny_unknown_fields)]
pub struct TierBindings {
    #[serde(default)]
    pub fast: Option<ModelRef>,
    #[serde(default)]
    pub standard: Option<ModelRef>,
    #[serde(default)]
    pub deep: Option<ModelRef>,
}

impl TierBindings {
    #[must_use]
    pub fn get(&self, tier: ModelTier) -> Option<&ModelRef> {
        match tier {
            ModelTier::Fast => self.fast.as_ref(),
            ModelTier::Standard => self.standard.as_ref(),
            ModelTier::Deep => self.deep.as_ref(),
        }
    }
}

/// Reference to a model by `(vendor, model_id)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, specta::Type)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
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

    /// A `[tiers]` binding references a `(vendor, model_id)` not present
    /// in `[[llm.models]]`.
    #[error("tier {tier:?} bound to unknown model {model_ref:?}")]
    UnknownTierModel {
        tier: ModelTier,
        model_ref: ModelRef,
    },

    /// A registered operator uses this tier, but no runtime binding
    /// exists for it.
    #[error("tier {tier:?} is required by registered operators but has no model binding")]
    MissingTierBinding { tier: ModelTier },

    /// `[embedding.active]` references a `(vendor, model_id)` not in
    /// `[[embedding.models]]`.
    #[error("active embedding {0:?} not in registered embedding models")]
    UnknownEmbeddingActive(ModelRef),

    /// Bound model's claimed caps do not satisfy the union of operator
    /// `requires` at this tier.
    #[error(
        "tier {tier:?} model {model_ref:?} caps {have:?} fail to satisfy operator-union {required:?}"
    )]
    InsufficientTierCaps {
        tier: ModelTier,
        model_ref: ModelRef,
        have: LlmCaps,
        required: LlmCaps,
    },

    /// Two `[[llm.models]]` rows share the same `(vendor, model_id)`.
    #[error("duplicate llm model {0:?}")]
    DuplicateLlmModel(ModelRef),

    /// Two `[[embedding.models]]` rows share the same `(vendor, model_id)`.
    #[error("duplicate embedding model {0:?}")]
    DuplicateEmbeddingModel(ModelRef),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::models::{Dialect, EmbedCaps, LlmCaps, ModelTier};

    fn sample_llm_model(vendor: &str, model_id: &str, caps: LlmCaps) -> LlmModelRecord {
        LlmModelRecord {
            vendor: vendor.to_string(),
            model_id: model_id.to_string(),
            dialect: Dialect::OpenAI,
            base_url: "https://api.example.com".to_string(),
            caps,
            secret_ref: None,
        }
    }

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
            llm: LlmConfig {
                models: vec![sample_llm_model("openai", "gpt-4o-mini", LlmCaps::none())],
            },
            embedding: EmbeddingConfig {
                models: vec![sample_embedding_model(
                    "openai",
                    "text-embedding-3-small",
                    1536,
                )],
                active: Some(ModelRef {
                    vendor: "openai".to_string(),
                    model_id: "text-embedding-3-small".to_string(),
                }),
            },
            tiers: TierBindings {
                fast: Some(ModelRef {
                    vendor: "openai".to_string(),
                    model_id: "gpt-4o-mini".to_string(),
                }),
                standard: None,
                deep: None,
            },
        };
        let s = toml::to_string(&cfg).expect("to_string");
        let back: AppConfig = toml::from_str(&s).expect("from_str");
        assert_eq!(cfg, back);
    }

    #[test]
    fn empty_file_parses_to_default() {
        let s = "";
        let cfg: AppConfig = toml::from_str(s).expect("empty parses");
        assert_eq!(cfg, AppConfig::default());
    }

    #[test]
    fn deny_unknown_fields() {
        let s = r#"unknown_key = "bad""#;
        let result: Result<AppConfig, _> = toml::from_str(s);
        assert!(result.is_err());
    }

    #[test]
    fn tier_bindings_get() {
        let bindings = TierBindings {
            fast: Some(ModelRef {
                vendor: "a".to_string(),
                model_id: "1".to_string(),
            }),
            standard: Some(ModelRef {
                vendor: "b".to_string(),
                model_id: "2".to_string(),
            }),
            deep: None,
        };
        assert_eq!(
            bindings.get(ModelTier::Fast),
            Some(&ModelRef {
                vendor: "a".to_string(),
                model_id: "1".to_string()
            })
        );
        assert_eq!(
            bindings.get(ModelTier::Standard),
            Some(&ModelRef {
                vendor: "b".to_string(),
                model_id: "2".to_string()
            })
        );
        assert_eq!(bindings.get(ModelTier::Deep), None);
    }
}
