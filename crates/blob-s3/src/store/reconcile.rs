//! One pass over the bucket and the table, reporting where they disagree.

use std::collections::BTreeSet;

use proxima_core::StorageError;
use proxima_core::storage_ports::{
    CitedBlobMissingObject, CitedBlobReconcileOutcome, CitedBlobReconcilePort,
    MAX_RECONCILE_SAMPLE, OperatorMaintenanceProof,
};
use sqlx::Row as _;

use super::CitedBlobStore;

/// Rows read per round trip.
///
/// Paged on `cited_object_id` because it is the PRIMARY KEY and therefore
/// the only indexed column on this table. Paging on `object_key` — the
/// column this verb actually cares about — would sort the whole table on
/// every batch, turning a linear sweep into a quadratic one. The comparison
/// below is set-based and order-free, so the key it pages by is free to be
/// whichever one is indexed.
const ROW_PAGE: i64 = 1000;

/// Every canonical object lives under this prefix; `pending/` deliberately
/// does not, and must not be swept.
///
/// A pending object has NO `cited_uploaded_blob_v1` row by design — the row
/// is written by the completion, not the transfer — so including `pending/`
/// would report every upload currently in flight as an orphan. That is not
/// a hypothetical race: on a 632-page book at concurrency 4 there are
/// always several.
const CANONICAL_PREFIX: &str = "objects/";

#[async_trait::async_trait]
impl CitedBlobReconcilePort for CitedBlobStore {
    /// Forwards to the inherent verb, which is also what the operator CLI
    /// calls. Same split as `PgStorage`'s embedding maintenance: the proof
    /// witnesses that an in-process caller came through engine
    /// authorization, and an operator holding the store's own credentials
    /// is already past any gate the proof could represent.
    async fn reconcile_cited_blobs(
        &self,
        _proof: OperatorMaintenanceProof,
    ) -> Result<CitedBlobReconcileOutcome, StorageError> {
        CitedBlobStore::reconcile_cited_blobs(self).await
    }
}

impl CitedBlobStore {
    /// Reconcile this store's bucket against the rows that name it.
    ///
    /// Operator surface, like `PgStorage::sweep_orphan_embedding_rows`:
    /// reaching this requires the database credentials and the bucket
    /// credentials, which is the authority it is gated on.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Unavailable`] when either side cannot be
    /// read. Never a partial answer — half a comparison would report every
    /// row on the other side as diverged.
    pub async fn reconcile_cited_blobs(&self) -> Result<CitedBlobReconcileOutcome, StorageError> {
        // THE OBJECT SIDE FIRST, AND ENTIRELY. Holding the key set is what
        // lets the row side stream, and it is what removes any dependency
        // on the two sides being ordered the same way — a merge join would
        // have to match S3's UTF-8 byte order against Postgres' collation,
        // and under a non-C collation `/` and `-` sort in ways that would
        // silently report intact artefacts as missing. A set has no such
        // failure mode. The cost is one key per object in memory: ~120
        // bytes each, so a shelf of 52,000 artefacts is about 6 MB.
        let mut objects = self.list_canonical_keys().await?;
        let objects_scanned = objects.len() as u64;

        let mut outcome = CitedBlobReconcileOutcome {
            objects_scanned,
            ..CitedBlobReconcileOutcome::default()
        };

        // The row side is paged, so the number of rows never bounds memory
        // even though the object side does.
        let mut after = uuid::Uuid::nil();
        loop {
            let page = sqlx::query(
                "SELECT cited_object_id, bucket, object_key, byte_len, filename \
                   FROM proxima_core.cited_uploaded_blob_v1 \
                  WHERE cited_object_id > $1 \
                  ORDER BY cited_object_id \
                  LIMIT $2",
            )
            .bind(after)
            .bind(ROW_PAGE)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Unavailable(format!("read cited blob locators: {e}")))?;

            if page.is_empty() {
                break;
            }
            for row in &page {
                let bucket: String = row.try_get("bucket").map_err(|e| map_row(&e))?;
                let object_key: String = row.try_get("object_key").map_err(|e| map_row(&e))?;
                after = row.try_get("cited_object_id").map_err(|e| map_row(&e))?;

                // Counted apart from the sweep, because the cause and the
                // repair are both different — see the field's own doc.
                if bucket != self.config.bucket || !object_key.starts_with(CANONICAL_PREFIX) {
                    outcome.foreign_locators = outcome.foreign_locators.saturating_add(1);
                    if outcome.foreign_sample.len() < MAX_RECONCILE_SAMPLE {
                        outcome
                            .foreign_sample
                            .push(format!("{bucket}/{object_key}"));
                    }
                    continue;
                }

                outcome.rows_scanned = outcome.rows_scanned.saturating_add(1);
                // Removing rather than testing is what leaves the orphans
                // behind: whatever is still in the set when the rows run
                // out is an object no row named.
                if objects.remove(&object_key) {
                    continue;
                }
                outcome.missing_objects = outcome.missing_objects.saturating_add(1);
                if outcome.missing_sample.len() < MAX_RECONCILE_SAMPLE {
                    let byte_len: i64 = row.try_get("byte_len").map_err(|e| map_row(&e))?;
                    outcome.missing_sample.push(CitedBlobMissingObject {
                        cited_object_id: row.try_get("cited_object_id").map_err(|e| map_row(&e))?,
                        object_key,
                        byte_len: u64::try_from(byte_len).unwrap_or(0),
                        filename: row.try_get("filename").map_err(|e| map_row(&e))?,
                    });
                }
            }
            if page.len() < usize::try_from(ROW_PAGE).unwrap_or(usize::MAX) {
                break;
            }
        }

        outcome.orphan_objects = objects.len() as u64;
        outcome.orphan_sample = objects.into_iter().take(MAX_RECONCILE_SAMPLE).collect();
        Ok(outcome)
    }

    /// Every key under `objects/`, paged to exhaustion.
    ///
    /// `list_objects_v2` rather than `list_object_versions`: a noncurrent
    /// version is not something a row can point at — the locator names a
    /// key — so counting versions would inflate both sides of the
    /// comparison. The erase path pages versions because it must delete
    /// them; this one must only know whether the key resolves.
    async fn list_canonical_keys(&self) -> Result<BTreeSet<String>, StorageError> {
        let client = self
            .client()
            .await
            .map_err(|e| StorageError::Unavailable(format!("s3 client: {e}")))?;
        let mut keys = BTreeSet::new();
        let mut token: Option<String> = None;
        loop {
            let mut list = client
                .list_objects_v2()
                .bucket(&self.config.bucket)
                .prefix(CANONICAL_PREFIX);
            if let Some(next) = &token {
                list = list.continuation_token(next);
            }
            let page = list.send().await.map_err(|e| {
                StorageError::Unavailable(format!("list objects under {CANONICAL_PREFIX}: {e}"))
            })?;
            for object in page.contents() {
                if let Some(key) = object.key() {
                    keys.insert(key.to_owned());
                }
            }
            if page.is_truncated() == Some(true) {
                // A truncated page with no token would loop forever on the
                // first page; treat it as the end rather than spin.
                token = page.next_continuation_token().map(str::to_string);
                if token.is_none() {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(keys)
    }
}

fn map_row(err: &sqlx::Error) -> StorageError {
    StorageError::Unavailable(format!("decode cited blob locator: {err}"))
}
