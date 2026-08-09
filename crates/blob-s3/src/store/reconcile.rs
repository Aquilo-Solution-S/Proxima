//! One pass over the bucket and the table, reporting where they disagree.

use std::collections::BTreeSet;

use proxima_core::authz::SystemAuthority;
use proxima_core::storage_ports::{
    CitedBlobMissingObject, CitedBlobOwnerMissingObject, CitedBlobOwnerReconcileOutcome,
    CitedBlobOwnerReconcilePort, CitedBlobReconcileOutcome, CitedBlobReconcilePort,
    MAX_RECONCILE_SAMPLE,
};
use proxima_core::{AuthzContext, OwnerRef, StorageError};
use sqlx::Row as _;

use super::CitedBlobStore;
use super::guards::ensure_owner_access;
use super::keys::{db_owner_columns, objects_owner_prefix, owner_hash_hex};
use super::port::blob_error_to_storage;

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
    async fn reconcile_all(
        &self,
        authority: &SystemAuthority,
    ) -> Result<CitedBlobReconcileOutcome, StorageError> {
        CitedBlobStore::reconcile_all(self, authority).await
    }
}

#[async_trait::async_trait]
impl CitedBlobOwnerReconcilePort for CitedBlobStore {
    async fn reconcile_owner(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
    ) -> Result<CitedBlobOwnerReconcileOutcome, StorageError> {
        CitedBlobStore::reconcile_owner(self, authz, owner).await
    }
}

