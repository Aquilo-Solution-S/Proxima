mod engine;
mod harness;
mod inference_targets;
mod mcp;
mod models;
mod recipes;
mod repo_ingest;
mod repos;
mod tools;
mod ts_types;

pub use ts_types::{IndexReportTs, IngestProgressTs, RepoIngestEventTs};

use tauri_specta::{Builder, collect_commands};

pub(crate) fn specta_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new().commands(collect_commands![
        // existing wire-protocol commands
        engine::schema,
        engine::query,
        engine::event_history,
        engine::event_ingest,
        engine::goal_write,
        engine::goal_reactivate,
        engine::list_personality_instances,
        engine::list_wake_invocations,
        engine::instantiate_personality,
        engine::set_wake_entries,
        engine::tombstone_personality,
        engine::subscribe,
        // InferenceTarget settings
        inference_targets::register_inference_target,
        inference_targets::list_inference_targets,
        inference_targets::remove_inference_target,
        inference_targets::bind_inference_tier,
        inference_targets::list_inference_tier_bindings,
        harness::detect_local_harness,
        recipes::list_owner_recipes,
        recipes::list_bundled_recipes,
        tools::list_mcp_tools,
        tools::list_workspace_tools,
        tools::list_relations,
        tools::wake_entry_produces,
        mcp::mcp_connection_get,
        mcp::mcp_master_token_rotate,
        // Embedding settings
        models::models_list_embedding,
        models::models_register_embedding,
        models::models_delete_embedding,
        models::embedding_active_get,
        models::embedding_active_set,
        models::embedding_active_clear,
        // repo registry commands (M6.S2)
        repos::repos_list,
        repos::repos_register,
        repos::repos_delete,
        repos::repos_erase,
        repos::repo_ingest_start,
        repos::repo_ingest_status,
        repos::repo_ingest_subscribe,
        ts_types::payload_types_anchor,
        // dev-only perf capture (no-op when PROXIMA_PERF_SESSION_DIR unset)
        crate::perf::fe::perf_log,
        crate::perf::fe::perf_log_field,
    ])
}
