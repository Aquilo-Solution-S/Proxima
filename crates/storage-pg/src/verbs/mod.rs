//! Per-verb PG implementations.
//!
//! Each module exposes `pub(crate)` async functions with the same
//! shape as the narrow storage-port methods. The `PgStorage` port
//! impls in `lib.rs` are thin delegation layers over these.

pub(crate) mod active_goals;
pub(crate) mod change_history;
pub mod consolidate;
pub(crate) mod content;
// Nothing in here is API any more: the derive verb is the body of the
// memory-write ports. The module stays visible to the crate only.
pub(crate) mod derive_append;
pub mod fact_embeddings;
pub mod fact_ingest;
pub mod forget;
pub mod goal_timeseries;
pub(crate) mod goal_wake_candidates;
pub(crate) mod goal_write;
pub mod maintenance;
pub(crate) mod mcp_call_history;
pub mod memory_timeseries;
pub(crate) mod owner_erase;
pub(crate) mod owner_export;
pub mod query;
pub mod query_timeseries;
pub(crate) mod sketch;
pub mod source_cursors;
pub mod wake_timeseries;
