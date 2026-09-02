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
use proxima_core::{AuthzContext, Owner, OwnerRef};
use time::OffsetDateTime;
use uuid::Uuid;

use super::CitedBlobStore;
use super::digest::{LengthExpectation, hash_uploaded_object};
use super::dto::{
    CitedBlobUploadAbortOutcomeTs, CitedBlobUploadAbortTs, CitedBlobUploadCompleteTs,
    CitedBlobUploadPrepareOutcomeTs, CitedBlobUploadPrepareTs, PresignedHeaderTs,
};
use super::guards::{
    ensure_owner_write_access, format_time, parse_opaque_identifier, presign_config,
    validate_prepare,
};
use super::keys::{canonical_object_key, pending_object_key};
use super::rows::{
    UploadStatus, load_staged_payload, load_upload, load_upload_for_update, mark_upload_expired,
};
use super::transitions::{
    AbortTransitionDecision, FinishTransitionDecision, StageLocatorDecision,
    TerminalLocatorRepairDecision, abort_transition_decision, finish_transition_decision,
    stage_locator_decision, terminal_locator_repair_decision,
};
use crate::error::BlobError;

/// Re-run a whole `begin → body → commit` transaction while it fails with a
/// transient conflict, up to the workspace-wide attempt budget.
///
/// The upload lane now takes object-key fences, and it takes them in a
/// different relative order from erase — erase fences the key before deleting
/// its rows, an abort locks its row before fencing the key — so `40P01` is a
/// normal outcome of two correct transactions rather than a fault. The only
/// correct answer is to roll back and try again, which is safe exactly because
/// `op` opens its own transaction each call.
///
/// Provider work is deliberately not inside `op`: an S3 failure is not a
/// serialization failure, and re-running a conditional PUT is a different
/// decision from re-running a database transaction.
async fn with_retried_tx<T, F, Fut>(mut op: F) -> Result<T, BlobError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, BlobError>>,
{
    let mut attempt = 1;
    loop {
        match op().await {
            Err(BlobError::Db(error))
                if attempt < proxima_storage_pg::MAX_TRANSACTION_ATTEMPTS
                    && proxima_storage_pg::is_transient_conflict(&error) =>
            {
                attempt += 1;
            }
            outcome => return outcome,
        }
    }
}

/// What the abort transaction committed, and what is left to do outside it.
enum AbortCommit {
    /// Nothing further: the row was already terminal in a way that owes no
    /// provider work.
    Settled(CitedBlobUploadAbortOutcomeTs),
    /// The row was already aborted; only the transfer-key cleanup remains.
    CleanUpThenAborted,
    /// A pending row was transitioned; classify `(locked status, rows)`.
    Decide(UploadStatus, u64),
}

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
    expected: LengthExpectation,
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
    let streamed = Box::pin(hash_uploaded_object(object.body, expected, max_blob_bytes))
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
    body: Vec<u8>,
    max_blob_bytes: Option<u64>,
) -> Result<(String, Option<String>), BlobError> {
    let result = client
        .put_object()
        .bucket(bucket)
        .key(canonical_key)
        .if_none_match("*")
        .body(ByteStream::from(body))
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
                    // Nothing to enforce here: the object that won the key may
                    // differ in length, and that difference is a canonical
                    // conflict, not a client length error.
                    LengthExpectation::CapOnly,
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

/// The answer a terminal upload row gives. Shared so the entry check, the
/// reload after a vanished pending key, and the post-write repair cannot drift
/// apart.
const fn terminal_status_message(status: UploadStatus) -> &'static str {
    match status {
        UploadStatus::Aborted => "upload is aborted",
        UploadStatus::Expired => "upload is expired",
        UploadStatus::Completed | UploadStatus::Pending => {
            "upload has reached an unexpected terminal state"
        }
    }
}

/// Where a stage reads its bytes from, once the pending key it expected has
/// gone missing underneath it.
enum StageSource {
    /// The upload already reached the corpus; replay from its row.
    Replay { bucket: String, blob_id: Uuid },
    /// The row is terminal: report that rather than stage.
    Terminal {
        bucket: String,
        status: UploadStatus,
    },
    /// Read the object the reloaded row now points at.
    Read(super::rows::UploadRow),
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