impl CitedBlobStore {
    /// Reconcile this store's bucket against every row that names it.
    ///
    /// The runtime-held [`SystemAuthority`] is required even when the caller
    /// already holds the concrete store. Database and bucket credentials are
    /// capabilities to connect, not authority to inspect every owner.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Unavailable`] when either side cannot be
    /// read. Never a partial answer — half a comparison would report every
    /// row on the other side as diverged.
    pub async fn reconcile_all(
        &self,
        authority: &SystemAuthority,
    ) -> Result<CitedBlobReconcileOutcome, StorageError> {
        let binding = self.system_authority.get().ok_or_else(|| {
            StorageError::ConstraintViolation(
                "global cited-blob reconciliation requires a boot-bound store".into(),
            )
        })?;
        if !authority.authorizes(binding) {
            return Err(StorageError::ConstraintViolation(
                "SystemAuthority belongs to a different cited-blob store boot".into(),
            ));
        }
        // THE OBJECT SIDE FIRST, AND ENTIRELY. Holding the key set is what
        // lets the row side stream, and it is what removes any dependency
        // on the two sides being ordered the same way — a merge join would
        // have to match S3's UTF-8 byte order against Postgres' collation,
        // and under a non-C collation `/` and `-` sort in ways that would
        // silently report intact artefacts as missing. A set has no such
        // failure mode. The cost is one key per object in memory: ~120
        // bytes each, so a shelf of 52,000 artefacts is about 6 MB.
        let mut objects = self.list_keys(CANONICAL_PREFIX).await?;
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

    /// Reconcile one authorized owner's rows and canonical object prefix.
    ///
    /// Authorization is deliberately the first fallible operation. A denied
    /// caller must not turn this report into either a Postgres existence probe
    /// or an S3 listing oracle.
    ///
    /// # Errors
    ///
    /// Returns a constraint violation when `authz` cannot read this owner's
    /// Facts, and [`StorageError::Unavailable`] when either backing store
    /// cannot be read. The returned DTO contains no storage coordinates.
    pub async fn reconcile_owner(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
    ) -> Result<CitedBlobOwnerReconcileOutcome, StorageError> {
        ensure_owner_access(authz, &owner).map_err(blob_error_to_storage)?;

        let owner_prefix = objects_owner_prefix(&owner_hash_hex(&owner));
        let mut objects = self.list_keys(&owner_prefix).await?;
        let objects_scanned = objects.len() as u64;
        let (owner_kind, owner_id) = db_owner_columns(&owner);
        let mut outcome = CitedBlobOwnerReconcileOutcome {
            objects_scanned,
            ..CitedBlobOwnerReconcileOutcome::default()
        };

        let mut after = uuid::Uuid::nil();
        loop {
            let page = sqlx::query(
                "SELECT b.cited_object_id, b.bucket, b.object_key, b.byte_len, b.filename \
                   FROM proxima_core.cited_uploaded_blob_v1 b \
                   JOIN proxima_core.cited_objects co USING (cited_object_id) \
                  WHERE co.owner_kind = $1 \
                    AND co.owner_id IS NOT DISTINCT FROM $2 \
                    AND b.cited_object_id > $3 \
                  ORDER BY b.cited_object_id \
                  LIMIT $4",
            )
            .bind(owner_kind)
            .bind(owner_id)
            .bind(after)
            .bind(ROW_PAGE)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| {
                StorageError::Unavailable(format!("read owner cited blob locators: {e}"))
            })?;

            if page.is_empty() {
                break;
            }
            for row in &page {
                let cited_object_id = row.try_get("cited_object_id").map_err(|e| map_row(&e))?;
                let bucket: String = row.try_get("bucket").map_err(|e| map_row(&e))?;
                let object_key: String = row.try_get("object_key").map_err(|e| map_row(&e))?;
                after = cited_object_id;

                if bucket != self.config.bucket || !object_key.starts_with(&owner_prefix) {
                    outcome.foreign_locators = outcome.foreign_locators.saturating_add(1);
                    continue;
                }

                outcome.rows_scanned = outcome.rows_scanned.saturating_add(1);
                if objects.remove(&object_key) {
                    continue;
                }
                outcome.missing_objects = outcome.missing_objects.saturating_add(1);
                if outcome.missing_sample.len() < MAX_RECONCILE_SAMPLE {
                    let byte_len: i64 = row.try_get("byte_len").map_err(|e| map_row(&e))?;
                    outcome.missing_sample.push(CitedBlobOwnerMissingObject {
                        cited_object_id,
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
        Ok(outcome)
    }

    /// Every key under `prefix`, paged to exhaustion.
    ///
    /// `list_objects_v2` rather than `list_object_versions`: a noncurrent
    /// version is not something a row can point at — the locator names a
    /// key — so counting versions would inflate both sides of the
    /// comparison. The erase path pages versions because it must delete
    /// them; this one must only know whether the key resolves.
    async fn list_keys(&self, prefix: &str) -> Result<BTreeSet<String>, StorageError> {
        let client = self
            .client()
            .await
            .map_err(|e| StorageError::Unavailable(format!("s3 client: {e}")))?;
        let mut keys = BTreeSet::new();
        let mut token: Option<String> = None;
        let mut seen_tokens = BTreeSet::new();
        loop {
            let mut list = client
                .list_objects_v2()
                .bucket(&self.config.bucket)
                .prefix(prefix);
            if let Some(next) = &token {
                list = list.continuation_token(next);
            }
            let page = list.send().await.map_err(|e| {
                StorageError::Unavailable(format!("list objects under {prefix}: {e}"))
            })?;
            for object in page.contents() {
                if let Some(key) = object.key() {
                    keys.insert(key.to_owned());
                }
            }
            let Some(next) = next_list_token(
                page.is_truncated() == Some(true),
                page.next_continuation_token(),
                &mut seen_tokens,
                prefix,
            )?
            else {
                break;
            };
            token = Some(next);
        }
        Ok(keys)
    }
}

fn next_list_token(
    truncated: bool,
    next: Option<&str>,
    seen: &mut BTreeSet<String>,
    prefix: &str,
) -> Result<Option<String>, StorageError> {
    if !truncated {
        return Ok(None);
    }
    let next = next.filter(|value| !value.is_empty()).ok_or_else(|| {
        StorageError::Unavailable(format!(
            "list objects under {prefix}: truncated response omitted continuation token"
        ))
    })?;
    if !seen.insert(next.to_owned()) {
        return Err(StorageError::Unavailable(format!(
            "list objects under {prefix}: repeated continuation token"
        )));
    }
    Ok(Some(next.to_owned()))
}

fn map_row(err: &sqlx::Error) -> StorageError {
    StorageError::Unavailable(format!("decode cited blob locator: {err}"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use proxima_core::storage_ports::CitedBlobOwnerReconcileService;
    use proxima_core::{AuthPath, AuthzContext, Engine, FlavorRegistry, OwnerRef, UserId};
    use uuid::Uuid;

    use super::super::testkit::{lazy_test_pool, store_config};
    use super::*;

    #[tokio::test]
    async fn denied_owner_is_rejected_before_postgres_or_s3() {
        // Neither backing service exists. Reaching either one would return an
        // infrastructure error (or wait for a connection); the advertised
        // service instead returns the owner denial synchronously.
        let store = CitedBlobStore::new(lazy_test_pool(), store_config(None, None))
            .expect("test store config");
        let readable = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let denied = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let authz = AuthzContext::single_owner(&readable, AuthPath::HostBearer);
        let service = CitedBlobOwnerReconcileService::new(Arc::new(store));

        let error = service
            .reconcile_owner(&authz, denied)
            .await
            .expect_err("foreign owner must be denied before backing-store I/O");

        assert!(
            matches!(error, StorageError::ConstraintViolation(ref message) if message.contains("access denied")),
            "owner denial must not be replaced by a database or S3 error: {error}"
        );
    }

    #[tokio::test]
    async fn global_reconcile_rejects_an_unbound_store_before_io() {
        let store = CitedBlobStore::new(lazy_test_pool(), store_config(None, None))
            .expect("test store config");
        let (_, authority) =
            Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests()).into_system_authority();

        let error = store
            .reconcile_all(&authority)
            .await
            .expect_err("an unbound store cannot run a global operation");

        assert!(
            matches!(error, StorageError::ConstraintViolation(ref message) if message.contains("boot-bound store")),
            "binding denial must precede backing-store I/O: {error}"
        );
    }

    #[tokio::test]
    async fn global_reconcile_rejects_a_foreign_boot_before_io() {
        let store = CitedBlobStore::new(lazy_test_pool(), store_config(None, None))
            .expect("test store config");
        let (_, authority) =
            Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests()).into_system_authority();
        let (_, foreign) =
            Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests()).into_system_authority();
        store
            .bind_system_authority(&authority)
            .expect("store binds to the first boot");

        let error = store
            .reconcile_all(&foreign)
            .await
            .expect_err("another boot cannot run a global operation");

        assert!(
            matches!(error, StorageError::ConstraintViolation(ref message) if message.contains("different cited-blob store boot")),
            "foreign-boot denial must precede backing-store I/O: {error}"
        );
    }

    #[test]
    fn truncated_object_listing_requires_a_fresh_continuation_token() {
        let mut seen = BTreeSet::new();
        let missing = next_list_token(true, None, &mut seen, "objects/")
            .expect_err("a partial listing is never a complete answer");
        assert!(matches!(missing, StorageError::Unavailable(_)));

        assert_eq!(
            next_list_token(true, Some("next"), &mut seen, "objects/")
                .expect("first token advances"),
            Some("next".to_owned())
        );
        let repeated = next_list_token(true, Some("next"), &mut seen, "objects/")
            .expect_err("a repeated token must not loop or return a partial answer");
        assert!(matches!(repeated, StorageError::Unavailable(_)));

        assert_eq!(
            next_list_token(false, None, &mut seen, "objects/").expect("complete page stops"),
            None
        );
    }
}
