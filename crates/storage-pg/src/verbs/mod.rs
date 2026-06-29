//! Per-verb PG implementations.
//!
//! Each module exposes `pub(crate)` async functions with the same
//! shape as the narrow storage-port methods. The `PgStorage` port
//! impls in `lib.rs` are thin delegation layers over these.

pub(crate) mod active_goals;
pub mod close_batch;
pub mod consolidate;
pub mod derive_append;
pub mod edge_append;
pub(crate) mod event_history;
pub mod event_ingest;
pub mod fact_cleanup;
pub mod fact_embeddings;
pub mod fact_retention;
pub(crate) mod goal_write;
pub mod hard_delete;
pub mod master_token_personality;
pub(crate) mod mcp_call_history;
pub mod persist_mcp_call;
pub mod query;
pub mod subject_personality;