        // A pending row is an unpublished upload. Serialize its creation
        // with transfer's exclusive source fence so the transfer cannot
        // pass its unresolved-publication check and then observe a row that
        // appeared concurrently.
        let mut tx = self.pool.begin().await.map_err(BlobError::Db)?;
        proxima_storage_pg::access::owner_columns::lock_owner_fence_shared_tx(&mut tx, &owner)
            .await
            .map_err(|err| BlobError::State(format!("lock upload owner fence: {err}")))?;
        let owner_id =
            proxima_storage_pg::access::owner_columns::ensure_owner_row(tx.as_mut(), &owner)
                .await
                .map_err(|err| BlobError::State(format!("ensure upload owner: {err}")))?;
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
        .execute(&mut *tx)
        .await
        .map_err(BlobError::Db)?;
        tx.commit().await.map_err(BlobError::Db)?;

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
    /// overwritten. Retiring the redundant pending transfer copy is
    /// best-effort hygiene, never an outcome: `finish_upload` retries it, and
    /// the bucket lifecycle rule reclaims whatever every attempt missed.
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
        let upload_id = parse_opaque_identifier(&req.upload_id)?;
        let row = load_upload(&self.pool, &owner, upload_id).await?;
        if let Some(replay) = self.stage_entry_shortcut(&owner, upload_id, &row).await? {
            return Ok(replay);
        }

        let client = self.client().await?;
        let pending_key = pending_object_key(upload_id);
        let mut source_object_key = row.object_key.clone();
        let (mut streamed, source_etag) = match read_hashed_object(
            client,
            &row.bucket,
            &row.object_key,
            LengthExpectation::Declared(row.expected_byte_len),
            self.config.max_blob_bytes,
        )
        .await
        {
            Ok(result) => result,
            Err(ObjectReadError::Missing) if row.object_key == pending_key => {
                let Some(source) = self
                    .resolve_missing_pending(&owner, upload_id, &pending_key)
                    .await?
                else {
                    self.purge_pending_upload_best_effort(&row.bucket, upload_id)
                        .await;
                    return Err(BlobError::State("upload not found for Owner".into()));
                };
                match source {
                    StageSource::Replay { bucket, blob_id } => {
                        self.purge_pending_upload_best_effort(&bucket, upload_id)
                            .await;
                        return load_staged_payload(&self.pool, &owner, upload_id, blob_id).await;
                    }
                    StageSource::Terminal { bucket, status } => {
                        // An expired row's transfer key is left to the bucket
                        // lifecycle rule, exactly as the entry check leaves it.
                        if status == UploadStatus::Aborted {
                            self.purge_pending_upload_best_effort(&bucket, upload_id)
                                .await;
                        }
                        return Err(BlobError::State(terminal_status_message(status).into()));
                    }
                    StageSource::Read(reloaded) => {
                        source_object_key = reloaded.object_key.clone();
                        read_hashed_object(
                            client,
                            &reloaded.bucket,
                            &reloaded.object_key,
                            LengthExpectation::Declared(reloaded.expected_byte_len),
                            self.config.max_blob_bytes,
                        )
                        .await
                        .map_err(ObjectReadError::into_blob_error)?
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
            let body = streamed.take_bytes();
            publish_canonical(
                client,
                &row.bucket,
                &canonical_key,
                &streamed,
                body,
                self.config.max_blob_bytes,
            )
            .await?
        } else {
            // A prior mismatch already recorded the canonical locator. The
            // immutable object is the source of truth and must not be PUT
            // again, even if a client has recreated the pending key.
            (source_object_key.clone(), source_etag)
        };

        self.record_stage_locator(&owner, upload_id, &row, canonical_key, etag, &streamed)
            .await
    }

    /// The row states a stage can answer before it touches the object store.
    ///
    /// `Ok(None)` means the row is pending and unexpired, so staging proceeds.
    async fn stage_entry_shortcut(
        &self,
        owner: &OwnerRef,
        upload_id: Uuid,
        row: &super::rows::UploadRow,
    ) -> Result<Option<CitedBlobStaged>, BlobError> {
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
                self.purge_pending_upload_best_effort(&row.bucket, upload_id)
                    .await;
                return load_staged_payload(&self.pool, owner, upload_id, blob_id)
                    .await
                    .map(Some);
            }
            UploadStatus::Aborted => {
                self.purge_pending_upload_best_effort(&row.bucket, upload_id)
                    .await;
                return Err(BlobError::State(
                    terminal_status_message(UploadStatus::Aborted).into(),
                ));
            }
            UploadStatus::Expired => {
                return Err(BlobError::State(
                    terminal_status_message(UploadStatus::Expired).into(),
                ));
            }
            UploadStatus::Pending => {}
        }
        if row.expires_at < OffsetDateTime::now_utc() {
            mark_upload_expired(&self.pool, owner, upload_id).await?;
            return Err(BlobError::State(
                terminal_status_message(UploadStatus::Expired).into(),
            ));
        }
        Ok(None)
    }

