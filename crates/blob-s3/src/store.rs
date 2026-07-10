use std::time::Duration;

use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use proxima_core::storage_ports::CitedObjectErasePort;
use proxima_core::{
    AccessKind, AuthzContext, Owner, OwnerRef, OwnerRefKind, StorageError, UPLOADED_BLOB_SCHEMA_ID,
};
use sha2::{Digest, Sha256};
use sqlx::Row;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use crate::config::S3RuntimeConfig;
use crate::error::BlobError;

/// Tauri/TS-compatible cited-blob upload request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CitedBlobUploadPrepareTs {
    pub owner: OwnerRef,
    pub filename: String,
    pub mime: String,
    pub byte_len: u64,
}

/// Tauri/TS-compatible cited-blob upload preparation response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CitedBlobUploadPrepareOutcomeTs {
    pub upload_id: String,
    pub upload_url: String,
    pub expires_at: String,
    pub headers: Vec<PresignedHeaderTs>,
}

/// Header required by a presigned upload.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PresignedHeaderTs {
    pub name: String,
    pub value: String,
}

/// Tauri/TS-compatible cited-blob completion request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CitedBlobUploadCompleteTs {
    pub owner: OwnerRef,
    pub upload_id: String,
}

/// Tauri/TS-compatible cited-blob completion response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CitedBlobUploadCompleteOutcomeTs {
    pub cited_object_id: String,
    pub schema: String,
    pub content_hash: String,
    pub sha256: String,
    pub byte_len: u64,
    pub mime: String,
    pub filename: String,
    pub idempotent_replay: bool,
}

/// Tauri/TS-compatible cited-blob abort request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CitedBlobUploadAbortTs {
    pub owner: OwnerRef,
    pub upload_id: String,
}

/// Tauri/TS-compatible cited-blob abort response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CitedBlobUploadAbortOutcomeTs {
    pub aborted: bool,
}

/// Tauri/TS-compatible cited-blob read URL request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CitedBlobReadUrlTs {
    pub owner: OwnerRef,
    pub cited_object_id: String,
}

/// Tauri/TS-compatible cited-blob read URL response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CitedBlobReadUrlOutcomeTs {
    pub read_url: String,
    pub expires_at: String,
}

impl CitedBlobUploadPrepareTs {
    /// The storage `Owner` for this request.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.owner
    }
}

impl CitedBlobUploadCompleteTs {
    /// The storage `Owner` for this request.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.owner
    }
}

impl CitedBlobUploadAbortTs {
    /// The storage `Owner` for this request.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.owner
    }
}

impl CitedBlobReadUrlTs {
    /// The storage `Owner` for this request.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.owner
    }
}

/// Cited-blob upload/read service over one Postgres pool and one
/// S3 target. Construct once at boot; methods are independently
/// callable per request.
#[derive(Debug, Clone)]
pub struct CitedBlobStore {
    pool: sqlx::PgPool,
    config: S3RuntimeConfig,
    /// Lazily-built S3 client, memoized so the full credential chain is
    /// resolved once per store rather than on every request.
    client: tokio::sync::OnceCell<aws_sdk_s3::Client>,
}

impl CitedBlobStore {
    #[must_use]
    pub fn new(pool: sqlx::PgPool, config: S3RuntimeConfig) -> Self {
        Self {
            pool,
            config,
            client: tokio::sync::OnceCell::new(),
        }
    }

