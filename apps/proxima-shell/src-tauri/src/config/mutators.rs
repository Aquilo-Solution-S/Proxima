use super::types::{AppConfig, ConfigError, EmbeddingModelRecord, LlmModelRecord, ModelRef};
use proxima_core::models::ModelTier;

/// Append `record` to `[[llm.models]]`. Rejects duplicates on
/// `(vendor, model_id)` to mirror `validate_config`'s rule —
/// failing fast at registration is friendlier than failing at
/// the next validate.
///
/// # Errors
///
/// `ConfigError::DuplicateLlmModel` if a model with the same
/// `(vendor, model_id)` is already registered.
pub fn register_llm_model(cfg: &mut AppConfig, record: LlmModelRecord) -> Result<(), ConfigError> {
    let exists = cfg
        .llm
        .models
        .iter()
        .any(|m| m.vendor == record.vendor && m.model_id == record.model_id);
    if exists {
        return Err(ConfigError::DuplicateLlmModel(ModelRef {
            vendor: record.vendor,
            model_id: record.model_id,
        }));
    }
    cfg.llm.models.push(record);
    Ok(())
}

/// Append `record` to `[[embedding.models]]`. Rejects duplicates on
/// `(vendor, model_id)` to mirror `validate_config`'s rule.
///
/// # Errors
///
/// `ConfigError::DuplicateEmbeddingModel` if a model with the same
/// `(vendor, model_id)` is already registered.
pub fn register_embedding_model(
    cfg: &mut AppConfig,
    record: EmbeddingModelRecord,
) -> Result<(), ConfigError> {
    let exists = cfg
        .embedding
        .models
        .iter()
        .any(|m| m.vendor == record.vendor && m.model_id == record.model_id);
    if exists {
        return Err(ConfigError::DuplicateEmbeddingModel(ModelRef {
            vendor: record.vendor,
            model_id: record.model_id,
        }));
    }
    cfg.embedding.models.push(record);
    Ok(())
}

/// Set the binding for `tier` to `model_ref`. The referenced model
/// must already be registered in `[[llm.models]]`.
///
/// Caps validation runs at `validate_config` time, not here — this
/// function only checks model-ref *reachability* within the config,
/// not whether the bound model is sufficient for the tier's
/// operator-union.
///
/// # Errors
///
/// `ConfigError::UnknownTierModel` if `model_ref` is not present
/// in `[[llm.models]]`.
pub fn bind_tier(
    cfg: &mut AppConfig,
    tier: ModelTier,
    model_ref: ModelRef,
) -> Result<(), ConfigError> {
    let known = cfg
        .llm
        .models
        .iter()
        .any(|m| m.vendor == model_ref.vendor && m.model_id == model_ref.model_id);
    if !known {
        return Err(ConfigError::UnknownTierModel { tier, model_ref });
    }
    match tier {
        ModelTier::Fast => cfg.tiers.fast = Some(model_ref),
        ModelTier::Standard => cfg.tiers.standard = Some(model_ref),
        ModelTier::Deep => cfg.tiers.deep = Some(model_ref),
    }
    Ok(())
}

/// Clear the binding for `tier` (sets it to `None`). No-op if
/// already unbound.
pub fn unbind_tier(cfg: &mut AppConfig, tier: ModelTier) {
    match tier {
        ModelTier::Fast => cfg.tiers.fast = None,
        ModelTier::Standard => cfg.tiers.standard = None,
        ModelTier::Deep => cfg.tiers.deep = None,
    }
}

/// Set the globally-active embedding model. The referenced model
/// must already be registered in `[[embedding.models]]`.
///
/// # Errors
///
/// `ConfigError::UnknownEmbeddingActive` if `model_ref` is not
/// present in `[[embedding.models]]`.
pub fn set_embedding_active(cfg: &mut AppConfig, model_ref: ModelRef) -> Result<(), ConfigError> {
    let known = cfg
        .embedding
        .models
        .iter()
        .any(|m| m.vendor == model_ref.vendor && m.model_id == model_ref.model_id);
    if !known {
        return Err(ConfigError::UnknownEmbeddingActive(model_ref));
    }
    cfg.embedding.active = Some(model_ref);
    Ok(())
}

