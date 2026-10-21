//! One pass over the bucket and the table, reporting where they disagree.

use std::collections::{BTreeMap, BTreeSet};

use proxima_core::authz::SystemAuthority;
use proxima_core::storage_ports::{
    CitedBlobMissingObject, CitedBlobOwnerMissingObject, CitedBlobOwnerReconcileOutcome,
    CitedBlobOwnerReconcilePort, CitedBlobReconcileOutcome, CitedBlobReconcilePort,
    MAX_RECONCILE_SAMPLE,
};
use proxima_core::{AuthzContext, OwnerId, StorageError, UPLOADED_BLOB_SCHEMA_ID};
use proxima_storage_pg::begin_owner_transaction;
use time::OffsetDateTime;
use uuid::Uuid;

use super::CitedBlobStore;
use super::guards::ensure_owner_access;
use super::keys::{
    CANONICAL_OBJECT_PREFIX, PENDING_OBJECT_PREFIX, canonical_object_key, minting_upload_id,
    pending_object_key,
};
use super::live_upload::with_live_blob_upload;
use super::port::blob_error_to_storage;
use super::rows::byte_len_from_column;

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
    object_store: String,
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
struct ClaimRow {
    owner_id: Uuid,
    upload_id: Uuid,
    mounted_from_upload_id: Option<Uuid>,
    /// Pending or completed: the upload may still publish, or did publish,
    /// its canonical object.
    live: bool,
    /// Still pending and inside its TTL: its transfer key is in flight.
    in_flight: bool,
}

/// A canonical object only ended uploads imply: the upload id the key is
/// derived from, and an owner one of those uploads belongs to.
#[derive(Debug, Clone, Copy)]
struct EndedClaim {
    implied: Uuid,
    owner_id: Uuid,
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
        owner: OwnerId,
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
        self.require_system_authority(authority, "global cited-blob reconciliation")?;
        // THE OBJECT SIDE FIRST, AND ENTIRELY. Holding the key set is what
        // lets the row side stream, and it is what removes any dependency
        // on the two sides being ordered the same way — a merge join would
        // have to match S3's UTF-8 byte order against Postgres' collation,
        // and under a non-C collation `/` and `-` sort in ways that would
        // silently report intact artefacts as missing. A set has no such
        // failure mode. The cost is one key per object in memory: ~120
        // bytes each, so a shelf of 52,000 artefacts is about 6 MB.
        // `pending/` is listed too: a transfer key no in-flight upload
        // claims is stale, reported apart from canonical orphans.
        let mut objects = self.list_keys(CANONICAL_OBJECT_PREFIX).await?;
        let mut transfers = self.list_keys(PENDING_OBJECT_PREFIX).await?;
        let objects_scanned = objects.len() as u64;

        let mut outcome = CitedBlobReconcileOutcome::default();
        outcome.objects_scanned = objects_scanned;

