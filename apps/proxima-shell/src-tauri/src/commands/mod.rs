mod cited_blob;
mod engine;
mod inference_targets;
mod inference_test;
mod mcp;
mod models;
mod repo_ingest;
mod repos;
mod tools;
mod ts_types;

pub use ts_types::{IndexReportTs, IngestProgressTs, RepoIngestEventTs};

use tauri_specta::{Builder, collect_commands};

/// Local-session authorization for IPC commands, minted once at boot
/// for the shell's owner and managed in Tauri state. Commands take it
/// as `State<'_, AuthzContext>`, so a request claiming a different
/// owner is `Forbidden` by the engine's owner gate (same behavior the
/// engine-composed resolver enforced before the verb migration).
pub(crate) fn session_authz(owner: &proxima_core::Owner) -> proxima_core::AuthzContext {
    proxima_core::AuthzContext::single_owner(owner, proxima_core::AuthPath::System)
}

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
        inference_test::inference_env_status,
        inference_test::codex_auth_status,
        inference_test::test_inference_target,
        tools::list_mcp_tools,
        tools::list_relations,
        tools::wake_entry_produces,
        mcp::mcp_connection_get,
        mcp::mcp_master_token_rotate,
        cited_blob::cited_blob_upload_prepare,
        cited_blob::cited_blob_upload_complete,
        cited_blob::cited_blob_upload_abort,
        cited_blob::cited_blob_read_url,
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
        repos::code_set_repo_target_branch,
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
