//! `Query` verb — paginated read starting at `memory_head` / `goal_head`.
//! Payload projection: selected rows hydrate through typed PG sidecar loaders.

use proxima_core::{OwnerRef, OwnerRefKind};

mod abstraction_heads;
mod citations;
mod code_chunk_vectors;
mod edges;
mod goals;
mod lineage;
mod memories;
mod rows;
mod search;

pub use abstraction_heads::authorized_code_chunk_head_candidates;
pub(crate) use citations::{citation_of_fact, facts_citing_object};
pub use code_chunk_vectors::{
    CodeChunkVectorCandidate, CodeChunkVectorFilters, nearest_code_chunk_candidates,
};
pub use edges::MAX_SNAPSHOT_EDGES;
pub(crate) use edges::{load_inbound_pin_nodes, load_pin_nodes};
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use goals::goal_page_sql_for_tests;
pub(crate) use lineage::walk_memory_lineage;
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use memories::memory_page_sql_for_tests;
pub(crate) use memories::query_memories;
pub(crate) use rows::read_seq_high_water;
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use rows::read_seq_high_water_sql_for_tests;
pub(crate) use search::search_memories;

pub(crate) fn read_owner_columns(
    read_owners: &[OwnerRef],
) -> (Vec<OwnerRefKind>, Vec<Option<uuid::Uuid>>) {
    crate::access::owner_columns::owner_arrays(read_owners)
}
