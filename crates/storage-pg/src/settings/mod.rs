//! Per-Owner settings registration — runtime model/tier/embedding
//! state. Backs the desktop-shell Tauri commands at S1.e.2.
//!
//! Tables: `proxima_core.llm_models`, `embedding_models`,
//! `tier_bindings`, `embedding_active` (migration m6.20).
//!
//! Not on the `Storage` wire trait — settings are a desktop/admin
//! concern, not a verb in docs/14. Methods are free functions taking
//! `&PgPool`; `PgStorage` exposes thin wrapper methods in lib.rs.

pub mod bindings;
pub mod embedding;
pub mod llm;
pub mod types;

pub use bindings::{bind_tier, list_tier_bindings, unbind_tier};
pub use embedding::{
    clear_embedding_active, delete_embedding_model, get_embedding_active, list_embedding_models,
    register_embedding_model, set_embedding_active,
};
pub use llm::{delete_llm_model, list_llm_models, register_llm_model};
pub use types::{EmbeddingModel, LlmModel, SettingsError};
