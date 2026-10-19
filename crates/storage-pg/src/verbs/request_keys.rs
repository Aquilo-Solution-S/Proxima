//! Tool `idempotency_key` request ledger, stored in `proxima_core.ingest_keys`
//! under source `core/key/<scope>`.

use proxima_core::verbs::idempotency::{
    ExistingHandle, KeyConflict, KeyVerdict, RequestKey, judge,
};
use proxima_core::{OwnerId, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::{internal, map_err};

/// Replay answer from the request ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestReplay {
    /// Admission `t` (= `memory_id`).
    pub memory_id: Uuid,
    /// Series handle.
    pub handle: Uuid,
    pub forgotten: bool,
}

impl ExistingHandle for RequestReplay {
    fn as_existing_uuid(&self) -> Uuid {
        self.memory_id
    }
}

/// Judge a keyed request against the ledger. Forgotten admissions still
/// answer (as `#488`), with `forgotten: true`.
pub(crate) async fn judge_request(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    request: &RequestKey,
) -> Result<KeyVerdict<RequestReplay>, StorageError> {
    let owner_id = owner.into_inner();
    let source = request.scope().ledger_source();
    let request_row = request.request_row_key();
    let fingerprint_row = request.fingerprint_row_key();

    let Some(t): Option<Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_core.ingest_keys
          WHERE owner_id = $1 AND source_id = $2 AND ingest_key = $3",
    )
    .bind(owner_id)
    .bind(&source)
    .bind(&request_row)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    else {
        return judge(None, request.key()).map_err(conflict_to_storage);
    };

    let lifecycle = crate::lifecycle_scope::PgLifecycleScope::enter(tx.as_mut()).await?;
    let row: Result<Option<(Uuid, bool)>, StorageError> = sqlx::query_as(
        "SELECT handle, forgotten_at IS NOT NULL
           FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (keyed replay answers forgotten) */
          WHERE t = $1 AND owner_id = $2",
    )
    .bind(t)
    .bind(owner_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err);
    let (series_handle, forgotten) = lifecycle
        .finish(tx.as_mut(), row)
        .await?
        .ok_or_else(|| internal(format!("request key claims t {t} with no memory row")))?;

    let fingerprint_matches: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM proxima_core.ingest_keys
              WHERE owner_id = $1 AND source_id = $2 AND ingest_key = $3 AND t = $4
         )",
    )
    .bind(owner_id)
    .bind(&source)
    .bind(&fingerprint_row)
    .bind(t)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;

    let replay = RequestReplay {
        memory_id: t,
        handle: series_handle,
        forgotten,
    };
    judge(Some((replay, fingerprint_matches)), request.key()).map_err(conflict_to_storage)
}

/// Claim both ledger rows for a freshly admitted `t`. On a race, re-judges
/// and returns the concurrent verdict instead of admitting twice.
pub(crate) async fn claim_request(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    request: &RequestKey,
    t: Uuid,
) -> Result<KeyVerdict<RequestReplay>, StorageError> {
    let owner_id = owner.into_inner();
    let source = request.scope().ledger_source();
    crate::access::owner_columns::lock_source_fence_shared_tx(tx, owner, &source).await?;

    let request_row = request.request_row_key();
    let fingerprint_row = request.fingerprint_row_key();

    let claimed = sqlx::query(
        "INSERT INTO proxima_core.ingest_keys (owner_id, source_id, ingest_key, t)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (owner_id, source_id, ingest_key) DO NOTHING",
    )
    .bind(owner_id)
    .bind(&source)
    .bind(&request_row)
    .bind(t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    if claimed.rows_affected() == 0 {
        return judge_request(tx, owner, request).await;
    }

    sqlx::query(
        "INSERT INTO proxima_core.ingest_keys (owner_id, source_id, ingest_key, t)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (owner_id, source_id, ingest_key) DO NOTHING",
    )
    .bind(owner_id)
    .bind(&source)
    .bind(&fingerprint_row)
    .bind(t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    Ok(KeyVerdict::Fresh)
}

fn conflict_to_storage(conflict: KeyConflict) -> StorageError {
    StorageError::IdempotencyConflict {
        request_id: conflict.key,
        existing: Some(conflict.existing),
    }
}
