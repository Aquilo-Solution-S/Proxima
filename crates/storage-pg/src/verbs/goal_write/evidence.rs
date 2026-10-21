use super::{
    EntityKind, EvidenceTarget, GoalEvidenceRef, GoalId, HashSet, MemoryId, OwnerId, Postgres,
    StorageError, Transaction, map_err,
};
use crate::pg_enums::PgMemoryKind;

/// The one evidence-kind rule for every goal write (set, modify, decompose
/// child, episode goal, `mark_achieved`): a live Fact or Abstraction; a
/// Perspective is refused. A memory of another store is judged by the kind
/// that store reported when the caller read it.
pub(super) async fn validate_evidence_in_owner(
    tx: &mut Transaction<'_, Postgres>,
    _owner: &OwnerId,
    evidence: &[GoalEvidenceRef],
) -> Result<Vec<EvidenceTarget>, StorageError> {
    let mut seen = HashSet::with_capacity(evidence.len());
    let mut out = Vec::with_capacity(evidence.len());
    for item in evidence {
        if !seen.insert(item.memory_id()) {
            return Err(StorageError::ConstraintViolation(
                "duplicate goal evidence".into(),
            ));
        }
        out.push(evidence_target(tx, *item).await?);
    }
    Ok(out)
}

async fn evidence_target(
    tx: &mut Transaction<'_, Postgres>,
    item: GoalEvidenceRef,
) -> Result<EvidenceTarget, StorageError> {
    let kind = if let Some(origin) = item.remote_origin() {
        origin.kind()
    } else {
        let stored: Option<PgMemoryKind> =
            sqlx::query_scalar("SELECT kind FROM proxima_core.memory WHERE t = $1")
                .bind(item.memory_id().into_inner())
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_err)?;
        let Some(stored) = stored else {
            return Err(StorageError::ConstraintViolation(
                proxima_core::verbs::goal_write::EVIDENCE_DOES_NOT_EXIST.into(),
            ));
        };
        EntityKind::from(stored)
    };
    match kind {
        EntityKind::Fact | EntityKind::Abstraction => Ok(EvidenceTarget {
            memory_id: item.memory_id(),
        }),
        _ => Err(StorageError::ConstraintViolation(
            "evidence must be Fact or Abstraction".into(),
        )),
    }
}

/// The evidence an omitted-evidence modify carries from its prior Goal, in the
/// prior order. Evidence held by this store is judged again; evidence of
/// another store was read there when the Goal named it, and stays.
///
/// The evidence column is read from the Goal row itself: joining through
/// Memory would turn a forgotten or missing target into a shorter successor
/// statement.
pub(super) async fn carried_evidence(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    goal_id: GoalId,
    store_number: u16,
) -> Result<Vec<EvidenceTarget>, StorageError> {
    let ids: Option<Vec<uuid::Uuid>> = sqlx::query_scalar(
        "SELECT evidence_t
           FROM proxima_core.goal
          WHERE t = $1 AND owner_id = $2",
    )
    .bind(goal_id.into_inner())
    .bind(owner.into_inner())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    let mut out = Vec::new();
    for id in ids.unwrap_or_default() {
        let memory_id = MemoryId::new(id);
        out.push(if proxima_core::memory_store_number(id) == store_number {
            evidence_target(tx, GoalEvidenceRef::new(memory_id)).await?
        } else {
            EvidenceTarget { memory_id }
        });
    }
    Ok(out)
}
