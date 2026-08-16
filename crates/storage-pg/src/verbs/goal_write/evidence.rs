use super::{
    EntityKind, EvidenceTarget, GoalAuthorship, GoalEvidenceRef, GoalId, HashSet,
    MemoryId, Owner, Postgres, StorageError, SystemOrigin, Transaction, map_err,
};

pub(super) fn validate_operator_goal_evidence(
    authorship: &GoalAuthorship,
    evidence: &[EvidenceTarget],
) -> Result<(), StorageError> {
    if !matches!(
        authorship,
        GoalAuthorship::System(SystemOrigin::Operator { .. })
    ) {
        return Ok(());
    }
    if evidence.is_empty() {
        return Err(StorageError::ConstraintViolation(
            "operator-authored Goal requires non-empty Abstraction evidence".into(),
        ));
    }
    if evidence
        .iter()
        .any(|target| target.kind != EntityKind::Abstraction)
    {
        return Err(StorageError::ConstraintViolation(
            "operator-authored Goal evidence must be Abstraction".into(),
        ));
    }
    Ok(())
}

pub(super) async fn validate_evidence_in_owner(
    tx: &mut Transaction<'_, Postgres>,
    _owner: &Owner,
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
        let kind_text: Option<String> = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.memory WHERE t = $1",
        )
        .bind(item.memory_id().into_inner())
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_err)?;
        let Some(kind_text) = kind_text else {
            return Err(StorageError::ConstraintViolation(
                "evidence does not exist".into(),
            ));
        };
        let kind = match kind_text.as_str() {
            "fact" => EntityKind::Fact,
            "abstraction" => EntityKind::Abstraction,
            "perspective" => EntityKind::Perspective,
            _ => {
                return Err(StorageError::ConstraintViolation(
                    "evidence does not exist".into(),
                ));
            }
        };
        match kind {
            EntityKind::Fact | EntityKind::Abstraction => out.push(EvidenceTarget {
                kind,
                memory_id: item.memory_id(),
            }),
            _ => {
                return Err(StorageError::ConstraintViolation(
                    "evidence must be Fact or Abstraction".into(),
                ));
            }
        }
    }
    Ok(out)
}

/// The evidence a Goal already rests on, read from the Goal's own column.
///
/// The kinds come from the memories rows because the column stores what the
/// Goal said, not what those memories are — the index rows carry the kind for
/// traversal, and this path needs it for the operator-evidence rule.
pub(super) async fn outgoing_motivated_by_evidence(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    goal_id: GoalId,
) -> Result<Vec<EvidenceTarget>, StorageError> {
    let owner_id = owner.stored_owner_id();
    let rows: Vec<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT pin, m.kind::text
           FROM proxima_core.goal g
           JOIN unnest(g.evidence_t) WITH ORDINALITY AS ev(pin, ord) ON TRUE
           JOIN proxima_core.memory m ON m.t = ev.pin
          WHERE g.t = $1 AND g.owner_id = $2
          ORDER BY ev.ord",
    )
    .bind(goal_id.into_inner())
    .bind(owner_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .filter_map(|(memory_id, kind)| {
            let kind = match kind.as_str() {
                "fact" => EntityKind::Fact,
                "abstraction" => EntityKind::Abstraction,
                "perspective" => EntityKind::Perspective,
                _ => return None,
            };
            Some(EvidenceTarget {
                kind,
                memory_id: MemoryId::new(memory_id),
            })
        })
        .collect())
}
