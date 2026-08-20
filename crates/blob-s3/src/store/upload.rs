//! The write lane: prepare → stage → finish, with abort as the other exit.
//!
//! These four stay in one file because their contract is an ordering, not a
//! set of independent verbs. `stage_upload` deliberately leaves the pending
//! object in place so a caller whose transaction fails can retry;
//! `finish_upload` is what removes it, and only after the row says the
//! artefact is recorded. Splitting them apart would hide that.

use aws_sdk_s3::primitives::ByteStream;
use proxima_core::citations::UploadedBlobPayload;
use proxima_core::storage_ports::CitedBlobStaged;
use proxima_core::{AuthzContext, OwnerRef};
use time::OffsetDateTime;
use uuid::Uuid;

use super::CitedBlobStore;
use super::digest::hash_uploaded_object;
use super::dto::{
    CitedBlobUploadAbortOutcomeTs, CitedBlobUploadAbortTs, CitedBlobUploadCompleteTs,
    CitedBlobUploadPrepareOutcomeTs, CitedBlobUploadPrepareTs, PresignedHeaderTs,
};
use super::guards::{
    ensure_owner_write_access, format_time, parse_uuid, presign_config, validate_prepare,
};
use super::keys::{canonical_object_key, pending_object_key};
use super::rows::{UploadStatus, load_staged_payload, load_upload, mark_upload_expired};
use super::transitions::{
    AbortTransitionDecision, FinishTransitionDecision, abort_transition_decision,
    finish_transition_decision,
};
use crate::error::BlobError;

