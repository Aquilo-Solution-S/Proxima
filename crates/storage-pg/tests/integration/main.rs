//! Consolidated storage-pg integration tests (single linked binary).
//
// QUARANTINED pending group-ownership port (grant-era access setup):
//   event_ingest_with_sidecar_pg, goal_write_surface_pg — cfg-gated below;
//   port to membership/entity_owner setup when the write path lands (Phase 3).
// DELETED (pure grant/visibility feature, replaced by membership +
// entity-share tests in Phase 6): access_admin_pg, entry_access_pg.

mod change_event_invariants_pg;
mod close_batch_pg;
mod common;
mod connect;
mod derive_append_pg;
mod edge_append_pg;
mod edge_invariants_pg;
mod edge_read_pg;
mod entity_owner_pg;
mod event_history_pg;
mod event_ingest_pg;
#[cfg(any())] // QUARANTINED: grant-era access setup; port in Phase 3
mod event_ingest_with_sidecar_pg;
mod external_agent_constraint_pg;
mod fact_cleanup_pg;
mod fact_embeddings_pg;
mod fact_entity_citations_pg;
mod fact_entity_cleanup_pg;
mod fact_entity_edges_pg;
mod fact_entity_ingest_pg;
mod fact_entity_schema_pg;
mod fact_with_citation_pg;
mod goal_external_authorship_pg;
mod goal_state_transitions_pg;
mod goal_write_pg;
#[cfg(any())] // QUARANTINED: grant-era access setup; port in Phase 3
mod goal_write_surface_pg;
mod lineage_pg;
mod list_active_goals_pg;
mod master_token_personality_pg;
mod migrations;
mod neighbor_redaction_pg;
mod persist_mcp_call;
mod personality_wake_pg;
mod query_pg;
mod read_mcp_call_history_pg;
mod read_scope_pg;
mod search_pg;
mod set_wake_entries_within_pg;
mod sidecar_macro_pg;
mod sql_enums_pg;
mod subject_personality_pg;
