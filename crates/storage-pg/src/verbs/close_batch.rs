//! `CloseBatch` verb — owner-scoped, idempotent UPDATE on
//! `proxima_core.source_batches.closed_at`.
//!
//! Re-close is a no-op returning the existing `closed_at` with
//! `already_closed = true`. A batch belonging to a different owner
//! principal returns `StorageError::NotFound` to avoid information
//! leak.
//!
//! v1 emits no `change_event` for batch-closed; M5's F→A operator
//! reads `closed_at` directly off `source_batches`. We add a
//! `change_event` here once a consumer needs the live signal.

use proxima_core::verbs::close_batch::CloseBatchOutcome;
use proxima_core::{OwnerPrincipalKind, Principal, SourceBatchId, StorageError};
use sqlx::PgPool;

use crate::error::map_err;

/// # Errors
///
/// Returns `NotFound` when the batch doesn't exist for `owner`;
/// `Internal` on sqlx failure.
pub async fn close_batch(
    pool: &PgPool,
    principal: &Principal,
    source_batch_id: SourceBatchId,
) -> Result<CloseBatchOutcome, StorageError> {
    let (owner_kind, owner_principal_id) = principal.columns();
    let batch_id = source_batch_id.into_inner();

    // Read current closed_at under owner scope.
    let existing = sqlx::query!(
        r#"SELECT closed_at FROM proxima_core.source_batches
             WHERE id = $1
               AND owner_principal_kind = $2
               AND owner_principal_id = $3"#,
        batch_id,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    let Some(row) = existing else {
        return Err(StorageError::NotFound);
    };

    if let Some(closed_at) = row.closed_at {
        return Ok(CloseBatchOutcome {
            source_batch_id,
            closed_at,
            already_closed: true,
        });
    }

    // Idempotent UPDATE: only flip when still NULL. If a concurrent
    // closer beat us, RETURNING is empty and we fall through to a
    // re-read.
    let updated = sqlx::query!(
        r#"UPDATE proxima_core.source_batches
             SET closed_at = now()
             WHERE id = $1
               AND owner_principal_kind = $2
               AND owner_principal_id = $3
               AND closed_at IS NULL
             RETURNING closed_at AS "closed_at!""#,
        batch_id,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    if let Some(row) = updated {
        return Ok(CloseBatchOutcome {
            source_batch_id,
            closed_at: row.closed_at,
            already_closed: false,
        });
    }

    // Lost the race; re-read to get the winner's closed_at.
    let row = sqlx::query!(
        r#"SELECT closed_at AS "closed_at!" FROM proxima_core.source_batches
             WHERE id = $1
               AND owner_principal_kind = $2
               AND owner_principal_id = $3
               AND closed_at IS NOT NULL"#,
        batch_id,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
    )
    .fetch_one(pool)
    .await
    .map_err(map_err)?;

    Ok(CloseBatchOutcome {
        source_batch_id,
        closed_at: row.closed_at,
        already_closed: true,
    })
}
