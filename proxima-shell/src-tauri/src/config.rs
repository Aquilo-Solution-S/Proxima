//! Runtime model registration — `proxima.config.toml` schema.
//!
//! Build-time owns the capability vocabulary and operator `requires`
//! (see `proxima_core::models` + `F2AOperator::tier()`/`requires()`);
//! runtime owns `(vendor, model_id)` records, the tier→model bindings,
//! and the `secret_ref` strings used to fetch credentials from the
//! `ResolverRegistry` (see `proxima_core::secrets`).
//!
//! Lives in the desktop shell rather than `core` because TOML-on-disk
//! is a single-user-deployment detail. Multi-tenant deployments
//! (v1.1+) replace this loader with per-`Owner` storage-backed
//! resolution; the engine surface (`tier_requires_union` etc.) stays
//! storage-agnostic in core.
//!
//! Validation (caps, embedding-dim, secret-ref reachability) runs
//! against the loaded config; mismatches are fatal at boot.

use std::collections::HashSet;
use std::path::Path;

use proxima_core::engine::Engine;
use proxima_core::models::{Dialect, EmbedCaps, LlmCaps, ModelTier};
use proxima_core::Owner;
use proxima_storage_pg::{PgStorage, settings};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------
// Boundary mapping — settings::* (storage-pg row types) ↔
// AppConfig DTO types. Tauri commands at the IPC boundary use
// these From impls to expose the DTO shape to the frontend
// without leaking storage-pg's internal types.
// ---------------------------------------------------------------

impl From<settings::LlmModel> for LlmModelRecord {
    fn from(m: settings::LlmModel) -> Self {
        LlmModelRecord {
            vendor: m.vendor,
            model_id: m.model_id,
            dialect: m.dialect,
            base_url: m.base_url,
            caps: m.caps,
            secret_ref: m.secret_ref,
        }
    }
}

impl From<LlmModelRecord> for settings::LlmModel {
    fn from(r: LlmModelRecord) -> Self {
        settings::LlmModel {
            vendor: r.vendor,
            model_id: r.model_id,
            dialect: r.dialect,
            base_url: r.base_url,
            caps: r.caps,
            secret_ref: r.secret_ref,
        }
    }
}

impl From<settings::EmbeddingModel> for EmbeddingModelRecord {
    fn from(m: settings::EmbeddingModel) -> Self {
        EmbeddingModelRecord {
            vendor: m.vendor,
            model_id: m.model_id,
            base_url: m.base_url,
            caps: m.caps,
            secret_ref: m.secret_ref,
        }
    }
}

impl From<EmbeddingModelRecord> for settings::EmbeddingModel {
    fn from(r: EmbeddingModelRecord) -> Self {
        settings::EmbeddingModel {
            vendor: r.vendor,
            model_id: r.model_id,
            base_url: r.base_url,
            caps: r.caps,
            secret_ref: r.secret_ref,
        }
    }
}

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
            models: embedding.into_iter().map(EmbeddingModelRecord::from).collect(),
            active: active.map(|(vendor, model_id)| ModelRef { vendor, model_id }),
        },
        tiers,
    })
}

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
/// 3. Every populated `[tiers]` binding refers to a known LLM model.
/// 4. Bound model's caps satisfy `engine.tier_requires_union(tier)`.
/// 5. `[embedding.active]` (if set) refers to a known embedding model.
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
        f.write_all(body.as_bytes()).map_err(|e| ConfigError::IoSave {
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

/// Append `record` to `[[llm.models]]`. Rejects duplicates on
/// `(vendor, model_id)` to mirror `validate_config`'s rule —
/// failing fast at registration is friendlier than failing at
/// the next validate.
///
/// # Errors
///
/// `ConfigError::DuplicateLlmModel` if a model with the same
/// `(vendor, model_id)` is already registered.
pub fn register_llm_model(
    cfg: &mut AppConfig,
    record: LlmModelRecord,
) -> Result<(), ConfigError> {
    let exists = cfg.llm.models.iter().any(|m| {
        m.vendor == record.vendor && m.model_id == record.model_id
    });
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
    let exists = cfg.embedding.models.iter().any(|m| {
        m.vendor == record.vendor && m.model_id == record.model_id
    });
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
    let known = cfg.llm.models.iter().any(|m| {
        m.vendor == model_ref.vendor && m.model_id == model_ref.model_id
    });
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
pub fn set_embedding_active(
    cfg: &mut AppConfig,
    model_ref: ModelRef,
) -> Result<(), ConfigError> {
    let known = cfg.embedding.models.iter().any(|m| {
        m.vendor == model_ref.vendor && m.model_id == model_ref.model_id
    });
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
    use async_trait::async_trait;
    use proxima_core::auth::NoAuth;
    use proxima_core::ids::{OrgId, UserId};
    use proxima_core::operators::{F2AContext, F2AOperator, NewAbstraction, OperatorError, OperatorRegistry};
    use proxima_core::verbs::query::MemoryStore;
    use proxima_core::verbs::schema::SchemaRegistry;
    use proxima_core::{Owner, Principal, SchemaId};
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

        fn consumes(&self, _: &SchemaId) -> bool {
            true
        }

        async fn run(&self, _: F2AContext<'_>) -> Result<Vec<NewAbstraction>, OperatorError> {
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

    // --- save_config tests ---

    #[test]
    fn save_config_roundtrip() {
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

    // --- register_llm_model tests ---

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
        cfg.llm.models.push(sample_llm_model("openai", "gpt-4o-mini", LlmCaps::none()));
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
        cfg.llm.models.push(sample_llm_model("openai", "gpt-4o-mini", LlmCaps::none()));
        cfg.llm.models.push(sample_llm_model("anthropic", "claude-3-haiku", LlmCaps::none()));
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