        // Every pending or completed row of this store claims the canonical
        // object its id implies, whatever its recorded key: a staged
        // pending row, a crash-window row still recording its transfer key.
        // An aborted or expired row claims nothing: its end already owed
        // both keys, so a canonical object it alone implies was put after
        // that debt drained, and this pass owes it again. Only an in-flight
        // row claims its transfer key.
        // This pass checks no availability and adds no samples: only the
        // upload ordinary reads resolve decides whether a citation is
        // missing. Keys move from `objects` to `claimed`, keeping both sets
        // together bounded by the original bucket listing.
        let mut claimed: BTreeSet<String> = BTreeSet::new();
        let ended = self
            .claim_row_keys(&mut objects, &mut transfers, &mut claimed)
            .await?;
        // A key that already has purge debt with this store is owed, not
        // orphaned: the drain will remove it.
        outcome.owed_objects = self.drop_owed_keys(&mut objects, &mut transfers).await?;
        outcome.newly_owed_objects = self.owe_ended_objects(&mut objects, ended).await?;
        outcome.owed_objects = outcome
            .owed_objects
            .saturating_add(outcome.newly_owed_objects);

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
                if !self.mints_locator(
                    &row.object_store,
                    &row.bucket,
                    &row.object_key,
                    row.upload_id,
                    row.mounted_from_upload_id,
                ) {
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
                        byte_len: byte_len_from_column(row.byte_len)
                            .map_err(blob_error_to_storage)?,
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
        outcome.stale_transfer_objects = transfers.len() as u64;
        outcome.stale_transfer_sample = transfers.into_iter().take(MAX_RECONCILE_SAMPLE).collect();
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
        owner: OwnerId,
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
            let mut tx = begin_owner_transaction(
                &self.pool,
                authz.owner_scope().ok_or_else(|| {
                    StorageError::ConstraintViolation("owner scope required".into())
                })?,
            )
            .await
            .map_err(|err| StorageError::Unavailable(err.to_string()))?;
            let page = load_live_upload_page(tx.as_mut(), Some(owner.into_inner()), after).await?;
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
                if !self.mints_locator(
                    &row.object_store,
                    &row.bucket,
                    &row.object_key,
                    row.upload_id,
                    row.mounted_from_upload_id,
                ) {
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
                        byte_len: byte_len_from_column(row.byte_len)
                            .map_err(blob_error_to_storage)?,
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
        self.platform_scope
            .begin()
            .await
            .map_err(|err| StorageError::Unavailable(err.to_string()))
    }

    /// Claim every key a live upload row of this store implies: the
    /// canonical object for a pending or completed row, the transfer key for
    /// an in-flight one. Returns the listed canonical objects only ended
    /// rows imply.
    async fn claim_row_keys(
        &self,
        objects: &mut BTreeSet<String>,
        transfers: &mut BTreeSet<String>,
        claimed: &mut BTreeSet<String>,
    ) -> Result<BTreeMap<String, EndedClaim>, StorageError> {
        let mut ended: BTreeMap<String, EndedClaim> = BTreeMap::new();
        let mut after: Option<Uuid> = None;
        loop {
            let mut tx = self.begin_reconcile_transaction().await?;
            let page = sqlx::query_as::<_, ClaimRow>(
                "SELECT owner_id, upload_id, mounted_from_upload_id,
                        status IN ('pending', 'completed') AS live,
                        (status = 'pending' AND expires_at > now()) AS in_flight
                   FROM proxima_core.blob_uploads
                  WHERE object_store = $3 AND bucket = $4
                    AND ($1::uuid IS NULL OR upload_id > $1)
                  ORDER BY upload_id
                  LIMIT $2",
            )
            .bind(after)
            .bind(ROW_PAGE)
            .bind(self.store_id.as_str())
            .bind(&self.config.bucket)
            .fetch_all(&mut *tx)
            .await
            .map_err(|err| StorageError::Unavailable(format!("read cited blob claims: {err}")))?;
            tx.commit()
                .await
                .map_err(|err| StorageError::Unavailable(err.to_string()))?;

            for row in &page {
                after = Some(row.upload_id);
                let implied = minting_upload_id(row.upload_id, row.mounted_from_upload_id);
                let canonical = canonical_object_key(implied);
                if row.live {
                    ended.remove(&canonical);
                    if objects.remove(&canonical) {
                        claimed.insert(canonical);
                    }
                } else if objects.contains(&canonical) {
                    ended.entry(canonical).or_insert(EndedClaim {
                        implied,
                        owner_id: row.owner_id,
                    });
                }
                if row.in_flight {
                    transfers.remove(&pending_object_key(row.upload_id));
                }
            }
            if page.len() < usize::try_from(ROW_PAGE).unwrap_or(usize::MAX) {
                break;
            }
        }
        Ok(ended)
    }

    /// Owe each listed canonical object that only ended uploads imply and no
    /// debt already covers, and count them. Each is decided in its own
    /// transaction, in the erase's lock order: owner fence, then the key.
    /// Under the key fence the claim is checked again, so an object a live
    /// row now implies is never owed.
    async fn owe_ended_objects(
        &self,
        objects: &mut BTreeSet<String>,
        ended: BTreeMap<String, EndedClaim>,
    ) -> Result<u64, StorageError> {
        use proxima_storage_pg::access::owner_columns as columns;

        let mut owed = 0_u64;
        for (key, claim) in ended {
            if !objects.contains(&key) {
                continue;
            }
            let mut tx = self.begin_reconcile_transaction().await?;
            columns::lock_owner_fence_shared_tx(&mut tx, &OwnerId::from(claim.owner_id)).await?;
            columns::lock_object_keys_tx(&mut tx, std::slice::from_ref(&key)).await?;
            let inserted = sqlx::query(
                "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, object_store)
                 SELECT $1, $2, $3
                  WHERE EXISTS (
                        SELECT 1 FROM proxima_core.blob_uploads
                         WHERE owner_id = $2 AND object_store = $3
                           AND status IN ('aborted', 'expired')
                           AND ((upload_id = $4 AND mounted_from_upload_id IS NULL)
                                OR mounted_from_upload_id = $4))
                    AND NOT EXISTS (
                        SELECT 1 FROM proxima_core.blob_uploads
                         WHERE object_store = $3
                           AND status IN ('pending', 'completed')
                           AND ((upload_id = $4 AND mounted_from_upload_id IS NULL)
                                OR mounted_from_upload_id = $4))
                 ON CONFLICT (object_store, object_key) DO NOTHING",
            )
            .bind(&key)
            .bind(claim.owner_id)
            .bind(self.store_id.as_str())
            .bind(claim.implied)
            .execute(&mut *tx)
            .await
            .map_err(|err| {
                StorageError::Unavailable(format!("owe an ended upload's object: {err}"))
            })?
            .rows_affected();
            tx.commit()
                .await
                .map_err(|err| StorageError::Unavailable(err.to_string()))?;
            if inserted == 1 {
                objects.remove(&key);
                owed += 1;
            }
        }
        Ok(owed)
    }

    /// Remove every listed key this store owes purge debt for, and count
    /// them.
    async fn drop_owed_keys(
        &self,
        objects: &mut BTreeSet<String>,
        transfers: &mut BTreeSet<String>,
    ) -> Result<u64, StorageError> {
        let mut owed = 0_u64;
        let mut after = String::new();
        loop {
            let mut tx = self.begin_reconcile_transaction().await?;
            let page: Vec<String> = sqlx::query_scalar(
                "SELECT object_key FROM proxima_core.cold_purge_pending
                  WHERE object_store = $1 AND object_key COLLATE \"C\" > $2
                  ORDER BY object_key COLLATE \"C\"
                  LIMIT $3",
            )
            .bind(self.store_id.as_str())
            .bind(&after)
            .bind(ROW_PAGE)
            .fetch_all(&mut *tx)
            .await
            .map_err(|err| StorageError::Unavailable(format!("read purge debt: {err}")))?;
            tx.commit()
                .await
                .map_err(|err| StorageError::Unavailable(err.to_string()))?;
            for key in &page {
                if objects.remove(key) || transfers.remove(key) {
                    owed += 1;
                }
            }
            match page.last() {
                Some(last) if page.len() == usize::try_from(ROW_PAGE).unwrap_or(usize::MAX) => {
                    after.clone_from(last);
                }
                _ => break,
            }
        }
        Ok(owed)
    }

    /// Does exactly this key resolve? `head_object` rather than a listing:
    /// with owner-free keys the owner-scoped pass knows precisely which
    /// keys to ask about, and asking costs one request per row instead of a
    /// walk of the whole bucket.
    async fn object_exists(&self, key: &str) -> Result<bool, StorageError> {
        let client = self.client();
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
        let client = self.client();
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
        "SELECT b.blob_id AS cited_object_id, u.bucket, u.object_store, u.object_key,
                u.upload_id,
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
    use proxima_core::{AuthPath, AuthzContext, Engine, FlavorRegistry, OwnerId, UserId};
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
        let store = CitedBlobStore::new(
            lazy_test_pool(),
            store_config(None, None),
            proxima_storage_pg::PgPlatformScope::for_tests(),
        )
        .expect("test store config");
        let readable = OwnerId::from(UserId::new(Uuid::now_v7()));
        let denied = OwnerId::from(UserId::new(Uuid::now_v7()));
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
        let store = CitedBlobStore::new(
            lazy_test_pool(),
            store_config(None, None),
            proxima_storage_pg::PgPlatformScope::for_tests(),
        )
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
        let store = CitedBlobStore::new(
            lazy_test_pool(),
            store_config(None, None),
            proxima_storage_pg::PgPlatformScope::for_tests(),
        )
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
