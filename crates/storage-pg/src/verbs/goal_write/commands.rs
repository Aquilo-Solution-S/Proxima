use super::{
    AchieveGoalAtomicRequest, CreateGoalAtomicRequest, CreateGoalReplayExpectation,
    DecomposeGoalAtomicRequest, DecomposeGoalOutcome, DecomposedGoalOutcome, DraftFromPayload,
    EvidenceTarget, GoalAtomicContext, GoalAuthorship, GoalId, GoalLifecycleFact, GoalState,
    GoalWriteOutcome, InsertedGoal, MemoryId, ModifyGoalAtomicRequest, Owner, OwnerWritePermit,
    PgPool, PgSidecarRegistryFrozen, Postgres, StorageError, SystemOrigin, Transaction,
    TransitionGoalAtomicRequest, WakeWrite, assert_goal_topology_references, child_draft,
    draft_from_payload, draft_from_stored, emit_lifecycle_fact,
    ensure_create_goal_replay_side_effects_match, goal_evidence_matches, goal_topology_edge_count,
    idempotency_conflict, insert_or_replay_goal, internal, lifecycle_outcome, load_prior_goal,
    map_err, outgoing_motivated_by_evidence, replay_goal_outcome, validate_active_head,
    validate_evidence_in_owner, validate_goal_achievement, validate_goal_transition,
    validate_operator_goal_evidence,
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
    let evidence =
        validate_evidence_in_owner(&mut tx, &req.draft.owner(), req.draft.topology.evidence())
            .await?;
    validate_operator_goal_evidence(&req.draft.authorship, &evidence)?;
    let inserted = insert_or_replay_goal(
        &mut tx,
        sidecars,
        &req.draft,
        None,
        req.context,
        WakeWrite::Explicit(req.draft.wake.as_ref()),
    )
    .await?;
    let dependencies = draft_dependency_ids(&req.draft);
    let outcome = if inserted.idempotent_replay {
        ensure_create_goal_replay_side_effects_match(
            &mut tx,
            CreateGoalReplayExpectation {
                goal_id: inserted.goal_id,
                target_self_perspective_id: req.draft.topology.assignment().perspective_id(),
                evidence: &evidence,
                author_self_perspective_id: req.context.author_self_perspective_id,
                wake_write: WakeWrite::Explicit(req.draft.wake.as_ref()),
                expected_prior: None,
                request_id: &req.draft.request_id,
            },
        )
        .await?;
        replay_goal_outcome(
            &mut tx,
            inserted,
            GoalLifecycleFact::Activated,
            goal_topology_edge_count(&dependencies, &evidence),
        )
        .await?
    } else {
        let lifecycle_memory_id = Some(
            emit_lifecycle_fact(
                &mut tx,
                permit,
                req.context,
                &req.draft.owner(),
                inserted.goal_id,
                GoalLifecycleFact::Activated,
                &evidence,
            )
            .await?,
        );
        let edge_count = assert_goal_topology_references(
            &mut tx,
            &req.draft.owner(),
            inserted.goal_id,
            req.draft.topology.assignment().perspective_id(),
            &dependencies,
            &evidence,
        )
        .await?;
        GoalWriteOutcome {
            goal_id: inserted.goal_id,
            change_event_seq: inserted.change_event_seq,
            lifecycle_memory_id,
            edge_count,
            idempotent_replay: false,
        }
    };
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

pub(crate) async fn transition_goal_atomic_in_pool(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &TransitionGoalAtomicRequest<'_>,
    permit: &OwnerWritePermit,
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
    )
    .await?;
    let lifecycle = GoalLifecycleFact::for_state(req.next_state);
    let dependencies = prior.dependencies.clone();
    let outcome = lifecycle_outcome(
        &mut tx,
        permit,
        &req.owner,
        req.context,
        inserted,
        lifecycle,
        prior.assignment,
        &dependencies,
    )
    .await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

pub(crate) async fn achieve_goal_atomic_in_pool(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &AchieveGoalAtomicRequest<'_>,
    permit: &OwnerWritePermit,
) -> Result<GoalWriteOutcome, StorageError> {
    if req.evidence.is_empty() {
        return Err(StorageError::ConstraintViolation(
            "achievement evidence must be nonempty".into(),
        ));
    }
    let mut tx = pool.begin().await.map_err(internal)?;
    let evidence = validate_evidence_in_owner(&mut tx, &req.owner, &req.evidence).await?;
    validate_operator_goal_evidence(&req.authorship, &evidence)?;
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
    )
    .await?;
    let dependencies = prior.dependencies.clone();
    let outcome = if inserted.idempotent_replay {
        if !goal_evidence_matches(&mut tx, inserted.goal_id, &evidence).await? {
            return Err(idempotency_conflict(req.request_id.as_str()));
        }
        replay_goal_outcome(
            &mut tx,
            inserted,
            GoalLifecycleFact::Achieved,
            goal_topology_edge_count(&dependencies, &evidence),
        )
        .await?
    } else {
        achieve_goal_non_replay(
            &mut tx,
            AchieveGoalNonReplay {
                owner: &req.owner,
                permit,
                context: req.context,
                inserted,
                evidence: &evidence,
                assignment: prior.assignment,
                dependencies: &dependencies,
            },
        )
        .await?
    };
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

struct AchieveGoalNonReplay<'a> {
    owner: &'a Owner,
    permit: &'a OwnerWritePermit,
    context: GoalAtomicContext<'a>,
    inserted: InsertedGoal,
    evidence: &'a [EvidenceTarget],
    assignment: MemoryId,
    dependencies: &'a [GoalId],
}

