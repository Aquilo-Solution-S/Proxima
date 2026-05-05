#![allow(clippy::missing_errors_doc, clippy::doc_markdown)]
//! Typed atomic Fact + sidecar writes for the proxima-code flavor.
//!
//! Each helper wraps `proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx`
//! and the matching sidecar `INSERT` in a single Postgres transaction. On
//! idempotent replay (event_id collision) the sidecar insert is skipped —
//! the prior transaction already wrote it, and the natural-key uniqueness
//! is by construction (same payload → same event_id).
//!
//! The flavor depends on `proxima-storage-pg` for these helpers; the
//! flavor crate is no longer storage-agnostic post-M3.B.5. That coupling
//! is the v1 trade-off — keeping Fact materialization and sidecar
//! population in one tx is non-negotiable (AGENTS.md invariant 15).

pub mod blobs;
pub mod calls;
pub mod draft;
pub mod engine;
pub mod heads;
pub mod schemas;

pub use blobs::{close_local_git_batch, ingest_code_chunk, ingest_commit, ingest_file_revision};
pub use calls::{CallEdgeDraft, ingest_calls_edge};
pub use engine::build_engine;
pub use heads::{
    FileRevisionHead, file_revision_heads, lookup_present_chunk_memory_id_by_text,
    present_chunk_indexes,
};
pub use schemas::{
    CODE_BLOB_BYTE_RANGE_SCHEMA, CODE_BLOB_SCHEMA, CODE_BLOB_WHOLE_SCHEMA,
    CODE_COMMIT_OBJECT_SCHEMA, CODE_COMMIT_WHOLE_SCHEMA, LOCAL_GIT_SOURCE_ID, schema_registry,
};

use proxima_core::error::ProtocolError;

/// Errors raised by the typed-ingest helpers.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("serialization: {0}")]
    Serialize(String),
    #[error("storage: {0}")]
    Storage(String),
    #[error("protocol: {0}")]
    Protocol(#[from] ProtocolError),
}

impl From<sqlx::Error> for IngestError {
    fn from(e: sqlx::Error) -> Self {
        Self::Storage(e.to_string())
    }
}

impl From<proxima_core::StorageError> for IngestError {
    fn from(e: proxima_core::StorageError) -> Self {
        Self::Storage(e.to_string())
    }
}
