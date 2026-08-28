use super::{
    EntityKind, EvidenceTarget, GoalAuthorship, GoalEvidenceRef, GoalId, HashSet, MemoryId, Owner,
    Postgres, StorageError, SystemOrigin, Transaction, map_err,
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
        let kind_text: Option<String> =
            sqlx::query_scalar("SELECT kind::text FROM proxima_core.memory WHERE t = $1")
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

/// Read the exact prior Goal evidence vector for a storage-level omitted
/// modify. This remains a Goal-column read: joining through Memory would turn
/// a cooled or missing target into a shorter successor statement.
pub(super) async fn load_goal_evidence_exact(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    goal_id: GoalId,
) -> Result<Option<Vec<GoalEvidenceRef>>, StorageError> {
    let ids: Option<Vec<uuid::Uuid>> = sqlx::query_scalar(
        "SELECT evidence_t
           FROM proxima_core.goal
          WHERE t = $1 AND owner_id = $2",
    )
    .bind(goal_id.into_inner())
    .bind(owner.stored_owner_id())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(ids.map(|ids| {
        ids.into_iter()
            .map(MemoryId::new)
            .map(GoalEvidenceRef::new)
            .collect()
    }))
}
