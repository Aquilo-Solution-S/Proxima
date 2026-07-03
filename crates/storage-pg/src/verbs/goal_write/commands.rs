use super::{
    AchieveGoalAtomicRequest, CORE_DERIVED_FROM_RELATION, CORE_MOTIVATED_BY_RELATION,
    CreateGoalAtomicRequest, CreateGoalReplayExpectation, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, DecomposedGoalOutcome, DraftFromPayload, EdgeAuthorshipKind,
    EvidenceTarget, GoalAtomicContext, GoalAuthorship, GoalLifecycleFact, GoalState,
    GoalWriteOutcome, InsertedGoal, MemoryId, ModifyGoalAtomicRequest, Owner, OwnerWritePermit,
    PgPool, PgSidecarRegistryFrozen, Postgres, StorageError, SystemOrigin, Transaction,
    TransitionGoalAtomicRequest, WakeWrite, append_goal_to_self_edge,
    append_lifecycle_authored_edge, append_lifecycle_derived_from_edges, append_motivated_by_edges,
    child_draft, draft_from_payload, draft_from_stored, emit_lifecycle_fact,
    ensure_create_goal_replay_side_effects_match, goal_evidence_edges_match, idempotency_conflict,
    insert_or_replay_goal, internal, lifecycle_outcome, load_prior_goal, map_err,
    motivated_by_authorship_kind, outgoing_motivated_by_evidence, replay_goal_outcome,
    validate_active_head, validate_evidence_in_owner, validate_goal_transition,
    validate_operator_goal_evidence,
};

