//! The Postgres side of the lane: the shapes it reads back, and the
//! queries behind them.
//!
//! Citation is `proxima_core.blob` + `memory.blob_id`. Upload staging
//! lives in `proxima_core.blob_uploads`. Every query is owner-scoped.

use proxima_core::citations::UploadedBlobPayload;
use proxima_core::storage_ports::{CitedBlobHeld, CitedBlobStaged};
use proxima_core::{OwnerId, UPLOADED_BLOB_SCHEMA_ID};
use proxima_storage_pg::verbs::forget::ColdPurgeEntry;
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::BlobError;

use super::guards::lock_fence_and_keys;
use super::live_upload::with_live_blob_upload;

#[derive(Debug, Clone)]
pub(super) struct UploadRow {
    pub(super) bucket: String,
    pub(super) object_key: String,
    pub(super) filename: String,
    pub(super) mime: String,
    pub(super) expected_byte_len: u64,
    pub(super) status: UploadStatus,
    pub(super) blob_id: Option<Uuid>,
    pub(super) expires_at: OffsetDateTime,
}

impl UploadRow {
    /// The one decoder of the columns both upload reads select.
    fn from_pg_row(row: &sqlx::postgres::PgRow) -> Result<Self, BlobError> {
        Ok(Self {
            bucket: row.get("bucket"),
            object_key: row.get("object_key"),
            filename: row.get("filename"),
            mime: row.get("mime"),
            expected_byte_len: byte_len_from_column(row.get("expected_byte_len"))?,
            status: row.get("status"),
            blob_id: row.get("blob_id"),
            expires_at: row.get("expires_at"),
        })
    }
}

pub(super) use proxima_storage_pg::access::owner_columns::UploadStatus;

/// A stored bigint length as the length it is. A negative one is a corrupt row, refused where
/// it is read rather than replaced by a length nobody stored.
pub(super) fn byte_len_from_column(byte_len: i64) -> Result<u64, BlobError> {
    u64::try_from(byte_len).map_err(|_| BlobError::State("stored byte_len is negative".into()))
}

/// The SHA-256 a completed upload row must carry: its absence is a corrupt row, not zeros.
fn completed_sha256(row: &sqlx::postgres::PgRow) -> Result<[u8; 32], BlobError> {
    let bytes = row
        .get::<Option<Vec<u8>>, _>("sha256")
        .ok_or_else(|| BlobError::State("completed upload has no sha256".into()))?;
    hash32(&bytes, "sha256")
}

