//! `Query` verb — paginated read starting at `memory_head` / `goal_head`.
//! Payload projection: selected rows hydrate through typed PG sidecar loaders.

mod citations;
mod counts;
mod edges;
mod embedding_candidates;
mod goals;
mod lineage;
mod memories;
mod rows;
mod search;
mod series_handle;
mod sidecar_heads;

pub(crate) use citations::citation_of_fact_on_connection;
pub(crate) use citations::facts_citing_object_on_connection;
pub(crate) use counts::count_memories_on_connection;
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use edges::inbound_pin_sql_for_tests;
pub(crate) use edges::{
    load_inbound_pin_nodes_on_connection, load_pin_nodes_on_connection,
    load_visible_goal_ids_on_connection,
};
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use lineage::{ancestor_hop_sql_for_tests, descendant_hop_sql_for_tests};
pub(crate) use lineage::{
    external_edges_on_connection, forgotten_nodes_on_connection, lineage_nodes_on_connection,
    walk_memory_lineage_on_connection,
};
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use memories::memory_page_sql_for_tests;
pub(crate) use memories::query_memories_on_connection;
pub(crate) use search::projection_candidate_statements;
pub(crate) use search::search_memories_on_connection;
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use search::{
    ranked_projection_sql_for_tests, search_admit_sql_for_tests,
    semantic_candidate_admission_sql_for_tests, substring_candidate_sql_for_tests,
    substring_sql_for_tests,
};
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use series_handle::owned_head_handle_sql_for_tests;
pub(crate) use series_handle::{owned_head_handle, owned_head_memory_id, push_atom};
pub(crate) use sidecar_heads::sidecar_heads_by_key_on_connection;

#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn set_hnsw_search_sql_for_tests(tuning: &crate::tuning::PgTuning) -> String {
    crate::pgvector::set_hnsw_search_sql(tuning)
}
