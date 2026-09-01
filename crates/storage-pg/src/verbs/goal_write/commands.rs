use super::{
    AchieveGoalAtomicRequest, CreateGoalAtomicRequest, CreateGoalReplayExpectation,
    DecomposeGoalAtomicRequest, DecomposeGoalOutcome, DecomposedGoalOutcome, DraftFromPayload,
    GoalAuthorship, GoalId, GoalLifecycleFact, GoalState, GoalWriteOutcome, LifecycleWrite,
    ModifyGoalAtomicRequest, OwnerWritePermit, PgPool, PgSidecarRegistryFrozen, PreparedGoalInsert,
    StorageError, SystemOrigin, TransitionGoalAtomicRequest, WakeWrite, child_draft,
    draft_from_payload, draft_from_stored, ensure_create_goal_replay_side_effects_match,
    insert_or_replay_goal, internal, lifecycle_outcome, load_goal_evidence_exact, load_prior_goal,
    lock_prepared_goal_writes, map_err, persist_prepared_goal_insert, prepare_goal_insert,
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

    let mut prepared_children: Vec<(PreparedGoalInsert<'_>, Vec<super::EvidenceTarget>)> =
        Vec::with_capacity(req.children.len());
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
        let prepared = prepare_goal_insert(
            &mut tx,
            &draft,
            None,
            req.context,
            WakeWrite::Explicit(child.wake.as_ref()),
            None,
        )
        .await?;
        prepared_children.push((prepared, evidence));
    }

    // Every child is prepared before any child gets a Goal head, wake row, or
    // Goal row. One union lock makes crossed child target sets wait in the
    // same sorted order rather than acquiring a child footprint incrementally.
    let lock_items: Vec<&super::GoalWritePreparation> = prepared_children
        .iter()
        .map(|(child, _)| &child.preparation)
        .collect();
    lock_prepared_goal_writes(&mut tx, &lock_items).await?;
    // The parent is part of every child's dependency footprint. Re-check its
    // active-head status after that union lock, so a parent transition that
    // committed while children were being prepared aborts before the first
    // child Goal, sidecar, sketch, announce, or lifecycle Fact is written.
    validate_active_head(&mut tx, &req.owner, req.parent_goal_id).await?;

    let mut children = Vec::with_capacity(prepared_children.len());
    for (prepared, evidence) in prepared_children {
        let inserted = persist_prepared_goal_insert(&mut tx, sidecars, &prepared).await?;
        let dependencies = draft_dependency_ids(&prepared.draft);
        let outcome = lifecycle_outcome(
            &mut tx,
            LifecycleWrite {
                owner: &req.owner,
                inserted,
                lifecycle: GoalLifecycleFact::Activated,
                assignment: req.topology.assignment().perspective_id(),
                dependencies: &dependencies,
                evidence: &evidence,
                request_id: &prepared.draft.request_id,
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

#[cfg(test)]
mod lifecycle_lock_tests {
    use super::super::insert::prepare_goal_insert;
    use super::super::types::WakeWrite;
    use super::super::{
        GoalWritePreparation, lock_prepared_goal_write, lock_prepared_goal_writes,
        persist_prepared_goal_write, prepare_goal_write,
    };
    use crate::PgStorage;
    use crate::verbs::goal_timeseries::{GoalWakePlan, GoalWriteCommand, write_goal};
    use proxima_core::GoalPayload;
    use proxima_core::storage_ports::FactIngestPort;
    use proxima_core::storage_ports::OwnerWritePermit;
    use proxima_core::verbs::fact_ingest::FactWriteCommand;
    use proxima_core::verbs::goal_write::{
        GoalAssignmentTarget, GoalAtomicContext, GoalAuthorship, GoalDraft, GoalState,
        GoalTopologyWrite,
    };
    use proxima_core::{
        AccessKind, EdgeEndpoint, EntityKind, FlavorRegistry, OwnerRef, SchemaId, SchemaVersion,
        SimpleTextGoalV1, StorageError, UserId,
    };
    use proxima_pg_testkit::{create_db, db_url, drop_db};
    use uuid::Uuid;

    fn fact() -> FactWriteCommand {
        FactWriteCommand {
            schema_id: SchemaId::new("core/test-fact-v1".into()),
            schema_version: SchemaVersion::new(1),
            handle: None,
            source_id: None,
            ingest_key: None,
            payload: Vec::new(),
            rendered_text: None,
            lexical_language: None,
            receipt: None,
            citation: None,
            derived_from: Vec::new(),
            refs: Vec::new(),
            blob_id: None,
            kind: "fact".into(),
        }
    }

    async fn grounded_perspective(
        pg: &PgStorage,
        permit: &OwnerWritePermit,
    ) -> Result<proxima_core::verbs::fact_ingest::FactIngestOutcome, StorageError> {
        let fact_out = pg.ingest_fact_atomic(permit, &fact(), None).await?;
        let mut abstraction = fact();
        abstraction.kind = "abstraction".into();
        abstraction.derived_from = vec![EdgeEndpoint::memory(EntityKind::Fact, fact_out.memory_id)];
        let abstraction = pg.ingest_fact_atomic(permit, &abstraction, None).await?;
        let mut perspective = fact();
        perspective.kind = "perspective".into();
        perspective.derived_from = vec![EdgeEndpoint::memory(
            EntityKind::Abstraction,
            abstraction.memory_id,
        )];
        pg.ingest_fact_atomic(permit, &perspective, None).await
    }

    async fn lock_target(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        t: Uuid,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "SELECT pg_advisory_xact_lock(
                 hashtextextended('proxima-forget:' || $1::text, 0)
             )",
        )
        .bind(t)
        .execute(&mut **tx)
        .await
        .map(|_| ())
    }

    async fn assert_stale_successor_not_persisted(
        pool: &sqlx::PgPool,
        request_id: &str,
        write_act_t: Uuid,
        handle: Uuid,
        expected_head_t: Uuid,
    ) -> Result<(), sqlx::Error> {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.goal
                 WHERE request_id = $1",
            )
            .bind(request_id)
            .fetch_one(pool)
            .await?,
            0,
            "stale successor must not persist a Goal"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1",
            )
            .bind(write_act_t)
            .fetch_one(pool)
            .await?,
            0,
            "stale successor must not persist its reserved lifecycle Fact"
        );
        let head_t: Uuid =
            sqlx::query_scalar("SELECT t FROM proxima_core.goal_head WHERE handle = $1")
                .bind(handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head_t, expected_head_t);
        Ok(())
    }

    async fn run_concurrent_successor_race(
        pool: &sqlx::PgPool,
        owner: &OwnerRef,
        handle: Uuid,
        expected_prior_t: Option<Uuid>,
        first_request_id: &str,
        second_request_id: &str,
    ) -> Result<(StorageError, Uuid), Box<dyn std::error::Error>> {
        let successor = |request_id: &str| GoalWriteCommand {
            handle: Some(handle),
            schema_id: "core/task-v1".into(),
            title: request_id.into(),
            state: GoalState::Active,
            request_id: request_id.into(),
            close_fact_t: None,
            assignment_t: None,
            dependency_t: vec![],
            evidence_t: vec![],
            wake_id: None,
            mint_write_act: true,
            write_act_t: None,
        };

        // Both preparations observe the same predecessor before either
        // transaction acquires its lifecycle union.
        let mut first_tx = pool.begin().await?;
        let first = prepare_goal_write(
            &mut first_tx,
            owner,
            &successor(first_request_id),
            GoalWakePlan::None,
            expected_prior_t,
        )
        .await?;
        let mut second_tx = pool.begin().await?;
        let second = prepare_goal_write(
            &mut second_tx,
            owner,
            &successor(second_request_id),
            GoalWakePlan::None,
            expected_prior_t,
        )
        .await?;
        let GoalWritePreparation::New(first) = first else {
            unreachable!();
        };
        let GoalWritePreparation::New(second) = second else {
            unreachable!();
        };
        let second_write_act_t = second
            .reserved_write_act_t()
            .expect("successor reserves a write-act identity");

        lock_prepared_goal_write(&mut first_tx, &first).await?;
        let mut second_lock = Box::pin(lock_prepared_goal_write(&mut second_tx, &second));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut second_lock)
                .await
                .is_err(),
            "second successor must wait on the predecessor lifecycle lock"
        );

        let first_outcome = persist_prepared_goal_write(&mut first_tx, &first).await?;
        first_tx.commit().await?;
        let second_err = second_lock
            .await
            .expect_err("stale second successor must be rejected");
        second_tx.rollback().await?;

        assert_stale_successor_not_persisted(
            pool,
            second_request_id,
            second_write_act_t,
            handle,
            first_outcome.t,
        )
        .await?;
        Ok((second_err, first_outcome.t))
    }

    #[tokio::test]
    async fn decomposition_locks_the_union_before_first_goal_persist() {
        let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
        if let Err(e) = create_db(&db_name).await {
            panic!("PG required for tests but admin connect failed: {e}");
        }
        let url = db_url(&db_name);
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let pg = PgStorage::connect(&url).await?;
            pg.run_migrations().await?;
            let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
            let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
            let first = pg.ingest_fact_atomic(&permit, &fact(), None).await?;
            let second = pg.ingest_fact_atomic(&permit, &fact(), None).await?;
            let pool = pg.pool_for_tests();
            let mut tx = pool.begin().await?;
            let command = |request_id: &str, evidence_t| GoalWriteCommand {
                handle: None,
                schema_id: "core/task-v1".into(),
                title: request_id.into(),
                state: GoalState::Active,
                request_id: request_id.into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![evidence_t],
                wake_id: None,
                mint_write_act: false,
                write_act_t: None,
            };
            let prepared_first = prepare_goal_write(
                &mut tx,
                &owner,
                &command("union-first", second.memory_id.into_inner()),
                GoalWakePlan::None,
                None,
            )
            .await?;
            let prepared_second = prepare_goal_write(
                &mut tx,
                &owner,
                &command("union-second", first.memory_id.into_inner()),
                GoalWakePlan::None,
                None,
            )
            .await?;
            assert!(matches!(&prepared_first, GoalWritePreparation::New(_)));
            assert!(matches!(&prepared_second, GoalWritePreparation::New(_)));
            let prepared = vec![prepared_first, prepared_second];
            let lock_items: Vec<&GoalWritePreparation> = prepared.iter().collect();

            // The later child (`union-second`) uses `first.memory_id`; block that
            // target to prove the full child union is acquired before any child
            // can persist.
            let mut blocker = pool.begin().await?;
            lock_target(&mut blocker, first.memory_id.into_inner()).await?;
            let mut lock = Box::pin(lock_prepared_goal_writes(&mut tx, &lock_items));
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), &mut lock)
                    .await
                    .is_err(),
                "union acquisition must wait on a target held by another lifecycle"
            );
            let before: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM proxima_core.goal
                 WHERE request_id IN ('union-first', 'union-second')",
            )
            .fetch_one(pool)
            .await?;
            assert_eq!(before, 0, "preparation and blocked union persist no Goal");
            blocker.commit().await?;
            lock.await?;
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*)::bigint FROM proxima_core.goal
                     WHERE request_id IN ('union-first', 'union-second')",
                )
                .fetch_one(pool)
                .await?,
                0,
                "the union lock itself persists no Goal"
            );
            for item in &prepared {
                let GoalWritePreparation::New(item) = item else {
                    unreachable!()
                };
                persist_prepared_goal_write(&mut tx, item).await?;
            }
            tx.commit().await?;
            let after: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM proxima_core.goal
                 WHERE request_id IN ('union-first', 'union-second')",
            )
            .fetch_one(pool)
            .await?;
            assert_eq!(after, 2);
            Ok(())
        }
        .await;
        let _ = drop_db(&db_name).await;
        result.expect("decomposition union lock test failed");
    }

    #[tokio::test]
    async fn concurrent_successors_classify_stale_head_before_persisting() {
        let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
        if let Err(e) = create_db(&db_name).await {
            panic!("PG required for tests but admin connect failed: {e}");
        }
        let url = db_url(&db_name);
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let pg = PgStorage::connect(&url).await?;
            pg.run_migrations().await?;
            let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
            let pool = pg.pool_for_tests();

            let mut create_tx = pool.begin().await?;
            let created = write_goal(
                &mut create_tx,
                &owner,
                &GoalWriteCommand {
                    handle: None,
                    schema_id: "core/task-v1".into(),
                    title: "prior".into(),
                    state: GoalState::Active,
                    request_id: "successor-prior".into(),
                    close_fact_t: None,
                    assignment_t: None,
                    dependency_t: vec![],
                    evidence_t: vec![],
                    wake_id: None,
                    mint_write_act: false,
                    write_act_t: None,
                },
            )
            .await?;
            create_tx.commit().await?;

            let (second_err, _) = run_concurrent_successor_race(
                pool,
                &owner,
                created.handle,
                Some(created.t),
                "successor-first",
                "successor-second",
            )
            .await?;
            assert!(matches!(second_err, StorageError::Conflict(_)));

            // A low-level write without a named predecessor still uses its
            // prepared head snapshot, so the same race remains retryable.
            let (second_err, _) = run_concurrent_successor_race(
                pool,
                &owner,
                created.handle,
                None,
                "snapshot-first",
                "snapshot-second",
            )
            .await?;
            assert!(matches!(second_err, StorageError::Retryable(_)));
            Ok(())
        }
        .await;
        let _ = drop_db(&db_name).await;
        result.expect("concurrent Goal successors must serialize on the prior head");
    }

    #[tokio::test]
    async fn low_level_write_act_is_reserved_until_union_lock_release() {
        let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
        if let Err(e) = create_db(&db_name).await {
            panic!("PG required for tests but admin connect failed: {e}");
        }
        let url = db_url(&db_name);
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let pg = PgStorage::connect(&url).await?;
            pg.run_migrations().await?;
            let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
            let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
            let target = pg.ingest_fact_atomic(&permit, &fact(), None).await?;
            let pool = pg.pool_for_tests();
            let mut tx = pool.begin().await?;
            let prepared = prepare_goal_write(
                &mut tx,
                &owner,
                &GoalWriteCommand {
                    handle: None,
                    schema_id: "core/task-v1".into(),
                    title: "reserved write-act".into(),
                    state: GoalState::Active,
                    request_id: "reserved-write-act".into(),
                    close_fact_t: None,
                    assignment_t: None,
                    dependency_t: vec![],
                    evidence_t: vec![target.memory_id.into_inner()],
                    wake_id: None,
                    mint_write_act: true,
                    write_act_t: None,
                },
                GoalWakePlan::None,
                None,
            )
            .await?;
            let GoalWritePreparation::New(prepared) = prepared else {
                unreachable!()
            };
            let write_act_t = prepared
                .reserved_write_act_t()
                .expect("reserved write-act t");
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM proxima_core.memory WHERE t = $1",
                )
                .bind(write_act_t)
                .fetch_one(&mut *tx)
                .await?,
                0,
                "reservation has no Memory row before the union lock is attempted"
            );
            let mut blocker = pool.begin().await?;
            lock_target(&mut blocker, target.memory_id.into_inner()).await?;
            let mut lock = Box::pin(lock_prepared_goal_write(&mut tx, &prepared));
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), &mut lock)
                    .await
                    .is_err(),
                "a blocked Goal union must not persist the reserved write-act"
            );
            blocker.commit().await?;
            lock.await?;
            persist_prepared_goal_write(&mut tx, &prepared).await?;
            tx.commit().await?;
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM proxima_core.memory WHERE t = $1",
                )
                .bind(write_act_t)
                .fetch_one(pool)
                .await?,
                1,
                "the exact reserved t is inserted after the union lock"
            );
            Ok(())
        }
        .await;
        let _ = drop_db(&db_name).await;
        result.expect("low-level write-act reservation test failed");
    }

    #[tokio::test]
    async fn terminal_close_fact_is_reserved_until_union_lock_release() {
        let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
        if let Err(e) = create_db(&db_name).await {
            panic!("PG required for tests but admin connect failed: {e}");
        }
        let url = db_url(&db_name);
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let pg = PgStorage::connect(&url).await?;
            pg.run_migrations().await?;
            let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
            let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
            let assignment = grounded_perspective(&pg, &permit).await?;
            let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
            let draft = GoalDraft {
                owner,
                schema_id: SchemaId::new(SimpleTextGoalV1::SCHEMA_ID.into()),
                schema_version: SchemaVersion::new(SimpleTextGoalV1::SCHEMA_VERSION),
                title: "reserved close".into(),
                text: "reserved close".into(),
                payload: Vec::new(),
                sidecar_payload: None,
                state: GoalState::Abandoned,
                topology: GoalTopologyWrite::new(
                    GoalAssignmentTarget::perspective(assignment.memory_id),
                    Vec::new(),
                    Vec::new(),
                )?,
                wake: None,
                supersedes_goal_id: None,
                authorship: GoalAuthorship::User,
                request_id: "reserved-close".into(),
            };
            let pool = pg.pool_for_tests();
            let mut tx = pool.begin().await?;
            let prepared = prepare_goal_insert(
                &mut tx,
                &draft,
                None,
                GoalAtomicContext {
                    registry: &registry,
                    embedding_model_id: None,
                    author_self_perspective_id: None,
                },
                WakeWrite::Explicit(None),
                None,
            )
            .await?;
            let GoalWritePreparation::New(prepared_write) = &prepared.preparation else {
                unreachable!()
            };
            let close_fact_t = prepared_write
                .reserved_close_fact_t()
                .expect("terminal Goal reserves a close Fact t");
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM proxima_core.memory WHERE t = $1",
                )
                .bind(close_fact_t)
                .fetch_one(&mut *tx)
                .await?,
                0,
                "terminal close reservation has no Memory row before the union lock is attempted"
            );
            let mut blocker = pool.begin().await?;
            lock_target(&mut blocker, assignment.memory_id.into_inner()).await?;
            let mut lock = Box::pin(lock_prepared_goal_write(&mut tx, prepared_write));
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), &mut lock)
                    .await
                    .is_err(),
                "a blocked terminal Goal union must not persist its close Fact"
            );
            blocker.commit().await?;
            lock.await?;
            persist_prepared_goal_write(&mut tx, prepared_write).await?;
            tx.commit().await?;
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM proxima_core.memory WHERE t = $1",
                )
                .bind(close_fact_t)
                .fetch_one(pool)
                .await?,
                1,
                "the exact terminal close t is inserted after release"
            );
            Ok(())
        }
        .await;
        let _ = drop_db(&db_name).await;
        result.expect("terminal close reservation test failed");
    }
}
