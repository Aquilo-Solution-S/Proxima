use super::types::{
    AppConfig, ConfigError, EmbeddingConfig, EmbeddingModelRecord, EmbeddingModelRef,
    InferenceConfig, InferenceTargetRecord, InferenceTierBindings,
};
use proxima_core::Owner;
use proxima_core::models::ModelTier;
use proxima_storage_pg::{PgStorage, settings};
use std::collections::HashSet;
use std::path::Path;

/// Read settings tables and assemble an `AppConfig`.
///
/// # Errors
///
/// Returns `SettingsError` from any of the underlying PG calls.
pub async fn load_app_config(
    pg: &PgStorage,
    owner: &Owner,
) -> Result<AppConfig, settings::SettingsError> {
    let inference_targets = settings::list_inference_targets(pg.pool(), owner).await?;
    let inference_tier_bindings = settings::list_inference_tier_bindings(pg.pool(), owner).await?;
    let embedding = pg.list_embedding_models().await?;
    let active = pg.get_embedding_active().await?;

    let mut tiers = InferenceTierBindings::default();
    for binding in inference_tier_bindings {
        match binding.tier {
            ModelTier::Fast => tiers.fast = Some(binding.target_ref),
            ModelTier::Standard => tiers.standard = Some(binding.target_ref),
            ModelTier::Deep => tiers.deep = Some(binding.target_ref),
        }
    }

    Ok(AppConfig {
        inference: InferenceConfig {
            targets: inference_targets
                .into_iter()
                .map(InferenceTargetRecord::from)
                .collect(),
            inference_tier_bindings: tiers,
        },
        embedding: EmbeddingConfig {
            models: embedding
                .into_iter()
                .map(EmbeddingModelRecord::from)
                .collect(),
            active: active.map(|(vendor, model_id)| EmbeddingModelRef { vendor, model_id }),
        },
    })
}

/// Read TOML from `path` and parse into `AppConfig`.
///
/// # Errors
///
/// - `ConfigError::Io` if the file cannot be read.
/// - `ConfigError::Parse` if the TOML payload is malformed or contains
///   unknown fields.
pub fn load_config(path: &Path) -> Result<AppConfig, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let cfg: AppConfig = toml::from_str(&raw)?;
    Ok(cfg)
}

/// Validate config-local embedding references. Inference tier validation
/// happens at wake-entry write and dispatch time.
///
/// # Errors
///
/// Returns duplicate/unknown embedding reference errors.
pub fn validate_config(config: &AppConfig) -> Result<(), ConfigError> {
    let mut embed_seen: HashSet<EmbeddingModelRef> = HashSet::new();
    for model in &config.embedding.models {
        let model_ref = EmbeddingModelRef {
            vendor: model.vendor.clone(),
            model_id: model.model_id.clone(),
        };
        if !embed_seen.insert(model_ref.clone()) {
            return Err(ConfigError::DuplicateEmbeddingModel(model_ref));
        }
    }

    if let Some(active) = &config.embedding.active {
        let known = config
            .embedding
            .models
            .iter()
            .any(|m| m.vendor == active.vendor && m.model_id == active.model_id);
        if !known {
            return Err(ConfigError::UnknownEmbeddingActive(active.clone()));
        }
    }

    Ok(())
}

/// Serialize `cfg` to TOML and write to `path` atomically.
///
/// # Errors
///
/// - `ConfigError::Serialize` if TOML serialization fails.
/// - `ConfigError::IoSave` for any filesystem error.
pub fn save_config(path: &Path, cfg: &AppConfig) -> Result<(), ConfigError> {
    use std::io::Write;
    let body = toml::to_string_pretty(cfg)?;
    let tmp = path.with_extension("toml.tmp");
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| ConfigError::IoSave {
            path: tmp.display().to_string(),
            source: e,
        })?;
        f.write_all(body.as_bytes())
            .map_err(|e| ConfigError::IoSave {
                path: tmp.display().to_string(),
                source: e,
            })?;
        f.sync_all().map_err(|e| ConfigError::IoSave {
            path: tmp.display().to_string(),
            source: e,
        })?;
    }
    std::fs::rename(&tmp, path).map_err(|e| ConfigError::IoSave {
        path: path.display().to_string(),
        source: e,
    })?;
    Ok(())
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
        let err = validate_config(&cfg).unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateEmbeddingModel(_)));
    }

    #[test]
    fn unknown_embedding_active() {
        let cfg = AppConfig {
            embedding: EmbeddingConfig {
                models: vec![sample_embedding_model(
                    "openai",
                    "text-embedding-3-small",
                    1536,
                )],
                active: Some(EmbeddingModelRef {
                    vendor: "anthropic".to_string(),
                    model_id: "claude-3-haiku".to_string(),
                }),
            },
            ..AppConfig::default()
        };
        let err = validate_config(&cfg).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownEmbeddingActive(_)));
    }

    #[test]
    fn empty_config_is_valid() {
        assert!(validate_config(&AppConfig::default()).is_ok());
    }

    #[test]
    fn load_config_io_error() {
        let path = Path::new("/nonexistent/path/to/config.toml");
        let err = load_config(path).unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }));
    }

    #[test]
    fn save_config_roundtrip() {
        let cfg = AppConfig {
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
            ..AppConfig::default()
        };
        let dir = std::env::temp_dir().join(format!("proxima-config-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("proxima.config.toml");
        save_config(&path, &cfg).expect("save");
        let back = load_config(&path).expect("load");
        assert_eq!(cfg, back);
        std::fs::remove_dir_all(&dir).ok();
    }
}
