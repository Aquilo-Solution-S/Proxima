//! Abstract storage trait — backend-neutral surface for the
//! engine.
//!
//! See docs/07-storage.md and AGENTS.md invariants 2, 3, 5.
//! Method signatures land per-verb in subsequent M2 substeps:
//! the trait's role here is to fix the trait-object boundary
//! (`Send + Sync`, `async`-capable via async-fn-in-trait) so
//! the engine can hold `Box<dyn Storage>` regardless of
//! backend.

use std::sync::Arc;

#[derive(Debug, Clone, thiserror::Error)]
pub enum StorageError {
    #[error("storage backend unavailable: {0}")]
    Unavailable(String),
    #[error("constraint violation: {0}")]
    ConstraintViolation(String),
    #[error("not found")]
    NotFound,
    #[error("internal storage error: {0}")]
    Internal(String),
}

/// Backend-neutral persistence boundary. Implementations land
/// in dedicated crates (e.g. `storage-pg`); the trait stays
/// in core so the engine and verbs are backend-agnostic.
///
/// Methods are added per-verb in M2.4+ (EventIngest,
/// GoalWrite, Subscribe, Query). Empty for now.
pub trait Storage: Send + Sync {}

/// Engine-side handle to a Storage impl. `Arc<dyn Storage>`
/// so the engine can clone the handle into the outbox
/// publisher (M2.6) and the Subscribe stream (M2.7) without
/// transferring ownership.
pub type StorageHandle = Arc<dyn Storage>;
