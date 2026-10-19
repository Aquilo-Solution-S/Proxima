//! Expire abandoned uploads nobody touches again.
//!
//! An upload expires on touch: a stage or finish that finds its TTL past
//! marks it `expired`. A prepare that nobody touches again would otherwise
//! stay `pending` forever, and the bytes a client PUT under its transfer key
//! with it. The sweep expires such rows in bounded batches and owes both of
//! their keys, exactly as the touch path does.

use proxima_core::StorageError;
use proxima_core::authz::SystemAuthority;
use proxima_storage_pg::verbs::forget::ColdPurgeEntry;
use uuid::Uuid;

use super::CitedBlobStore;
use super::keys::canonical_object_key;

/// What one [`CitedBlobStore::sweep_expired_uploads`] call did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct UploadExpirySweepOutcome {
    /// Uploads this call moved from `pending` to `expired`.
    pub expired: u64,
    /// Object keys those expiries owed (two per upload).
    pub owed: u64,
    /// Expirable uploads of this store still `pending` after the call: past
    /// the batch limit, or held by a concurrent stage or abort.
    pub remaining: u64,
}

/// Expirable: a pending, unstaged upload of this store whose TTL is past. A
/// staged row is not expirable: only a finish or an abort decides it. The
/// three statements below spell the predicate out alike.
macro_rules! expirable {
    () => {
        "status = 'pending' AND content_hash IS NULL AND expires_at < now() AND object_store = $1"
    };
}

impl CitedBlobStore {
    /// Expire up to `limit` uploads of this store whose TTL passed without a
    /// stage, owe both keys of each, and purge them after commit.
    ///
    /// Each row is decided in its own transaction, in the order the touch
    /// path takes its locks: owner fence, canonical key, row. A row a
    /// concurrent stage or abort holds is skipped and counted in
    /// `remaining`; the next sweep, or the touch itself, decides it.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::ConstraintViolation`] when `authority` did not
    /// boot this store, and [`StorageError::Unavailable`] when the database
    /// cannot be reached. A failed purge is not an error: the debt stays for
    /// the drain.
    pub async fn sweep_expired_uploads(
        &self,
        authority: &SystemAuthority,
        limit: u32,
    ) -> Result<UploadExpirySweepOutcome, StorageError> {
        self.require_system_authority(authority, "the expired-upload sweep")?;
        let mut outcome = UploadExpirySweepOutcome::default();
        let mut owed: Vec<ColdPurgeEntry> = Vec::new();
        if limit > 0 {
            let mut tx = self.begin_platform_transaction().await?;
            let candidates: Vec<(Uuid, Uuid)> = sqlx::query_as(concat!(
                "SELECT owner_id, upload_id FROM proxima_core.blob_uploads WHERE ",
                expirable!(),
                " ORDER BY expires_at, upload_id LIMIT $2"
            ))
            .bind(self.store_id.as_str())
            .bind(i64::from(limit))
            .fetch_all(tx.as_mut())
            .await
            .map_err(unavailable)?;
            tx.commit().await.map_err(unavailable)?;
            for (owner_id, upload_id) in candidates {
                let owner = proxima_core::OwnerId::from(owner_id);
                if let Some(entries) = self.expire_one(&owner, upload_id).await? {
                    outcome.expired += 1;
                    outcome.owed += entries.len() as u64;
                    owed.extend(entries);
                }
            }
        }
        self.purge_owed_after_commit(owed).await;
        let mut tx = self.begin_platform_transaction().await?;
        let remaining: i64 = sqlx::query_scalar(concat!(
            "SELECT count(*) FROM proxima_core.blob_uploads WHERE ",
            expirable!()
        ))
        .bind(self.store_id.as_str())
        .fetch_one(tx.as_mut())
        .await
        .map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        outcome.remaining = u64::try_from(remaining).unwrap_or(0);
        Ok(outcome)
    }

    /// Expire one upload if it is still expirable and nobody holds it.
    async fn expire_one(
        &self,
        owner: &proxima_core::OwnerId,
        upload_id: Uuid,
    ) -> Result<Option<Vec<ColdPurgeEntry>>, StorageError> {
        use proxima_storage_pg::access::owner_columns as columns;

        let mut tx = self.begin_platform_transaction().await?;
        columns::lock_owner_fence_shared_tx(&mut tx, owner).await?;
        columns::lock_object_keys_tx(&mut tx, &[canonical_object_key(upload_id)]).await?;
        let expired = sqlx::query(concat!(
            "UPDATE proxima_core.blob_uploads
                SET status = 'expired', error_message = 'upload expired'
              WHERE upload_id = (
                    SELECT upload_id FROM proxima_core.blob_uploads
                     WHERE owner_id = $2 AND upload_id = $3 AND ",
            expirable!(),
            " FOR UPDATE SKIP LOCKED)"
        ))
        .bind(self.store_id.as_str())
        .bind(owner.into_inner())
        .bind(upload_id)
        .execute(tx.as_mut())
        .await
        .map_err(unavailable)?
        .rows_affected();
        if expired != 1 {
            tx.rollback().await.map_err(unavailable)?;
            return Ok(None);
        }
        let entries =
            columns::owe_terminal_upload_keys_tx(&mut tx, &self.store_id, owner, upload_id).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(Some(entries))
    }

    async fn begin_platform_transaction(
        &self,
    ) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, StorageError> {
        self.platform_scope.begin().await
    }
}

fn unavailable(err: impl std::fmt::Display) -> StorageError {
    StorageError::Unavailable(err.to_string())
}
