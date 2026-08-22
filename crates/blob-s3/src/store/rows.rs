//! The Postgres side of the lane: the shapes it reads back, and the
//! queries behind them.
//!
//! Citation is `proxima_core.blob` + `memory.blob_id`. Upload staging
//! lives in `proxima_core.blob_uploads`. Every query is owner-scoped.

use proxima_core::citations::UploadedBlobPayload;
use proxima_core::storage_ports::{CitedBlobHeld, CitedBlobStaged};
use proxima_core::{Owner, StorageError, UPLOADED_BLOB_SCHEMA_ID};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::BlobError;

#[derive(Debug, Clone)]
pub(super) struct UploadRow {
    pub(super) bucket: String,
    pub(super) object_key: String,
    pub(super) filename: String,
    pub(super) mime: String,
    pub(super) expected_byte_len: i64,
    pub(super) status: UploadStatus,
    pub(super) blob_id: Option<Uuid>,
    pub(super) expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(
    type_name = "proxima_core.blob_upload_status",
    rename_all = "lowercase"
)]
pub(super) enum UploadStatus {
    Pending,
    Completed,
    Aborted,
    Expired,
}

#[derive(Debug, Clone)]
pub(super) struct BlobLocation {
    pub(super) bucket: String,
    pub(super) object_key: String,
    /// The upload row's own primary key. The read gate re-derives the
    /// canonical key from it and demands a byte-exact match, which is what
    /// stops a locator this store did not mint.
    pub(super) upload_id: Uuid,
    /// Set when this row mounts an object minted by another upload row --
    /// a cross-owner transfer's deduped reference. The gate derives from
    /// this instead of [`Self::upload_id`] when it is present; carrying it
    /// alongside rather than folding it in keeps the row's stored columns
    /// visible at the call site.
    pub(super) mounted_from_upload_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub(super) struct BlobReadRecord {
    pub(super) cited_object_id: Uuid,
    pub(super) content_hash: [u8; 32],
    pub(super) bucket: String,
    pub(super) object_key: String,
    /// See [`BlobLocation::upload_id`].
    pub(super) upload_id: Uuid,
    /// See [`BlobLocation::mounted_from_upload_id`].
    pub(super) mounted_from_upload_id: Option<Uuid>,
    pub(super) sha256: [u8; 32],
    pub(super) byte_len: u64,
    pub(super) mime: String,
    pub(super) filename: String,
}

pub(super) async fn ensure_owner_row(
    pool: &sqlx::PgPool,
    owner: &Owner,
) -> Result<Uuid, BlobError> {
    let mut conn = pool.acquire().await?;
    proxima_storage_pg::access::owner_columns::ensure_owner_row(&mut conn, owner)
        .await
        .map_err(map_owner_row)
}

fn map_owner_row(err: StorageError) -> BlobError {
    match err {
        StorageError::ConstraintViolation(msg) => BlobError::State(msg),
        other => BlobError::State(format!("db error: {other}")),
    }
}

pub(super) async fn load_upload(
    pool: &sqlx::PgPool,
    owner: &Owner,
    upload_id: Uuid,
) -> Result<UploadRow, BlobError> {
    let owner_id = owner.stored_owner_id();
    let row = sqlx::query(
        "SELECT bucket, object_key, filename, mime, expected_byte_len, \
                status, blob_id, expires_at \
           FROM proxima_core.blob_uploads \
          WHERE owner_id = $1 \
            AND upload_id = $2",
    )
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
        blob_id: row.get("blob_id"),
        expires_at: row.get("expires_at"),
    })
}

pub(super) async fn mark_upload_expired(
    pool: &sqlx::PgPool,
    owner: &Owner,
    upload_id: Uuid,
) -> Result<(), BlobError> {
    let owner_id = owner.stored_owner_id();
    sqlx::query(
        "UPDATE proxima_core.blob_uploads \
            SET status = 'expired', error_message = 'upload expired' \
          WHERE owner_id = $1 \
            AND upload_id = $2 \
            AND status = 'pending'",
    )
    .bind(owner_id)
    .bind(upload_id)
    .execute(pool)
    .await
    .map_err(BlobError::Db)?;
    Ok(())
}

