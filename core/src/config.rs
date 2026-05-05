//! Runtime model registration — `proxima.config.toml` schema.
//!
//! Build-time owns the capability vocabulary and operator `requires`
//! (see `models.rs` + `F2AOperator::tier()`/`requires()`); runtime owns
//! `(vendor, model_id)` records, the tier→model bindings, and the
//! `secret_ref` strings used to fetch credentials from the
//! `ResolverRegistry` (see `secrets.rs`).
//!
//! Validation (caps, embedding-dim, secret-ref reachability) runs
//! against the loaded config; mismatches are fatal at boot.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::engine::Engine;
use crate::models::{Dialect, EmbedCaps, LlmCaps, ModelTier};

/// Top-level config parsed from `proxima.config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    #[serde(default)]
    pub models: Vec<LlmModelRecord>,
}

/// A single LLM model entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
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

    /// Config TOML parse failed.
    #[error("config TOML parse failed: {0}")]
    Parse(#[from] toml::de::Error),

    /// A `[tiers]` binding references a `(vendor, model_id)` not present
    /// in `[[llm.models]]`.
    #[error("tier {tier:?} bound to unknown model {model_ref:?}")]
    UnknownTierModel {
        tier: ModelTier,
        model_ref: ModelRef,
    },

    /// `[embedding.active]` references a `(vendor, model_id)` not in
    /// `[[embedding.models]]`.
    #[error("active embedding {0:?} not in registered embedding models")]
    UnknownEmbeddingActive(ModelRef),

    /// Bound model's claimed caps do not satisfy the union of operator
    /// `requires` at this tier.
    #[error("tier {tier:?} model {model_ref:?} caps {have:?} fail to satisfy operator-union {required:?}")]
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

/// Read TOML from `path` and parse into `AppConfig`. Does not
/// validate — call `validate_config` separately so callers can choose
/// whether validation requires an `Engine` or runs in a config-only
/// mode (e.g. `proxima config check`).
pub fn load_config(path: &Path) -> Result<AppConfig, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let cfg: AppConfig = toml::from_str(&raw)?;
    Ok(cfg)
}

