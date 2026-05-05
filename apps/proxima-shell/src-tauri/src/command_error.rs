//! Serializable error type for Tauri commands.
//!
//! Storage-pg's `SettingsError` and config's `ConfigError` carry
//! Rust-side error info that doesn't cleanly serialize across the
//! Tauri IPC boundary (`sqlx::Error`, `std::io::Error`, toml errors).
//! `CommandError` is the flattened wire shape — derives serde +
//! `specta::Type` — that the frontend sees.

use proxima_core::models::ModelTier;
use proxima_storage_pg::settings::SettingsError;
use serde::Serialize;
use specta::Type;

use crate::config::{ConfigError, ModelRef};

/// Errors returned from settings Tauri commands. Variants flatten
/// the underlying `SettingsError` / `ConfigError` shapes into
/// frontend-friendly typed payloads.
#[derive(Debug, Clone, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum CommandError {
    #[error("storage error: {message}")]
    Storage { message: String },

    #[error("duplicate llm model {model_ref:?}")]
    DuplicateLlmModel { model_ref: ModelRef },

    #[error("duplicate embedding model {model_ref:?}")]
    DuplicateEmbeddingModel { model_ref: ModelRef },

    #[error("unknown llm model {model_ref:?}")]
    UnknownLlmModel { model_ref: ModelRef },

    #[error("unknown embedding model {model_ref:?}")]
    UnknownEmbeddingModel { model_ref: ModelRef },

    /// Tier binding's model fails operator-union caps satisfaction.
    /// Returned by `tier_bind` when the engine's required caps for
    /// the tier exceed the bound model's claimed caps.
    #[error(
        "tier {tier:?} model {model_ref:?} has insufficient caps; \
         required by registered operators"
    )]
    InsufficientTierCaps {
        tier: ModelTier,
        model_ref: ModelRef,
    },

    /// CHECK constraint violation in PG — signals Rust↔SQL drift.
    /// User can't fix; logs to console and reports as bug.
    #[error("settings invariant violation: {message}")]
    Invariant { message: String },

    #[error("invalid repo path {path}: {reason}")]
    InvalidRepoPath { path: String, reason: String },

    #[error("not a git repository: {path}")]
    NotAGitRepo { path: String },

    #[error("repo already registered at canonical path: {canonical_path}")]
    DuplicateRepo { canonical_path: String },

    #[error("unknown repo: {repo_id}")]
    UnknownRepo { repo_id: String },

    #[error("invalid uuid: {value}")]
    InvalidUuid { value: String },
}

impl From<SettingsError> for CommandError {
    fn from(e: SettingsError) -> Self {
        match e {
            SettingsError::Database(err) => Self::Storage {
                message: err.to_string(),
            },
            SettingsError::DuplicateLlmModel { vendor, model_id } => Self::DuplicateLlmModel {
                model_ref: ModelRef { vendor, model_id },
            },
            SettingsError::DuplicateEmbeddingModel { vendor, model_id } => {
                Self::DuplicateEmbeddingModel {
                    model_ref: ModelRef { vendor, model_id },
                }
            }
            SettingsError::UnknownLlmModel { vendor, model_id } => Self::UnknownLlmModel {
                model_ref: ModelRef { vendor, model_id },
            },
            SettingsError::UnknownEmbeddingModel { vendor, model_id } => {
                Self::UnknownEmbeddingModel {
                    model_ref: ModelRef { vendor, model_id },
                }
            }
            SettingsError::Invariant(msg) => Self::Invariant { message: msg },
        }
    }
}

// Note: ConfigError → CommandError is needed for tier_bind which
// runs validate_config before persisting. Cap-insufficient is the
// main case; other ConfigError variants (toml-related) shouldn't
// appear in the command path.
impl From<ConfigError> for CommandError {
    fn from(e: ConfigError) -> Self {
        match e {
            ConfigError::InsufficientTierCaps {
                tier, model_ref, ..
            } => Self::InsufficientTierCaps { tier, model_ref },
            ConfigError::UnknownTierModel { model_ref, .. } => Self::UnknownLlmModel { model_ref },
            ConfigError::UnknownEmbeddingActive(model_ref) => {
                Self::UnknownEmbeddingModel { model_ref }
            }
            ConfigError::DuplicateLlmModel(model_ref) => Self::DuplicateLlmModel { model_ref },
            ConfigError::DuplicateEmbeddingModel(model_ref) => {
                Self::DuplicateEmbeddingModel { model_ref }
            }
            // I/O and TOML errors are not reachable from the command
            // path — config writeback isn't on this code path. If
            // they show up, surface as Storage to keep the union closed.
            other => Self::Storage {
                message: other.to_string(),
            },
        }
    }
}

impl From<proxima_code::RepoRegistryError> for CommandError {
    fn from(e: proxima_code::RepoRegistryError) -> Self {
        match e {
            proxima_code::RepoRegistryError::DuplicatePath { canonical_path } => {
                Self::DuplicateRepo { canonical_path }
            }
            proxima_code::RepoRegistryError::NotFound { repo_id } => Self::UnknownRepo {
                repo_id: repo_id.to_string(),
            },
            proxima_code::RepoRegistryError::RunNotFound { run_id } => Self::Storage {
                message: format!("ingestion run not found: {run_id}"),
            },
            proxima_code::RepoRegistryError::RunAlreadyTerminal { run_id, status } => {
                Self::Storage {
                    message: format!("ingestion run already terminal: {run_id} ({status:?})"),
                }
            }
            proxima_code::RepoRegistryError::Database(e) => Self::Storage {
                message: e.to_string(),
            },
        }
    }
}
