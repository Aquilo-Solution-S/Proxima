//! Per-verb PG implementations.
//!
//! Each module exposes `pub(crate)` async functions with the same
//! shape as the narrow storage-port methods. The `PgStorage` port
//! impls in `lib.rs` are thin delegation layers over these.

pub(crate) mod active_goals;
pub(crate) mod change_history;
pub mod close_batch;
pub mod code_repo_erase;
pub(crate) mod compliance_erase;
pub(crate) mod compliance_export;
pub mod consolidate;
pub mod derive_append;
pub(crate) mod edge_index;
pub mod fact_embeddings;
pub mod fact_ingest;
pub mod fact_ingest_batch;
pub mod fact_retention;
pub(crate) mod goal_wake_candidates;
pub(crate) mod goal_write;
pub(crate) mod lexical_language;
pub(crate) mod mcp_call_history;
pub mod persist_mcp_call;
pub mod query;
pub mod retention_maintenance;
pub mod source_cursors;
