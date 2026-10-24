//! The write lane: prepare → stage → corpus completion, with abort as the other exit.
//!
//! These four stay in one file because their contract is an ordering, not a
//! set of independent verbs. `stage_upload` records the canonical locator
//! and retires the redundant pending transfer copy. Corpus completion settles
//! the upload row in the same transaction as the cited object and Fact;
//! pending-key deletion remains best effort outside it.

use aws_sdk_s3::primitives::ByteStream;
use proxima_core::citations::UploadedBlobPayload;
use proxima_core::storage_ports::CitedBlobStaged;
use proxima_core::{AuthzContext, OwnerId};
use proxima_storage_pg::verbs::forget::ColdPurgeEntry;
use time::OffsetDateTime;
use uuid::Uuid;

use super::CitedBlobStore;
use super::digest::{LengthExpectation, Retain, hash_uploaded_object};
use super::dto::{
    CitedBlobUploadAbortOutcomeTs, CitedBlobUploadAbortTs, CitedBlobUploadCompleteTs,
    CitedBlobUploadPrepareOutcomeTs, CitedBlobUploadPrepareTs, PresignedHeaderTs,
};
use super::guards::{
    ensure_owner_write_access, format_time, lock_fence_and_keys, lock_owner_fence,
    parse_opaque_identifier, presign_config, validate_prepare,
};
use super::keys::{canonical_object_key, pending_object_key};
use super::rows::{
    UploadStatus, load_staged_payload_on_connection, load_upload_for_update,
    load_upload_optional_on_connection, mark_upload_expired_on_connection,
};
use super::transitions::{
    AbortTransitionDecision, StageLocatorDecision, abort_transition_decision,
    stage_locator_decision,
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
            Err(BlobError::Storage(proxima_core::StorageError::Retryable(_)))
                if attempt < proxima_storage_pg::MAX_TRANSACTION_ATTEMPTS =>
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
    /// The row was already aborted; its debts may still be undischarged.
    CleanUpThenAborted,
    /// A pending row was transitioned; classify `(locked status, rows)` and
    /// purge what the transition owed.
    Decide(UploadStatus, u64, Vec<ColdPurgeEntry>),
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
    max_blob_bytes: u64,
    retain: Retain,
) -> Result<(super::digest::StreamedObject, Option<String>), ObjectReadError> {
    let object = client
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|error| {
            let response = error.raw_response();
            let status = response.map(|response| response.status().as_u16());
            let service = error.as_service_error();
            let code = service.and_then(|service| service.meta().code());
            // The SDK synthesizes NotFound for an empty HTTP 404 body.
            // An explicit XML NotFound code must not use that exception.
            let code_less = code.is_none()
                || (code == Some("NotFound")
                    && response
                        .and_then(|response| response.body().bytes())
                        .is_some_and(<[u8]>::is_empty));
            // An explicit S3 code takes precedence: NoSuchBucket also uses
            // HTTP 404, but is a provider fault. Keep the existing fallback
            // for code-less 404 responses as limited compatibility behavior,
            // not a claim that supported providers require it.
            if service
                .is_some_and(aws_sdk_s3::operation::get_object::GetObjectError::is_no_such_key)
                || (status == Some(404) && code_less)
            {
                ObjectReadError::Missing
            } else {
                ObjectReadError::Other(BlobError::S3(format!("read upload object failed: {error}")))
            }
        })?;
    let etag = object.e_tag().map(ToString::to_string);
    let streamed = Box::pin(hash_uploaded_object(
        object.body,
        expected,
        max_blob_bytes,
        retain,
    ))
    .await
    .map_err(ObjectReadError::Other)?;
    Ok((streamed, etag))
}