impl CitedBlobStore {
    /// Prepare a presigned upload and record its pending row.
    ///
    /// # Errors
    /// Returns `BlobError` when the request is invalid, S3 presigning
    /// fails, or the pending upload row cannot be inserted.
    pub async fn prepare_upload(
        &self,
        ctx: &AuthzContext,
        req: CitedBlobUploadPrepareTs,
    ) -> Result<CitedBlobUploadPrepareOutcomeTs, BlobError> {
        validate_prepare(&req, self.config.max_blob_bytes)?;
        let owner = req.owner();
        ensure_owner_write_access(ctx, &owner)?;
        let upload_id = Uuid::now_v7();
        let object_key = pending_object_key(upload_id);
        let expires_at = OffsetDateTime::now_utc()
            + time::Duration::seconds(
                i64::try_from(self.config.upload_ttl_seconds).unwrap_or(i64::MAX),
            );
        let client = self.client().await?;
        let presigned = client
            .put_object()
            .bucket(&self.config.bucket)
            .key(&object_key)
            .content_type(&req.mime)
            .presigned(presign_config(self.config.upload_ttl_seconds)?)
            .await
            .map_err(|e| BlobError::S3(format!("prepare upload URL failed: {e}")))?;

        let owner_id = super::rows::ensure_owner_row(&self.pool, &owner).await?;
        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads \
                (owner_id, upload_id, \
                 bucket, object_key, filename, mime, expected_byte_len, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(owner_id)
        .bind(upload_id)
        .bind(&self.config.bucket)
        .bind(&object_key)
        .bind(req.filename.trim())
        .bind(req.mime.trim())
        .bind(
            i64::try_from(req.byte_len)
                .map_err(|_| BlobError::State("byte_len exceeds Postgres bigint".into()))?,
        )
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(BlobError::Db)?;

        Ok(CitedBlobUploadPrepareOutcomeTs {
            upload_id: upload_id.to_string(),
            upload_url: presigned.uri().to_string(),
            expires_at: format_time(expires_at)?,
            headers: presigned
                .headers()
                .filter_map(|(name, value)| {
                    if name.eq_ignore_ascii_case("host") {
                        return None;
                    }
                    Some(PresignedHeaderTs {
                        name: name.to_string(),
                        value: value.to_string(),
                    })
                })
                .collect(),
        })
    }

    /// Verify a pending upload's bytes and move them to their canonical
    /// content-addressed key, recording nothing.
    ///
    /// Everything this returns is destined for ONE database transaction
    /// that the caller runs: the cited object, its typed row, and the
    /// Fact that records the arrival. Staging deliberately stops short of
    /// that transaction rather than opening one of its own — which is the
    /// reason completion is split at all.
    ///
    /// The pending object survives this call. It is the only copy a retry
    /// can re-read if the caller's transaction fails; `finish_upload`
    /// removes it once the artefact is recorded.
    ///
    /// # Errors
    /// Returns `BlobError` when the upload is missing, expired,
    /// malformed, or any S3/database operation fails.
    pub async fn stage_upload(
        &self,
        ctx: &AuthzContext,
        req: CitedBlobUploadCompleteTs,
    ) -> Result<CitedBlobStaged, BlobError> {
        let owner = req.owner();
        ensure_owner_write_access(ctx, &owner)?;
        let upload_id = parse_uuid(&req.upload_id)?;
        let row = load_upload(&self.pool, &owner, upload_id).await?;
        match row.status {
            UploadStatus::Completed => {
                let Some(blob_id) = row.blob_id else {
                    return Err(BlobError::State(
                        "completed upload is missing blob_id".into(),
                    ));
                };
                // Already in the corpus. Read back what was stored rather
                // than re-deriving it: the stored row is the truth about
                // this artefact, and re-hashing would need a pending
                // object that `finish_upload` has already deleted.
                return load_staged_payload(&self.pool, &owner, blob_id).await;
            }
            UploadStatus::Aborted => {
                return Err(BlobError::State("upload is aborted".into()));
            }
            UploadStatus::Expired => {
                return Err(BlobError::State("upload is expired".into()));
            }
            UploadStatus::Pending => {}
        }
        if row.expires_at < OffsetDateTime::now_utc() {
            mark_upload_expired(&self.pool, &owner, upload_id).await?;
            return Err(BlobError::State("upload is expired".into()));
        }

        let client = self.client().await?;
        let object = client
            .get_object()
            .bucket(&row.bucket)
            .key(&row.object_key)
            .send()
            .await
            .map_err(|e| BlobError::S3(format!("read pending upload failed: {e}")))?;
        if let Some(len) = object.content_length()
            && len != row.expected_byte_len
        {
            return Err(BlobError::State(format!(
                "uploaded byte length {len} does not match expected {}",
                row.expected_byte_len
            )));
        }

        let streamed = Box::pin(hash_uploaded_object(
            object.body,
            row.expected_byte_len,
            self.config.max_blob_bytes,
        ))
        .await?;
        // Derived from this upload row's own primary key, not from its
        // bytes or its owner: the key the read gate will re-derive and
        // compare against for the life of the row.
        let canonical_key = canonical_object_key(upload_id);
        // GET+PUT, not CopyObject: RustFS (dev S3) acks CopyObject without
        // writing the target, so a later presigned GET 404s.
        let again = client
            .get_object()
            .bucket(&row.bucket)
            .key(&row.object_key)
            .send()
            .await
            .map_err(|e| BlobError::S3(format!("reread pending upload failed: {e}")))?;
        let bytes = again
            .body
            .collect()
            .await
            .map_err(|e| BlobError::S3(format!("buffer pending upload failed: {e}")))?
            .into_bytes();
        let put = client
            .put_object()
            .bucket(&row.bucket)
            .key(&canonical_key)
            .body(ByteStream::from(bytes.to_vec()))
            .send()
            .await
            .map_err(|e| BlobError::S3(format!("put canonical object failed: {e}")))?;
        let etag = put.e_tag().map(ToString::to_string);

        sqlx::query(
            "UPDATE proxima_core.blob_uploads \
                SET object_key = $1, sha256 = $2, etag = $3 \
              WHERE owner_id = $4 AND upload_id = $5 AND status = 'pending'",
        )
        .bind(&canonical_key)
        .bind(&streamed.sha256[..])
        .bind(&etag)
        .bind(owner.stored_owner_id())
        .bind(upload_id)
        .execute(&self.pool)
        .await
        .map_err(BlobError::Db)?;

        Ok(CitedBlobStaged {
            payload: UploadedBlobPayload {
                content_hash: streamed.blake3,
                bucket: row.bucket,
                object_key: canonical_key,
                sha256: streamed.sha256,
                byte_len: streamed.byte_len,
                mime: row.mime,
                filename: row.filename,
                etag,
                uploaded_at: OffsetDateTime::now_utc(),
            },
            already_completed: None,
        })
    }

    /// Close out an upload whose artefact is now recorded: mark the row
    /// completed against `blob_id`, then drop the pending object.
    ///
    /// The order matters. The canonical blob is already referenced by a
    /// committed cited object, so the pending copy is redundant — but only
    /// once the row says so. Deleting first would leave a retry with
    /// nothing to re-read if the mark failed.
    ///
    /// There is no in-process pending-expiry sweep: leftover `pending/`
    /// objects (from a crashed complete, an abandoned prepare, or an
    /// expired upload) MUST be reclaimed by a mandatory S3
    /// lifecycle-expiration rule on the `pending/` prefix (see docs/15
    /// deployment). Do not rely on application code to GC them.
    ///
    /// # Errors
    /// Returns `BlobError` when the upload is missing for `owner`, is not
    /// finishable from its current status, or any S3/database operation
    /// fails.
    pub async fn finish_upload(
        &self,
        ctx: &AuthzContext,
        owner: OwnerRef,
        upload_id: &str,
        blob_id: Uuid,
    ) -> Result<(), BlobError> {
        ensure_owner_write_access(ctx, &owner)?;
        let upload_id = parse_uuid(upload_id)?;
        let row = load_upload(&self.pool, &owner, upload_id).await?;
        let owner_id = owner.stored_owner_id();
        let decision = match row.status {
            UploadStatus::Pending => {
                let rows_affected = sqlx::query(
                    "UPDATE proxima_core.blob_uploads \
                        SET status = 'completed', blob_id = $1, completed_at = now() \
                      WHERE owner_id = $2 \
                        AND upload_id = $3 \
                        AND status = 'pending'",
                )
                .bind(blob_id)
                .bind(owner_id)
                .bind(upload_id)
                .execute(&self.pool)
                .await
                .map_err(BlobError::Db)?
                .rows_affected();
                if rows_affected == 1 {
                    FinishTransitionDecision::WonPending
                } else {
                    // Lost the race against a concurrent abort or finish.
                    // Re-read rather than assume: the row loaded above is
                    // stale, and the caller's write has already committed.
                    let observed = load_upload(&self.pool, &owner, upload_id).await?;
                    finish_transition_decision(observed.status, observed.blob_id, blob_id, 0)?
                }
            }
            status => finish_transition_decision(status, row.blob_id, blob_id, 0)?,
        };

        if decision == FinishTransitionDecision::OvertookTerminal {
            // The artefact and its Fact are committed — `blob_id`
            // exists only because that transaction succeeded — so the
            // transfer's outcome is `completed` no matter what an abort or
            // an expiry sweep wrote while the transaction was in flight.
            // Failing here would report a committed write as failed and
            // leave an upload id the caller can never retry.
            sqlx::query(
                "UPDATE proxima_core.blob_uploads \
                    SET status = 'completed', blob_id = $1, completed_at = now() \
                  WHERE owner_id = $2 \
                    AND upload_id = $3 \
                    AND status IN ('aborted', 'expired')",
            )
            .bind(blob_id)
            .bind(owner_id)
            .bind(upload_id)
            .execute(&self.pool)
            .await
            .map_err(BlobError::Db)?;
        }

        // `stage_upload` rewrites `object_key` to the canonical objects/
        // path. The pending object is always `pending/<upload_id>`.
        let pending_key = pending_object_key(upload_id);
        self.client()
            .await?
            .delete_object()
            .bucket(&row.bucket)
            .key(&pending_key)
            .send()
            .await
            .map_err(|e| BlobError::S3(format!("delete pending upload failed: {e}")))?;
        Ok(())
    }

    /// Abort a pending upload.
    ///
    /// # Errors
    /// Returns `BlobError` when the upload is missing or any S3/database
    /// operation fails.
    pub async fn abort_upload(
        &self,
        ctx: &AuthzContext,
        req: CitedBlobUploadAbortTs,
    ) -> Result<CitedBlobUploadAbortOutcomeTs, BlobError> {
        let owner = req.owner();
        ensure_owner_write_access(ctx, &owner)?;
        let upload_id = parse_uuid(&req.upload_id)?;
        let row = load_upload(&self.pool, &owner, upload_id).await?;
        match row.status {
            UploadStatus::Completed => {
                return Ok(CitedBlobUploadAbortOutcomeTs { aborted: false });
            }
            UploadStatus::Aborted | UploadStatus::Expired => {
                return Ok(CitedBlobUploadAbortOutcomeTs { aborted: true });
            }
            UploadStatus::Pending => {}
        }

        let owner_id = owner.stored_owner_id();
        let rows_affected = sqlx::query(
            "UPDATE proxima_core.blob_uploads \
                SET status = 'aborted', aborted_at = now() \
              WHERE owner_id = $1 \
                AND upload_id = $2 \
                AND status = 'pending'",
        )
        .bind(owner_id)
        .bind(upload_id)
        .execute(&self.pool)
        .await
        .map_err(BlobError::Db)?
        .rows_affected();

        let decision_status = if rows_affected == 0 {
            load_upload(&self.pool, &owner, upload_id).await?.status
        } else {
            row.status
        };
        match abort_transition_decision(decision_status, rows_affected)? {
            AbortTransitionDecision::WonPending => {
                let client = self.client().await?;
                client
                    .delete_object()
                    .bucket(&row.bucket)
                    .key(&row.object_key)
                    .send()
                    .await
                    .map_err(|e| BlobError::S3(format!("delete pending upload failed: {e}")))?;
                Ok(CitedBlobUploadAbortOutcomeTs { aborted: true })
            }
            AbortTransitionDecision::Completed => {
                Ok(CitedBlobUploadAbortOutcomeTs { aborted: false })
            }
            AbortTransitionDecision::AbortedOrExpired => {
                Ok(CitedBlobUploadAbortOutcomeTs { aborted: true })
            }
        }
    }
}