async fn achieve_goal_non_replay(
    tx: &mut Transaction<'_, Postgres>,
    args: AchieveGoalNonReplay<'_>,
) -> Result<GoalWriteOutcome, StorageError> {
    // The achievement Fact declares the evidence it was made from, so its
    // origin rows land inside its own ingest transaction.
    let lifecycle_memory_id = Some(
        emit_lifecycle_fact(
            tx,
            args.permit,
            args.context,
            args.owner,
            args.inserted.goal_id,
            GoalLifecycleFact::Achieved,
            args.evidence,
        )
        .await?,
    );
    let edge_count = assert_goal_topology_references(
        tx,
        args.owner,
        args.inserted.goal_id,
        args.assignment,
        args.dependencies,
        args.evidence,
    )
    .await?;
    Ok(GoalWriteOutcome {
        goal_id: args.inserted.goal_id,
        change_event_seq: args.inserted.change_event_seq,
        lifecycle_memory_id,
        edge_count,
        idempotent_replay: false,
    })
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

#[allow(clippy::too_many_lines)] // atomic Goal replace path keeps replay/proof side effects together
pub(crate) async fn modify_goal_atomic_in_pool(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &ModifyGoalAtomicRequest<'_>,
    permit: &OwnerWritePermit,
) -> Result<GoalWriteOutcome, StorageError> {
    let mut tx = pool.begin().await.map_err(internal)?;
    let evidence = match &req.evidence {
        Some(evidence) => validate_evidence_in_owner(&mut tx, &req.owner, evidence).await?,
        None => outgoing_motivated_by_evidence(&mut tx, &req.owner, req.prior_goal_id).await?,
    };
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
    )
    .await?;
    let dependencies = draft_dependency_ids(&draft);
    let outcome = if inserted.idempotent_replay {
        if !goal_evidence_matches(&mut tx, inserted.goal_id, &evidence).await? {
            return Err(idempotency_conflict(req.request_id.as_str()));
        }
        replay_goal_outcome(
            &mut tx,
            inserted,
            GoalLifecycleFact::Activated,
            goal_topology_edge_count(&dependencies, &evidence),
        )
        .await?
    } else {
        let lifecycle_memory_id = Some(
            emit_lifecycle_fact(
                &mut tx,
                permit,
                req.context,
                &req.owner,
                inserted.goal_id,
                GoalLifecycleFact::Activated,
                &evidence,
            )
            .await?,
        );
        let edge_count = assert_goal_topology_references(
            &mut tx,
            &req.owner,
            inserted.goal_id,
            prior.assignment,
            &dependencies,
            &evidence,
        )
        .await?;
        GoalWriteOutcome {
            goal_id: inserted.goal_id,
            change_event_seq: inserted.change_event_seq,
            lifecycle_memory_id,
            edge_count,
            idempotent_replay: false,
        }
    };
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

#[allow(clippy::too_many_lines)] // atomic child Goal creation path keeps replay/proof side effects together
pub(crate) async fn decompose_goal_atomic_in_pool(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &DecomposeGoalAtomicRequest<'_>,
    permit: &OwnerWritePermit,
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
        )
        .await?;
        let dependencies = draft_dependency_ids(&draft);
        let outcome = if inserted.idempotent_replay {
            if !goal_evidence_matches(&mut tx, inserted.goal_id, &evidence).await? {
                return Err(idempotency_conflict(child.request_id.as_str()));
            }
            replay_goal_outcome(
                &mut tx,
                inserted,
                GoalLifecycleFact::Activated,
                goal_topology_edge_count(&dependencies, &evidence),
            )
            .await?
        } else {
            let lifecycle_memory_id = Some(
                emit_lifecycle_fact(
                    &mut tx,
                    permit,
                    req.context,
                    &req.owner,
                    inserted.goal_id,
                    GoalLifecycleFact::Activated,
                    &evidence,
                )
                .await?,
            );
            let edge_count = assert_goal_topology_references(
                &mut tx,
                &req.owner,
                inserted.goal_id,
                req.topology.assignment().perspective_id(),
                &dependencies,
                &evidence,
            )
            .await?;
            GoalWriteOutcome {
                goal_id: inserted.goal_id,
                change_event_seq: inserted.change_event_seq,
                lifecycle_memory_id,
                edge_count,
                idempotent_replay: false,
            }
        };
        children.push(DecomposedGoalOutcome { outcome });
    }

    tx.commit().await.map_err(map_err)?;
    let idempotent_replay = children.iter().all(|child| child.outcome.idempotent_replay);
    Ok(DecomposeGoalOutcome {
        children,
        idempotent_replay,
    })
}