    /// Record the canonical locator against the still-pending row, then decide
    /// what a zero-row update meant. A stage that lost its guard while the
    /// provider write was in flight is repaired, not reported as a failure.
    async fn record_stage_locator(
        &self,
        owner: &OwnerRef,
        upload_id: Uuid,
        row: &super::rows::UploadRow,
        canonical_key: String,
        etag: Option<String>,
        streamed: &super::digest::StreamedObject,
    ) -> Result<CitedBlobStaged, BlobError> {
        let staged = CitedBlobStaged {
            payload: UploadedBlobPayload {
                content_hash: streamed.blake3,
                bucket: row.bucket.clone(),
                object_key: canonical_key.clone(),
                sha256: streamed.sha256,
                byte_len: streamed.byte_len,
                mime: row.mime.clone(),
                filename: row.filename.clone(),
                etag: etag.clone(),
                uploaded_at: OffsetDateTime::now_utc(),
            },
            already_completed: None,
        };
        let decision = with_retried_tx(|| {
            self.commit_stage_locator(
                owner,
                upload_id,
                row,
                &canonical_key,
                etag.as_deref(),
                streamed,
            )
        })
        .await?;
        match decision {
            StageLocatorDecision::Staged => {
                self.purge_pending_upload_best_effort(&row.bucket, upload_id)
                    .await;
                Ok(staged)
            }
            StageLocatorDecision::Replay(blob_id) => {
                load_staged_payload(&self.pool, owner, upload_id, blob_id).await
            }
            StageLocatorDecision::RepairTerminal(status) => {
                self.repair_terminal_stage(owner, upload_id, row, status, &staged, &canonical_key)
                    .await
            }
        }
    }

