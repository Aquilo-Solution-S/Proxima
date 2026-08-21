//! Per-verb PG implementations.
//!
//! Each module exposes `pub(crate)` async functions with the same
//! shape as the narrow storage-port methods. The `PgStorage` port
//! impls in `lib.rs` are thin delegation layers over these.

pub(crate) mod active_goals;
pub(crate) mod change_history;
pub(crate) mod compliance_erase;
pub(crate) mod compliance_export;
pub mod consolidate;
pub(crate) mod content;
pub mod derive_append;
pub mod fact_embeddings;
pub mod fact_ingest;
pub mod forget;
pub mod goal_timeseries;
pub(crate) mod goal_wake_candidates;
pub(crate) mod goal_write;
pub mod maintenance;
pub(crate) mod mcp_call_history;
pub mod memory_timeseries;
pub mod persist_mcp_call;
pub mod query;
pub mod query_timeseries;
pub(crate) mod sketch;
pub mod source_cursors;
pub mod wake_timeseries;
