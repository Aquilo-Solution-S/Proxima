//! One pass over the bucket and the table, reporting where they disagree.

use std::collections::BTreeSet;

use proxima_core::authz::SystemAuthority;
use proxima_core::storage_ports::{
    CitedBlobMissingObject, CitedBlobOwnerMissingObject, CitedBlobOwnerReconcileOutcome,
    CitedBlobOwnerReconcilePort, CitedBlobReconcileOutcome, CitedBlobReconcilePort,
    MAX_RECONCILE_SAMPLE,
};
use proxima_core::{AuthzContext, OwnerRef, StorageError, UPLOADED_BLOB_SCHEMA_ID};
use proxima_storage_pg::begin_compatible_owner_transaction;
use time::OffsetDateTime;
use uuid::Uuid;

use super::CitedBlobStore;
use super::guards::ensure_owner_access;
use super::keys::{CANONICAL_OBJECT_PREFIX, locator_was_minted_here};
use super::live_upload::with_live_blob_upload;
use super::port::blob_error_to_storage;

/// Rows read per round trip.
///
/// Live rows page newest completion first, with upload id as the tie-breaker.
/// Historical claims page on the upload primary key, independently of blob id.
const ROW_PAGE: i64 = 1000;

#[derive(Debug, Clone, Copy)]
struct LiveUploadCursor {
    completed_at: Option<OffsetDateTime>,
    upload_id: Uuid,
}

#[derive(Debug, sqlx::FromRow)]
struct LiveUploadRow {
    cited_object_id: Uuid,
    bucket: String,
    object_key: String,
    upload_id: Uuid,
    mounted_from_upload_id: Option<Uuid>,
    completed_at: Option<OffsetDateTime>,
    byte_len: i64,
    filename: String,
}

