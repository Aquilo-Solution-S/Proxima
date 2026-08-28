use super::{
    AchieveGoalAtomicRequest, CreateGoalAtomicRequest, CreateGoalReplayExpectation,
    DecomposeGoalAtomicRequest, DecomposeGoalOutcome, DecomposedGoalOutcome, DraftFromPayload,
    GoalAuthorship, GoalId, GoalLifecycleFact, GoalState, GoalWriteOutcome, LifecycleWrite,
    ModifyGoalAtomicRequest, OwnerWritePermit, PgPool, PgSidecarRegistryFrozen, StorageError,
    SystemOrigin, TransitionGoalAtomicRequest, WakeWrite, child_draft, draft_from_payload,
    draft_from_stored, ensure_create_goal_replay_side_effects_match, insert_or_replay_goal,
    internal, lifecycle_outcome, load_goal_evidence_exact, load_prior_goal, map_err,
    validate_active_head, validate_evidence_in_owner, validate_goal_achievement,
    validate_goal_transition, validate_operator_goal_evidence,
};
use crate::error::with_bounded_retry;

// The goal `*_atomic` verbs are pool-scoped write transactions. Each wraps
// its `_in_pool` body in `with_bounded_retry` so a transient deadlock /
// serialization failure (SQLSTATE 40P01/40001) re-runs the whole idempotent
// transaction instead of surfacing to the host as a bare `Internal`.

pub(crate) async fn create_goal_atomic(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &CreateGoalAtomicRequest<'_>,
    permit: &OwnerWritePermit,
) -> Result<GoalWriteOutcome, StorageError> {
    with_bounded_retry(move || async move {
        create_goal_atomic_in_pool(pool, sidecars, req, permit).await
    })
    .await
}

pub(crate) async fn transition_goal_atomic(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &TransitionGoalAtomicRequest<'_>,
    permit: &OwnerWritePermit,
) -> Result<GoalWriteOutcome, StorageError> {
    with_bounded_retry(move || async move {
        transition_goal_atomic_in_pool(pool, sidecars, req, permit).await
    })
    .await
}

pub(crate) async fn achieve_goal_atomic(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &AchieveGoalAtomicRequest<'_>,
    permit: &OwnerWritePermit,
) -> Result<GoalWriteOutcome, StorageError> {
    with_bounded_retry(move || async move {
        achieve_goal_atomic_in_pool(pool, sidecars, req, permit).await
    })
    .await
}

pub(crate) async fn modify_goal_atomic(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &ModifyGoalAtomicRequest<'_>,
    permit: &OwnerWritePermit,
) -> Result<GoalWriteOutcome, StorageError> {
    with_bounded_retry(move || async move {
        modify_goal_atomic_in_pool(pool, sidecars, req, permit).await
    })
    .await
}

pub(crate) async fn decompose_goal_atomic(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &DecomposeGoalAtomicRequest<'_>,
    permit: &OwnerWritePermit,
) -> Result<DecomposeGoalOutcome, StorageError> {
    with_bounded_retry(move || async move {
        decompose_goal_atomic_in_pool(pool, sidecars, req, permit).await
    })
    .await
}

pub(crate) async fn create_goal_atomic_in_pool(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &CreateGoalAtomicRequest<'_>,
    permit: &OwnerWritePermit,
) -> Result<GoalWriteOutcome, StorageError> {
    let mut tx = pool.begin().await.map_err(internal)?;
    let outcome = create_goal_in_tx(&mut tx, sidecars, req, permit).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

pub(crate) async fn create_goal_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    req: &CreateGoalAtomicRequest<'_>,
    _permit: &OwnerWritePermit,
) -> Result<GoalWriteOutcome, StorageError> {
    let owner = req.draft.owner();
    let evidence = validate_evidence_in_owner(tx, &owner, req.draft.topology.evidence()).await?;
    validate_operator_goal_evidence(&req.draft.authorship, &evidence)?;
    let inserted = insert_or_replay_goal(
        tx,
        sidecars,
        &req.draft,
        None,
        req.context,
        WakeWrite::Explicit(req.draft.wake.as_ref()),
        req.write_act_t.map(proxima_core::MemoryId::into_inner),
    )
    .await?;
    // A create writes rows no other Goal verb does, so its replay carries
    // this extra proof on top of the one the shared tail applies to all five.
    if inserted.idempotent_replay {
        ensure_create_goal_replay_side_effects_match(
            tx,
            CreateGoalReplayExpectation {
                goal_id: inserted.goal_id,
                target_self_perspective_id: req.draft.topology.assignment().perspective_id(),
                author_self_perspective_id: req.context.author_self_perspective_id,
                wake_write: WakeWrite::Explicit(req.draft.wake.as_ref()),
                expected_prior: None,
                request_id: &req.draft.request_id,
            },
        )
        .await?;
    }
    let dependencies = draft_dependency_ids(&req.draft);
    lifecycle_outcome(
        tx,
        LifecycleWrite {
            owner: &owner,
            inserted,
            lifecycle: GoalLifecycleFact::Activated,
            assignment: req.draft.topology.assignment().perspective_id(),
            dependencies: &dependencies,
            evidence: &evidence,
            request_id: &req.draft.request_id,
        },
    )
    .await
}

