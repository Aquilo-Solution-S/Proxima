//! Runtime model registration — settings-backed app config schema.
//!
//! Lives in the desktop shell rather than `core` because TOML-on-disk
//! is a single-user-deployment detail. Multi-tenant deployments
//! (v1.1+) replace this loader with per-`Owner` storage-backed
//! resolution.

pub mod conversions;
pub mod io;
pub mod mutators;
pub mod types;

pub use io::{load_app_config, load_config, save_config, validate_config};
pub use mutators::{clear_embedding_active, register_embedding_model, set_embedding_active};
pub use types::{
    AppConfig, ConfigError, EmbeddingConfig, EmbeddingModelRecord, EmbeddingModelRef,
    InferenceConfig, InferenceTargetRecord, InferenceTierBindings,
};