impl LiveUploadRow {
    fn cursor(&self) -> LiveUploadCursor {
        LiveUploadCursor {
            completed_at: self.completed_at,
            upload_id: self.upload_id,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct CompletedClaimRow {
    bucket: String,
    object_key: String,
    upload_id: Uuid,
    mounted_from_upload_id: Option<Uuid>,
}

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
    /// Check live uploads for availability; retain all completed object claims.
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
        // `pending/` deliberately is not swept: in-flight uploads have no
        // completed `blob_uploads` row yet and would appear orphaned.
        let mut objects = self.list_keys(CANONICAL_OBJECT_PREFIX).await?;
        let objects_scanned = objects.len() as u64;

        let mut outcome = CitedBlobReconcileOutcome {
            objects_scanned,
            ..CitedBlobReconcileOutcome::default()
        };

        // Superseded uploads still claim present bytes. This separate pass
        // does not check availability or add samples: only the upload ordinary
        // reads resolve determines whether a citation is missing.
        // Keys move from `objects` to `claimed`, keeping both sets together
        // bounded by the original bucket listing.
        let mut claimed: BTreeSet<String> = BTreeSet::new();
        self.claim_completed_objects(&mut objects, &mut claimed)
            .await?;

        let mut after = None;
        loop {
            let mut tx = self.begin_reconcile_transaction().await?;
            let page = load_live_upload_page(tx.as_mut(), None, after).await?;
            tx.commit()
                .await
                .map_err(|err| StorageError::Unavailable(err.to_string()))?;

            if page.is_empty() {
                break;
            }
            for row in &page {
                after = Some(row.cursor());

                // Counted apart from the sweep, because the cause and the
                // repair are both different — see the field's own doc. The
                // test is the read gate's, not a prefix shape: the key a
                // row is allowed to name is the one derived from its own
                // `upload_id`, and nothing else in the canonical prefix
                // counts as this row's object.
                if row.bucket != self.config.bucket
                    || !locator_was_minted_here(
                        &row.object_key,
                        row.upload_id,
                        row.mounted_from_upload_id,
                    )
                {
                    outcome.foreign_locators = outcome.foreign_locators.saturating_add(1);
                    if outcome.foreign_sample.len() < MAX_RECONCILE_SAMPLE {
                        outcome
                            .foreign_sample
                            .push(format!("{}/{}", row.bucket, row.object_key));
                    }
                    continue;
                }

                outcome.rows_scanned = outcome.rows_scanned.saturating_add(1);
                // A mount may share a key with another live or superseded
                // upload. Presence in either partition proves it was listed.
                let named = objects.remove(&row.object_key) || claimed.contains(&row.object_key);
                if named {
                    // Only remember keys proven present. Every row mounting
                    // an absent object is itself a missing citation.
                    claimed.insert(row.object_key.clone());
                    continue;
                }
                outcome.missing_objects = outcome.missing_objects.saturating_add(1);
                if outcome.missing_sample.len() < MAX_RECONCILE_SAMPLE {
                    outcome.missing_sample.push(CitedBlobMissingObject {
                        cited_object_id: row.cited_object_id,
                        object_key: row.object_key.clone(),
                        byte_len: u64::try_from(row.byte_len).unwrap_or(0),
                        filename: row.filename.clone(),
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

    /// Check the live uploads of one authorized owner's cited blobs.
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

        // Keys carry no owner any more, so there is no `objects/<hash>/`
        // prefix to list and an owner-scoped ORPHAN is not a thing that can
        // be computed: an object with no row has no owner to attribute it
        // to. The rows are the index here, and each one's key is probed for
        // existence. Bucket-wide orphan detection lives in `reconcile_all`,
        // which is where it was always authoritative.
        let mut outcome = CitedBlobOwnerReconcileOutcome::default();

        let mut after = None;
        loop {
            let mut tx = begin_compatible_owner_transaction(&self.pool, authz.owner_scope())
                .await
                .map_err(|err| StorageError::Unavailable(err.to_string()))?;
            let page =
                load_live_upload_page(tx.as_mut(), Some(owner.stored_owner_id()), after).await?;
            tx.commit()
                .await
                .map_err(|err| StorageError::Unavailable(err.to_string()))?;

            if page.is_empty() {
                break;
            }
            for row in &page {
                after = Some(row.cursor());

                // The same provenance rule the read gate applies: a locator
                // this store did not mint is a foreign locator, not a
                // missing object.
                if row.bucket != self.config.bucket
                    || !locator_was_minted_here(
                        &row.object_key,
                        row.upload_id,
                        row.mounted_from_upload_id,
                    )
                {
                    outcome.foreign_locators = outcome.foreign_locators.saturating_add(1);
                    continue;
                }

                outcome.rows_scanned = outcome.rows_scanned.saturating_add(1);
                if self.object_exists(&row.object_key).await? {
                    outcome.objects_scanned = outcome.objects_scanned.saturating_add(1);
                    continue;
                }
                outcome.missing_objects = outcome.missing_objects.saturating_add(1);
                if outcome.missing_sample.len() < MAX_RECONCILE_SAMPLE {
                    outcome.missing_sample.push(CitedBlobOwnerMissingObject {
                        cited_object_id: row.cited_object_id,
                        byte_len: u64::try_from(row.byte_len).unwrap_or(0),
                        filename: row.filename.clone(),
                    });
                }
            }
            if page.len() < usize::try_from(ROW_PAGE).unwrap_or(usize::MAX) {
                break;
            }
        }

        // Structurally unavailable owner-scoped; see the note above.
        outcome.orphan_objects = 0;
        Ok(outcome)
    }

    async fn begin_reconcile_transaction(
        &self,
    ) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, StorageError> {
        match &self.platform_scope {
            Some(scope) => scope.begin().await,
            None => begin_compatible_owner_transaction(&self.pool, None).await,
        }
        .map_err(|err| StorageError::Unavailable(err.to_string()))
    }

    /// Claim every completed, locally minted locator, including superseded
    /// uploads. Missing historical keys have no effect on live availability.
    async fn claim_completed_objects(
        &self,
        objects: &mut BTreeSet<String>,
        claimed: &mut BTreeSet<String>,
    ) -> Result<(), StorageError> {
        let mut after: Option<Uuid> = None;
        loop {
            let mut tx = self.begin_reconcile_transaction().await?;
            let page = sqlx::query_as::<_, CompletedClaimRow>(
                "SELECT bucket, object_key, upload_id, mounted_from_upload_id
                   FROM proxima_core.blob_uploads
                  WHERE status = 'completed'
                    AND ($1::uuid IS NULL OR upload_id > $1)
                  ORDER BY upload_id
                  LIMIT $2",
            )
            .bind(after)
            .bind(ROW_PAGE)
            .fetch_all(&mut *tx)
            .await
            .map_err(|err| {
                StorageError::Unavailable(format!("read completed cited blob claims: {err}"))
            })?;
            tx.commit()
                .await
                .map_err(|err| StorageError::Unavailable(err.to_string()))?;

            for row in &page {
                after = Some(row.upload_id);
                if row.bucket == self.config.bucket
                    && locator_was_minted_here(
                        &row.object_key,
                        row.upload_id,
                        row.mounted_from_upload_id,
                    )
                    && objects.remove(&row.object_key)
                {
                    claimed.insert(row.object_key.clone());
                }
            }
            if page.len() < usize::try_from(ROW_PAGE).unwrap_or(usize::MAX) {
                break;
            }
        }
        Ok(())
    }

    /// Does exactly this key resolve? `head_object` rather than a listing:
    /// with owner-free keys the owner-scoped pass knows precisely which
    /// keys to ask about, and asking costs one request per row instead of a
    /// walk of the whole bucket.
    async fn object_exists(&self, key: &str) -> Result<bool, StorageError> {
        let client = self
            .client()
            .await
            .map_err(|e| StorageError::Unavailable(format!("s3 client: {e}")))?;
        match client
            .head_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(err) => {
                let service = err.into_service_error();
                if service.is_not_found() {
                    Ok(false)
                } else {
                    Err(StorageError::Unavailable(format!("head object: {service}")))
                }
            }
        }
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

/// The keyset follows the descending completion order through equal timestamps
/// and then through NULL completions. Upload primary keys make every boundary
/// unique, including when many live rows mount the same object.
async fn load_live_upload_page(
    connection: &mut sqlx::PgConnection,
    owner_id: Option<Uuid>,
    after: Option<LiveUploadCursor>,
) -> Result<Vec<LiveUploadRow>, StorageError> {
    const SQL: &str = with_live_blob_upload!(
        "SELECT b.blob_id AS cited_object_id, u.bucket, u.object_key, u.upload_id,
                u.mounted_from_upload_id, u.completed_at,
                u.expected_byte_len AS byte_len, u.filename
           FROM proxima_core.blob b
           JOIN",
        "WHERE b.schema_id = $1
            AND ($2::uuid IS NULL OR b.owner_id = $2)
            AND (
                $4::uuid IS NULL
                OR ($3::timestamptz IS NOT NULL AND (
                    u.completed_at < $3
                    OR u.completed_at IS NULL
                    OR (u.completed_at = $3 AND u.upload_id < $4)
                ))
                OR ($3::timestamptz IS NULL
                    AND u.completed_at IS NULL AND u.upload_id < $4)
            )
          ORDER BY u.completed_at DESC NULLS LAST, u.upload_id DESC
          LIMIT $5"
    );
    sqlx::query_as::<_, LiveUploadRow>(SQL)
        .bind(UPLOADED_BLOB_SCHEMA_ID)
        .bind(owner_id)
        .bind(after.and_then(|cursor| cursor.completed_at))
        .bind(after.map(|cursor| cursor.upload_id))
        .bind(ROW_PAGE)
        .fetch_all(connection)
        .await
        .map_err(|err| StorageError::Unavailable(format!("read live cited blob locators: {err}")))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use proxima_core::storage_ports::CitedBlobOwnerReconcileService;
    use proxima_core::{AuthPath, AuthzContext, Engine, FlavorRegistry, OwnerRef, UserId};
    use uuid::Uuid;

    use super::super::keys::{canonical_object_key, pending_object_key};
    use super::super::testkit::{lazy_test_pool, store_config};
    use super::*;

    #[test]
    fn sweep_prefix_contains_written_canonical_keys_only() {
        let upload_id = Uuid::now_v7();
        let canonical = canonical_object_key(upload_id);
        let pending = pending_object_key(upload_id);

        assert!(canonical.starts_with(CANONICAL_OBJECT_PREFIX));
        assert!(!pending.starts_with(CANONICAL_OBJECT_PREFIX));
    }

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
        let missing = next_list_token(true, None, &mut seen, CANONICAL_OBJECT_PREFIX)
            .expect_err("a partial listing is never a complete answer");
        assert!(matches!(missing, StorageError::Unavailable(_)));

        assert_eq!(
            next_list_token(true, Some("next"), &mut seen, CANONICAL_OBJECT_PREFIX)
                .expect("first token advances"),
            Some("next".to_owned())
        );
        let repeated = next_list_token(true, Some("next"), &mut seen, CANONICAL_OBJECT_PREFIX)
            .expect_err("a repeated token must not loop or return a partial answer");
        assert!(matches!(repeated, StorageError::Unavailable(_)));

        assert_eq!(
            next_list_token(false, None, &mut seen, CANONICAL_OBJECT_PREFIX)
                .expect("complete page stops"),
            None
        );
    }
}