#[derive(Debug, Clone)]
pub(super) struct BlobLocation {
    pub(super) bucket: String,
    /// The store the row's bytes were published to; the read gate refuses
    /// a row naming any other.
    pub(super) object_store: String,
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
    /// See [`BlobLocation::object_store`].
    pub(super) object_store: String,
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

pub(super) async fn load_upload_optional_on_connection(
    pool: &mut sqlx::PgConnection,
    owner: &OwnerId,
    upload_id: Uuid,
) -> Result<Option<UploadRow>, BlobError> {
    let owner_id = owner.into_inner();
    let row = sqlx::query(
        "SELECT bucket, object_key, filename, mime, expected_byte_len, \
                status, blob_id, expires_at \
           FROM proxima_core.blob_uploads \
          WHERE owner_id = $1 \
            AND upload_id = $2",
    )
    .bind(owner_id)
    .bind(upload_id)
    .fetch_optional(&mut *pool)
    .await
    .map_err(BlobError::Db)?;

    let Some(row) = row else {
        return Ok(None);
    };

    UploadRow::from_pg_row(&row).map(Some)
}

/// Read an upload row while the caller holds the owner admission fence.
/// `FOR UPDATE` makes the status decision and its owner-scoped transition one
/// row-level critical section; the fence then serializes it with owner
/// transfer and citation admission.
pub(super) async fn load_upload_for_update(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &OwnerId,
    upload_id: Uuid,
) -> Result<UploadRow, BlobError> {
    load_upload_optional_for_update(tx, owner, upload_id)
        .await?
        .ok_or_else(|| BlobError::State("upload not found for Owner".into()))
}

/// The same locked read for callers that must tell an erased owner row from a
/// terminal one rather than collapsing both into an error.
pub(super) async fn load_upload_optional_for_update(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &OwnerId,
    upload_id: Uuid,
) -> Result<Option<UploadRow>, BlobError> {
    let owner_id = owner.into_inner();
    let row = sqlx::query(
        "SELECT bucket, object_key, filename, mime, expected_byte_len, \
                status, blob_id, expires_at \
           FROM proxima_core.blob_uploads \
          WHERE owner_id = $1 \
            AND upload_id = $2 \
          FOR UPDATE",
    )
    .bind(owner_id)
    .bind(upload_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(BlobError::Db)?;

    let Some(row) = row else {
        return Ok(None);
    };

    UploadRow::from_pg_row(&row).map(Some)
}

/// Expire a pending row past its TTL under the owner fence, the canonical
/// key's object fence and the row lock, and owe both keys the upload implies
/// in the same transaction.
///
/// Returns the status the row now has — `Pending` for a staged row (one with
/// a recorded content identity), which never expires — and the debts the
/// expiry owes, for the caller to purge after commit.
pub(super) async fn mark_upload_expired_on_connection(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    store: &proxima_core::ObjectStoreId,
    owner: &OwnerId,
    upload_id: Uuid,
) -> Result<(UploadStatus, Vec<ColdPurgeEntry>), BlobError> {
    // Fence → key → row, as abort and the stage locator write take them: a
    // stage that is about to record its canonical locator holds this key.
    lock_fence_and_keys(tx, owner, &[super::keys::canonical_object_key(upload_id)]).await?;
    let row = load_upload_optional_for_update(tx, owner, upload_id)
        .await?
        .ok_or_else(|| BlobError::State("upload not found for Owner".into()))?;
    if row.status != UploadStatus::Pending {
        return Ok((row.status, Vec::new()));
    }
    // A staged row is not expirable: its bytes are verified and published
    // under the canonical key, and only a finish (or an abort) may decide it.
    let expired = sqlx::query(
        "UPDATE proxima_core.blob_uploads SET status = 'expired', error_message = 'upload expired'
          WHERE owner_id = $1 AND upload_id = $2 AND status = 'pending'
            AND content_hash IS NULL",
    )
    .bind(owner.into_inner())
    .bind(upload_id)
    .execute(&mut **tx)
    .await
    .map_err(BlobError::Db)?
    .rows_affected();
    if expired != 1 {
        return Ok((UploadStatus::Pending, Vec::new()));
    }
    let owed = proxima_storage_pg::access::owner_columns::owe_terminal_upload_keys_tx(
        tx, store, owner, upload_id,
    )
    .await
    .map_err(|err| BlobError::State(format!("owe expired upload keys: {err}")))?;
    Ok((UploadStatus::Expired, owed))
}

pub(super) async fn load_staged_payload_on_connection(
    pool: &mut sqlx::PgConnection,
    owner: &OwnerId,
    upload_id: Uuid,
    blob_id: Uuid,
) -> Result<CitedBlobStaged, BlobError> {
    let owner_id = owner.into_inner();
    let row = sqlx::query(
        "SELECT b.content_hash, u.bucket, u.object_key, u.sha256, \
                u.expected_byte_len AS byte_len, u.mime, u.filename, u.etag, \
                u.completed_at AS uploaded_at \
           FROM proxima_core.blob b \
           JOIN proxima_core.blob_uploads u ON u.blob_id = b.blob_id \
          WHERE b.blob_id = $1 \
            AND b.owner_id = $2 \
            AND u.owner_id = $2 \
            AND b.schema_id = $3 \
            AND u.status = 'completed' \
            AND u.owner_id = $2 \
            AND u.upload_id = $4",
    )
    .bind(blob_id)
    .bind(owner_id)
    .bind(UPLOADED_BLOB_SCHEMA_ID)
    .bind(upload_id)
    .fetch_optional(&mut *pool)
    .await
    .map_err(BlobError::Db)?
    .ok_or_else(|| BlobError::State("cited object not found for Owner".into()))?;

    let sha256 = completed_sha256(&row)?;
    let uploaded_at = row
        .get::<Option<OffsetDateTime>, _>("uploaded_at")
        .ok_or_else(|| BlobError::State("completed upload has no completion time".into()))?;
    Ok(CitedBlobStaged {
        payload: UploadedBlobPayload {
            content_hash: hash32(&row.get::<Vec<u8>, _>("content_hash"), "content_hash")?,
            bucket: row.get("bucket"),
            object_key: row.get("object_key"),
            sha256,
            byte_len: byte_len_from_column(row.get("byte_len"))?,
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

pub(super) async fn find_held_blobs_on_connection(
    pool: &mut sqlx::PgConnection,
    owner: &OwnerId,
    content_hashes: &[[u8; 32]],
) -> Result<Vec<CitedBlobHeld>, BlobError> {
    const SQL: &str = with_live_blob_upload!(
        "SELECT b.content_hash, b.blob_id,
                u.expected_byte_len AS byte_len, u.mime, u.filename
           FROM proxima_core.blob b
           JOIN",
        "WHERE b.owner_id = $1
            AND b.schema_id = $2
            AND b.content_hash = ANY($3::bytea[])"
    );
    let owner_id = owner.into_inner();
    let digests: Vec<Vec<u8>> = content_hashes.iter().map(|hash| hash.to_vec()).collect();
    let rows = sqlx::query(SQL)
        .bind(owner_id)
        .bind(UPLOADED_BLOB_SCHEMA_ID)
        .bind(&digests)
        .fetch_all(&mut *pool)
        .await
        .map_err(BlobError::Db)?;

    rows.into_iter()
        .map(|row| {
            Ok(CitedBlobHeld {
                content_hash: hash32(&row.get::<Vec<u8>, _>("content_hash"), "content_hash")?,
                cited_object_id: row.get("blob_id"),
                byte_len: byte_len_from_column(row.get("byte_len"))?,
                mime: row.get("mime"),
                filename: row.get("filename"),
            })
        })
        .collect()
}

pub(super) async fn load_blob_location_on_connection(
    pool: &mut sqlx::PgConnection,
    owner: &OwnerId,
    blob_id: Uuid,
) -> Result<BlobLocation, BlobError> {
    // The shared selection requires the blob and upload owners to agree:
    // a stale upload left behind by a partial transfer cannot locate bytes
    // for the blob's new owner.
    const SQL: &str = with_live_blob_upload!(
        "SELECT u.bucket, u.object_store, u.object_key, u.upload_id, u.mounted_from_upload_id
           FROM proxima_core.blob b
           JOIN",
        "WHERE b.blob_id = $1
            AND b.owner_id = $2
            AND b.schema_id = $3"
    );
    let owner_id = owner.into_inner();
    if let Some(row) = sqlx::query(SQL)
        .bind(blob_id)
        .bind(owner_id)
        .bind(UPLOADED_BLOB_SCHEMA_ID)
        .fetch_optional(&mut *pool)
        .await
        .map_err(BlobError::Db)?
    {
        return Ok(BlobLocation {
            bucket: row.get("bucket"),
            object_store: row.get("object_store"),
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

pub(super) async fn load_blob_read_record_on_connection(
    pool: &mut sqlx::PgConnection,
    owner: &OwnerId,
    blob_id: Uuid,
) -> Result<Option<BlobReadRecord>, BlobError> {
    const SQL: &str = with_live_blob_upload!(
        "SELECT b.blob_id, b.content_hash, u.bucket, u.object_store, u.object_key, u.upload_id,
                u.mounted_from_upload_id,
                u.sha256, u.expected_byte_len AS byte_len, u.mime, u.filename
           FROM proxima_core.blob b
           JOIN",
        "WHERE b.blob_id = $1
            AND b.owner_id = $2
            AND b.schema_id = $3"
    );
    let owner_id = owner.into_inner();
    let row = sqlx::query(SQL)
        .bind(blob_id)
        .bind(owner_id)
        .bind(UPLOADED_BLOB_SCHEMA_ID)
        .fetch_optional(&mut *pool)
        .await
        .map_err(BlobError::Db)?;

    row.map(|row| {
        let sha256 = completed_sha256(&row)?;
        Ok(BlobReadRecord {
            cited_object_id: row.get("blob_id"),
            content_hash: hash32(&row.get::<Vec<u8>, _>("content_hash"), "content_hash")?,
            bucket: row.get("bucket"),
            object_store: row.get("object_store"),
            object_key: row.get("object_key"),
            upload_id: row.get("upload_id"),
            mounted_from_upload_id: row.get("mounted_from_upload_id"),
            sha256,
            byte_len: byte_len_from_column(row.get("byte_len"))?,
            mime: row.get("mime"),
            filename: row.get("filename"),
        })
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use proxima_pg_testkit::{db_url, drop_db, unique_db_name};
    use proxima_storage_pg::PgStorage;
    use proxima_storage_pg::test_fixtures::create_core_db;

    use super::*;

    #[test]
    fn a_negative_stored_length_is_refused_not_replaced() {
        assert_eq!(byte_len_from_column(7).expect("a length"), 7);
        let error = byte_len_from_column(-1).expect_err("a negative length is corrupt");
        assert!(matches!(error, BlobError::State(_)), "{error}");
    }

    /// A completed row that lost its digest or its completion time is a corrupt row: a replay must
    /// refuse it, not return zeros or the current time as if they had been stored.
    #[tokio::test]
    async fn a_completed_row_without_its_digest_or_time_is_refused_not_invented() {
        let db_name = unique_db_name("proxima_blob_rows_test");
        create_core_db(&db_name).await.expect("test database");
        let pg = PgStorage::connect_for_tests(db_url(&db_name))
            .await
            .expect("test storage");
        let pool = pg.pool_for_tests().clone();
        let owner_id = Uuid::now_v7();
        let upload_id = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner_id)
            .execute(&pool)
            .await
            .expect("owner");
        let blob_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.blob (schema_id, owner_id, content_hash)
             VALUES ($1, $2, $3) RETURNING blob_id",
        )
        .bind(UPLOADED_BLOB_SCHEMA_ID)
        .bind(owner_id)
        .bind(vec![1_u8; 32])
        .fetch_one(&pool)
        .await
        .expect("blob");
        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads
                (upload_id, owner_id, bucket, object_key, filename, mime, expected_byte_len,
                 status, blob_id, sha256, expires_at, completed_at, object_store)
             VALUES ($1, $2, 'bucket', 'objects/key', 'f.pdf', 'application/pdf', 4,
                     'completed', $3, $4, now() + interval '1 hour', now(), 'obj:main')",
        )
        .bind(upload_id)
        .bind(owner_id)
        .bind(blob_id)
        .bind(vec![2_u8; 32])
        .execute(&pool)
        .await
        .expect("completed upload");
        let owner = OwnerId::from(proxima_core::UserId::new(owner_id));

        let mut connection = pool.acquire().await.expect("connection");
        let staged = load_staged_payload_on_connection(&mut connection, &owner, upload_id, blob_id)
            .await
            .expect("a complete row reads back");
        assert_eq!(staged.payload.sha256, [2; 32]);
        assert_eq!(staged.payload.byte_len, 4);

        sqlx::query("UPDATE proxima_core.blob_uploads SET sha256 = NULL WHERE upload_id = $1")
            .bind(upload_id)
            .execute(&pool)
            .await
            .expect("drop digest");
        let error = load_staged_payload_on_connection(&mut connection, &owner, upload_id, blob_id)
            .await
            .expect_err("no digest, no payload");
        assert!(error.to_string().contains("no sha256"), "{error}");
        let error = load_blob_read_record_on_connection(&mut connection, &owner, blob_id)
            .await
            .expect_err("no digest, no read record");
        assert!(error.to_string().contains("no sha256"), "{error}");

        sqlx::query(
            "UPDATE proxima_core.blob_uploads SET sha256 = $2, completed_at = NULL
              WHERE upload_id = $1",
        )
        .bind(upload_id)
        .bind(vec![2_u8; 32])
        .execute(&pool)
        .await
        .expect("drop time");
        let error = load_staged_payload_on_connection(&mut connection, &owner, upload_id, blob_id)
            .await
            .expect_err("no completion time, no payload");
        assert!(error.to_string().contains("no completion time"), "{error}");

        drop(connection);
        drop_db(&db_name).await.expect("drop db");
    }
}
