//! The object-store half of an owner erase: remove an owner's BYTES, not
//! merely the rows that point at them.
//!
//! Postgres and S3 are two stores, and only one of them is reached by a
//! `DELETE`. A host that has promised a user their documents are gone has
//! not kept that promise while the objects are still fetchable, so the
//! inverse of storing a blob has to include this half.

use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use proxima_core::storage_ports::CitedObjectErasePort;
use proxima_core::{Owner, StorageError};

use super::CitedBlobStore;

#[async_trait::async_trait]
impl CitedObjectErasePort for CitedBlobStore {
    /// Purge every S3 object this owner's rows point at.
    ///
    /// Enumerated from Postgres, not from a key prefix. Keys carry no owner
    /// — that is what lets a transfer move a series without touching S3 —
    /// so there is no owner-scoped prefix to list. The rows are the index:
    /// `blob_uploads` for cited blobs at
    /// every status (pending bytes are as PII-bearing as committed ones)
    /// and `cooled` for cold Memory objects.
    ///
    /// The consequence is deliberate and worth naming: an object whose row
    /// is already gone (a crashed prepare, an abandoned upload) is no
    /// longer reachable by this purge. Those are reclaimed by the mandatory
    /// S3 lifecycle rule on `pending/` and by the orphan sweep
    /// `reconcile_all` reports.
    ///
    /// Wired in-band by the owner-scope erase so destroying an owner takes
    /// the owner's uploaded documents with it, not just the Postgres rows.
    /// Best-effort: the caller has already committed the row deletes and
    /// treats any error here as non-fatal.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Unavailable`] when the row enumeration or any
    /// S3 deletion fails.
    async fn purge_owner_objects(&self, owner: Owner) -> Result<u64, StorageError> {
        let client = self
            .client()
            .await
            .map_err(|e| StorageError::Unavailable(format!("s3 client: {e}")))?;
        let mut deleted = 0_u64;
        for key in owned_object_keys(&self.pool, &owner).await? {
            deleted =
                deleted.saturating_add(purge_exact_key(client, &self.config.bucket, &key).await?);
        }
        Ok(deleted)
    }
}

/// Every object key reachable from `owner`'s rows, deduplicated.
///
/// `blob_uploads` is read at every status on purpose: a pending row still
/// names bytes that were uploaded, and an aborted one may too.
async fn owned_object_keys(
    pool: &sqlx::PgPool,
    owner: &Owner,
) -> Result<Vec<String>, StorageError> {
    let owner_id = owner.stored_owner_id();
    sqlx::query_scalar::<_, String>(
        "SELECT object_key FROM proxima_core.blob_uploads WHERE owner_id = $1
         UNION
         SELECT object_key FROM proxima_core.cooled WHERE owner_id = $1",
    )
    .bind(owner_id)
    .fetch_all(pool)
    .await
    .map_err(|e| StorageError::Unavailable(format!("enumerate owner object keys: {e}")))
}

/// Permanently delete every version and delete marker of exactly one key.
/// Prefix-colliding keys are deliberately excluded.
///
/// Deletion is by `(key, version_id)`, not by key alone. On a *versioned*
/// bucket — the deployment recommended in `docs/how-to/operate.md` — a
/// key-only `delete_objects` merely inserts a delete marker and leaves the
/// noncurrent object versions recoverable via `GetObject?versionId` — the
/// bytes an erase claimed to destroy, still readable. Enumerating versions and
/// deleting each by its `version_id` physically removes the bytes. On a
/// non-versioned bucket every entry has `version_id = "null"`, so the same
/// path deletes the live object and remains correct.
pub(super) async fn purge_exact_key(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
) -> Result<u64, StorageError> {
    purge_versions(client, bucket, key, true).await
}

async fn purge_versions(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    listing_prefix: &str,
    exact: bool,
) -> Result<u64, StorageError> {
    let mut deleted = 0_u64;
    let mut key_marker: Option<String> = None;
    let mut version_id_marker: Option<String> = None;
    loop {
        let mut list = client
            .list_object_versions()
            .bucket(bucket)
            .prefix(listing_prefix);
        if let Some(km) = &key_marker {
            list = list.key_marker(km);
        }
        if let Some(vm) = &version_id_marker {
            list = list.version_id_marker(vm);
        }
        let page = list.send().await.map_err(|e| {
            StorageError::Unavailable(format!("list object versions under {listing_prefix}: {e}"))
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
            if let Some(key) = key
                && (!exact || key == listing_prefix)
            {
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
                    StorageError::Unavailable(format!("delete objects under {listing_prefix}: {e}"))
                })?;
            let errors = response.errors();
            if !errors.is_empty() {
                let first = errors
                    .first()
                    .and_then(aws_sdk_s3::types::Error::message)
                    .unwrap_or("unknown");
                return Err(StorageError::Unavailable(format!(
                    "delete objects under {listing_prefix} reported {} error(s): {first}",
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
