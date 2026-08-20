//! The read lane: one presigned GET, over a locator this store itself wrote.

use proxima_core::{AuthzContext, OwnerRef};
use time::OffsetDateTime;

use super::CitedBlobStore;
use super::dto::{CitedBlobReadUrlOutcomeTs, CitedBlobReadUrlTs};
use super::guards::{ensure_owner_access, format_time, parse_uuid, presign_config};
use super::keys::locator_was_minted_here;
use super::rows::{find_held_blobs, load_blob_location};
use crate::error::BlobError;
use proxima_core::storage_ports::{CitedBlobHeld, MAX_HELD_BLOB_DIGESTS};

impl CitedBlobStore {
    /// Which of `content_hashes` this owner already holds.
    ///
    /// READ AUTHORITY, NOT WRITE. The caller is asking what the corpus
    /// contains, and the answer is metadata about artefacts it may already
    /// read — so this gates exactly like [`Self::read_url`] does. Gating it
    /// as a write instead would refuse a group Viewer an answer it is
    /// entitled to, and would make "may I check before uploading" a stricter
    /// question than "may I download the bytes".
    ///
    /// TOUCHES NO OBJECT STORAGE. It is one indexed Postgres query; nothing
    /// here reaches S3, so a caller can ask cheaply and often.
    ///
    /// # Errors
    /// Returns `BlobError::Denied` when `owner` is not readable under `ctx`,
    /// `BlobError::State` when more than [`MAX_HELD_BLOB_DIGESTS`] digests
    /// are asked about, and `BlobError::Db` on a database fault.
    pub async fn find_held_blobs(
        &self,
        ctx: &AuthzContext,
        owner: OwnerRef,
        content_hashes: &[[u8; 32]],
    ) -> Result<Vec<CitedBlobHeld>, BlobError> {
        ensure_owner_access(ctx, &owner)?;
        if content_hashes.len() > MAX_HELD_BLOB_DIGESTS {
            return Err(BlobError::State(format!(
                "at most {MAX_HELD_BLOB_DIGESTS} content hashes per call; got {}",
                content_hashes.len()
            )));
        }
        // Asking about nothing is answered, not refused. A caller cutting a
        // long digest list into batches reaches an empty tail naturally, and
        // "I hold none of the zero artefacts you named" is both the true
        // answer and the one that saves every such caller a guard.
        if content_hashes.is_empty() {
            return Ok(Vec::new());
        }
        find_held_blobs(&self.pool, &owner, content_hashes).await
    }
    /// Produce a presigned read URL for a completed cited blob.
    ///
    /// # Errors
    /// Returns `BlobError` when the cited object is missing or S3
    /// presigning fails.
    pub async fn read_url(
        &self,
        ctx: &AuthzContext,
        req: CitedBlobReadUrlTs,
    ) -> Result<CitedBlobReadUrlOutcomeTs, BlobError> {
        let owner = req.owner();
        ensure_owner_access(ctx, &owner)?;
        let cited_object_id = parse_uuid(&req.cited_object_id)?;
        let row = load_blob_location(&self.pool, &owner, cited_object_id).await?;
        // The locator columns are client-writable: `core/uploaded-blob-v1`
        // is a registered cited-object schema, so an inline citation can
        // persist an arbitrary bucket/object_key row under the caller's own
        // owner — and SigV4 presigning is offline, with no S3 existence
        // check. Owning the row is therefore NOT enough to be allowed to
        // presign its key: a forged row is owned by the forger. Presign
        // only locators this store itself minted, and answer anything else
        // exactly like a missing row so a probe cannot learn whether the
        // forged row exists.
        if row.bucket != self.config.bucket
            || !locator_was_minted_here(&row.object_key, row.upload_id)
        {
            return Err(BlobError::State("cited object not found for Owner".into()));
        }
        let client = self.client().await?;
        let expires_at = OffsetDateTime::now_utc()
            + time::Duration::seconds(
                i64::try_from(self.config.read_ttl_seconds).unwrap_or(i64::MAX),
            );
        // Force a download disposition and a non-renderable content type on the
        // presigned GET so a blob whose stored mime is `text/html` (or similar)
        // cannot execute in the browser as stored XSS when the link is opened.
        let presigned = client
            .get_object()
            .bucket(&row.bucket)
            .key(&row.object_key)
            .response_content_disposition("attachment")
            .response_content_type("application/octet-stream")
            .presigned(presign_config(self.config.read_ttl_seconds)?)
            .await
            .map_err(|e| BlobError::S3(format!("prepare read URL failed: {e}")))?;
        Ok(CitedBlobReadUrlOutcomeTs {
            read_url: presigned.uri().to_string(),
            expires_at: format_time(expires_at)?,
        })
    }
}

// Forced attachment disposition is only assertable through the store:
// `blob_roundtrip_pg::presigned_put_and_get_carry_the_bytes` (HTTP headers)
// and `prepare_then_complete_then_read_roundtrip` (production URL).
