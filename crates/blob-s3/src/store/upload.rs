//! The write lane: prepare → stage → finish, with abort as the other exit.
//!
//! These four stay in one file because their contract is an ordering, not a
//! set of independent verbs. `stage_upload` records the canonical locator
//! and retires the redundant pending transfer copy; `finish_upload` repeats
//! that cleanup after the corpus transaction records the artefact. Splitting
//! them apart would hide that.

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
    ensure_owner_write_access, format_time, parse_opaque_identifier, presign_config,
    validate_prepare,
};
use super::keys::{canonical_object_key, pending_object_key};
use super::rows::{UploadStatus, load_staged_payload, load_upload, mark_upload_expired};
use super::transitions::{
    AbortTransitionDecision, FinishTransitionDecision, StageLocatorDecision,
    TerminalLocatorRepairDecision, abort_transition_decision, finish_transition_decision,
    stage_locator_decision, terminal_locator_repair_decision,
};
use crate::error::BlobError;

enum ObjectReadError {
    Missing,
    Other(BlobError),
}

impl ObjectReadError {
    fn into_blob_error(self) -> BlobError {
        match self {
            Self::Missing => BlobError::S3("object not found".into()),
            Self::Other(error) => error,
        }
    }
}

/// Read one bounded response and derive both digests, its true length, and
/// the exact bytes used for a later conditional publication.
async fn read_hashed_object(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
    expected_byte_len: i64,
    max_blob_bytes: Option<u64>,
) -> Result<(super::digest::StreamedObject, Option<String>), ObjectReadError> {
    let object = client
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|error| {
            let status = error
                .raw_response()
                .map(|response| response.status().as_u16());
            if status == Some(404)
                || error
                    .as_service_error()
                    .is_some_and(aws_sdk_s3::operation::get_object::GetObjectError::is_no_such_key)
            {
                ObjectReadError::Missing
            } else {
                ObjectReadError::Other(BlobError::S3(format!("read upload object failed: {error}")))
            }
        })?;
    let etag = object.e_tag().map(ToString::to_string);
    let streamed = Box::pin(hash_uploaded_object(
        object.body,
        expected_byte_len,
        max_blob_bytes,
    ))
    .await
    .map_err(ObjectReadError::Other)?;
    Ok((streamed, etag))
}

fn verify_canonical_candidate(
    candidate: &super::digest::StreamedObject,
    existing: &super::digest::StreamedObject,
) -> Result<(), BlobError> {
    if candidate.blake3 != existing.blake3
        || candidate.sha256 != existing.sha256
        || candidate.byte_len != existing.byte_len
    {
        return Err(BlobError::State(
            "canonical object conflicts with staged bytes".into(),
        ));
    }
    Ok(())
}

/// Whether an S3 provider rejected the conditional canonical publication
/// because another writer already owns the key. Providers vary in whether
/// they preserve the HTTP status, the service code, or both; only these
/// documented conflict signals may enter adoption/verification. Treating an
/// unrelated provider error as a race would otherwise hide a real outage.
fn is_conditional_publication_conflict(status: Option<u16>, code: Option<&str>) -> bool {
    // RustFS and S3 document these pairs for an If-None-Match failure. A
    // bare 409 is deliberately not enough: providers also use it for
    // unrelated request conflicts, which must remain a hard failure. Some
    // compatible providers omit the HTTP status or service code.
    matches!(
        (status, code),
        (Some(409), Some("ConditionalRequestConflict"))
            | (Some(412), Some("PreconditionFailed") | None)
            | (
                None,
                Some("ConditionalRequestConflict" | "PreconditionFailed")
            )
    )
}