pub(crate) async fn transition_goal_atomic_in_pool(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &TransitionGoalAtomicRequest<'_>,
    _permit: &OwnerWritePermit,
) -> Result<GoalWriteOutcome, StorageError> {
    if matches!(req.next_state, GoalState::Achieved) {
        return Err(StorageError::ConstraintViolation(
            "use achieve_goal_atomic for Achieved transitions".into(),
        ));
    }
    if matches!(
        &req.authorship,
        GoalAuthorship::System(SystemOrigin::Operator { .. })
    ) {
        return Err(StorageError::ConstraintViolation(
            "operator-authored Goal transition requires explicit Abstraction evidence".into(),
        ));
    }
    let mut tx = pool.begin().await.map_err(internal)?;
    let prior = load_prior_goal(&mut tx, &req.owner, req.prior_goal_id).await?;
    validate_goal_transition(prior.state, req.next_state)?;
    let draft = draft_from_stored(
        &req.owner,
        &prior,
        req.next_state,
        Some(req.prior_goal_id),
        req.authorship.clone(),
        req.request_id.as_str(),
        // A plain transition rests on nothing it names: no evidence column,
        // and so no evidence reference rows.
        &[],
    );
    let inserted = insert_or_replay_goal(
        &mut tx,
        sidecars,
        &draft,
        Some(req.prior_goal_id),
        req.context,
        WakeWrite::CarryFrom(req.prior_goal_id),
        None,
    )
    .await?;
    let outcome = lifecycle_outcome(
        &mut tx,
        LifecycleWrite {
            owner: &req.owner,
            inserted,
            lifecycle: GoalLifecycleFact::for_state(req.next_state),
            assignment: prior.assignment,
            dependencies: &prior.dependencies,
            evidence: &[],
            request_id: req.request_id.as_str(),
        },
    )
    .await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

pub(crate) async fn achieve_goal_atomic_in_pool(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &AchieveGoalAtomicRequest<'_>,
    _permit: &OwnerWritePermit,
) -> Result<GoalWriteOutcome, StorageError> {
    if req.evidence.is_empty() {
        return Err(StorageError::ConstraintViolation(
            "achievement evidence must be nonempty".into(),
        ));
    }
    let mut tx = pool.begin().await.map_err(internal)?;
    let evidence = validate_evidence_in_owner(&mut tx, &req.owner, &req.evidence).await?;
    let prior = load_prior_goal(&mut tx, &req.owner, req.prior_goal_id).await?;
    validate_goal_achievement(prior.state)?;
    let draft = draft_from_stored(
        &req.owner,
        &prior,
        GoalState::Achieved,
        Some(req.prior_goal_id),
        req.authorship.clone(),
        req.request_id.as_str(),
        &evidence,
    );
    let inserted = insert_or_replay_goal(
        &mut tx,
        sidecars,
        &draft,
        Some(req.prior_goal_id),
        req.context,
        WakeWrite::CarryFrom(req.prior_goal_id),
        None,
    )
    .await?;
    let outcome = lifecycle_outcome(
        &mut tx,
        LifecycleWrite {
            owner: &req.owner,
            inserted,
            lifecycle: GoalLifecycleFact::Achieved,
            assignment: prior.assignment,
            dependencies: &prior.dependencies,
            evidence: &evidence,
            request_id: req.request_id.as_str(),
        },
    )
    .await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

/// Dependency ids a draft declares, in the shape the topology writer wants.
fn draft_dependency_ids(draft: &proxima_core::verbs::goal_write::GoalDraft) -> Vec<GoalId> {
    draft
        .topology
        .dependencies()
        .iter()
        .map(|dependency| dependency.goal_id())
        .collect()
}

pub(crate) async fn modify_goal_atomic_in_pool(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &ModifyGoalAtomicRequest<'_>,
    _permit: &OwnerWritePermit,
) -> Result<GoalWriteOutcome, StorageError> {
    let mut tx = pool.begin().await.map_err(internal)?;
    let carried_or_explicit = match &req.evidence {
        Some(evidence) => evidence.clone(),
        None => load_goal_evidence_exact(&mut tx, &req.owner, req.prior_goal_id)
            .await?
            .unwrap_or_default(),
    };
    let evidence = validate_evidence_in_owner(&mut tx, &req.owner, &carried_or_explicit).await?;
    let prior = load_prior_goal(&mut tx, &req.owner, req.prior_goal_id).await?;
    if prior.state != GoalState::Active {
        return Err(StorageError::ConstraintViolation(
            "goal_modify requires an Active prior head".into(),
        ));
    }
    let draft = draft_from_payload(DraftFromPayload {
        owner: &req.owner,
        payload: &req.replacement,
        state: GoalState::Active,
        assignment: prior.assignment,
        dependencies: prior.dependencies,
        supersedes: Some(req.prior_goal_id),
        authorship: req.authorship.clone(),
        request_id: req.request_id.as_str(),
        evidence: &evidence,
    });
    validate_operator_goal_evidence(&draft.authorship, &evidence)?;
    let wake_write = match &req.wake {
        Some(wake) => WakeWrite::Explicit(wake.as_ref()),
        None => WakeWrite::CarryFrom(req.prior_goal_id),
    };
    let inserted = insert_or_replay_goal(
        &mut tx,
        sidecars,
        &draft,
        Some(req.prior_goal_id),
        req.context,
        wake_write,
        None,
    )
    .await?;
    let dependencies = draft_dependency_ids(&draft);
    let outcome = lifecycle_outcome(
        &mut tx,
        LifecycleWrite {
            owner: &req.owner,
            inserted,
            lifecycle: GoalLifecycleFact::Activated,
            assignment: prior.assignment,
            dependencies: &dependencies,
            evidence: &evidence,
            request_id: req.request_id.as_str(),
        },
    )
    .await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

pub(crate) async fn decompose_goal_atomic_in_pool(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &DecomposeGoalAtomicRequest<'_>,
    _permit: &OwnerWritePermit,
) -> Result<DecomposeGoalOutcome, StorageError> {
    let mut tx = pool.begin().await.map_err(internal)?;
    validate_active_head(&mut tx, &req.owner, req.parent_goal_id).await?;

    let mut children = Vec::with_capacity(req.children.len());
    for child in &req.children {
        let evidence = validate_evidence_in_owner(&mut tx, &req.owner, &child.evidence).await?;
        validate_operator_goal_evidence(&req.authorship, &evidence)?;
        let draft = child_draft(
            &req.owner,
            req.parent_goal_id,
            &req.topology,
            &req.authorship,
            child,
        )?;
        let inserted = insert_or_replay_goal(
            &mut tx,
            sidecars,
            &draft,
            None,
            req.context,
            WakeWrite::Explicit(child.wake.as_ref()),
            None,
        )
        .await?;
        let dependencies = draft_dependency_ids(&draft);
        let outcome = lifecycle_outcome(
            &mut tx,
            LifecycleWrite {
                owner: &req.owner,
                inserted,
                lifecycle: GoalLifecycleFact::Activated,
                assignment: req.topology.assignment().perspective_id(),
                dependencies: &dependencies,
                evidence: &evidence,
                request_id: child.request_id.as_str(),
            },
        )
        .await?;
        children.push(DecomposedGoalOutcome { outcome });
    }

    tx.commit().await.map_err(map_err)?;
    let idempotent_replay = children.iter().all(|child| child.outcome.idempotent_replay);
    Ok(DecomposeGoalOutcome {
        children,
        idempotent_replay,
    })
}
