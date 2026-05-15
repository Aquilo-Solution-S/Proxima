//! Runtime settings registration.
//!
//! Tables: `proxima_core.inference_targets`,
//! `inference_tier_bindings`, `embedding_models`, `embedding_active`.
//! Inference targets and tier bindings are Owner-scoped. Embedding
//! models and active embedding selection are binary-wide.
//!
//! Not on the `Storage` wire trait — settings are a desktop/admin
//! concern, not a verb in docs/14. Methods are free functions taking
//! `&PgPool`; `PgStorage` exposes thin wrapper methods in lib.rs.

pub mod embedding;
pub mod inference_targets;
pub mod types;

pub use embedding::{
    clear_embedding_active, delete_embedding_model, get_embedding_active, list_embedding_models,
    register_embedding_model, set_embedding_active,
};
pub use inference_targets::{
    bind_inference_tier, list_inference_targets, list_inference_tier_bindings,
    register_inference_target, remove_inference_target, unbind_inference_tier,
};
pub use types::{EmbeddingModel, SettingsError};