/// Publish a candidate without ever overwriting a canonical object. The
/// conditional PUT closes the race between concurrent stages. Recognized
/// precondition/conditional-conflict responses enter canonical verification;
/// no provider error falls back to an unconditional write.
async fn publish_canonical(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    canonical_key: &str,
    candidate: &super::digest::StreamedObject,
    max_blob_bytes: Option<u64>,
) -> Result<(String, Option<String>), BlobError> {
    let result = client
        .put_object()
        .bucket(bucket)
        .key(canonical_key)
        .if_none_match("*")
        .body(ByteStream::from(candidate.bytes.clone()))
        .send()
        .await;
    match result {
        Ok(output) => Ok((
            canonical_key.to_owned(),
            output.e_tag().map(ToString::to_string),
        )),
        Err(error) => {
            let status = error
                .raw_response()
                .map(|response| response.status().as_u16());
            let code = error
                .as_service_error()
                .and_then(|service| service.meta().code());
            if is_conditional_publication_conflict(status, code) {
                let (existing, etag) = read_hashed_object(
                    client,
                    bucket,
                    canonical_key,
                    i64::try_from(candidate.byte_len).unwrap_or(i64::MAX),
                    max_blob_bytes,
                )
                .await
                .map_err(|error| match error {
                    ObjectReadError::Missing => BlobError::State(
                        "canonical object disappeared during conditional publication".into(),
                    ),
                    ObjectReadError::Other(error) => error,
                })?;
                verify_canonical_candidate(candidate, &existing)?;
                Ok((canonical_key.to_owned(), etag))
            } else {
                Err(BlobError::S3(format!(
                    "conditional canonical publication failed: {error}"
                )))
            }
        }
    }
}

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

    /// Verify a pending upload's bytes and publish them to the canonical key
    /// derived from their `upload_id`, recording no corpus rows.
    ///
    /// Everything this returns is destined for ONE database transaction
    /// that the caller runs: the cited object, its typed row, and the
    /// Fact that records the arrival. Staging deliberately stops short of
    /// that transaction rather than opening one of its own — which is the
    /// reason completion is split at all.
    ///
    /// On the first stage the row's locator changes to the canonical key, so a
    /// retry re-reads that canonical copy even if the presigned pending key is
    /// overwritten. The redundant pending transfer copy is retired before a
    /// successful stage returns; `finish_upload` retries that cleanup.
    ///
    /// # Errors
    /// Returns `BlobError` when the upload is missing, expired,
    /// malformed, or any S3/database operation fails.
    #[allow(clippy::too_many_lines)] // the stage/repair ordering is one protocol boundary
    pub async fn stage_upload(
        &self,
        ctx: &AuthzContext,
        req: CitedBlobUploadCompleteTs,
    ) -> Result<CitedBlobStaged, BlobError> {
        let owner = req.owner();
        ensure_owner_write_access(ctx, &owner)?;
        let upload_id = parse_opaque_identifier(&req.upload_id)?;
        let row = load_upload(&self.pool, &owner, upload_id).await?;
        match row.status {
            UploadStatus::Completed => {
                let Some(blob_id) = row.blob_id else {
                    return Err(BlobError::State(
                        "completed upload is missing blob_id".into(),
                    ));
                };
                // Already in the corpus. Retry the expendable transfer-key
                // cleanup before reading back what was stored: the row is
                // the truth about this artefact, and replay must not depend
                // on bytes a client can recreate through its presigned URL.
                self.purge_pending_upload(&row.bucket, upload_id).await?;
                return load_staged_payload(&self.pool, &owner, upload_id, blob_id).await;
            }
            UploadStatus::Aborted => {
                self.purge_pending_upload(&row.bucket, upload_id).await?;
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
        let pending_key = pending_object_key(upload_id);
        let mut source_object_key = row.object_key.clone();
        let (streamed, source_etag) = match read_hashed_object(
            client,
            &row.bucket,
            &row.object_key,
            row.expected_byte_len,
            self.config.max_blob_bytes,
        )
        .await
        {
            Ok(result) => result,
            Err(ObjectReadError::Missing) if row.object_key == pending_key => {
                // A concurrent stage may have already recorded and retired
                // the pending copy. Reload exactly once and continue from the
                // canonical locator it recorded; never fall back to a new
                // pending GET that can be overwritten by a presigned client.
                let Some(reloaded) =
                    super::rows::load_upload_optional(&self.pool, &owner, upload_id).await?
                else {
                    self.purge_pending_upload(&row.bucket, upload_id).await?;
                    return Err(BlobError::State("upload not found for Owner".into()));
                };
                match reloaded.status {
                    UploadStatus::Completed => {
                        let Some(blob_id) = reloaded.blob_id else {
                            return Err(BlobError::State(
                                "completed upload is missing blob_id".into(),
                            ));
                        };
                        self.purge_pending_upload(&reloaded.bucket, upload_id)
                            .await?;
                        return load_staged_payload(&self.pool, &owner, upload_id, blob_id).await;
                    }
                    UploadStatus::Aborted => {
                        self.purge_pending_upload(&reloaded.bucket, upload_id)
                            .await?;
                        return Err(BlobError::State("upload is aborted".into()));
                    }
                    UploadStatus::Expired => {
                        return Err(BlobError::State("upload is expired".into()));
                    }
                    UploadStatus::Pending if reloaded.object_key != pending_key => {
                        source_object_key = reloaded.object_key.clone();
                        read_hashed_object(
                            client,
                            &reloaded.bucket,
                            &reloaded.object_key,
                            reloaded.expected_byte_len,
                            self.config.max_blob_bytes,
                        )
                        .await
                        .map_err(ObjectReadError::into_blob_error)?
                    }
                    UploadStatus::Pending => {
                        return Err(ObjectReadError::Missing.into_blob_error());
                    }
                }
            }
            Err(error) => return Err(error.into_blob_error()),
        };
        // Derived from this upload row's own primary key, not from its
        // bytes or its owner: the key the read gate will re-derive and
        // compare against for the life of the row.
        let canonical_key = canonical_object_key(upload_id);
        let (canonical_key, etag) = if source_object_key == pending_key {
            publish_canonical(
                client,
                &row.bucket,
                &canonical_key,
                &streamed,
                self.config.max_blob_bytes,
            )
            .await?
        } else {
            // A prior mismatch already recorded the canonical locator. The
            // immutable object is the source of truth and must not be PUT
            // again, even if a client has recreated the pending key.
            (source_object_key.clone(), source_etag)
        };

        let rows_affected = sqlx::query(
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
        .map_err(BlobError::Db)?
        .rows_affected();

        let staged = CitedBlobStaged {
            payload: UploadedBlobPayload {
                content_hash: streamed.blake3,
                bucket: row.bucket.clone(),
                object_key: canonical_key.clone(),
                sha256: streamed.sha256,
                byte_len: streamed.byte_len,
                mime: row.mime.clone(),
                filename: row.filename.clone(),
                etag,
                uploaded_at: OffsetDateTime::now_utc(),
            },
            already_completed: None,
        };
        let decision = if rows_affected == 0 {
            let Some(observed) =
                super::rows::load_upload_optional(&self.pool, &owner, upload_id).await?
            else {
                // The owner row may have been erased while the provider write
                // was in flight. The derived pending key is the only key this
                // upload owns without a row; canonical bytes must remain for
                // an in-place transfer or mounted reference.
                self.purge_pending_upload(&row.bucket, upload_id).await?;
                return Err(BlobError::State("upload not found for Owner".into()));
            };
            stage_locator_decision(observed.status, observed.blob_id, rows_affected)?
        } else {
            stage_locator_decision(row.status, None, rows_affected)?
        };
        match decision {
            StageLocatorDecision::Staged => {
                self.purge_pending_upload(&row.bucket, upload_id).await?;
                Ok(staged)
            }
            StageLocatorDecision::Replay(blob_id) => {
                load_staged_payload(&self.pool, &owner, upload_id, blob_id).await
            }
            StageLocatorDecision::RepairTerminal(status) => {
                self.repair_terminal_stage(&owner, upload_id, &row, status, &staged, &canonical_key)
                    .await
            }
        }
    }

    /// A stage can lose its pending-row guard after writing the canonical
    /// bytes. Keep that locator on a still-terminal row so erase/reconcile can
    /// find the retained canonical object, then reclaim only the expendable
    /// pending transfer copy. A completion that overtook the terminal write
    /// is replayed from its exact upload row instead.
    async fn repair_terminal_stage(
        &self,
        owner: &OwnerRef,
        upload_id: Uuid,
        row: &super::rows::UploadRow,
        terminal: UploadStatus,
        staged: &CitedBlobStaged,
        canonical_key: &str,
    ) -> Result<CitedBlobStaged, BlobError> {
        let owner_id = owner.stored_owner_id();
        let rows_affected = sqlx::query(
            "UPDATE proxima_core.blob_uploads \
                SET object_key = $1, sha256 = $2, etag = $3 \
              WHERE owner_id = $4 AND upload_id = $5 AND status = $6",
        )
        .bind(canonical_key)
        .bind(&staged.payload.sha256[..])
        .bind(&staged.payload.etag)
        .bind(owner_id)
        .bind(upload_id)
        .bind(terminal)
        .execute(&self.pool)
        .await
        .map_err(BlobError::Db)?
        .rows_affected();

        let decision = if rows_affected == 0 {
            let Some(observed) =
                super::rows::load_upload_optional(&self.pool, owner, upload_id).await?
            else {
                self.purge_pending_upload(&row.bucket, upload_id).await?;
                return Err(BlobError::State("upload not found for Owner".into()));
            };
            terminal_locator_repair_decision(
                terminal,
                observed.status,
                observed.blob_id,
                rows_affected,
            )?
        } else {
            terminal_locator_repair_decision(terminal, terminal, None, rows_affected)?
        };

        match decision {
            TerminalLocatorRepairDecision::Replay(blob_id) => {
                load_staged_payload(&self.pool, owner, upload_id, blob_id).await
            }
            TerminalLocatorRepairDecision::Terminal(status) => {
                self.purge_pending_upload(&row.bucket, upload_id).await?;
                Err(BlobError::State(
                    match status {
                        UploadStatus::Aborted => "upload is aborted",
                        UploadStatus::Expired => "upload is expired",
                        _ => "upload has reached an unexpected terminal state",
                    }
                    .into(),
                ))
            }
        }
    }

    /// Purge the presigned transfer key by version id, including all delete
    /// markers. The canonical object is deliberately not touched here.
    async fn purge_pending_upload(&self, bucket: &str, upload_id: Uuid) -> Result<(), BlobError> {
        let client = self.client().await?;
        super::erase::purge_exact_key(client, bucket, &pending_object_key(upload_id))
            .await
            .map(|_| ())
            .map_err(|err| BlobError::S3(err.to_string()))
    }

    /// Close out an upload whose artefact is now recorded: mark the row
    /// completed against `blob_id`, then drop the pending object.
    ///
    /// The order matters. The canonical blob is already referenced by a
    /// committed cited object, so the pending copy is redundant. Resolving
    /// the row transition first makes retries observe the committed outcome;
    /// the version-aware purge then remains safe to repeat if a provider
    /// failure interrupts cleanup.
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
        let upload_id = parse_opaque_identifier(upload_id)?;
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

        // The pending transfer copy is expendable only after the transition
        // decision has been resolved. Replays run this too, so provider
        // failures after a successful status write are retryable.
        self.purge_pending_upload(&row.bucket, upload_id).await?;
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
        let upload_id = parse_opaque_identifier(&req.upload_id)?;
        let row = load_upload(&self.pool, &owner, upload_id).await?;
        match row.status {
            UploadStatus::Completed => {
                return Ok(CitedBlobUploadAbortOutcomeTs { aborted: false });
            }
            UploadStatus::Aborted => {
                self.cleanup_aborted_upload(&owner, upload_id).await?;
                return Ok(CitedBlobUploadAbortOutcomeTs { aborted: true });
            }
            UploadStatus::Expired => {
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
            AbortTransitionDecision::WonPending | AbortTransitionDecision::AlreadyAborted => {
                self.cleanup_aborted_upload(&owner, upload_id).await?;
                Ok(CitedBlobUploadAbortOutcomeTs { aborted: true })
            }
            AbortTransitionDecision::Completed => {
                Ok(CitedBlobUploadAbortOutcomeTs { aborted: false })
            }
            AbortTransitionDecision::AlreadyExpired => {
                Ok(CitedBlobUploadAbortOutcomeTs { aborted: true })
            }
        }
    }

    /// Remove the expendable transfer key an aborted upload may have left
    /// behind. Canonical bytes remain owned by the row: finish may have
    /// truthfully committed the corpus before an abort observation won.
    ///
    /// A staged row stores the canonical key in `object_key`, while the
    /// presigned upload always started at `pending/<upload_id>`. The status
    /// update wins the database race before provider cleanup; keeping this
    /// helper retryable closes the window where a provider failure after
    /// that update would strand either copy.
    async fn cleanup_aborted_upload(
        &self,
        owner: &proxima_core::Owner,
        upload_id: Uuid,
    ) -> Result<(), BlobError> {
        // Re-read after the status transition so a finish that overtook the
        // abort is never mistaken for an aborted row whose transfer copy is
        // ours to clean.
        let row = load_upload(&self.pool, owner, upload_id).await?;
        if row.status != UploadStatus::Aborted {
            return Ok(());
        }
        self.purge_pending_upload(&row.bucket, upload_id).await
    }
}

#[cfg(test)]
mod conditional_publication_tests {
    use super::is_conditional_publication_conflict;

    #[test]
    fn recognizes_conditional_request_conflict() {
        assert!(is_conditional_publication_conflict(
            Some(409),
            Some("ConditionalRequestConflict")
        ));
        assert!(is_conditional_publication_conflict(
            None,
            Some("ConditionalRequestConflict")
        ));
    }

    #[test]
    fn recognizes_precondition_failed() {
        assert!(is_conditional_publication_conflict(
            Some(412),
            Some("PreconditionFailed")
        ));
        assert!(is_conditional_publication_conflict(Some(412), None));
        assert!(is_conditional_publication_conflict(
            None,
            Some("PreconditionFailed")
        ));
    }

    #[test]
    fn rejects_unrelated_statuses_and_codes() {
        assert!(!is_conditional_publication_conflict(Some(409), None));
        assert!(!is_conditional_publication_conflict(
            Some(409),
            Some("Conflict")
        ));
        assert!(!is_conditional_publication_conflict(
            Some(412),
            Some("Conflict")
        ));
        assert!(!is_conditional_publication_conflict(
            Some(400),
            Some("BadRequest")
        ));
        assert!(!is_conditional_publication_conflict(
            Some(500),
            Some("InternalError")
        ));
        assert!(!is_conditional_publication_conflict(
            None,
            Some("AccessDenied")
        ));
        assert!(!is_conditional_publication_conflict(
            Some(200),
            Some("BadRequest")
        ));
    }
}