/// Validate `config` against the engine's registered operators.
/// Returns the first error encountered. Caller should iterate over
/// fix-it-and-retry workflows; we don't aggregate errors in v1.
///
/// Checks (in order):
/// 1. `[[llm.models]]` is unique on `(vendor, model_id)`.
/// 2. `[[embedding.models]]` is unique on `(vendor, model_id)`.
/// 3. Every populated `[tiers]` binding refers to a known LLM model.
/// 4. Bound model's caps satisfy `engine.tier_requires_union(tier)`.
/// 5. `[embedding.active]` (if set) refers to a known embedding model.
pub fn validate_config(config: &AppConfig, engine: &Engine) -> Result<(), ConfigError> {
    // 1. + 2. uniqueness
    let mut llm_seen: HashSet<ModelRef> = HashSet::new();
    for m in &config.llm.models {
        let r = ModelRef {
            vendor: m.vendor.clone(),
            model_id: m.model_id.clone(),
        };
        if !llm_seen.insert(r.clone()) {
            return Err(ConfigError::DuplicateLlmModel(r));
        }
    }
    let mut embed_seen: HashSet<ModelRef> = HashSet::new();
    for m in &config.embedding.models {
        let r = ModelRef {
            vendor: m.vendor.clone(),
            model_id: m.model_id.clone(),
        };
        if !embed_seen.insert(r.clone()) {
            return Err(ConfigError::DuplicateEmbeddingModel(r));
        }
    }

    // 3. + 4. tier bindings
    for tier in [ModelTier::Fast, ModelTier::Standard, ModelTier::Deep] {
        let Some(model_ref) = config.tiers.get(tier) else { continue };
        let bound = config.llm.models.iter().find(|m| {
            m.vendor == model_ref.vendor && m.model_id == model_ref.model_id
        });
        let Some(bound) = bound else {
            return Err(ConfigError::UnknownTierModel {
                tier,
                model_ref: model_ref.clone(),
            });
        };
        let required = engine.tier_requires_union(tier);
        if !bound.caps.satisfies(&required) {
            return Err(ConfigError::InsufficientTierCaps {
                tier,
                model_ref: model_ref.clone(),
                have: bound.caps,
                required,
            });
        }
    }

    // 5. embedding active
    if let Some(active) = &config.embedding.active {
        let known = config.embedding.models.iter().any(|m| {
            m.vendor == active.vendor && m.model_id == active.model_id
        });
        if !known {
            return Err(ConfigError::UnknownEmbeddingActive(active.clone()));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::NoAuth;
    use crate::ids::{OrgId, UserId};
    use crate::operators::{F2AContext, F2AOperator, OperatorError, OperatorRegistry};
    use crate::verbs::query::MemoryStore;
    use crate::verbs::schema::SchemaRegistry;
    use crate::{Owner, Principal};
    use async_trait::async_trait;
    use uuid::Uuid;

    #[derive(Debug)]
    struct TestOp {
        tier: ModelTier,
        requires: LlmCaps,
    }

    #[async_trait]
    impl F2AOperator for TestOp {
        fn operator_id(&self) -> &'static str {
            "test/op"
        }

        fn output_schema_id(&self) -> &'static str {
            "test/out"
        }

        fn output_schema_version(&self) -> u32 {
            1
        }

        fn prompt_version(&self) -> &'static str {
            "v1"
        }

        fn consumes(&self, _: &crate::SchemaId) -> bool {
            true
        }

        async fn run(&self, _: F2AContext<'_>) -> Result<Vec<crate::operators::NewAbstraction>, OperatorError> {
            Ok(Vec::new())
        }

        fn tier(&self) -> ModelTier {
            self.tier
        }

        fn requires(&self) -> LlmCaps {
            self.requires
        }
    }

    fn engine_with_ops(ops: Vec<TestOp>) -> Engine {
        let mut reg = OperatorRegistry::new();
        for op in ops {
            reg.register_f2a(op);
        }
        let principal = Principal::User(UserId::new(Uuid::now_v7()));
        let owner = Owner {
            principal: principal.clone(),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        Engine::new(
            SchemaRegistry::new(),
            MemoryStore::new(),
            Box::new(NoAuth::new(principal, owner)),
        )
        .with_operators(reg)
    }

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
                models: vec![sample_embedding_model("openai", "text-embedding-3-small", 1536)],
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
    fn duplicate_llm_model() {
        let cfg = AppConfig {
            llm: LlmConfig {
                models: vec![
                    sample_llm_model("openai", "gpt-4o-mini", LlmCaps::none()),
                    sample_llm_model("openai", "gpt-4o-mini", LlmCaps::none()),
                ],
            },
            ..AppConfig::default()
        };
        let eng = engine_with_ops(vec![]);
        let err = validate_config(&cfg, &eng).unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateLlmModel(_)));
    }

    #[test]
    fn duplicate_embedding_model() {
        let cfg = AppConfig {
            embedding: EmbeddingConfig {
                models: vec![
                    sample_embedding_model("openai", "text-embedding-3-small", 1536),
                    sample_embedding_model("openai", "text-embedding-3-small", 1536),
                ],
                ..EmbeddingConfig::default()
            },
            ..AppConfig::default()
        };
        let eng = engine_with_ops(vec![]);
        let err = validate_config(&cfg, &eng).unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateEmbeddingModel(_)));
    }

    #[test]
    fn unknown_tier_model() {
        let cfg = AppConfig {
            llm: LlmConfig {
                models: vec![sample_llm_model("openai", "gpt-4o-mini", LlmCaps::none())],
            },
            tiers: TierBindings {
                fast: Some(ModelRef {
                    vendor: "anthropic".to_string(),
                    model_id: "claude-3-haiku".to_string(),
                }),
                ..TierBindings::default()
            },
            ..AppConfig::default()
        };
        let eng = engine_with_ops(vec![]);
        let err = validate_config(&cfg, &eng).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownTierModel { .. }));
    }

    #[test]
    fn unknown_embedding_active() {
        let cfg = AppConfig {
            embedding: EmbeddingConfig {
                models: vec![sample_embedding_model("openai", "text-embedding-3-small", 1536)],
                active: Some(ModelRef {
                    vendor: "anthropic".to_string(),
                    model_id: "claude-3-haiku".to_string(),
                }),
            },
            ..AppConfig::default()
        };
        let eng = engine_with_ops(vec![]);
        let err = validate_config(&cfg, &eng).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownEmbeddingActive(_)));
    }

    #[test]
    fn caps_insufficient() {
        let cfg = AppConfig {
            llm: LlmConfig {
                models: vec![sample_llm_model(
                    "openai",
                    "gpt-4o-mini",
                    LlmCaps {
                        tool_use: false,
                        ..LlmCaps::none()
                    },
                )],
            },
            tiers: TierBindings {
                standard: Some(ModelRef {
                    vendor: "openai".to_string(),
                    model_id: "gpt-4o-mini".to_string(),
                }),
                ..TierBindings::default()
            },
            ..AppConfig::default()
        };
        // Register an operator that requires tool_use at Standard tier
        let eng = engine_with_ops(vec![TestOp {
            tier: ModelTier::Standard,
            requires: LlmCaps {
                tool_use: true,
                ..LlmCaps::none()
            },
        }]);
        let err = validate_config(&cfg, &eng).unwrap_err();
        assert!(matches!(err, ConfigError::InsufficientTierCaps { .. }));
    }

    #[test]
    fn caps_satisfied() {
        let cfg = AppConfig {
            llm: LlmConfig {
                models: vec![sample_llm_model(
                    "openai",
                    "gpt-4o-mini",
                    LlmCaps {
                        tool_use: true,
                        ..LlmCaps::none()
                    },
                )],
            },
            tiers: TierBindings {
                standard: Some(ModelRef {
                    vendor: "openai".to_string(),
                    model_id: "gpt-4o-mini".to_string(),
                }),
                ..TierBindings::default()
            },
            ..AppConfig::default()
        };
        // Register an operator that requires tool_use at Standard tier
        let eng = engine_with_ops(vec![TestOp {
            tier: ModelTier::Standard,
            requires: LlmCaps {
                tool_use: true,
                ..LlmCaps::none()
            },
        }]);
        assert!(validate_config(&cfg, &eng).is_ok());
    }

    #[test]
    fn empty_engine_any_model_satisfies() {
        let cfg = AppConfig {
            llm: LlmConfig {
                models: vec![sample_llm_model(
                    "openai",
                    "gpt-4o-mini",
                    LlmCaps {
                        tool_use: false,
                        ..LlmCaps::none()
                    },
                )],
            },
            tiers: TierBindings {
                standard: Some(ModelRef {
                    vendor: "openai".to_string(),
                    model_id: "gpt-4o-mini".to_string(),
                }),
                ..TierBindings::default()
            },
            ..AppConfig::default()
        };
        // No operators registered — tier_requires_union returns LlmCaps::none()
        let eng = engine_with_ops(vec![]);
        assert!(validate_config(&cfg, &eng).is_ok());
    }

    #[test]
    fn load_config_io_error() {
        let path = Path::new("/nonexistent/path/to/config.toml");
        let err = load_config(path).unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }));
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
