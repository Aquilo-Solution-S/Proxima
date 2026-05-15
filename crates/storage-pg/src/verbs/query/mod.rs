//! `Query` verb — paginated read of `memories` with optional
//! head filtering. Two head modes (docs/02 §Re-derivation, docs/03
//! §Stateful Fact schemas):
//!
//! - A/P: `NOT EXISTS (m2.supersedes = m.memory_id)` (lineage scan).
//! - Stateful Fact: `NOT EXISTS` of a row under the same NK tuple
//!   with a later `created_at` (head-by-natural-key).
//!
//! `stateful_heads` is set by the engine from the schema registry
//! before dispatch when the request is heads-only and `schema_id`
//! resolves to a stateful Fact schema.
//!
//! Payload projection: for each schema with a sidecar table, we
//! LEFT JOIN the sidecar, project the row into a typed JSON value,
//! then encode the wire payload as CBOR bytes.

mod edges;
mod goals;
mod memories;
mod rows;
mod search;

pub use edges::MAX_SNAPSHOT_EDGES;
pub(crate) use memories::query_memories;
pub(crate) use search::search_memories;
