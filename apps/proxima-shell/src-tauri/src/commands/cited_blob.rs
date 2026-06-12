use proxima_blob_s3::{BlobError, CitedBlobStore, S3RuntimeConfig};
use proxima_storage_pg::PgStorage;
use tauri::State;

use crate::command_error::CommandError;

pub use proxima_blob_s3::{
    CitedBlobReadUrlOutcomeTs, CitedBlobReadUrlTs, CitedBlobUploadAbortOutcomeTs,
    CitedBlobUploadAbortTs, CitedBlobUploadCompleteOutcomeTs, CitedBlobUploadCompleteTs,
    CitedBlobUploadPrepareOutcomeTs, CitedBlobUploadPrepareTs,
};

#[tauri::command]
#[specta::specta]
pub async fn cited_blob_upload_prepare(
    req: CitedBlobUploadPrepareTs,
    pg: State<'_, PgStorage>,
) -> Result<CitedBlobUploadPrepareOutcomeTs, CommandError> {
    let store = CitedBlobStore::new(pg.pool().clone(), S3RuntimeConfig::from_env()?);
    Ok(store.prepare_upload(req).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn cited_blob_upload_complete(
    req: CitedBlobUploadCompleteTs,
    pg: State<'_, PgStorage>,
) -> Result<CitedBlobUploadCompleteOutcomeTs, CommandError> {
    let store = CitedBlobStore::new(pg.pool().clone(), S3RuntimeConfig::from_env()?);
    Ok(store.complete_upload(req).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn cited_blob_upload_abort(
    req: CitedBlobUploadAbortTs,
    pg: State<'_, PgStorage>,
) -> Result<CitedBlobUploadAbortOutcomeTs, CommandError> {
    let store = CitedBlobStore::new(pg.pool().clone(), S3RuntimeConfig::from_env()?);
    Ok(store.abort_upload(req).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn cited_blob_read_url(
    req: CitedBlobReadUrlTs,
    pg: State<'_, PgStorage>,
) -> Result<CitedBlobReadUrlOutcomeTs, CommandError> {
    let store = CitedBlobStore::new(pg.pool().clone(), S3RuntimeConfig::from_env()?);
    Ok(store.read_url(req).await?)
}

impl From<BlobError> for CommandError {
    fn from(err: BlobError) -> Self {
        match err {
            BlobError::Config(message) => Self::s3_config(message),
            BlobError::S3(message) => Self::s3(message),
            BlobError::Db(error) => Self::cited_object_upload(error.to_string()),
            BlobError::State(message) => Self::cited_object_upload(message),
        }
    }
}
