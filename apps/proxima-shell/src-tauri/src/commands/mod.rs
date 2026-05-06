mod engine;
mod models;
mod repo_ingest;
mod repos;
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
        engine::subscribe,
        // settings commands (m6.23)
        models::models_list_llm,
        models::models_list_embedding,
        models::models_register_llm,
        models::models_register_embedding,
        models::models_delete_llm,
        models::models_delete_embedding,
        models::tier_bindings_get,
        models::tier_bind,
        models::tier_unbind,
        models::tier_requires,
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
        // dev-only perf capture (no-op when PROXIMA_PERF_SESSION_DIR unset)
        crate::perf::fe::perf_log,
        crate::perf::fe::perf_log_field,
    ])
}
