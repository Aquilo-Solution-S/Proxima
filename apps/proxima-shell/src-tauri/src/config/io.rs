use super::types::{
    AppConfig, ConfigError, EmbeddingConfig, EmbeddingModelRecord, LlmConfig, LlmModelRecord,
    ModelRef, TierBindings,
};
use proxima_core::Owner;
use proxima_core::engine::Engine;
use proxima_core::models::ModelTier;
use proxima_storage_pg::{PgStorage, settings};
use std::collections::HashSet;
use std::path::Path;

/// Read all four settings tables and assemble an `AppConfig`.
/// Used at engine boot for `validate_config`, and (later) by
/// Tauri commands that want a single snapshot.
///
/// # Errors
///
/// Returns `SettingsError` from any of the underlying PG calls.
pub async fn load_app_config(
    pg: &PgStorage,
    owner: &Owner,
) -> Result<AppConfig, settings::SettingsError> {
    let llm = pg.list_llm_models(owner).await?;
    let embedding = pg.list_embedding_models(owner).await?;
    let bindings = pg.list_tier_bindings(owner).await?;
    let active = pg.get_embedding_active(owner).await?;

    let mut tiers = TierBindings::default();
    for (tier, vendor, model_id) in bindings {
        let r = ModelRef { vendor, model_id };
        match tier {
            ModelTier::Fast => tiers.fast = Some(r),
            ModelTier::Standard => tiers.standard = Some(r),
            ModelTier::Deep => tiers.deep = Some(r),
        }
    }

    Ok(AppConfig {
        llm: LlmConfig {
            models: llm.into_iter().map(LlmModelRecord::from).collect(),
        },
        embedding: EmbeddingConfig {
            models: embedding
                .into_iter()
                .map(EmbeddingModelRecord::from)
                .collect(),
            active: active.map(|(vendor, model_id)| ModelRef { vendor, model_id }),
        },
        tiers,
    })
}