pub(super) async fn load_staged_payload(
    pool: &sqlx::PgPool,
    owner: &Owner,
    blob_id: Uuid,
) -> Result<CitedBlobStaged, BlobError> {
    let owner_id = owner.stored_owner_id();
    let row = sqlx::query(
        "SELECT b.content_hash, u.bucket, u.object_key, u.sha256, \
                u.expected_byte_len AS byte_len, u.mime, u.filename, u.etag, \
                u.completed_at AS uploaded_at \
           FROM proxima_core.blob b \
           JOIN proxima_core.blob_uploads u ON u.blob_id = b.blob_id \
          WHERE b.blob_id = $1 \
            AND b.owner_id = $2 \
            AND b.schema_id = $3 \
            AND u.status = 'completed' \
          ORDER BY u.completed_at DESC NULLS LAST \
          LIMIT 1",
    )
    .bind(blob_id)
    .bind(owner_id)
    .bind(UPLOADED_BLOB_SCHEMA_ID)
    .fetch_optional(pool)
    .await
    .map_err(BlobError::Db)?
    .ok_or_else(|| BlobError::State("cited object not found for Owner".into()))?;

    let byte_len: i64 = row.get("byte_len");
    let sha256 = row
        .get::<Option<Vec<u8>>, _>("sha256")
        .map(|bytes| hash32(&bytes, "sha256"))
        .transpose()?
        .unwrap_or([0; 32]);
    let uploaded_at = row
        .get::<Option<OffsetDateTime>, _>("uploaded_at")
        .unwrap_or_else(OffsetDateTime::now_utc);
    Ok(CitedBlobStaged {
        payload: UploadedBlobPayload {
            content_hash: hash32(&row.get::<Vec<u8>, _>("content_hash"), "content_hash")?,
            bucket: row.get("bucket"),
            object_key: row.get("object_key"),
            sha256,
            byte_len: u64::try_from(byte_len).unwrap_or(u64::MAX),
            mime: row.get("mime"),
            filename: row.get("filename"),
            etag: row.get("etag"),
            uploaded_at,
        },
        already_completed: Some(blob_id),
    })
}

fn hash32(bytes: &[u8], field: &str) -> Result<[u8; 32], BlobError> {
    <[u8; 32]>::try_from(bytes)
        .map_err(|_| BlobError::State(format!("stored {field} is not 32 bytes")))
}

pub(super) async fn find_held_blobs(
    pool: &sqlx::PgPool,
    owner: &Owner,
    content_hashes: &[[u8; 32]],
) -> Result<Vec<CitedBlobHeld>, BlobError> {
    let owner_id = owner.stored_owner_id();
    let digests: Vec<Vec<u8>> = content_hashes.iter().map(|hash| hash.to_vec()).collect();
    let rows = sqlx::query(
        "SELECT b.content_hash, b.blob_id, \
                COALESCE(u.expected_byte_len, 0) AS byte_len, \
                COALESCE(u.mime, '') AS mime, \
                COALESCE(u.filename, '') AS filename \
           FROM proxima_core.blob b \
           LEFT JOIN LATERAL (
                SELECT expected_byte_len, mime, filename
                  FROM proxima_core.blob_uploads u
                 WHERE u.blob_id = b.blob_id
                   AND u.status = 'completed'
                 ORDER BY u.completed_at DESC NULLS LAST
                 LIMIT 1
           ) u ON true \
          WHERE b.owner_id = $1 \
            AND b.schema_id = $2 \
            AND b.content_hash = ANY($3::bytea[])",
    )
    .bind(owner_id)
    .bind(UPLOADED_BLOB_SCHEMA_ID)
    .bind(&digests)
    .fetch_all(pool)
    .await
    .map_err(BlobError::Db)?;

    rows.into_iter()
        .map(|row| {
            let byte_len: i64 = row.get("byte_len");
            Ok(CitedBlobHeld {
                content_hash: hash32(&row.get::<Vec<u8>, _>("content_hash"), "content_hash")?,
                cited_object_id: row.get("blob_id"),
                byte_len: u64::try_from(byte_len).unwrap_or(u64::MAX),
                mime: row.get("mime"),
                filename: row.get("filename"),
            })
        })
        .collect()
}