/// Clear the globally-active embedding model.
pub fn clear_embedding_active(cfg: &mut AppConfig) {
    cfg.embedding.active = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EmbeddingConfig, TierBindings};
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
    fn register_llm_model_happy_path() {
        let mut cfg = AppConfig::default();
        let record = sample_llm_model("openai", "gpt-4o-mini", LlmCaps::none());
        register_llm_model(&mut cfg, record).expect("register");
        assert_eq!(cfg.llm.models.len(), 1);
        assert_eq!(cfg.llm.models[0].vendor, "openai");
        assert_eq!(cfg.llm.models[0].model_id, "gpt-4o-mini");
    }

    #[test]
    fn register_llm_model_rejects_duplicate() {
        let mut cfg = AppConfig::default();
        let record = sample_llm_model("openai", "gpt-4o-mini", LlmCaps::none());
        register_llm_model(&mut cfg, record.clone()).expect("first register");
        let err = register_llm_model(&mut cfg, record).unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateLlmModel(_)));
    }

    // --- register_embedding_model tests ---

    #[test]
    fn register_embedding_model_happy_path() {
        let mut cfg = AppConfig::default();
        let record = sample_embedding_model("openai", "text-embedding-3-small", 1536);
        register_embedding_model(&mut cfg, record).expect("register");
        assert_eq!(cfg.embedding.models.len(), 1);
        assert_eq!(cfg.embedding.models[0].vendor, "openai");
        assert_eq!(cfg.embedding.models[0].model_id, "text-embedding-3-small");
    }

    #[test]
    fn register_embedding_model_rejects_duplicate() {
        let mut cfg = AppConfig::default();
        let record = sample_embedding_model("openai", "text-embedding-3-small", 1536);
        register_embedding_model(&mut cfg, record.clone()).expect("first register");
        let err = register_embedding_model(&mut cfg, record).unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateEmbeddingModel(_)));
    }

    // --- bind_tier tests ---

    #[test]
    fn bind_tier_happy_path() {
        let mut cfg = AppConfig::default();
        cfg.llm
            .models
            .push(sample_llm_model("openai", "gpt-4o-mini", LlmCaps::none()));
        let model_ref = ModelRef {
            vendor: "openai".to_string(),
            model_id: "gpt-4o-mini".to_string(),
        };
        bind_tier(&mut cfg, ModelTier::Fast, model_ref.clone()).expect("bind");
        assert_eq!(cfg.tiers.fast, Some(model_ref));
    }

    #[test]
    fn bind_tier_rejects_unknown_model() {
        let mut cfg = AppConfig::default();
        let model_ref = ModelRef {
            vendor: "openai".to_string(),
            model_id: "gpt-4o-mini".to_string(),
        };
        let err = bind_tier(&mut cfg, ModelTier::Fast, model_ref).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownTierModel { .. }));
    }

    #[test]
    fn bind_tier_overwrites_existing() {
        let mut cfg = AppConfig::default();
        cfg.llm
            .models
            .push(sample_llm_model("openai", "gpt-4o-mini", LlmCaps::none()));
        cfg.llm.models.push(sample_llm_model(
            "anthropic",
            "claude-3-haiku",
            LlmCaps::none(),
        ));
        let first = ModelRef {
            vendor: "openai".to_string(),
            model_id: "gpt-4o-mini".to_string(),
        };
        let second = ModelRef {
            vendor: "anthropic".to_string(),
            model_id: "claude-3-haiku".to_string(),
        };
        bind_tier(&mut cfg, ModelTier::Fast, first.clone()).expect("first bind");
        assert_eq!(cfg.tiers.fast, Some(first));
        bind_tier(&mut cfg, ModelTier::Fast, second.clone()).expect("second bind");
        assert_eq!(cfg.tiers.fast, Some(second));
    }

    // --- unbind_tier tests ---

    #[test]
    fn unbind_tier_clears() {
        let mut cfg = AppConfig {
            tiers: TierBindings {
                fast: Some(ModelRef {
                    vendor: "openai".to_string(),
                    model_id: "gpt-4o-mini".to_string(),
                }),
                standard: None,
                deep: None,
            },
            ..AppConfig::default()
        };
        unbind_tier(&mut cfg, ModelTier::Fast);
        assert_eq!(cfg.tiers.fast, None);
    }

    #[test]
    fn unbind_tier_noop_if_already_none() {
        let mut cfg = AppConfig::default();
        assert_eq!(cfg.tiers.fast, None);
        unbind_tier(&mut cfg, ModelTier::Fast);
        assert_eq!(cfg.tiers.fast, None);
    }

    // --- set_embedding_active tests ---

    #[test]
    fn set_embedding_active_happy_path() {
        let mut cfg = AppConfig::default();
        cfg.embedding.models.push(sample_embedding_model(
            "openai",
            "text-embedding-3-small",
            1536,
        ));
        let model_ref = ModelRef {
            vendor: "openai".to_string(),
            model_id: "text-embedding-3-small".to_string(),
        };
        set_embedding_active(&mut cfg, model_ref.clone()).expect("set active");
        assert_eq!(cfg.embedding.active, Some(model_ref));
    }

    #[test]
    fn set_embedding_active_rejects_unknown() {
        let mut cfg = AppConfig::default();
        let model_ref = ModelRef {
            vendor: "openai".to_string(),
            model_id: "text-embedding-3-small".to_string(),
        };
        let err = set_embedding_active(&mut cfg, model_ref).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownEmbeddingActive(_)));
    }

    // --- clear_embedding_active tests ---

    #[test]
    fn clear_embedding_active_clears() {
        let mut cfg = AppConfig {
            embedding: EmbeddingConfig {
                models: vec![],
                active: Some(ModelRef {
                    vendor: "openai".to_string(),
                    model_id: "text-embedding-3-small".to_string(),
                }),
            },
            ..AppConfig::default()
        };
        clear_embedding_active(&mut cfg);
        assert_eq!(cfg.embedding.active, None);
    }
}