/// Only the transfer copy is published, so only its read keeps the bytes; an
/// object already at its canonical key is verified by digest.
fn retain_for(source_key: &str, pending_key: &str) -> Retain {
    if source_key == pending_key {
        Retain::Bytes
    } else {
        Retain::DigestsOnly
    }
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
    max_blob_bytes: u64,
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
                    // Compared by digest: the winner is never held in memory.
                    Retain::DigestsOnly,
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

struct StageLocatorInput {
    canonical_key: String,
    etag: Option<String>,
    streamed: super::digest::StreamedObject,
}

impl CitedBlobStore {
    /// Read the object a stage publishes from, held to the declared length.
    /// Only the transfer copy is published, so only its read keeps the bytes.
    async fn read_stage_source(
        &self,
        client: &aws_sdk_s3::Client,
        upload_id: Uuid,
        row: &super::rows::UploadRow,
    ) -> Result<(super::digest::StreamedObject, Option<String>), ObjectReadError> {
        read_hashed_object(
            client,
            &row.bucket,
            &row.object_key,
            LengthExpectation::Declared(row.expected_byte_len),
            self.max_blob_bytes,
            retain_for(&row.object_key, &pending_object_key(upload_id)),
        )
        .await
    }

    async fn load_upload_scoped(
        &self,
        scope: Option<&proxima_core::OwnerScope>,
        owner: &OwnerId,
        upload_id: Uuid,
    ) -> Result<super::rows::UploadRow, BlobError> {
        self.load_upload_scoped_optional(scope, owner, upload_id)
            .await?
            .ok_or_else(|| BlobError::State("upload not found for Owner".into()))
    }

    async fn load_upload_scoped_optional(
        &self,
        scope: Option<&proxima_core::OwnerScope>,
        owner: &OwnerId,
        upload_id: Uuid,
    ) -> Result<Option<super::rows::UploadRow>, BlobError> {
        let mut tx = self.begin_owner_tx(scope).await?;
        let row = load_upload_optional_on_connection(tx.as_mut(), owner, upload_id).await?;
        tx.commit().await.map_err(BlobError::Db)?;
        Ok(row)
    }

    async fn load_staged_scoped(
        &self,
        scope: Option<&proxima_core::OwnerScope>,
        owner: &OwnerId,
        upload_id: Uuid,
        blob_id: Uuid,
    ) -> Result<CitedBlobStaged, BlobError> {
        let mut tx = self.begin_owner_tx(scope).await?;
        let result =
            load_staged_payload_on_connection(tx.as_mut(), owner, upload_id, blob_id).await?;
        tx.commit().await.map_err(BlobError::Db)?;
        Ok(result)
    }

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
        validate_prepare(&req, self.max_blob_bytes)?;
        let owner = req.owner();
        ensure_owner_write_access(ctx, &owner)?;
        let upload_id = Uuid::now_v7();
        let object_key = pending_object_key(upload_id);
        let expires_at = OffsetDateTime::now_utc()
            + time::Duration::seconds(
                i64::try_from(self.config.upload_ttl_seconds).unwrap_or(i64::MAX),
            );
        let client = self.upload_presigning_client();
        let presigned = client
            .put_object()
            .bucket(&self.config.bucket)
            .key(&object_key)
            .content_type(&req.mime)
            .content_length(
                i64::try_from(req.byte_len)
                    .map_err(|_| BlobError::State("byte_len exceeds Postgres bigint".into()))?,
            )
            .presigned(presign_config(self.config.upload_ttl_seconds)?)
            .await
            .map_err(|e| BlobError::S3(format!("prepare upload URL failed: {e}")))?;

        // A pending row is an unpublished upload. Serialize its creation
        // with transfer's exclusive source fence so the transfer cannot
        // pass its unresolved-publication check and then observe a row that
        // appeared concurrently.
        let mut tx = self.begin_owner_tx(ctx.owner_scope()).await?;
        lock_owner_fence(&mut tx, &owner).await?;
        let owner_id =
            proxima_storage_pg::access::owner_columns::ensure_owner_row(tx.as_mut(), &owner)
                .await
                .map_err(|err| BlobError::State(format!("ensure upload owner: {err}")))?;
        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads \
                (owner_id, upload_id, \
                 bucket, object_key, filename, mime, expected_byte_len, expires_at, \
                 object_store) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
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
        .bind(self.store_id.as_str())
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
    /// best-effort hygiene, never an outcome: the completion cleanup retries it,
    /// and erasing the upload owes whatever every attempt missed.
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
        let _slot = self
            .stage_slots
            .acquire()
            .await
            .map_err(|_| BlobError::State("stage slots closed".into()))?;
        let row = self
            .load_upload_scoped(ctx.owner_scope(), &owner, upload_id)
            .await?;
        if let Some(replay) = self
            .stage_entry_shortcut(ctx.owner_scope(), &owner, upload_id, &row)
            .await?
        {
            return Ok(replay);
        }

        let client = self.client();
        let pending_key = pending_object_key(upload_id);
        let mut source_object_key = row.object_key.clone();
        let (mut streamed, source_etag) =
            match self.read_stage_source(client, upload_id, &row).await {
                Ok(result) => result,
                Err(ObjectReadError::Missing) if row.object_key == pending_key => {
                    let Some(source) = self
                        .resolve_missing_pending(ctx.owner_scope(), &owner, upload_id, &pending_key)
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
                            return self
                                .load_staged_scoped(ctx.owner_scope(), &owner, upload_id, blob_id)
                                .await;
                        }
                        StageSource::Terminal { bucket, status } => {
                            // The abort or expiry owed both keys; retry whatever
                            // of that debt remains. A transfer key PUT again after
                            // the debt drained is cleaned here too: nobody may
                            // cite a terminal upload.
                            self.purge_owed_after_commit(self.terminal_upload_debts(upload_id))
                                .await;
                            self.purge_pending_upload_best_effort(&bucket, upload_id)
                                .await;
                            return Err(BlobError::State(terminal_status_message(status).into()));
                        }
                        StageSource::Read(reloaded) => {
                            source_object_key = reloaded.object_key.clone();
                            self.read_stage_source(client, upload_id, &reloaded)
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
                self.max_blob_bytes,
            )
            .await?
        } else {
            // A prior mismatch already recorded the canonical locator. The
            // immutable object is the source of truth and must not be PUT
            // again, even if a client has recreated the pending key.
            (source_object_key.clone(), source_etag)
        };

        self.record_stage_locator(
            ctx.owner_scope(),
            &owner,
            upload_id,
            &row,
            StageLocatorInput {
                canonical_key,
                etag,
                streamed,
            },
        )
        .await
    }

    /// The row states a stage can answer before it touches the object store.
    ///
    /// `Ok(None)` means the row is pending and unexpired, so staging proceeds.
    async fn stage_entry_shortcut(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &OwnerId,
        upload_id: Uuid,
        row: &super::rows::UploadRow,
    ) -> Result<Option<CitedBlobStaged>, BlobError> {
        let status = if row.status == UploadStatus::Pending
            && row.expires_at < OffsetDateTime::now_utc()
        {
            // Finish or abort may have won since this snapshot. Use the
            // status observed under the expiry helper's row lock.
            {
                let mut tx = self.begin_owner_tx(owner_scope).await?;
                let (status, owed) =
                    mark_upload_expired_on_connection(&mut tx, &self.store_id, owner, upload_id)
                        .await?;
                tx.commit().await.map_err(BlobError::Db)?;
                self.purge_owed_after_commit(owed).await;
                status
            }
        } else {
            row.status
        };
        match status {
            UploadStatus::Completed => {
                let blob_id = if row.status == UploadStatus::Pending && row.blob_id.is_none() {
                    self.load_upload_scoped(owner_scope, owner, upload_id)
                        .await?
                        .blob_id
                } else {
                    row.blob_id
                };
                let Some(blob_id) = blob_id else {
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
                self.load_staged_scoped(owner_scope, owner, upload_id, blob_id)
                    .await
                    .map(Some)
            }
            UploadStatus::Aborted => {
                self.purge_pending_upload_best_effort(&row.bucket, upload_id)
                    .await;
                Err(BlobError::State(
                    terminal_status_message(UploadStatus::Aborted).into(),
                ))
            }
            UploadStatus::Expired => Err(BlobError::State(
                terminal_status_message(UploadStatus::Expired).into(),
            )),
            UploadStatus::Pending => Ok(None),
        }
    }

    /// Record the canonical locator against the still-pending row, then decide
    /// what a zero-row update meant. A stage that lost its guard while the
    /// provider write was in flight retains a reachable canonical locator
    /// before reporting its terminal status.
    async fn record_stage_locator(
        &self,
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &OwnerId,
        upload_id: Uuid,
        row: &super::rows::UploadRow,
        input: StageLocatorInput,
    ) -> Result<CitedBlobStaged, BlobError> {
        let staged = CitedBlobStaged {
            payload: UploadedBlobPayload {
                content_hash: input.streamed.blake3,
                bucket: row.bucket.clone(),
                object_key: input.canonical_key,
                sha256: input.streamed.sha256,
                byte_len: input.streamed.byte_len,
                mime: row.mime.clone(),
                filename: row.filename.clone(),
                etag: input.etag,
                uploaded_at: OffsetDateTime::now_utc(),
            },
            already_completed: None,
        };
        let decision = with_retried_tx(|| {
            self.commit_stage_locator(owner_scope, owner, upload_id, row, &staged.payload)
        })
        .await?;
        match decision {
            StageLocatorDecision::Staged => {
                self.purge_pending_upload_best_effort(&row.bucket, upload_id)
                    .await;
                Ok(staged)
            }
            StageLocatorDecision::Replay(blob_id) => {
                self.load_staged_scoped(owner_scope, owner, upload_id, blob_id)
                    .await
            }
            StageLocatorDecision::RepairTerminal(status) => {
                // The repair owed both keys under the canonical key's fence.
                self.purge_owed_after_commit(self.terminal_upload_debts(upload_id))
                    .await;
                Err(BlobError::State(terminal_status_message(status).into()))
            }
        }
    }

    /// The database half of a stage, in ONE transaction so the locator write
    /// and any terminal repair retain the same fences and locked row.
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
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &OwnerId,
        upload_id: Uuid,
        row: &super::rows::UploadRow,
        staged: &UploadedBlobPayload,
    ) -> Result<StageLocatorDecision, BlobError> {
        let canonical_key = &staged.object_key;
        let mut tx = self.begin_owner_tx(owner_scope).await?;
        // The canonical key is about to gain (or has just lost) its only row
        // reference. An owner erase decides "no surviving row names this key"
        // under the same lock, so holding it here is what keeps the two
        // answers from being taken against different snapshots.
        lock_fence_and_keys(&mut tx, owner, std::slice::from_ref(canonical_key)).await?;
        let rows_affected = sqlx::query(
            "UPDATE proxima_core.blob_uploads \
                SET object_key = $1, content_hash = $2, sha256 = $3, etag = $4 \
              WHERE owner_id = $5 AND upload_id = $6 AND status = 'pending'",
        )
        .bind(canonical_key)
        .bind(&staged.content_hash[..])
        .bind(&staged.sha256[..])
        .bind(staged.etag.as_deref())
        .bind(owner.into_inner())
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
                self.resolve_erased_owner_row(upload_id, canonical_key)
                    .await?;
                tx.commit().await.map_err(BlobError::Db)?;
                self.purge_pending_upload_best_effort(&row.bucket, upload_id)
                    .await;
                return Err(BlobError::State("upload not found for Owner".into()));
            }
        } else {
            stage_locator_decision(row.status, None, rows_affected)?
        };
        let decision = match decision {
            StageLocatorDecision::RepairTerminal(terminal) => {
                self.repair_terminal_stage(&mut tx, owner, upload_id, terminal, staged)
                    .await?
            }
            other => other,
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
    /// be a retry: an earlier stage set `content_hash`, completion
    /// completed the row, and a transfer then moved that completed row to
    /// another owner. This stage's snapshot still says "pending", its update
    /// matches nothing, and the row it is looking for is alive elsewhere. The
    /// bytes are owed to that owner, so nothing is enqueued.
    ///
    /// Both the refcount and the debt run under the platform scope: a row a
    /// transfer moved to another owner is invisible under this owner's scope,
    /// and the debt row has no owner to scope it by. The caller's transaction
    /// still holds the canonical key's lock, so no erase can decide this key
    /// between the read and the write.
    async fn resolve_erased_owner_row(
        &self,
        upload_id: Uuid,
        canonical_key: &str,
    ) -> Result<(), BlobError> {
        let mut platform_tx = self
            .platform_scope
            .begin()
            .await
            .map_err(|error| BlobError::State(error.to_string()))?;
        let still_referenced: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1
                   FROM proxima_core.blob_uploads
                  WHERE (object_key = $1 AND object_store = $3)
                     OR upload_id = $2
                     OR mounted_from_upload_id = $2
             )",
        )
        .bind(canonical_key)
        .bind(upload_id)
        .bind(self.store_id.as_str())
        .fetch_one(&mut *platform_tx)
        .await
        .map_err(BlobError::Db)?;
        if still_referenced {
            return Ok(());
        }
        // `owner_id` is NULL, not this owner's id: the owner row is exactly
        // what may have been erased, and `cold_purge_pending.owner_id` carries
        // a foreign key to `owners`. The debt is about an OBJECT; whose erase
        // incurred it is metadata, and unavailable metadata is not a reason to
        // drop a debt on the floor. The store is not: it is the store this
        // stage published to, so the drain deletes against the right one.
        sqlx::query(
            "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, object_store)
             VALUES ($1, NULL, $2)
             ON CONFLICT (object_store, object_key) DO UPDATE
                SET enqueued_at = now(), next_attempt_at = now()",
        )
        .bind(canonical_key)
        .bind(self.store_id.as_str())
        .execute(&mut *platform_tx)
        .await
        .map_err(BlobError::Db)?;
        platform_tx
            .commit()
            .await
            .map_err(|error| BlobError::State(error.to_string()))?;
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
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &OwnerId,
        upload_id: Uuid,
        pending_key: &str,
    ) -> Result<Option<StageSource>, BlobError> {
        let Some(reloaded) = self
            .load_upload_scoped_optional(owner_scope, owner, upload_id)
            .await?
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
    /// bytes. Record that locator on the locked terminal row and owe both
    /// keys before releasing the owner/key fences: a terminal upload's bytes
    /// belong to no one. A completion or erase that won earlier was already
    /// classified by the caller's locked reload. The purge runs after commit.
    async fn repair_terminal_stage(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        owner: &OwnerId,
        upload_id: Uuid,
        terminal: UploadStatus,
        staged: &UploadedBlobPayload,
    ) -> Result<StageLocatorDecision, BlobError> {
        let owner_id = owner.into_inner();
        let rows_affected = sqlx::query(
            "UPDATE proxima_core.blob_uploads \
                SET object_key = $1, sha256 = $2, etag = $3 \
              WHERE owner_id = $4 AND upload_id = $5 AND status = $6",
        )
        .bind(&staged.object_key)
        .bind(&staged.sha256[..])
        .bind(&staged.etag)
        .bind(owner_id)
        .bind(upload_id)
        .bind(terminal)
        .execute(&mut **tx)
        .await
        .map_err(BlobError::Db)?
        .rows_affected();

        // The caller retains the row lock from its terminal observation, so
        // exactly one row can match; anything else is a contradiction.
        if rows_affected != 1 {
            return Err(BlobError::State(format!(
                "terminal upload locator repair affected {rows_affected} rows under the row lock"
            )));
        }
        // The abort or expiry this stage lost to may already have owed and
        // drained these keys before the canonical PUT landed. Nobody may
        // cite a terminal upload, so the bytes just published are owed again,
        // under the fence the terminal transition and every erase take.
        proxima_storage_pg::access::owner_columns::owe_terminal_upload_keys_tx(
            tx,
            &self.store_id,
            owner,
            upload_id,
        )
        .await
        .map_err(|err| BlobError::State(format!("owe terminal upload keys: {err}")))?;
        Ok(StageLocatorDecision::RepairTerminal(terminal))
    }

    /// Reclaim the expendable transfer key without ever deciding an outcome.
    ///
    /// Nothing reads `pending/<upload_id>` once a row records a canonical
    /// locator, and the presigned PUT stays usable until it expires — so this
    /// is hygiene, not a guarantee. A provider failure here must not turn an
    /// already-decided completion, replay or abort into an error for the
    /// caller: the stage and finish paths retry the purge, and erasing the
    /// upload owes whatever every attempt missed.
    async fn purge_pending_upload_best_effort(&self, bucket: &str, upload_id: Uuid) {
        if let Err(error) = self.purge_pending_upload(bucket, upload_id).await {
            tracing::warn!(
                bucket,
                %upload_id,
                %error,
                "pending transfer key left in place; erasing the upload owes it"
            );
        }
    }

    /// Purge the presigned transfer key by version id, including all delete
    /// markers. The canonical object is deliberately not touched here.
    async fn purge_pending_upload(&self, bucket: &str, upload_id: Uuid) -> Result<(), BlobError> {
        let client = self.client();
        super::erase::purge_exact_key(client, bucket, &pending_object_key(upload_id))
            .await
            .map(|_| ())
            .map_err(|err| BlobError::S3(err.to_string()))
    }

    /// Retry pending-key cleanup for an Engine-completed upload.
    ///
    /// The corpus transaction already completed the row. Direct port callers
    /// share that transaction's row-transition implementation; a completed
    /// replay only validates the cited-object id and returns the cleanup bucket.
    /// Pending-key deletion is best effort and never changes a committed outcome.
    ///
    /// A transfer key this cleanup misses stays implied by the row, so
    /// erasing the row owes it.
    ///
    /// # Errors
    /// Returns `BlobError` when the upload is missing for `owner`, is not
    /// finishable from its current status, or any S3/database operation
    /// fails.
    pub async fn finish_upload(
        &self,
        ctx: &AuthzContext,
        owner: OwnerId,
        upload_id: &str,
        blob_id: Uuid,
    ) -> Result<(), BlobError> {
        ensure_owner_write_access(ctx, &owner)?;
        let upload_id = parse_opaque_identifier(upload_id)?;
        let bucket =
            with_retried_tx(|| self.commit_finish(ctx.owner_scope(), &owner, upload_id, blob_id))
                .await?;

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
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &OwnerId,
        upload_id: Uuid,
        blob_id: Uuid,
    ) -> Result<String, BlobError> {
        let mut tx = self.begin_owner_tx(owner_scope).await?;
        let bucket = proxima_storage_pg::access::owner_columns::complete_upload_in_tx(
            &mut tx, owner, upload_id, blob_id,
        )
        .await
        .map_err(BlobError::from)?;
        tx.commit().await.map_err(BlobError::Db)?;
        Ok(bucket)
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
        match with_retried_tx(|| self.commit_abort(ctx.owner_scope(), &owner, upload_id)).await? {
            AbortCommit::Settled(outcome) => Ok(outcome),
            AbortCommit::CleanUpThenAborted => {
                // A repeated abort retries the first one's purge; a debt
                // another drain already cleared is not touched again.
                self.purge_owed_after_commit(self.terminal_upload_debts(upload_id))
                    .await;
                Ok(CitedBlobUploadAbortOutcomeTs { aborted: true })
            }
            AbortCommit::Decide(status, rows_affected, owed) => {
                // The row was locked for that transaction, so a zero-row result
                // is only an external writer violating the protocol. Classify
                // against the locked state rather than opening a second,
                // weaker read.
                match abort_transition_decision(status, rows_affected)? {
                    AbortTransitionDecision::WonPending
                    | AbortTransitionDecision::AlreadyAborted => {
                        let owed = if owed.is_empty() {
                            self.terminal_upload_debts(upload_id)
                        } else {
                            owed
                        };
                        self.purge_owed_after_commit(owed).await;
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
        owner_scope: Option<&proxima_core::OwnerScope>,
        owner: &OwnerId,
        upload_id: Uuid,
    ) -> Result<AbortCommit, BlobError> {
        // Abort participates in the same owner fence as finish and transfer.
        // The row lock is the exact status revalidation: an earlier pool read
        // must never authorize a pending->aborted write after another
        // transition published the upload.
        let mut tx = self.begin_owner_tx(owner_scope).await?;
        // Before the row lock, in the global fence → key → row order. An abort
        // retires the only row that keeps this upload's canonical object
        // reachable, which is the same question an erase's retain anti-join
        // asks; the derived key is used rather than the row's, because the
        // fence has to be held before the row is read.
        lock_fence_and_keys(&mut tx, owner, &[canonical_object_key(upload_id)]).await?;
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
        .bind(owner.into_inner())
        .bind(upload_id)
        .execute(&mut *tx)
        .await
        .map_err(BlobError::Db)?
        .rows_affected();
        // One outcome per upload: no Fact can cite an aborted upload, so its
        // transfer key and its canonical object (if a stage published one)
        // are owed now, in the transaction that decides the abort.
        let owed = if rows_affected == 1 {
            proxima_storage_pg::access::owner_columns::owe_terminal_upload_keys_tx(
                &mut tx,
                &self.store_id,
                owner,
                upload_id,
            )
            .await
            .map_err(|err| BlobError::State(format!("owe aborted upload keys: {err}")))?
        } else {
            Vec::new()
        };

        // Commit before any provider work: the status write is the decision.
        tx.commit().await.map_err(BlobError::Db)?;
        Ok(AbortCommit::Decide(row.status, rows_affected, owed))
    }

    /// The two debts a terminal upload owes, as the queue names them.
    fn terminal_upload_debts(&self, upload_id: Uuid) -> Vec<ColdPurgeEntry> {
        [
            pending_object_key(upload_id),
            canonical_object_key(upload_id),
        ]
        .into_iter()
        .map(|object_key| ColdPurgeEntry {
            object_key,
            object_store: self.store_id.to_string(),
        })
        .collect()
    }

    /// Destroy what a committed terminal transition owed. Best effort: a
    /// failure leaves the durable debt for the drain, backed off.
    pub(super) async fn purge_owed_after_commit(&self, owed: Vec<ColdPurgeEntry>) {
        if owed.is_empty() {
            return;
        }
        let cold = self.cold_store();
        let outcome = proxima_storage_pg::access::owner_columns::purge_owed_keys_after_commit(
            &self.pool,
            &self.platform_scope,
            &cold,
            owed,
        )
        .await;
        if outcome.pending {
            tracing::info!(
                failed = outcome.failed,
                "upload objects stay owed to the purge drain after a failed delete"
            );
        }
    }
}

#[cfg(test)]
#[path = "upload_read_tests.rs"]
mod read_classification_tests;

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