    /// The database half of a stage, in ONE transaction so the locator write
    /// and the answer to "what did a zero-row update mean" share a snapshot.
    ///
    /// The S3 copy is intentionally outside this critical section. Re-enter it
    /// under the owner fence and the canonical key's object fence, then lock
    /// the row before publishing the locator: an abort, expiry, or finish that
    /// won the status race must be observed, and its terminal state must not
    /// leave a canonical object with no readable row. `content_hash` is written
    /// here because finish refuses to publish an upload with no exact staged
    /// identity.
    async fn commit_stage_locator(
        &self,
        owner: &OwnerRef,
        upload_id: Uuid,
        row: &super::rows::UploadRow,
        canonical_key: &str,
        etag: Option<&str>,
        streamed: &super::digest::StreamedObject,
    ) -> Result<StageLocatorDecision, BlobError> {
        let mut tx = self.pool.begin().await.map_err(BlobError::Db)?;
        proxima_storage_pg::access::owner_columns::lock_owner_fence_shared_tx(&mut tx, owner)
            .await
            .map_err(|err| BlobError::State(format!("lock upload owner fence: {err}")))?;
        // The canonical key is about to gain (or has just lost) its only row
        // reference. An owner erase decides "no surviving row names this key"
        // under the same lock, so holding it here is what keeps the two
        // answers from being taken against different snapshots.
        proxima_storage_pg::access::owner_columns::lock_object_keys_tx(
            &mut tx,
            std::slice::from_ref(&canonical_key.to_owned()),
        )
        .await
        .map_err(|err| BlobError::State(format!("lock upload object key: {err}")))?;
        let rows_affected = sqlx::query(
            "UPDATE proxima_core.blob_uploads \
                SET object_key = $1, content_hash = $2, sha256 = $3, etag = $4 \
              WHERE owner_id = $5 AND upload_id = $6 AND status = 'pending'",
        )
        .bind(canonical_key)
        .bind(&streamed.blake3[..])
        .bind(&streamed.sha256[..])
        .bind(etag)
        .bind(owner.stored_owner_id())
        .bind(upload_id)
        .execute(&mut *tx)
        .await
        .map_err(BlobError::Db)?
        .rows_affected();
        let decision = if rows_affected == 0 {
            if let Some(observed) =
                super::rows::load_upload_optional_for_update(&mut tx, owner, upload_id).await?
            {
                stage_locator_decision(observed.status, observed.blob_id, rows_affected)?
            } else {
                self.resolve_erased_owner_row(&mut tx, upload_id, canonical_key, &row.bucket)
                    .await?;
                tx.commit().await.map_err(BlobError::Db)?;
                self.purge_pending_upload_best_effort(&row.bucket, upload_id)
                    .await;
                return Err(BlobError::State("upload not found for Owner".into()));
            }
        } else {
            stage_locator_decision(row.status, None, rows_affected)?
        };
        tx.commit().await.map_err(BlobError::Db)?;
        Ok(decision)
    }

