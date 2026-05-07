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
pub(crate) mod event_history;
pub mod event_ingest;
pub(crate) mod goal_write;
pub(crate) mod query;
pub(crate) mod subscribe;