pub(super) async fn load_blob_location(
    pool: &sqlx::PgPool,
    owner: &Owner,
    blob_id: Uuid,
) -> Result<BlobLocation, BlobError> {
    let owner_id = owner.stored_owner_id();
    if let Some(row) = sqlx::query(
        // `u.owner_id = $2` as well as `b.owner_id`: a transfer moves both
        // rows together, so the two must agree. Without it a stale upload
        // row left behind by a half-applied transfer would still locate
        // bytes for whoever now holds the blob row.
        "SELECT u.bucket, u.object_key, u.upload_id, u.mounted_from_upload_id \
           FROM proxima_core.blob b \
           JOIN proxima_core.blob_uploads u ON u.blob_id = b.blob_id \
          WHERE b.blob_id = $1 \
            AND b.owner_id = $2 \
            AND u.owner_id = $2 \
            AND b.schema_id = $3 \
            AND u.status = 'completed' \
          ORDER BY u.completed_at DESC NULLS LAST \
          LIMIT 1",
    )
    .bind(blob_id)
    .bind(owner_id)
    .bind(UPLOADED_BLOB_SCHEMA_ID)
    .fetch_optional(pool)
    .await
    .map_err(BlobError::Db)?
    {
        return Ok(BlobLocation {
            bucket: row.get("bucket"),
            object_key: row.get("object_key"),
            upload_id: row.get("upload_id"),
            mounted_from_upload_id: row.get("mounted_from_upload_id"),
        });
    }

    // No completed upload row for this owner: there is no locator to
    // return. A blob row alone never yields one, because an owner-free key
    // derives from the upload's primary key and not from a content hash.
    // One answer for "absent" and "not yours", so neither is a probe.
    Err(BlobError::State("cited object not found for Owner".into()))
}

pub(super) async fn load_blob_read_record(
    pool: &sqlx::PgPool,
    owner: &Owner,
    blob_id: Uuid,
) -> Result<Option<BlobReadRecord>, BlobError> {
    let owner_id = owner.stored_owner_id();
    let row = sqlx::query(
        "SELECT b.blob_id, b.content_hash, u.bucket, u.object_key, u.upload_id, \
                u.mounted_from_upload_id, \
                u.sha256, u.expected_byte_len AS byte_len, u.mime, u.filename \
           FROM proxima_core.blob b \
           JOIN proxima_core.blob_uploads u ON u.blob_id = b.blob_id \
          WHERE b.blob_id = $1 \
            AND b.owner_id = $2 \
            AND u.owner_id = $2 \
            AND b.schema_id = $3 \
            AND u.status = 'completed' \
          ORDER BY u.completed_at DESC NULLS LAST \
          LIMIT 1",
    )
    .bind(blob_id)
    .bind(owner_id)
    .bind(UPLOADED_BLOB_SCHEMA_ID)
    .fetch_optional(pool)
    .await
    .map_err(BlobError::Db)?;

    row.map(|row| {
        let byte_len: i64 = row.get("byte_len");
        let sha256 = row
            .get::<Option<Vec<u8>>, _>("sha256")
            .map(|bytes| hash32(&bytes, "sha256"))
            .transpose()?
            .unwrap_or([0; 32]);
        Ok(BlobReadRecord {
            cited_object_id: row.get("blob_id"),
            content_hash: hash32(&row.get::<Vec<u8>, _>("content_hash"), "content_hash")?,
            bucket: row.get("bucket"),
            object_key: row.get("object_key"),
            upload_id: row.get("upload_id"),
            mounted_from_upload_id: row.get("mounted_from_upload_id"),
            sha256,
            byte_len: u64::try_from(byte_len)
                .map_err(|_| BlobError::State("stored byte_len is negative".into()))?,
            mime: row.get("mime"),
            filename: row.get("filename"),
        })
    })
    .transpose()
}