    /// The row this stage was writing is gone. Decide, under the canonical
    /// key's object fence, whether the bytes it just published are owed to
    /// anyone — and if they are not, OWE them durably.
    ///
    /// The previous shape kept the canonical object on the reasoning that "an
    /// in-place transfer or mounted reference may still own them". That is a
    /// real case but not the only one, and the other one leaks: an owner erase
    /// that committed while the provider write was in flight enqueued the key
    /// the row named at the time, `pending/<upload_id>` — never the canonical
    /// key this stage went on to PUT. Nothing then referenced the canonical
    /// object and nothing was owed it, so it survived every erase, every
    /// drain, and `reconcile_all`'s report, which only reports.
    ///
    /// The question is a refcount, asked while holding the key: does any
    /// `blob_uploads` row under ANY owner name this object — by key, by being
    /// the upload that minted it, or by mounting it? The `mounted_from_upload_id`
    /// arm and the `object_key` arm are both needed: a mount records the
    /// MINTING id, and an in-place move keeps the original row.
    ///
    /// The "yes" branch IS reachable, contrary to what #271's
    /// `assert_no_unpublished_upload` alone would suggest. That check refuses
    /// to transfer a series while one of its uploads is unpublished, so a
    /// PENDING row can never be moved out from under a stage. But a stage can
    /// be a retry: an earlier stage set `content_hash`, `finish_upload`
    /// completed the row, and a transfer then moved that completed row to
    /// another owner. This stage's snapshot still says "pending", its update
    /// matches nothing, and the row it is looking for is alive elsewhere. The
    /// bytes are owed to that owner, so nothing is enqueued.
    async fn resolve_erased_owner_row(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        upload_id: Uuid,
        canonical_key: &str,
        bucket: &str,
    ) -> Result<(), BlobError> {
        let still_referenced: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1
                   FROM proxima_core.blob_uploads
                  WHERE object_key = $1
                     OR upload_id = $2
                     OR mounted_from_upload_id = $2
             )",
        )
        .bind(canonical_key)
        .bind(upload_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(BlobError::Db)?;
        if still_referenced {
            return Ok(());
        }
        // `owner_id` is NULL, not this owner's id: the owner row is exactly
        // what may have been erased, and `cold_purge_pending.owner_id` carries
        // a foreign key to `owners`. The debt is about an OBJECT; whose erase
        // incurred it is metadata, and unavailable metadata is not a reason to
        // drop a debt on the floor. The backend is not: it is the bucket this
        // stage published to, so the drain deletes against the right store.
        sqlx::query(
            "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, backend)
             VALUES ($1, NULL, $2)
             ON CONFLICT (object_key) DO UPDATE
                SET enqueued_at = now(), backend = EXCLUDED.backend",
        )
        .bind(canonical_key)
        .bind(bucket)
        .execute(&mut **tx)
        .await
        .map_err(BlobError::Db)?;
        Ok(())
    }

    /// A concurrent stage may already have recorded the canonical locator and
    /// retired the pending copy under us. Reload the row exactly once and say
    /// what its current state means; never fall back to a second pending GET,
    /// which a presigned client can overwrite between the two reads.
    ///
    /// `None` means the owner row is gone.
    async fn resolve_missing_pending(
        &self,
        owner: &OwnerRef,
        upload_id: Uuid,
        pending_key: &str,
    ) -> Result<Option<StageSource>, BlobError> {
        let Some(reloaded) =
            super::rows::load_upload_optional(&self.pool, owner, upload_id).await?
        else {
            return Ok(None);
        };
        Ok(Some(match reloaded.status {
            UploadStatus::Completed => {
                let Some(blob_id) = reloaded.blob_id else {
                    return Err(BlobError::State(
                        "completed upload is missing blob_id".into(),
                    ));
                };
                StageSource::Replay {
                    bucket: reloaded.bucket,
                    blob_id,
                }
            }
            UploadStatus::Aborted | UploadStatus::Expired => StageSource::Terminal {
                bucket: reloaded.bucket,
                status: reloaded.status,
            },
            UploadStatus::Pending if reloaded.object_key != pending_key => {
                StageSource::Read(reloaded)
            }
            // Still pending against the pending key: nobody moved the object,
            // so it really is gone.
            UploadStatus::Pending => return Err(ObjectReadError::Missing.into_blob_error()),
        }))
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
                self.purge_pending_upload_best_effort(&row.bucket, upload_id)
                    .await;
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
                self.purge_pending_upload_best_effort(&row.bucket, upload_id)
                    .await;
                Err(BlobError::State(terminal_status_message(status).into()))
            }
        }
    }

    /// Reclaim the expendable transfer key without ever deciding an outcome.
    ///
    /// Nothing reads `pending/<upload_id>` once a row records a canonical
    /// locator, and the presigned PUT stays usable until it expires — so this
    /// is hygiene, not a guarantee. A provider failure here must not turn an
    /// already-decided completion, replay or abort into an error for the
    /// caller: `finish_upload` retries the purge, and the bucket lifecycle
    /// rule reclaims whatever every attempt missed.
    async fn purge_pending_upload_best_effort(&self, bucket: &str, upload_id: Uuid) {
        if let Err(error) = self.purge_pending_upload(bucket, upload_id).await {
            tracing::warn!(
                bucket,
                %upload_id,
                %error,
                "leaving the pending transfer key to the bucket lifecycle rule"
            );
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
        let bucket = with_retried_tx(|| self.commit_finish(&owner, upload_id, blob_id)).await?;

        // The pending transfer copy is expendable only after the transition
        // decision has been resolved. Replays run this too, so provider
        // failures after a successful status write are retryable.
        self.purge_pending_upload_best_effort(&bucket, upload_id)
            .await;
        Ok(())
    }

    /// The one transaction a finish runs, so [`with_retried_tx`] can re-run it
    /// whole. Returns the bucket whose pending copy is now expendable.
    async fn commit_finish(
        &self,
        owner: &OwnerRef,
        upload_id: Uuid,
        blob_id: Uuid,
    ) -> Result<String, BlobError> {
        // Finish is the publication half of the upload protocol. It shares
        // the owner fence with Memory/citation admission and transfer, then
        // makes its status decision against a row locked in that same
        // transaction. Otherwise a committed Fact could be followed by a
        // transfer that moves the blob while this stale pending row is still
        // allowed to finish under the source owner.
        let mut tx = self.pool.begin().await.map_err(BlobError::Db)?;
        proxima_storage_pg::access::owner_columns::lock_owner_fence_shared_tx(&mut tx, owner)
            .await
            .map_err(|err| BlobError::State(format!("lock upload owner fence: {err}")))?;
        // Before the row lock, so this stays in the global fence → key → row
        // order. `complete_terminal_upload` below writes `object_key`, which
        // is a reference to this key, and an erase deciding whether to destroy
        // it takes the same lock.
        proxima_storage_pg::access::owner_columns::lock_object_keys_tx(
            &mut tx,
            std::slice::from_ref(&canonical_object_key(upload_id)),
        )
        .await
        .map_err(|err| BlobError::State(format!("lock upload object key: {err}")))?;
        let row = load_upload_for_update(&mut tx, owner, upload_id).await?;
        let owner_id = owner.stored_owner_id();
        // The cited blob must be the object this upload actually staged. A
        // terminal row keeps its canonical bytes, so there is nothing to
        // restore here — only this identity to prove before publication.
        if matches!(
            row.status,
            UploadStatus::Pending | UploadStatus::Aborted | UploadStatus::Expired
        ) {
            let staged_content_hash = row
                .content_hash
                .as_deref()
                .filter(|hash| hash.len() == 32)
                .ok_or_else(|| {
                    BlobError::State(
                        "upload has no exact staged content identity; restage before finish".into(),
                    )
                })?;
            sqlx::query_scalar::<_, Vec<u8>>(
                "SELECT content_hash
                       FROM proxima_core.blob
                      WHERE blob_id = $1
                        AND owner_id = $2
                        AND schema_id = $3
                        AND content_hash = $4
                      FOR SHARE",
            )
            .bind(blob_id)
            .bind(owner_id)
            .bind(proxima_core::UPLOADED_BLOB_SCHEMA_ID)
            .bind(staged_content_hash)
            .fetch_optional(&mut *tx)
            .await
            .map_err(BlobError::Db)?
            .ok_or_else(|| {
                BlobError::State(
                    "cited object does not match the staged upload content identity".into(),
                )
            })?;
        }
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
                .execute(&mut *tx)
                .await
                .map_err(BlobError::Db)?
                .rows_affected();
                if rows_affected == 1 {
                    FinishTransitionDecision::WonPending
                } else {
                    // `FOR UPDATE` plus the owner fence means this cannot
                    // race a compliant transition. Keep the classification
                    // for databases with an external writer, and use the
                    // locked row rather than a stale pool snapshot.
                    finish_transition_decision(row.status, row.blob_id, blob_id, 0)?
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
            complete_terminal_upload(&mut tx, owner, upload_id, blob_id).await?;
        }
        tx.commit().await.map_err(BlobError::Db)?;
        Ok(row.bucket)
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
        match with_retried_tx(|| self.commit_abort(&owner, upload_id)).await? {
            AbortCommit::Settled(outcome) => Ok(outcome),
            AbortCommit::CleanUpThenAborted => {
                self.cleanup_aborted_upload(&owner, upload_id).await;
                Ok(CitedBlobUploadAbortOutcomeTs { aborted: true })
            }
            AbortCommit::Decide(status, rows_affected) => {
                // The row was locked for that transaction, so a zero-row result
                // is only an external writer violating the protocol. Classify
                // against the locked state rather than opening a second,
                // weaker read.
                match abort_transition_decision(status, rows_affected)? {
                    AbortTransitionDecision::WonPending
                    | AbortTransitionDecision::AlreadyAborted => {
                        self.cleanup_aborted_upload(&owner, upload_id).await;
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
        }
    }

    /// The one transaction an abort runs, so [`with_retried_tx`] can re-run it
    /// whole. Provider cleanup happens after it returns, never inside it.
    async fn commit_abort(
        &self,
        owner: &OwnerRef,
        upload_id: Uuid,
    ) -> Result<AbortCommit, BlobError> {
        // Abort participates in the same owner fence as finish and transfer.
        // The row lock is the exact status revalidation: an earlier pool read
        // must never authorize a pending->aborted write after another
        // transition published the upload.
        let mut tx = self.pool.begin().await.map_err(BlobError::Db)?;
        proxima_storage_pg::access::owner_columns::lock_owner_fence_shared_tx(&mut tx, owner)
            .await
            .map_err(|err| BlobError::State(format!("lock upload owner fence: {err}")))?;
        // Before the row lock, in the global fence → key → row order. An abort
        // retires the only row that keeps this upload's canonical object
        // reachable, which is the same question an erase's retain anti-join
        // asks; the derived key is used rather than the row's, because the
        // fence has to be held before the row is read.
        proxima_storage_pg::access::owner_columns::lock_object_keys_tx(
            &mut tx,
            std::slice::from_ref(&canonical_object_key(upload_id)),
        )
        .await
        .map_err(|err| BlobError::State(format!("lock upload object key: {err}")))?;
        let row = load_upload_for_update(&mut tx, owner, upload_id).await?;
        match row.status {
            UploadStatus::Completed => {
                tx.commit().await.map_err(BlobError::Db)?;
                return Ok(AbortCommit::Settled(CitedBlobUploadAbortOutcomeTs {
                    aborted: false,
                }));
            }
            UploadStatus::Aborted => {
                tx.commit().await.map_err(BlobError::Db)?;
                return Ok(AbortCommit::CleanUpThenAborted);
            }
            UploadStatus::Expired => {
                tx.commit().await.map_err(BlobError::Db)?;
                return Ok(AbortCommit::Settled(CitedBlobUploadAbortOutcomeTs {
                    aborted: true,
                }));
            }
            UploadStatus::Pending => {}
        }

        let rows_affected = sqlx::query(
            "UPDATE proxima_core.blob_uploads \
                SET status = 'aborted', aborted_at = now() \
              WHERE owner_id = $1 \
                AND upload_id = $2 \
                AND status = 'pending'",
        )
        .bind(owner.stored_owner_id())
        .bind(upload_id)
        .execute(&mut *tx)
        .await
        .map_err(BlobError::Db)?
        .rows_affected();

        // Commit before any provider work: the status write is the decision,
        // and `cleanup_aborted_upload` re-reads the row on another connection
        // that would otherwise block on this transaction's own row lock.
        tx.commit().await.map_err(BlobError::Db)?;
        Ok(AbortCommit::Decide(row.status, rows_affected))
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
    async fn cleanup_aborted_upload(&self, owner: &proxima_core::Owner, upload_id: Uuid) {
        // Re-read after the status transition so a finish that overtook the
        // abort is never mistaken for an aborted row whose transfer copy is
        // ours to clean.
        let row = match load_upload(&self.pool, owner, upload_id).await {
            Ok(row) => row,
            Err(error) => {
                tracing::warn!(
                    %upload_id,
                    %error,
                    "could not re-read an aborted upload row for transfer-key cleanup"
                );
                return;
            }
        };
        if row.status != UploadStatus::Aborted {
            return;
        }
        self.purge_pending_upload_best_effort(&row.bucket, upload_id)
            .await;
    }
}

async fn complete_terminal_upload(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &Owner,
    upload_id: Uuid,
    blob_id: Uuid,
) -> Result<(), BlobError> {
    let rows_affected = sqlx::query(
        "UPDATE proxima_core.blob_uploads \
            SET object_key = $1, status = 'completed', blob_id = $2, completed_at = now() \
          WHERE owner_id = $3 \
            AND upload_id = $4 \
            AND status IN ('aborted', 'expired')",
    )
    .bind(canonical_object_key(upload_id))
    .bind(blob_id)
    .bind(owner.stored_owner_id())
    .bind(upload_id)
    .execute(&mut **tx)
    .await
    .map_err(BlobError::Db)?
    .rows_affected();
    if rows_affected != 1 {
        return Err(BlobError::State(
            "late upload finish did not transition terminal row".into(),
        ));
    }
    Ok(())
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
