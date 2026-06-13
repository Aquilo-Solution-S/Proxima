use proxima_blob_s3::{BlobError, CitedBlobStore, S3RuntimeConfig};
use proxima_core::{AuthzContext, Role};
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
    mut req: CitedBlobUploadPrepareTs,
    authz: State<'_, AuthzContext>,
    pg: State<'_, PgStorage>,
) -> Result<CitedBlobUploadPrepareOutcomeTs, CommandError> {
    stamp_blob_owner(&authz, &mut req, Role::GraphWrite)?;
    let store = CitedBlobStore::new(pg.pool().clone(), S3RuntimeConfig::from_env()?);
    Ok(store.prepare_upload(req).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn cited_blob_upload_complete(
    mut req: CitedBlobUploadCompleteTs,
    authz: State<'_, AuthzContext>,
    pg: State<'_, PgStorage>,
) -> Result<CitedBlobUploadCompleteOutcomeTs, CommandError> {
    stamp_blob_owner(&authz, &mut req, Role::GraphWrite)?;
    let store = CitedBlobStore::new(pg.pool().clone(), S3RuntimeConfig::from_env()?);
    Ok(store.complete_upload(req).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn cited_blob_upload_abort(
    mut req: CitedBlobUploadAbortTs,
    authz: State<'_, AuthzContext>,
    pg: State<'_, PgStorage>,
) -> Result<CitedBlobUploadAbortOutcomeTs, CommandError> {
    stamp_blob_owner(&authz, &mut req, Role::GraphWrite)?;
    let store = CitedBlobStore::new(pg.pool().clone(), S3RuntimeConfig::from_env()?);
    Ok(store.abort_upload(req).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn cited_blob_read_url(
    mut req: CitedBlobReadUrlTs,
    authz: State<'_, AuthzContext>,
    pg: State<'_, PgStorage>,
) -> Result<CitedBlobReadUrlOutcomeTs, CommandError> {
    stamp_blob_owner(&authz, &mut req, Role::GraphRead)?;
    let store = CitedBlobStore::new(pg.pool().clone(), S3RuntimeConfig::from_env()?);
    Ok(store.read_url(req).await?)
}

trait BlobRequestScope {
    fn principal(&self) -> proxima_core::Principal;
    fn stamp_owner(&mut self, owner: proxima_core::Owner);
}

impl BlobRequestScope for CitedBlobUploadPrepareTs {
    fn principal(&self) -> proxima_core::Principal {
        self.principal.clone()
    }

    fn stamp_owner(&mut self, owner: proxima_core::Owner) {
        CitedBlobUploadPrepareTs::stamp_owner(self, owner);
    }
}

impl BlobRequestScope for CitedBlobUploadCompleteTs {
    fn principal(&self) -> proxima_core::Principal {
        self.principal.clone()
    }

    fn stamp_owner(&mut self, owner: proxima_core::Owner) {
        CitedBlobUploadCompleteTs::stamp_owner(self, owner);
    }
}

impl BlobRequestScope for CitedBlobUploadAbortTs {
    fn principal(&self) -> proxima_core::Principal {
        self.principal.clone()
    }

    fn stamp_owner(&mut self, owner: proxima_core::Owner) {
        CitedBlobUploadAbortTs::stamp_owner(self, owner);
    }
}

impl BlobRequestScope for CitedBlobReadUrlTs {
    fn principal(&self) -> proxima_core::Principal {
        self.principal.clone()
    }

    fn stamp_owner(&mut self, owner: proxima_core::Owner) {
        CitedBlobReadUrlTs::stamp_owner(self, owner);
    }
}

fn stamp_blob_owner<T: BlobRequestScope>(
    authz: &AuthzContext,
    req: &mut T,
    role: Role,
) -> Result<(), CommandError> {
    let principal = req.principal();
    if !authz.identity.can_access_principal(&principal) {
        return Err(CommandError::cited_object_upload(
            "principal cannot access requested principal",
        ));
    }
    if !authz.capabilities.roles.has(role) {
        return Err(CommandError::cited_object_upload(role.denied_message()));
    }
    req.stamp_owner(authz.scoped_owner(principal));
    Ok(())
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
