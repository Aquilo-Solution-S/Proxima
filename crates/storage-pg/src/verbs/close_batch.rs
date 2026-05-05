//! `CloseBatch` verb — owner-scoped, idempotent UPDATE on
//! `proxima_core.source_batches.closed_at`.
//!
//! Re-close is a no-op returning the existing `closed_at` with
//! `already_closed = true`. A batch belonging to a different owner
//! returns `StorageError::NotFound` to avoid information leak.
//!
//! v1 emits no `change_event` for batch-closed; M5's F→A operator
//! reads `closed_at` directly off `source_batches`. We add an outbox
//! event here once a consumer needs the live signal.

use proxima_core::verbs::close_batch::CloseBatchOutcome;
use proxima_core::{Owner, Principal, SourceBatchId, StorageError};
use sqlx::PgPool;

use crate::error::map_err;

/// # Errors
///
/// Returns `NotFound` when the batch doesn't exist for `owner`;
/// `Internal` on sqlx failure.
pub async fn close_batch(
    pool: &PgPool,
    owner: &Owner,
    source_batch_id: SourceBatchId,
) -> Result<CloseBatchOutcome, StorageError> {
    let (owner_kind, owner_principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let owner_org_id = owner.org_id.into_inner();
    let batch_id = source_batch_id.into_inner();

    // Read current closed_at under owner scope.
    let existing: Option<(Option<time::OffsetDateTime>,)> = sqlx::query_as(
        "SELECT closed_at FROM proxima_core.source_batches \
         WHERE id = $1 \
           AND owner_principal_kind = $2 \
           AND owner_principal_id = $3 \
           AND owner_org_id = $4",
    )
    .bind(batch_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    let Some((maybe_closed_at,)) = existing else {
        return Err(StorageError::NotFound);
    };

    if let Some(closed_at) = maybe_closed_at {
        return Ok(CloseBatchOutcome {
            source_batch_id,
            closed_at,
            already_closed: true,
        });
    }

    // Idempotent UPDATE: only flip when still NULL. If a concurrent
    // closer beat us, RETURNING is empty and we fall through to a
    // re-read.
    let updated: Option<(time::OffsetDateTime,)> = sqlx::query_as(
        "UPDATE proxima_core.source_batches \
         SET closed_at = now() \
         WHERE id = $1 \
           AND owner_principal_kind = $2 \
           AND owner_principal_id = $3 \
           AND owner_org_id = $4 \
           AND closed_at IS NULL \
         RETURNING closed_at",
    )
    .bind(batch_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    if let Some((closed_at,)) = updated {
        return Ok(CloseBatchOutcome {
            source_batch_id,
            closed_at,
            already_closed: false,
        });
    }

    // Lost the race; re-read to get the winner's closed_at.
    let (closed_at,): (time::OffsetDateTime,) = sqlx::query_as(
        "SELECT closed_at FROM proxima_core.source_batches \
         WHERE id = $1 \
           AND owner_principal_kind = $2 \
           AND owner_principal_id = $3 \
           AND owner_org_id = $4 \
           AND closed_at IS NOT NULL",
    )
    .bind(batch_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_one(pool)
    .await
    .map_err(map_err)?;

    Ok(CloseBatchOutcome {
        source_batch_id,
        closed_at,
        already_closed: true,
    })
}
