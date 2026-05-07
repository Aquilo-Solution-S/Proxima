//! Serializable error type for Tauri commands.
//!
//! Storage-pg's `SettingsError` and config's `ConfigError` carry
//! Rust-side error info that doesn't cleanly serialize across the
//! Tauri IPC boundary (`sqlx::Error`, `std::io::Error`, toml errors).
//! `CommandError` is the flattened wire shape — derives serde +
//! `specta::Type` — that the frontend sees.

use proxima_storage_pg::settings::SettingsError;
use serde::Serialize;
use specta::Type;

use crate::config::{ConfigError, EmbeddingModelRef};

/// Errors returned from settings Tauri commands. Variants flatten
/// the underlying `SettingsError` / `ConfigError` shapes into
/// frontend-friendly typed payloads.
#[derive(Debug, Clone, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum CommandError {
    /// Settings storage failed.
    #[error("storage error: {message}")]
    Storage { message: String },

    /// Two `[[embedding.models]]` rows share the same `(vendor, model_id)`.
    #[error("duplicate embedding model {model_ref:?}")]
    DuplicateEmbeddingModel { model_ref: EmbeddingModelRef },

    /// The active embedding model is not registered in `[[embedding.models]]`.
    #[error("unknown embedding model {model_ref:?}")]
    UnknownEmbeddingModel { model_ref: EmbeddingModelRef },

    /// Config file could not be read.
    #[error("config IO failed at {path}: {message}")]
    ConfigIo { path: String, message: String },

    /// Config file could not be written.
    #[error("config save failed at {path}: {message}")]
    ConfigSaveIo { path: String, message: String },

    /// Config TOML did not parse or contains unknown fields.
    #[error("config TOML parse failed: {message}")]
    ConfigParse { message: String },

    /// Config TOML could not be serialized.
    #[error("config TOML serialize failed: {message}")]
    ConfigSerialize { message: String },

    /// CHECK constraint violation in PG signals Rust-to-SQL drift.
    /// User cannot fix this from settings UI; report as a bug.
    #[error("settings invariant violation: {message}")]
    Invariant { message: String },

    /// Repo path does not exist or cannot be canonicalized.
    #[error("invalid repo path {path}: {reason}")]
    InvalidRepoPath { path: String, reason: String },

    /// Repo path is not a Git worktree.
    #[error("not a git repository: {path}")]
    NotAGitRepo { path: String },

    /// Repo canonical path is already registered.
    #[error("repo already registered at canonical path: {canonical_path}")]
    DuplicateRepo { canonical_path: String },

    /// Repo id does not belong to a registered repo.
    #[error("unknown repo: {repo_id}")]
    UnknownRepo { repo_id: String },

    /// UUID string did not parse.
    #[error("invalid uuid: {value}")]
    InvalidUuid { value: String },
}

impl From<SettingsError> for CommandError {
    fn from(e: SettingsError) -> Self {
        match e {
            SettingsError::Database(err) => Self::Storage {
                message: err.to_string(),
            },
            SettingsError::DuplicateEmbeddingModel { vendor, model_id } => {
                Self::DuplicateEmbeddingModel {
                    model_ref: EmbeddingModelRef { vendor, model_id },
                }
            }
            SettingsError::UnknownEmbeddingModel { vendor, model_id } => {
                Self::UnknownEmbeddingModel {
                    model_ref: EmbeddingModelRef { vendor, model_id },
                }
            }
            SettingsError::Conflict(msg) | SettingsError::InUse(msg) => {
                Self::Storage { message: msg }
            }
            SettingsError::Json(err) => Self::Storage {
                message: err.to_string(),
            },
            SettingsError::Invariant(msg) => Self::Invariant { message: msg },
        }
    }
}

impl From<ConfigError> for CommandError {
    fn from(e: ConfigError) -> Self {
        match e {
            ConfigError::UnknownEmbeddingActive(model_ref) => {
                Self::UnknownEmbeddingModel { model_ref }
            }
            ConfigError::DuplicateEmbeddingModel(model_ref) => {
                Self::DuplicateEmbeddingModel { model_ref }
            }
            ConfigError::Io { path, source } => Self::ConfigIo {
                path,
                message: source.to_string(),
            },
            ConfigError::IoSave { path, source } => Self::ConfigSaveIo {
                path,
                message: source.to_string(),
            },
            ConfigError::Parse(err) => Self::ConfigParse {
                message: err.to_string(),
            },
            ConfigError::Serialize(err) => Self::ConfigSerialize {
                message: err.to_string(),
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