/// Read TOML from `path` and parse into `AppConfig`. Does not
/// validate — call `validate_config` separately so callers can choose
/// whether validation requires an `Engine` or runs in a config-only
/// mode (e.g. `proxima config check`).
///
/// # Errors
///
/// - `ConfigError::Io` if the file cannot be read.
/// - `ConfigError::Parse` if the TOML payload is malformed or contains
///   unknown fields (top-level and section structs are
///   `deny_unknown_fields`).
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
/// 3. Every operator-used tier has a binding.
/// 4. Every populated `[tiers]` binding refers to a known LLM model.
/// 5. Bound model's caps satisfy `engine.tier_requires_union(tier)`.
/// 6. `[embedding.active]` (if set) refers to a known embedding model.
///
/// # Errors
///
/// Returns the first violated invariant — see `ConfigError` variants
/// for the full set (duplicate model, unknown tier model, insufficient
/// caps, unknown active embedding).
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
        let required = engine.tier_requires_union(tier);
        let Some(model_ref) = config.tiers.get(tier) else {
            if engine.uses_llm_tier(tier) {
                return Err(ConfigError::MissingTierBinding { tier });
            }
            continue;
        };
        let bound = config
            .llm
            .models
            .iter()
            .find(|m| m.vendor == model_ref.vendor && m.model_id == model_ref.model_id);
        let Some(bound) = bound else {
            return Err(ConfigError::UnknownTierModel {
                tier,
                model_ref: model_ref.clone(),
            });
        };
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
/// Writes to `<path>.tmp` first then renames over `<path>` so a
/// crash mid-write cannot leave a half-written config on disk.
/// Caller is responsible for ensuring the parent directory exists.
///
/// # Errors
///
/// - `ConfigError::Serialize` if TOML serialization fails (should
///   not happen for shapes accepted by `validate_config`, but the
///   error path is preserved for completeness).
/// - `ConfigError::IoSave` for any filesystem error (write,
///   rename, sync).
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
    use async_trait::async_trait;
    use proxima_core::auth::NoAuth;
    use proxima_core::ids::{OrgId, UserId};
    use proxima_core::models::{Dialect, EmbedCaps, LlmCaps};
    use proxima_core::personality::{PersonalityFlavor, PersonalitySelfDraft, WakeFilter};
    use proxima_core::verbs::query::MemoryStore;
    use proxima_core::{FlavorRegistry, Owner, Principal, SchemaId, SchemaVersion};
    use uuid::Uuid;

    #[derive(Debug)]
    struct TestOp {
        tier: ModelTier,
        requires: LlmCaps,
    }

    #[async_trait]
    impl PersonalityFlavor for TestOp {
        fn personality_type_id(&self) -> &'static str {
            "test/personality"
        }

        fn self_schema(&self) -> SchemaId {
            SchemaId::new("test/self".into())
        }

        fn default_self_payload(
            &self,
            _owner: &Owner,
            _payload_overrides: Option<&serde_json::Value>,
        ) -> Result<PersonalitySelfDraft, proxima_core::ProtocolError> {
            Ok(PersonalitySelfDraft {
                schema_id: self.self_schema(),
                schema_version: SchemaVersion::new(1),
                text: "test".into(),
                typed_payload: serde_json::json!({}),
            })
        }

        fn system_prompt(&self) -> &'static str {
            "test"
        }

        fn writeable_schemas(&self) -> &'static [&'static str] {
            &[]
        }

        fn writeable_relations(&self) -> &'static [&'static str] {
            &[]
        }

        fn default_wake_filters(&self) -> Vec<WakeFilter> {
            Vec::new()
        }

        fn tier(&self) -> ModelTier {
            self.tier
        }

        fn requires(&self) -> LlmCaps {
            self.requires
        }
    }

    fn engine_with_ops(ops: Vec<TestOp>) -> Engine {
        let mut reg = FlavorRegistry::new();
        reg.add_flavor(proxima_core::FlavorDescriptor {
            flavor_id: "test".to_string(),
            display_name: "Test".to_string(),
            package_version: "0.0.0".to_string(),
            author: None,
            provenance: proxima_core::FlavorProvenance::Builtin,
        });
        for op in ops {
            reg.add_personality(op);
        }
        let reg = reg.freeze();
        let principal = Principal::User(UserId::new(Uuid::now_v7()));
        let owner = Owner {
            principal: principal.clone(),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        Engine::new(
            reg,
            MemoryStore::new(),
            Box::new(NoAuth::new(principal, owner)),
        )
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
                models: vec![sample_embedding_model(
                    "openai",
                    "text-embedding-3-small",
                    1536,
                )],
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
    fn missing_tier_binding_for_used_operator_tier() {
        let cfg = AppConfig {
            llm: LlmConfig {
                models: vec![sample_llm_model("openai", "gpt-4o-mini", LlmCaps::none())],
            },
            ..AppConfig::default()
        };
        let eng = engine_with_ops(vec![TestOp {
            tier: ModelTier::Deep,
            requires: LlmCaps::none(),
        }]);
        let err = validate_config(&cfg, &eng).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::MissingTierBinding {
                tier: ModelTier::Deep
            }
        ));
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
    fn save_config_roundtrip() {
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
        let dir = std::env::temp_dir();
        let path = dir.join(format!("proxima-test-{}.toml", uuid::Uuid::now_v7()));
        save_config(&path, &cfg).expect("save_config");
        let loaded = load_config(&path).expect("load_config");
        assert_eq!(cfg, loaded);
        // Cleanup
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_config_no_stale_tmp() {
        let cfg = AppConfig::default();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("proxima-test-{}.toml", uuid::Uuid::now_v7()));
        save_config(&path, &cfg).expect("save_config");
        let tmp_path = path.with_extension("toml.tmp");
        assert!(!tmp_path.exists(), "stale .tmp file should not exist");
        // Cleanup
        let _ = std::fs::remove_file(&path);
    }
}
