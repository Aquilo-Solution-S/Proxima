//! The Postgres side of the lane: the shapes it reads back, and the
//! queries behind them.
//!
//! Every query here is owner-scoped in its `WHERE` clause, and every read
//! reports a row that does not match the caller's `Owner` as *missing*
//! rather than forbidden — so a probe cannot learn that it exists.

use proxima_core::citations::UploadedBlobPayload;
use proxima_core::storage_ports::{CitedBlobHeld, CitedBlobStaged};
use proxima_core::{Owner, UPLOADED_BLOB_SCHEMA_ID};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use super::keys::db_owner_columns;
use crate::error::BlobError;

#[derive(Debug, Clone)]
pub(super) struct UploadRow {
    pub(super) bucket: String,
    pub(super) object_key: String,
    pub(super) filename: String,
    pub(super) mime: String,
    pub(super) expected_byte_len: i64,
    pub(super) status: UploadStatus,
    pub(super) cited_object_id: Option<Uuid>,
    pub(super) expires_at: OffsetDateTime,
}

/// `proxima_core.cited_object_upload_status`, decoded as the enum Postgres
/// declares it to be.
///
/// The column has been a database enum since `0001_init`. Reading it back
/// through `::text` and matching strings threw that away: every match
/// needed an arm for a value the database cannot hold, and a typo in a
/// literal compiled fine and simply never matched. Decoding the enum makes
/// these four the only four, so the matches over it are exhaustive by
/// construction and a fifth state could not be added without breaking
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(
    type_name = "proxima_core.cited_object_upload_status",
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
}

pub(super) async fn load_upload(
    pool: &sqlx::PgPool,
    owner: &Owner,
    upload_id: Uuid,
) -> Result<UploadRow, BlobError> {
    let (owner_kind, owner_id) = db_owner_columns(owner);
    let row = sqlx::query(
        "SELECT bucket, object_key, filename, mime, expected_byte_len, \
                status, cited_object_id, expires_at \
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

pub(super) async fn mark_upload_expired(
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

/// Read back the typed description of an artefact already in the corpus.
///
/// Used when an upload is staged a second time: the pending object is
/// gone, so the stored row is the only remaining truth about the bytes —
/// and, being content-addressed, it is the right one.
pub(super) async fn load_staged_payload(
    pool: &sqlx::PgPool,
    owner: &Owner,
    cited_object_id: Uuid,
) -> Result<CitedBlobStaged, BlobError> {
    let (owner_kind, owner_id) = db_owner_columns(owner);
    let row = sqlx::query(
        "SELECT co.content_hash, b.bucket, b.object_key, b.sha256, b.byte_len, \
                b.mime, b.filename, b.etag, b.uploaded_at \
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
    Ok(CitedBlobStaged {
        payload: UploadedBlobPayload {
            content_hash: hash32(&row.get::<Vec<u8>, _>("content_hash"), "content_hash")?,
            bucket: row.get("bucket"),
            object_key: row.get("object_key"),
            sha256: hash32(&row.get::<Vec<u8>, _>("sha256"), "sha256")?,
            byte_len: u64::try_from(byte_len).unwrap_or(u64::MAX),
            mime: row.get("mime"),
            filename: row.get("filename"),
            etag: row.get("etag"),
            uploaded_at: row.get("uploaded_at"),
        },
        already_completed: Some(cited_object_id),
    })
}

/// A stored 32-byte digest. A wrong width here means the row was written
/// by something other than this lane, so it is a fault, not a truncation.
fn hash32(bytes: &[u8], field: &str) -> Result<[u8; 32], BlobError> {
    <[u8; 32]>::try_from(bytes)
        .map_err(|_| BlobError::State(format!("stored {field} is not 32 bytes")))
}

/// Which of `content_hashes` this owner already holds, with the identity a
/// caller needs in order to skip re-uploading them.
///
/// ONE ARRAY PARAMETER, NOT AN `IN` LIST BUILT PER CALL. `= ANY($4::bytea[])`
/// keeps this a fixed string literal — so it is static SQL with no
/// `SQL-POLICY:` obligation, and the bind count does not grow with the batch,
/// which is what keeps Postgres' 65535-parameter ceiling out of the picture
/// however many digests are asked about.
///
/// The predicate lists `owner_kind, owner_id, schema_id, content_hash` in
/// that order on purpose: it is exactly `cited_objects_unique_per_owner`
/// (`0001_init.sql`:1133), so this is an index scan and the batch costs one
/// probe per digest rather than a sweep of the owner's artefacts.
pub(super) async fn find_held_blobs(
    pool: &sqlx::PgPool,
    owner: &Owner,
    content_hashes: &[[u8; 32]],
) -> Result<Vec<CitedBlobHeld>, BlobError> {
    let (owner_kind, owner_id) = db_owner_columns(owner);
    let digests: Vec<Vec<u8>> = content_hashes.iter().map(|hash| hash.to_vec()).collect();
    let rows = sqlx::query(
        "SELECT co.content_hash, co.cited_object_id, b.byte_len, b.mime, b.filename \
           FROM proxima_core.cited_objects co \
           JOIN proxima_core.cited_uploaded_blob_v1 b USING (cited_object_id) \
          WHERE co.owner_kind = $1 \
            AND co.owner_id IS NOT DISTINCT FROM $2 \
            AND co.schema_id = $3 \
            AND co.content_hash = ANY($4::bytea[])",
    )
    .bind(owner_kind)
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
                cited_object_id: row.get("cited_object_id"),
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
