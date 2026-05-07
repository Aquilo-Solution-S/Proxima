// Boundary mapping — settings::* (storage-pg row types) ↔
// AppConfig DTO types. Tauri commands at the IPC boundary use
// these From impls to expose the DTO shape to the frontend
// without leaking storage-pg's internal types.

use super::types::EmbeddingModelRecord;
use proxima_storage_pg::settings;

impl From<settings::EmbeddingModel> for EmbeddingModelRecord {
    fn from(m: settings::EmbeddingModel) -> Self {
        EmbeddingModelRecord {
            vendor: m.vendor,
            model_id: m.model_id,
            base_url: m.base_url,
            caps: m.caps,
            secret_ref: m.secret_ref,
        }
    }
}

impl From<EmbeddingModelRecord> for settings::EmbeddingModel {
    fn from(r: EmbeddingModelRecord) -> Self {
        settings::EmbeddingModel {
            vendor: r.vendor,
            model_id: r.model_id,
            base_url: r.base_url,
            caps: r.caps,
            secret_ref: r.secret_ref,
        }
    }
}
