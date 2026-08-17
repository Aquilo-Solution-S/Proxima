#![allow(
    dead_code,
    unused_imports,
    clippy::missing_errors_doc,
    clippy::doc_markdown
)]
//! Typed atomic Fact + sidecar writes for the proxima-code flavor.
//!
//! Fact ingest goes through `Engine::ingest_typed_fact_with` (UoW of one).
//! Opaque `CitationSpec` and stateful NK handle reuse live on that lane.
//! Code-slice Abstractions are one `UnitOfWork::author_derived_all`
//! (embed the batch, then one transaction).

pub mod blobs;
pub mod engine;
pub mod heads;
mod pg_sidecars;
pub mod schemas;

pub use blobs::{
    append_code_slice, append_code_slices, append_code_slices_with_handles,
    assign_code_chunk_handles, close_local_git_batch, code_slice_memory_id_for,
    existing_code_chunk_handle, ingest_commit, ingest_file_revision, resolve_code_chunk_handles,
};
pub use engine::{build_engine, build_engine_with};
pub use heads::FileRevisionHead;
pub use schemas::{
    ACCEPTANCE_CRITERIA_OBJECT_SCHEMA, ACCEPTANCE_CRITERIA_WHOLE_SCHEMA,
    ACCEPTANCE_VERIFICATION_OBJECT_SCHEMA, ACCEPTANCE_VERIFICATION_WHOLE_SCHEMA, CODE_BLOB_SCHEMA,
    CODE_BLOB_WHOLE_SCHEMA, CODE_COMMIT_OBJECT_SCHEMA, CODE_COMMIT_WHOLE_SCHEMA,
    EXECUTION_REQUEST_OBJECT_SCHEMA, EXECUTION_REQUEST_WHOLE_SCHEMA,
    EXECUTION_RESULT_OBJECT_SCHEMA, EXECUTION_RESULT_WHOLE_SCHEMA, LOCAL_GIT_SOURCE_ID,
    TEST_REQUEST_OBJECT_SCHEMA, TEST_REQUEST_WHOLE_SCHEMA, TEST_RESULT_OBJECT_SCHEMA,
    TEST_RESULT_WHOLE_SCHEMA, schema_registry, schema_registry_with,
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
