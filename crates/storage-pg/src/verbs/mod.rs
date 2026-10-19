//! Per-verb PG implementations.
//!
//! Each module exposes `pub(crate)` async functions with the same
//! shape as the narrow storage-port methods. The `PgStorage` port
//! impls in `lib.rs` are thin delegation layers over these.

pub(crate) mod active_goals;
pub(crate) mod consolidate;
pub(crate) mod content;
// Nothing in here is API any more: the derive verb is the body of the
// memory-write ports. The module stays visible to the crate only.
pub(crate) mod derive_append;
pub mod fact_embeddings;
pub(crate) mod fact_ingest;
pub mod forget;
pub(crate) mod goal_reads;
pub mod goal_timeseries;
pub(crate) mod goal_write;
pub(crate) mod maintenance;
pub(crate) mod memory_timeseries;
pub(crate) mod owner_erase;
pub(crate) mod owner_export;
pub(crate) mod publication_outbox;
pub mod query;
pub mod query_timeseries;
pub(crate) mod request_keys;
pub(crate) mod series_erase;
pub(crate) mod sketch;
pub(crate) mod source_cursors;
