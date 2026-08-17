//! `Query` verb — paginated read starting at `memory_head` / `goal_head`.
//! Payload projection: selected rows hydrate through typed PG sidecar loaders.

use proxima_core::{OwnerRef, OwnerRefKind};

mod citations;
mod code_chunk_vectors;
mod code_series_heads;
mod edges;
mod goals;
mod lineage;
mod memories;
mod projection_sql;
mod rows;
mod search;
mod series_handle;

pub(crate) use citations::{citation_of_fact, facts_citing_object};
pub use code_chunk_vectors::{
    CodeChunkVectorCandidate, CodeChunkVectorFilters, nearest_code_chunk_candidates,
};
pub use code_series_heads::{
    ChunkSeriesHead, FileRevisionHeadRow, owned_chunk_series_heads, owned_file_revision_heads,
    owned_present_chunk_indexes, owned_present_file_revision_heads_except,
    readable_chunk_head_ts_for_file, readable_file_revision_head_ts,
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
pub(crate) use series_handle::{owned_head_handle, sidecar_atoms_from_payload};

pub(crate) fn read_owner_columns(
    read_owners: &[OwnerRef],
) -> (Vec<OwnerRefKind>, Vec<Option<uuid::Uuid>>) {
    crate::access::owner_columns::owner_arrays(read_owners)
}