pub(crate) async fn create_goal_atomic(
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
    let outcome = if inserted.idempotent_replay {
        ensure_create_goal_replay_side_effects_match(
            &mut tx,
            CreateGoalReplayExpectation {
                goal_id: inserted.goal_id,
                target_self_perspective_id: req.draft.topology.assignment().perspective_id(),
                evidence: &evidence,
                evidence_authorship_kind: motivated_by_authorship_kind(&req.draft.authorship),
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
            &[
                CORE_MOTIVATED_BY_RELATION,
                proxima_core::relation::CORE_INSPIRES_RELATION,
            ],
        )
        .await?
    } else {
        let mut edge_ids = Vec::new();
        let lifecycle_memory_id = Some(
            emit_lifecycle_fact(
                &mut tx,
                permit,
                req.context,
                &req.draft.owner(),
                inserted.goal_id,
                GoalLifecycleFact::Activated,
            )
            .await?,
        );
        // Order matters: goal-sourced edges (inspires, then motivated_by)
        // first, then the lifecycle authored edge last — this mirrors
        // `replay_goal_outcome` (goal-relation edges by created_at, then
        // lifecycle-memory edges) so idempotent replay returns identical
        // edge_ids.
        edge_ids.push(
            append_goal_to_self_edge(
                &mut tx,
                req.context,
                &req.draft.owner(),
                inserted.goal_id,
                req.draft.topology.assignment().perspective_id(),
            )
            .await?,
        );
        edge_ids.extend(
            append_motivated_by_edges(
                &mut tx,
                req.context,
                &req.draft.owner(),
                inserted.goal_id,
                &evidence,
                motivated_by_authorship_kind(&req.draft.authorship),
            )
            .await?,
        );
        if let Some(lifecycle_id) = lifecycle_memory_id
            && let Some(edge_id) = append_lifecycle_authored_edge(
                &mut tx,
                req.context,
                &req.draft.owner(),
                lifecycle_id,
            )
            .await?
        {
            edge_ids.push(edge_id);
        }
        GoalWriteOutcome {
            goal_id: inserted.goal_id,
            change_event_seq: inserted.change_event_seq,
            lifecycle_memory_id,
            edge_ids,
            idempotent_replay: false,
        }
    };
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

pub(crate) async fn transition_goal_atomic(
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
    let outcome = lifecycle_outcome(
        &mut tx,
        permit,
        &req.owner,
        req.context,
        inserted,
        lifecycle,
        prior.assignment,
    )
    .await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

pub(crate) async fn achieve_goal_atomic(
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
    validate_goal_transition(prior.state, GoalState::Achieved)?;
    let draft = draft_from_stored(
        &req.owner,
        &prior,
        GoalState::Achieved,
        Some(req.prior_goal_id),
        req.authorship.clone(),
        req.request_id.as_str(),
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
    let outcome = if inserted.idempotent_replay {
        if !goal_evidence_edges_match(
            &mut tx,
            inserted.goal_id,
            &evidence,
            motivated_by_authorship_kind(&req.authorship),
        )
        .await?
        {
            return Err(idempotency_conflict(req.request_id.as_str()));
        }
        replay_goal_outcome(
            &mut tx,
            inserted,
            GoalLifecycleFact::Achieved,
            &[
                proxima_core::relation::CORE_INSPIRES_RELATION,
                CORE_MOTIVATED_BY_RELATION,
                CORE_DERIVED_FROM_RELATION,
            ],
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
                authorship_kind: motivated_by_authorship_kind(&req.authorship),
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
    authorship_kind: EdgeAuthorshipKind,
}

async fn achieve_goal_non_replay(
    tx: &mut Transaction<'_, Postgres>,
    args: AchieveGoalNonReplay<'_>,
) -> Result<GoalWriteOutcome, StorageError> {
    let lifecycle_memory_id = Some(
        emit_lifecycle_fact(
            tx,
            args.permit,
            args.context,
            args.owner,
            args.inserted.goal_id,
            GoalLifecycleFact::Achieved,
        )
        .await?,
    );
    let mut edge_ids = append_motivated_by_edges(
        tx,
        args.context,
        args.owner,
        args.inserted.goal_id,
        args.evidence,
        args.authorship_kind,
    )
    .await?;
    edge_ids.push(
        append_goal_to_self_edge(
            tx,
            args.context,
            args.owner,
            args.inserted.goal_id,
            args.assignment,
        )
        .await?,
    );
    if let Some(lifecycle_id) = lifecycle_memory_id {
        if let Some(edge_id) =
            append_lifecycle_authored_edge(tx, args.context, args.owner, lifecycle_id).await?
        {
            edge_ids.push(edge_id);
        }
        edge_ids.extend(
            append_lifecycle_derived_from_edges(
                tx,
                args.context,
                args.owner,
                lifecycle_id,
                args.evidence,
            )
            .await?,
        );
    }
    Ok(GoalWriteOutcome {
        goal_id: args.inserted.goal_id,
        change_event_seq: args.inserted.change_event_seq,
        lifecycle_memory_id,
        edge_ids,
        idempotent_replay: false,
    })
}

#[allow(clippy::too_many_lines)] // atomic Goal replace path keeps replay/proof side effects together
pub(crate) async fn modify_goal_atomic(
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
    let outcome = if inserted.idempotent_replay {
        if !goal_evidence_edges_match(
            &mut tx,
            inserted.goal_id,
            &evidence,
            motivated_by_authorship_kind(&req.authorship),
        )
        .await?
        {
            return Err(idempotency_conflict(req.request_id.as_str()));
        }
        replay_goal_outcome(
            &mut tx,
            inserted,
            GoalLifecycleFact::Activated,
            &[
                proxima_core::relation::CORE_INSPIRES_RELATION,
                CORE_MOTIVATED_BY_RELATION,
            ],
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
            )
            .await?,
        );
        let mut edge_ids = Vec::new();
        edge_ids.push(
            append_goal_to_self_edge(
                &mut tx,
                req.context,
                &req.owner,
                inserted.goal_id,
                prior.assignment,
            )
            .await?,
        );
        if let Some(lifecycle_id) = lifecycle_memory_id
            && let Some(edge_id) =
                append_lifecycle_authored_edge(&mut tx, req.context, &req.owner, lifecycle_id)
                    .await?
        {
            edge_ids.push(edge_id);
        }
        edge_ids.extend(
            append_motivated_by_edges(
                &mut tx,
                req.context,
                &req.owner,
                inserted.goal_id,
                &evidence,
                motivated_by_authorship_kind(&req.authorship),
            )
            .await?,
        );
        GoalWriteOutcome {
            goal_id: inserted.goal_id,
            change_event_seq: inserted.change_event_seq,
            lifecycle_memory_id,
            edge_ids,
            idempotent_replay: false,
        }
    };
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

#[allow(clippy::too_many_lines)] // atomic child Goal creation path keeps replay/proof side effects together
pub(crate) async fn decompose_goal_atomic(
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
        let outcome = if inserted.idempotent_replay {
            if !goal_evidence_edges_match(
                &mut tx,
                inserted.goal_id,
                &evidence,
                motivated_by_authorship_kind(&req.authorship),
            )
            .await?
            {
                return Err(idempotency_conflict(child.request_id.as_str()));
            }
            replay_goal_outcome(
                &mut tx,
                inserted,
                GoalLifecycleFact::Activated,
                &[
                    CORE_MOTIVATED_BY_RELATION,
                    proxima_core::relation::CORE_INSPIRES_RELATION,
                ],
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
                )
                .await?,
            );
            let mut edge_ids = Vec::new();
            if let Some(lifecycle_id) = lifecycle_memory_id
                && let Some(edge_id) =
                    append_lifecycle_authored_edge(&mut tx, req.context, &req.owner, lifecycle_id)
                        .await?
            {
                edge_ids.push(edge_id);
            }
            edge_ids.push(
                append_goal_to_self_edge(
                    &mut tx,
                    req.context,
                    &req.owner,
                    inserted.goal_id,
                    req.topology.assignment().perspective_id(),
                )
                .await?,
            );
            edge_ids.extend(
                append_motivated_by_edges(
                    &mut tx,
                    req.context,
                    &req.owner,
                    inserted.goal_id,
                    &evidence,
                    motivated_by_authorship_kind(&req.authorship),
                )
                .await?,
            );
            GoalWriteOutcome {
                goal_id: inserted.goal_id,
                change_event_seq: inserted.change_event_seq,
                lifecycle_memory_id,
                edge_ids,
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
