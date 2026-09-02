//! The object-store half of an owner erase: remove an owner's BYTES, not
//! merely the rows that point at them.
//!
//! Postgres and S3 are two stores, and only one of them is reached by a
//! `DELETE`. A host that has promised a user their documents are gone has
//! not kept that promise while the objects are still fetchable, so the
//! inverse of storing a blob has to include this half.
//!
//! The half lives in ONE place: the erase transaction enqueues every object
//! key it orphans in `proxima_core.cold_purge_pending` — under the object-key
//! lock, so the refcount that decided it is true at decision time — and the
//! wired `ColdObjectStore` drains that queue after the commit. There is no
//! second, owner-enumerating pass here. The `CitedObjectErasePort` that used
//! to run one was vacuous by construction: it read `blob_uploads` and
//! `cooled` for the owner AFTER the same transaction had deleted every one
//! of those rows, so it always enumerated nothing and always reported a
//! clean purge.

use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use proxima_core::StorageError;

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
