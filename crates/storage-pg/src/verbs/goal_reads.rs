//! Owner-scoped Goal read atoms used by Engine write preflight.

use proxima_core::{GoalId, MemoryId, OwnerRef, StorageError};
use sqlx::PgPool;

use crate::error::map_err;

/// Read the Goal's declared evidence without joining through Memory.
///
/// The array is the Goal's statement, so visibility of an evidence target
/// must not change its length before the transactional write validates it.
pub(crate) async fn load_goal_evidence(
    pool: &PgPool,
    owner: &OwnerRef,
    goal_id: GoalId,
) -> Result<Option<Vec<MemoryId>>, StorageError> {
    let evidence: Option<Vec<uuid::Uuid>> = sqlx::query_scalar(
        "SELECT evidence_t
           FROM proxima_core.goal
          WHERE t = $1 AND owner_id = $2",
    )
    .bind(goal_id.into_inner())
    .bind(owner.stored_owner_id())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(evidence.map(|ids| ids.into_iter().map(MemoryId::new).collect()))
}
