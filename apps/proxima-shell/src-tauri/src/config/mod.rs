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

pub mod conversions;
pub mod io;
pub mod mutators;
pub mod types;

pub use io::{load_app_config, load_config, save_config, validate_config};
pub use mutators::{
    bind_tier, clear_embedding_active, register_embedding_model, register_llm_model,
    set_embedding_active, unbind_tier,
};
pub use types::{
    AppConfig, ConfigError, EmbeddingConfig, EmbeddingModelRecord, LlmConfig, LlmModelRecord,
    ModelRef, TierBindings,
};