    /// The memoized S3 client, built on first use from the runtime config.
    ///
    /// # Errors
    /// Returns `BlobError` when the AWS client cannot be constructed.
    async fn client(&self) -> Result<&aws_sdk_s3::Client, BlobError> {
        self.client.get_or_try_init(|| self.config.client()).await
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
        validate_prepare(&req, self.config.max_blob_bytes)?;
        let owner = req.owner();
        ensure_owner_write_access(ctx, &owner)?;
        let upload_id = Uuid::now_v7();
        let owner_hash = owner_hash_hex(&owner);
        let object_key = pending_object_key(&owner_hash, upload_id);
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

        let (owner_kind, owner_id) = db_owner_columns(&owner);
        sqlx::query(
            "INSERT INTO proxima_core.cited_object_uploads \
                (owner_kind, owner_id, upload_id, \
                 bucket, object_key, filename, mime, expected_byte_len, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(owner_kind)
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

    /// Complete a pending upload and persist the cited-object row.
    ///
    /// # Errors
    /// Returns `BlobError` when the upload is missing, expired,
    /// malformed, or any S3/database operation fails.
    pub async fn complete_upload(
        &self,
        ctx: &AuthzContext,
        req: CitedBlobUploadCompleteTs,
    ) -> Result<CitedBlobUploadCompleteOutcomeTs, BlobError> {
        let owner = req.owner();
        ensure_owner_write_access(ctx, &owner)?;
        let upload_id = parse_uuid(&req.upload_id)?;
        let row = load_upload(&self.pool, &owner, upload_id).await?;
        match row.status.as_str() {
            "completed" => {
                let Some(cited_object_id) = row.cited_object_id else {
                    return Err(BlobError::State(
                        "completed upload is missing cited_object_id".into(),
                    ));
                };
                return load_completed_blob(&self.pool, &owner, cited_object_id, true).await;
            }
            "aborted" => {
                return Err(BlobError::State("upload is aborted".into()));
            }
            "expired" => {
                return Err(BlobError::State("upload is expired".into()));
            }
            "pending" => {}
            other => {
                return Err(BlobError::State(format!("unknown upload status {other}")));
            }
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
        let owner_hash = owner_hash_hex(&owner);
        let canonical_key = canonical_object_key(&owner_hash, &streamed.blake3_hex);
        let copy_source = format!("{}/{}", row.bucket, row.object_key);
        let copy_result = client
            .copy_object()
            .bucket(&row.bucket)
            .key(&canonical_key)
            .copy_source(copy_source)
            .send()
            .await
            .map_err(|e| BlobError::S3(format!("copy uploaded object failed: {e}")))?;
        let etag = copy_result
            .copy_object_result()
            .and_then(|r| r.e_tag())
            .map(ToString::to_string);

        // Persist the canonical blob BEFORE deleting the pending object. If
        // persistence fails, the pending object must survive so the client can
        // retry (the copy + persist are idempotent on the content hash). Deleting
        // first would orphan the canonical blob in S3 and leave the upload
        // unrecoverable on failure.
        let completed = persist_completed_blob(
            &self.pool,
            &owner,
            upload_id,
            &row,
            &canonical_key,
            &streamed,
            etag.as_deref(),
        )
        .await?;

        // Canonical blob is recorded; the pending object is now redundant. A
        // delete failure here is idempotently retryable. There is no in-process
        // pending-expiry sweep: leftover `pending/` objects (from a crashed
        // complete, an abandoned prepare, or an expired upload) MUST be reclaimed
        // by a mandatory S3 lifecycle-expiration rule on the `pending/` prefix
        // (see docs/15 deployment). Do not rely on application code to GC them.
        client
            .delete_object()
            .bucket(&row.bucket)
            .key(&row.object_key)
            .send()
            .await
            .map_err(|e| BlobError::S3(format!("delete pending upload failed: {e}")))?;
        load_completed_blob(
            &self.pool,
            &owner,
            completed.cited_object_id,
            completed.idempotent_replay,
        )
        .await
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
        if row.status == "completed" {
            return Ok(CitedBlobUploadAbortOutcomeTs { aborted: false });
        }
        if row.status == "aborted" || row.status == "expired" {
            return Ok(CitedBlobUploadAbortOutcomeTs { aborted: true });
        }

        let (owner_kind, owner_id) = db_owner_columns(&owner);
        let rows_affected = sqlx::query(
            "UPDATE proxima_core.cited_object_uploads \
                SET status = 'aborted', aborted_at = now() \
              WHERE owner_kind = $1 \
                AND owner_id IS NOT DISTINCT FROM $2 \
                AND upload_id = $3 \
                AND status = 'pending'",
        )
        .bind(owner_kind)
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
        match abort_transition_decision(&decision_status, rows_affected)? {
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

#[async_trait::async_trait]
impl CitedObjectErasePort for CitedBlobStore {
    /// Purge every S3 object owned by `owner` under both the canonical
    /// `objects/<owner_hash>/` and in-flight `pending/<owner_hash>/` prefixes.
    ///
    /// Wired in-band by owner-scope compliance erase so an Art. 17 owner
    /// erasure removes the owner's uploaded (PII-bearing) documents, not just
    /// the Postgres rows. Best-effort: the caller has already committed the row
    /// deletes and treats any error here as non-fatal.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Unavailable`] when S3 listing/deletion fails.
    async fn purge_owner_objects(&self, owner: Owner) -> Result<u64, StorageError> {
        let owner_hash = owner_hash_hex(&owner);
        let client = self
            .client()
            .await
            .map_err(|e| StorageError::Unavailable(format!("s3 client: {e}")))?;
        let mut deleted = 0_u64;
        for prefix in [
            objects_owner_prefix(&owner_hash),
            pending_owner_prefix(&owner_hash),
        ] {
            deleted =
                deleted.saturating_add(purge_prefix(client, &self.config.bucket, &prefix).await?);
        }
        Ok(deleted)
    }
}

/// List and batch-delete EVERY version (and delete marker) under one S3
/// `prefix`, paging over the `list_object_versions` key/version markers.
/// Returns the number of object versions + delete markers deleted.
///
/// Deletion is by `(key, version_id)`, not by key
/// alone. On a *versioned* bucket — the deployment recommended in
/// `docs/how-to/operate.md` — a key-only `delete_objects` merely inserts a
/// delete marker and leaves the noncurrent PII object versions recoverable via
/// `GetObject?versionId`, defeating the Art. 17 erasure guarantee. Enumerating
/// versions and deleting each by its `version_id` physically removes the bytes.
/// On a non-versioned bucket every entry has `version_id = "null"`, so the same
/// path deletes the live object and remains correct.
async fn purge_prefix(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: &str,
) -> Result<u64, StorageError> {
    let mut deleted = 0_u64;
    let mut key_marker: Option<String> = None;
    let mut version_id_marker: Option<String> = None;
    loop {
        let mut list = client.list_object_versions().bucket(bucket).prefix(prefix);
        if let Some(km) = &key_marker {
            list = list.key_marker(km);
        }
        if let Some(vm) = &version_id_marker {
            list = list.version_id_marker(vm);
        }
        let page = list.send().await.map_err(|e| {
            StorageError::Unavailable(format!("list object versions under {prefix}: {e}"))
        })?;

        // A single `list_object_versions` page returns at most `max-keys`
        // (default 1000) versions + delete markers combined, which is within
        // `delete_objects`' 1000-identifier limit, so one batch per page fits.
        let mut identifiers = Vec::new();
        for (key, version_id) in page
            .versions()
            .iter()
            .map(|v| (v.key(), v.version_id()))
            .chain(
                page.delete_markers()
                    .iter()
                    .map(|m| (m.key(), m.version_id())),
            )
        {
            if let Some(key) = key {
                let mut id = ObjectIdentifier::builder().key(key);
                if let Some(vid) = version_id {
                    id = id.version_id(vid);
                }
                identifiers.push(
                    id.build()
                        .map_err(|e| StorageError::Internal(format!("object identifier: {e}")))?,
                );
            }
        }
        if !identifiers.is_empty() {
            let batch = Delete::builder()
                .set_objects(Some(identifiers))
                .build()
                .map_err(|e| StorageError::Internal(format!("delete batch: {e}")))?;
            let response = client
                .delete_objects()
                .bucket(bucket)
                .delete(batch)
                .send()
                .await
                .map_err(|e| {
                    StorageError::Unavailable(format!("delete objects under {prefix}: {e}"))
                })?;
            let errors = response.errors();
            if !errors.is_empty() {
                let first = errors
                    .first()
                    .and_then(aws_sdk_s3::types::Error::message)
                    .unwrap_or("unknown");
                return Err(StorageError::Unavailable(format!(
                    "delete objects under {prefix} reported {} error(s): {first}",
                    errors.len()
                )));
            }
            deleted =
                deleted.saturating_add(u64::try_from(response.deleted().len()).unwrap_or(u64::MAX));
        }

        if page.is_truncated() == Some(true) {
            key_marker = page.next_key_marker().map(str::to_string);
            version_id_marker = page.next_version_id_marker().map(str::to_string);
        } else {
            break;
        }
    }
    Ok(deleted)
}

#[derive(Debug, Clone)]
struct UploadRow {
    bucket: String,
    object_key: String,
    filename: String,
    mime: String,
    expected_byte_len: i64,
    status: String,
    cited_object_id: Option<Uuid>,
    expires_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
struct StreamedObject {
    blake3: [u8; 32],
    blake3_hex: String,
    sha256: [u8; 32],
    byte_len: u64,
}

#[derive(Debug, Clone)]
struct CompletedBlob {
    cited_object_id: Uuid,
    idempotent_replay: bool,
}

#[derive(Debug, Clone)]
struct BlobLocation {
    bucket: String,
    object_key: String,
}

async fn load_upload(
    pool: &sqlx::PgPool,
    owner: &Owner,
    upload_id: Uuid,
) -> Result<UploadRow, BlobError> {
    let (owner_kind, owner_id) = db_owner_columns(owner);
    let row = sqlx::query(
        "SELECT bucket, object_key, filename, mime, expected_byte_len, \
                status::text AS status, cited_object_id, expires_at \
           FROM proxima_core.cited_object_uploads \
          WHERE owner_kind = $1 \
            AND owner_id IS NOT DISTINCT FROM $2 \
            AND upload_id = $3",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(upload_id)
    .fetch_optional(pool)
    .await
    .map_err(BlobError::Db)?
    .ok_or_else(|| BlobError::State("upload not found for Owner".into()))?;

    Ok(UploadRow {
        bucket: row.get("bucket"),
        object_key: row.get("object_key"),
        filename: row.get("filename"),
        mime: row.get("mime"),
        expected_byte_len: row.get("expected_byte_len"),
        status: row.get("status"),
        cited_object_id: row.get("cited_object_id"),
        expires_at: row.get("expires_at"),
    })
}

async fn mark_upload_expired(
    pool: &sqlx::PgPool,
    owner: &Owner,
    upload_id: Uuid,
) -> Result<(), BlobError> {
    let (owner_kind, owner_id) = db_owner_columns(owner);
    sqlx::query(
        "UPDATE proxima_core.cited_object_uploads \
            SET status = 'expired', error_message = 'upload expired' \
          WHERE owner_kind = $1 \
            AND owner_id IS NOT DISTINCT FROM $2 \
            AND upload_id = $3 \
            AND status = 'pending'",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(upload_id)
    .execute(pool)
    .await
    .map_err(BlobError::Db)?;
    Ok(())
}

async fn hash_uploaded_object(
    body: aws_sdk_s3::primitives::ByteStream,
    expected_byte_len: i64,
    max_blob_bytes: Option<u64>,
) -> Result<StreamedObject, BlobError> {
    let mut reader = body.into_async_read();
    let mut blake3_hasher = blake3::Hasher::new();
    let mut sha256_hasher = Sha256::new();
    let mut buf = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut byte_len = 0_u64;
    loop {
        let n = reader
            .read(&mut buf)
            .await
            .map_err(|e| BlobError::S3(format!("stream pending upload failed: {e}")))?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        blake3_hasher.update(chunk);
        sha256_hasher.update(chunk);
        byte_len = byte_len
            .checked_add(u64::try_from(n).unwrap_or(u64::MAX))
            .ok_or_else(|| BlobError::State("uploaded object is too large".into()))?;
        // Abort as soon as the streamed length crosses the cap so a client that
        // under-declared `byte_len` cannot force us to buffer/hash an oversized
        // object.
        if let Some(max) = max_blob_bytes
            && byte_len > max
        {
            return Err(BlobError::State(format!(
                "uploaded object exceeds max blob size {max}"
            )));
        }
    }
    if i64::try_from(byte_len).unwrap_or(i64::MAX) != expected_byte_len {
        return Err(BlobError::State(format!(
            "uploaded byte length {byte_len} does not match expected {expected_byte_len}"
        )));
    }
    let blake3 = *blake3_hasher.finalize().as_bytes();
    let sha256: [u8; 32] = sha256_hasher.finalize().into();
    Ok(StreamedObject {
        blake3,
        blake3_hex: hex::encode(blake3),
        sha256,
        byte_len,
    })
}

#[allow(clippy::too_many_arguments)]
async fn persist_completed_blob(
    pool: &sqlx::PgPool,
    owner: &Owner,
    upload_id: Uuid,
    upload: &UploadRow,
    canonical_key: &str,
    streamed: &StreamedObject,
    etag: Option<&str>,
) -> Result<CompletedBlob, BlobError> {
    let (owner_kind, owner_id) = db_owner_columns(owner);
    let mut tx = pool.begin().await.map_err(BlobError::Db)?;

    let row = sqlx::query(
        "WITH ins AS ( \
             INSERT INTO proxima_core.cited_objects \
                 (cited_object_id, schema_id, owner_kind, \
                  owner_id, content_hash) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (owner_kind, owner_id, schema_id, content_hash) \
             DO NOTHING \
             RETURNING cited_object_id \
         ) \
         SELECT cited_object_id, false AS idempotent_replay FROM ins \
         UNION ALL \
         SELECT cited_object_id, true AS idempotent_replay \
           FROM proxima_core.cited_objects \
          WHERE owner_kind = $3 \
            AND owner_id IS NOT DISTINCT FROM $4 \
            AND schema_id = $2 \
            AND content_hash = $5 \
            AND NOT EXISTS (SELECT 1 FROM ins) \
          LIMIT 1",
    )
    .bind(Uuid::now_v7())
    .bind(UPLOADED_BLOB_SCHEMA_ID)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(&streamed.blake3[..])
    .fetch_one(tx.as_mut())
    .await
    .map_err(BlobError::Db)?;
    let cited_object_id: Uuid = row.get("cited_object_id");
    let idempotent_replay: bool = row.get("idempotent_replay");

    sqlx::query(
        "INSERT INTO proxima_core.cited_uploaded_blob_v1 \
            (cited_object_id, bucket, object_key, sha256, byte_len, mime, filename, etag) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (cited_object_id) DO NOTHING",
    )
    .bind(cited_object_id)
    .bind(&upload.bucket)
    .bind(canonical_key)
    .bind(&streamed.sha256[..])
    .bind(i64::try_from(streamed.byte_len).unwrap_or(i64::MAX))
    .bind(&upload.mime)
    .bind(&upload.filename)
    .bind(etag)
    .execute(tx.as_mut())
    .await
    .map_err(BlobError::Db)?;

    let rows_affected = sqlx::query(
        "UPDATE proxima_core.cited_object_uploads \
            SET status = 'completed', cited_object_id = $1, completed_at = now() \
          WHERE owner_kind = $2 \
            AND owner_id IS NOT DISTINCT FROM $3 \
            AND upload_id = $4 \
            AND status = 'pending'",
    )
    .bind(cited_object_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(upload_id)
    .execute(tx.as_mut())
    .await
    .map_err(BlobError::Db)?
    .rows_affected();
    if rows_affected == 0 {
        return Err(BlobError::State(
            "upload no longer pending (aborted/expired)".into(),
        ));
    }

    tx.commit().await.map_err(BlobError::Db)?;
    Ok(CompletedBlob {
        cited_object_id,
        idempotent_replay,
    })
}

async fn load_completed_blob(
    pool: &sqlx::PgPool,
    owner: &Owner,
    cited_object_id: Uuid,
    idempotent_replay: bool,
) -> Result<CitedBlobUploadCompleteOutcomeTs, BlobError> {
    let (owner_kind, owner_id) = db_owner_columns(owner);
    let row = sqlx::query(
        "SELECT encode(co.content_hash, 'hex') AS content_hash, \
                encode(b.sha256, 'hex') AS sha256, b.byte_len, b.mime, b.filename \
           FROM proxima_core.cited_objects co \
           JOIN proxima_core.cited_uploaded_blob_v1 b USING (cited_object_id) \
          WHERE co.cited_object_id = $1 \
            AND co.owner_kind = $2 \
            AND co.owner_id IS NOT DISTINCT FROM $3 \
            AND co.schema_id = $4",
    )
    .bind(cited_object_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(UPLOADED_BLOB_SCHEMA_ID)
    .fetch_optional(pool)
    .await
    .map_err(BlobError::Db)?
    .ok_or_else(|| BlobError::State("cited object not found for Owner".into()))?;
    let byte_len: i64 = row.get("byte_len");
    Ok(CitedBlobUploadCompleteOutcomeTs {
        cited_object_id: cited_object_id.to_string(),
        schema: UPLOADED_BLOB_SCHEMA_ID.to_string(),
        content_hash: row.get("content_hash"),
        sha256: row.get("sha256"),
        byte_len: u64::try_from(byte_len).unwrap_or(u64::MAX),
        mime: row.get("mime"),
        filename: row.get("filename"),
        idempotent_replay,
    })
}

async fn load_blob_location(
    pool: &sqlx::PgPool,
    owner: &Owner,
    cited_object_id: Uuid,
) -> Result<BlobLocation, BlobError> {
    let (owner_kind, owner_id) = db_owner_columns(owner);
    let row = sqlx::query(
        "SELECT b.bucket, b.object_key \
           FROM proxima_core.cited_objects co \
           JOIN proxima_core.cited_uploaded_blob_v1 b USING (cited_object_id) \
          WHERE co.cited_object_id = $1 \
            AND co.owner_kind = $2 \
            AND co.owner_id IS NOT DISTINCT FROM $3 \
            AND co.schema_id = $4",
    )
    .bind(cited_object_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(UPLOADED_BLOB_SCHEMA_ID)
    .fetch_optional(pool)
    .await
    .map_err(BlobError::Db)?
    .ok_or_else(|| BlobError::State("cited object not found for Owner".into()))?;
    Ok(BlobLocation {
        bucket: row.get("bucket"),
        object_key: row.get("object_key"),
    })
}

fn presign_config(ttl_seconds: u64) -> Result<PresigningConfig, BlobError> {
    PresigningConfig::expires_in(Duration::from_secs(ttl_seconds))
        .map_err(|e| BlobError::Config(format!("invalid presign TTL: {e}")))
}

fn validate_prepare(
    req: &CitedBlobUploadPrepareTs,
    max_blob_bytes: Option<u64>,
) -> Result<(), BlobError> {
    if req.filename.trim().is_empty() {
        return Err(BlobError::State("filename is required".into()));
    }
    if req.mime.trim().is_empty() {
        return Err(BlobError::State("mime is required".into()));
    }
    if req.byte_len > i64::MAX as u64 {
        return Err(BlobError::State("byte_len exceeds Postgres bigint".into()));
    }
    if let Some(max) = max_blob_bytes
        && req.byte_len > max
    {
        return Err(BlobError::State(format!(
            "byte_len {} exceeds max blob size {max}",
            req.byte_len
        )));
    }
    Ok(())
}

/// Gate a blob READ on host-resolved Fact-read authority for `owner`, rather
/// than trusting the client-supplied `owner` field alone. Symmetric with
/// [`ensure_owner_write_access`]: a cited blob is a Fact-attached payload, so
/// read access is `may_read(owner, Fact)` — the same per-kind role ceiling the
/// write gate uses, not a coarser "any accessible principal" check that a
/// Goal-only-read role could slip through.
fn ensure_owner_access(ctx: &AuthzContext, owner: &Owner) -> Result<(), BlobError> {
    if ctx.may_read(owner, AccessKind::Fact) {
        Ok(())
    } else {
        Err(BlobError::Denied(
            "owner is not readable for this authorization context".into(),
        ))
    }
}

/// Gate a blob WRITE (prepare/complete/abort) on host-resolved write authority,
/// not mere read access. A cited blob is a Fact-attached payload, so the caller
/// must hold Fact-write (Ingest/Editor/Admin) on `owner`: a read-only group
/// Viewer, though it can *read* the group, must not be able to mint pending rows
/// or canonical cited-blob rows in the group's namespace. Also rejects World, which never owns cited blobs.
fn ensure_owner_write_access(ctx: &AuthzContext, owner: &Owner) -> Result<(), BlobError> {
    ensure_write_owner(owner)?;
    if ctx.may_write(owner, AccessKind::Fact) {
        Ok(())
    } else {
        Err(BlobError::Denied(
            "owner is not writable for this authorization context".into(),
        ))
    }
}

fn parse_uuid(value: &str) -> Result<Uuid, BlobError> {
    Uuid::parse_str(value).map_err(|_| BlobError::State(format!("invalid uuid: {value}")))
}

fn ensure_write_owner(owner: &Owner) -> Result<(), BlobError> {
    if matches!(owner, OwnerRef::World) {
        return Err(BlobError::State(
            "World is read-only and cannot own cited blobs".into(),
        ));
    }
    Ok(())
}

fn db_owner_columns(owner: &Owner) -> (OwnerRefKind, Option<Uuid>) {
    owner.columns()
}

fn owner_hash_hex(owner: &Owner) -> String {
    let kind = OwnerRefKind::of(owner);
    let owner_key_id = owner.stable_key_uuid();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proxima-owner-s3-key-v1\0");
    hasher.update(kind.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(owner_key_id.as_bytes());
    hex::encode(hasher.finalize().as_bytes())
}

/// Prefix under which an owner's canonical (completed) blobs live. Single
/// source of truth for the `objects/<owner_hash>/` key space so the erase
/// purge and the write path can never drift.
fn objects_owner_prefix(owner_hash: &str) -> String {
    format!("objects/{owner_hash}/")
}

/// Prefix under which an owner's in-flight (pending) uploads live.
fn pending_owner_prefix(owner_hash: &str) -> String {
    format!("pending/{owner_hash}/")
}

fn pending_object_key(owner_hash: &str, upload_id: Uuid) -> String {
    format!("{}{upload_id}", pending_owner_prefix(owner_hash))
}

fn canonical_object_key(owner_hash: &str, blake3_hex: &str) -> String {
    format!(
        "{}{UPLOADED_BLOB_SCHEMA_ID}/{blake3_hex}",
        objects_owner_prefix(owner_hash)
    )
}

fn format_time(value: OffsetDateTime) -> Result<String, BlobError> {
    value
        .format(&Rfc3339)
        .map_err(|e| BlobError::State(e.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbortTransitionDecision {
    WonPending,
    Completed,
    AbortedOrExpired,
}

fn abort_transition_decision(
    observed_status: &str,
    rows_affected: u64,
) -> Result<AbortTransitionDecision, BlobError> {
    match rows_affected {
        1 => Ok(AbortTransitionDecision::WonPending),
        0 => match observed_status {
            "completed" => Ok(AbortTransitionDecision::Completed),
            "aborted" | "expired" => Ok(AbortTransitionDecision::AbortedOrExpired),
            "pending" => Err(BlobError::State(
                "upload abort did not transition pending row".into(),
            )),
            other => Err(BlobError::State(format!("unknown upload status {other}"))),
        },
        other => Err(BlobError::State(format!(
            "upload abort affected {other} rows"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use proxima_core::UserId;

    use super::*;

    #[test]
    fn object_keys_do_not_embed_raw_owner_ids() {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let owner_kind = OwnerRefKind::of(&owner);
        let owner_key_id = owner.stable_key_uuid();
        let owner_hash = owner_hash_hex(&owner);
        let pending = pending_object_key(&owner_hash, Uuid::now_v7());
        let canonical = canonical_object_key(&owner_hash, &"a".repeat(64));

        assert_eq!(owner_hash.len(), 64);
        assert!(!pending.contains(owner_kind.as_str()));
        assert!(!pending.contains(&owner_key_id.to_string()));
        assert!(pending.starts_with("pending/"));
        assert!(canonical.contains(UPLOADED_BLOB_SCHEMA_ID));
        assert!(canonical.starts_with("objects/"));
    }

    /// The erase purge must target exactly the two owner-scoped prefixes that
    /// prepare/complete write under, derived from the same helpers (no
    /// hardcoded key format). The S3 round-trip itself is only exercised under
    /// `PROXIMA_S3_*` (see `blob_roundtrip_pg`); this pins the deterministic,
    /// network-free key/prefix derivation the purge relies on.
    #[test]
    fn purge_prefixes_are_owner_scoped_ancestors_of_written_keys() {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let owner_hash = owner_hash_hex(&owner);
        let objects = objects_owner_prefix(&owner_hash);
        let pending = pending_owner_prefix(&owner_hash);

        assert_eq!(objects, format!("objects/{owner_hash}/"));
        assert_eq!(pending, format!("pending/{owner_hash}/"));

        // Every key the write path emits sits under the prefix the purge scans.
        assert!(canonical_object_key(&owner_hash, &"a".repeat(64)).starts_with(&objects));
        assert!(pending_object_key(&owner_hash, Uuid::now_v7()).starts_with(&pending));

        // A different owner yields disjoint prefixes, so a purge never reaches
        // another owner's objects.
        let other_hash = owner_hash_hex(&OwnerRef::Personal(UserId::new(Uuid::now_v7())));
        assert_ne!(objects, objects_owner_prefix(&other_hash));
        assert_ne!(pending, pending_owner_prefix(&other_hash));
    }

    #[test]
    fn db_owner_columns_use_nullable_world_shape() {
        assert_eq!(
            db_owner_columns(&OwnerRef::World),
            (OwnerRefKind::World, None)
        );
    }

    #[test]
    fn world_cannot_prepare_cited_blob_write() {
        let err = ensure_write_owner(&OwnerRef::World).expect_err("world write rejected");
        assert!(err.to_string().contains("World is read-only"));
    }

    fn prepare_req(byte_len: u64) -> CitedBlobUploadPrepareTs {
        CitedBlobUploadPrepareTs {
            owner: OwnerRef::Personal(UserId::new(Uuid::now_v7())),
            filename: "test.pdf".into(),
            mime: "application/pdf".into(),
            byte_len,
        }
    }

    #[test]
    fn validate_prepare_rejects_byte_len_over_cap() {
        let err = validate_prepare(&prepare_req(1_025), Some(1_024))
            .expect_err("over-cap byte_len rejected");
        assert!(err.to_string().contains("exceeds max blob size"));
    }

    #[test]
    fn validate_prepare_allows_within_cap_and_when_uncapped() {
        validate_prepare(&prepare_req(1_024), Some(1_024)).expect("at-cap accepted");
        validate_prepare(&prepare_req(u64::from(u32::MAX)), None).expect("uncapped accepted");
    }

    #[test]
    fn owner_access_gate_allows_accessible_owner() {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let ctx = AuthzContext::single_owner(&owner, proxima_core::AuthPath::System);
        ensure_owner_access(&ctx, &owner).expect("accessible owner passes");
    }

    #[test]
    fn owner_access_gate_denies_foreign_owner() {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let ctx = AuthzContext::single_owner(&owner, proxima_core::AuthPath::System);
        let foreign = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let err = ensure_owner_access(&ctx, &foreign).expect_err("foreign owner denied");
        assert!(matches!(err, BlobError::Denied(_)));
    }

    // Blob writes require WRITE authority, not mere read access. A group
    // Viewer can read the group (owner_access gate passes) but must not be able
    // to create cited blobs in it (owner_write gate denies).
    #[test]
    fn owner_write_gate_denies_read_only_group_viewer() {
        use proxima_core::{GroupId, Role};
        let subject = UserId::new(Uuid::now_v7());
        let group = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let ctx = AuthzContext::for_subject_with_role(
            subject,
            [(group, Role::viewer())],
            proxima_core::AuthPath::HostBearer,
        );
        // Read gate passes (the viewer can see the group)…
        ensure_owner_access(&ctx, &group).expect("viewer can read the group");
        // …but the write gate denies.
        let err = ensure_owner_write_access(&ctx, &group).expect_err("viewer cannot write");
        assert!(matches!(err, BlobError::Denied(_)));
    }

    #[test]
    fn owner_write_gate_allows_editor_group_and_self() {
        use proxima_core::{GroupId, Role};
        let subject = UserId::new(Uuid::now_v7());
        let group = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let ctx = AuthzContext::for_subject_with_role(
            subject,
            [(group, Role::editor())],
            proxima_core::AuthPath::HostBearer,
        );
        ensure_owner_write_access(&ctx, &group).expect("editor can write the group");
        ensure_owner_write_access(&ctx, &OwnerRef::Personal(subject)).expect("self is writable");
    }

    #[tokio::test]
    async fn read_presign_forces_attachment_disposition() {
        use aws_config::BehaviorVersion;
        use aws_sdk_s3::config::{Credentials, Region};

        let creds = Credentials::new("AKIDTEST", "secret", None, None, "test");
        let conf = aws_sdk_s3::config::Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("us-east-1"))
            .credentials_provider(creds)
            .build();
        let client = aws_sdk_s3::Client::from_conf(conf);
        let presigned = client
            .get_object()
            .bucket("bucket")
            .key("objects/abc")
            .response_content_disposition("attachment")
            .response_content_type("application/octet-stream")
            .presigned(presign_config(300).expect("presign config"))
            .await
            .expect("presign get");
        let uri = presigned.uri().to_string();
        assert!(
            uri.contains("response-content-disposition=attachment"),
            "presigned GET must force attachment disposition: {uri}"
        );
    }

    #[test]
    fn owner_hash_is_owner_scoped() {
        let a = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let b = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        assert_ne!(owner_hash_hex(&a), owner_hash_hex(&b));
    }

    /// Pins the org-free S3 `owner_hash_hex` against drift. The BLAKE3 folds
    /// the domain tag ‖ principal kind/id — no org. A
    /// fixed principal must reproduce exactly this hex (and thus the same
    /// stored S3 object path) forever.
    #[test]
    fn owner_hash_hex_golden_is_org_free() {
        let owner = OwnerRef::Personal(UserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid literal"),
        ));
        assert_eq!(
            owner_hash_hex(&owner),
            "c022815b2b51727207c5f3014833f1a5c09ae92edfb752c394c9caa3d96374ce"
        );
    }

    #[test]
    fn abort_transition_decision_is_race_idempotent() {
        assert_eq!(
            abort_transition_decision("pending", 1).expect("pending transition wins"),
            AbortTransitionDecision::WonPending
        );
        assert_eq!(
            abort_transition_decision("completed", 0).expect("completed race loss is idempotent"),
            AbortTransitionDecision::Completed
        );
        assert_eq!(
            abort_transition_decision("aborted", 0).expect("aborted replay is idempotent"),
            AbortTransitionDecision::AbortedOrExpired
        );
        assert_eq!(
            abort_transition_decision("expired", 0).expect("expired replay is idempotent"),
            AbortTransitionDecision::AbortedOrExpired
        );
        assert!(matches!(
            abort_transition_decision("pending", 0),
            Err(BlobError::State(message))
                if message == "upload abort did not transition pending row"
        ));
    }
}
