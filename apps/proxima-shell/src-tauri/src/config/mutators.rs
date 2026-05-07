use super::types::{AppConfig, ConfigError, EmbeddingModelRecord, EmbeddingModelRef};

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
        return Err(ConfigError::DuplicateEmbeddingModel(EmbeddingModelRef {
            vendor: record.vendor,
            model_id: record.model_id,
        }));
    }
    cfg.embedding.models.push(record);
    Ok(())
}

/// Set the globally-active embedding model. The referenced model
/// must already be registered in `[[embedding.models]]`.
///
/// # Errors
///
/// `ConfigError::UnknownEmbeddingActive` if `model_ref` is not
/// present in `[[embedding.models]]`.
pub fn set_embedding_active(
    cfg: &mut AppConfig,
    model_ref: EmbeddingModelRef,
) -> Result<(), ConfigError> {
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
    use crate::config::EmbeddingConfig;
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

    #[test]
    fn set_embedding_active_happy_path() {
        let mut cfg = AppConfig {
            embedding: EmbeddingConfig {
                models: vec![sample_embedding_model(
                    "openai",
                    "text-embedding-3-small",
                    1536,
                )],
                active: None,
            },
            ..AppConfig::default()
        };
        let model_ref = EmbeddingModelRef {
            vendor: "openai".to_string(),
            model_id: "text-embedding-3-small".to_string(),
        };
        set_embedding_active(&mut cfg, model_ref.clone()).expect("set active");
        assert_eq!(cfg.embedding.active, Some(model_ref));
    }

    #[test]
    fn set_embedding_active_rejects_unknown() {
        let mut cfg = AppConfig::default();
        let model_ref = EmbeddingModelRef {
            vendor: "openai".to_string(),
            model_id: "text-embedding-3-small".to_string(),
        };
        let err = set_embedding_active(&mut cfg, model_ref).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownEmbeddingActive(_)));
    }

    #[test]
    fn clear_embedding_active_sets_none() {
        let mut cfg = AppConfig {
            embedding: EmbeddingConfig {
                active: Some(EmbeddingModelRef {
                    vendor: "openai".to_string(),
                    model_id: "text-embedding-3-small".to_string(),
                }),
                ..EmbeddingConfig::default()
            },
            ..AppConfig::default()
        };
        clear_embedding_active(&mut cfg);
        assert_eq!(cfg.embedding.active, None);
    }
}
