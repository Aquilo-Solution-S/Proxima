//! Per-verb PG implementations.
//!
//! Each module exposes a single `pub(crate) async fn` with the same
//! shape as its `Storage` trait method. The `Storage for PgStorage`
//! impl in `lib.rs` is a thin delegation layer over these.

pub(crate) mod active_goals;
pub mod close_batch;
pub mod consolidate;
pub mod derive_append;
pub mod edge_append;
pub(crate) mod entity_owner;
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
