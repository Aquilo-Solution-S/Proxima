//! `Query` verb — paginated read starting at `memory_head` / `goal_head`.
//! Payload projection: selected rows hydrate through typed PG sidecar loaders.

mod citations;
mod code_chunk_vectors;
mod code_series_heads;
mod edges;
mod goals;
mod lineage;
mod memories;
mod rows;
mod search;
mod series_handle;

pub(crate) use citations::citation_of_fact_on_connection;
pub(crate) use citations::facts_citing_object_on_connection;
pub use code_chunk_vectors::{
    CodeChunkVectorCandidate, CodeChunkVectorFilters, nearest_code_chunk_candidates,
    nearest_code_chunk_candidates_on_connection,
};
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use code_series_heads::file_revision_heads_sql_for_tests;
pub use code_series_heads::{
    ChunkSeriesHead, FileRevisionHeadRow, owned_chunk_series_heads, owned_file_revision_heads,
    owned_present_chunk_indexes, owned_present_file_revision_heads_except,
    readable_chunk_head_ts_for_file, readable_file_revision_head_ts,
};
pub use edges::MAX_SNAPSHOT_EDGES;
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use edges::inbound_pin_sql_for_tests;
pub(crate) use edges::{
    load_inbound_pin_nodes_on_connection, load_pin_nodes_on_connection,
    load_visible_goal_ids_on_connection,
};
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use goals::goal_page_sql_for_tests;
pub use goals::{
    ActiveGoalTargetRow, active_goals_for_memory_targets,
    active_goals_for_memory_targets_on_connection,
};
pub(crate) use lineage::walk_memory_lineage_on_connection;
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use lineage::{ancestor_hop_sql_for_tests, descendant_hop_sql_for_tests};
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use memories::memory_page_sql_for_tests;
pub(crate) use memories::query_memories_on_connection;
pub(crate) use rows::read_seq_high_water_on_connection;
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use rows::read_seq_high_water_sql_for_tests;
pub(crate) use search::search_memories_on_connection;
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use search::{
    ranked_projection_sql_for_tests, search_admit_sql_for_tests, semantic_search_sql_for_tests,
    substring_sql_for_tests,
};
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use series_handle::owned_head_handle_sql_for_tests;
pub(crate) use series_handle::{owned_head_handle, owned_head_memory_id, push_atom};

#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn set_hnsw_search_sql_for_tests(tuning: &crate::tuning::PgTuning) -> String {
    crate::pgvector::set_hnsw_search_sql(tuning)
}
