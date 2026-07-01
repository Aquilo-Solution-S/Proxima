//! Core Goal storage atoms.

use std::collections::HashSet;

use proxima_core::goal::payloads::{
    GoalAbandonedV1, GoalAchievedV1, GoalActivatedV1, GoalPausedV1,
};
use proxima_core::goal::relations::CORE_MOTIVATED_BY_RELATION;
use proxima_core::relation::{CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION};
use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, DecomposedGoalOutcome, GoalAtomicContext, GoalAuthorship, GoalDraft,
    GoalEvidenceRef, GoalLifecycleFact, GoalPayloadWrite, GoalState, GoalWakeConfigWrite,
    GoalWakeToolId, GoalWakeTrigger, GoalWriteOutcome, ModifyGoalAtomicRequest, SystemOrigin,
    TransitionGoalAtomicRequest,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    EdgeAuthorshipKind, EntityKind, FactPayload, GoalId, MemoryId, Owner, RegisteredRelation,
    SchemaId, SchemaVersion, SourceBatchId, SourceId, StorageError,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::authorship::{AuthorshipColumns, authorship_columns};
use crate::error::{internal, map_err};
use crate::sidecars::{PgSidecarKey, PgSidecarRegistryFrozen};
use crate::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use crate::verbs::fact_ingest::ingest_fact_command_in_tx;

const LIFECYCLE_SOURCE_ID: &str = "core/goal-lifecycle";

trait GoalLifecyclePayload: FactPayload {
    const SIDECAR_TABLE: &'static str;

    fn goal_id(&self) -> uuid::Uuid;

    fn transitioned_at(&self) -> time::OffsetDateTime;
}

impl GoalLifecyclePayload for GoalActivatedV1 {
    const SIDECAR_TABLE: &'static str = "proxima_core.goal_activated_v1";

    fn goal_id(&self) -> uuid::Uuid {
        self.goal_id
    }

    fn transitioned_at(&self) -> time::OffsetDateTime {
        self.transitioned_at
    }
}

impl GoalLifecyclePayload for GoalPausedV1 {
    const SIDECAR_TABLE: &'static str = "proxima_core.goal_paused_v1";

    fn goal_id(&self) -> uuid::Uuid {
        self.goal_id
    }

    fn transitioned_at(&self) -> time::OffsetDateTime {
        self.transitioned_at
    }
}

impl GoalLifecyclePayload for GoalAchievedV1 {
    const SIDECAR_TABLE: &'static str = "proxima_core.goal_achieved_v1";

    fn goal_id(&self) -> uuid::Uuid {
        self.goal_id
    }

    fn transitioned_at(&self) -> time::OffsetDateTime {
        self.transitioned_at
    }
}

impl GoalLifecyclePayload for GoalAbandonedV1 {
    const SIDECAR_TABLE: &'static str = "proxima_core.goal_abandoned_v1";

    fn goal_id(&self) -> uuid::Uuid {
        self.goal_id
    }

    fn transitioned_at(&self) -> time::OffsetDateTime {
        self.transitioned_at
    }
}

#[derive(Debug)]
struct InsertedGoal {
    goal_id: GoalId,
    change_event_seq: uuid::Uuid,
    idempotent_replay: bool,
}

#[derive(Debug, Clone)]
struct StoredGoal {
    schema_id: SchemaId,
    schema_version: SchemaVersion,
    title: String,
    text: String,
    payload: Vec<u8>,
    state: GoalState,
    assignment: MemoryId,
    dependencies: Vec<GoalId>,
}

#[derive(Debug, sqlx::FromRow)]
struct StoredGoalRow {
    schema_id: String,
    schema_version: i32,
    title: String,
    text: String,
    payload: Vec<u8>,
    state: GoalState,
}

#[derive(Debug, sqlx::FromRow)]
struct ExistingGoalRow {
    goal_id: uuid::Uuid,
    seq: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
struct GoalBodyRow {
    schema_id: String,
    schema_version: i32,
    title: String,
    text: String,
    payload: Vec<u8>,
    state: GoalState,
    supersedes: Option<uuid::Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
struct AuthorshipRow {
    authorship_kind: proxima_core::verbs::goal_write::GoalAuthorshipKind,
    authorship_origin: Option<proxima_core::verbs::goal_write::GoalAuthorshipOrigin>,
    authorship_operator_id: Option<uuid::Uuid>,
    authorship_tool_id: Option<String>,
    operator_kind: Option<proxima_core::verbs::goal_write::OperatorKind>,
    input_contract_id: Option<uuid::Uuid>,
    model_id: Option<String>,
    prompt_version: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct EvidenceRow {
    kind: Option<EntityKind>,
}

#[derive(Debug, Clone, Copy)]
struct EvidenceTarget {
    kind: EntityKind,
    memory_id: MemoryId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WakeConfigShape {
    trigger_kind: String,
    trigger_schema_id: Option<String>,
    trigger_schema_version: Option<i32>,
    trigger_memory_id: Option<uuid::Uuid>,
    tool_ids: Vec<String>,
    prompt: String,
    hard_memory_ids: Vec<uuid::Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
struct WakeConfigRow {
    trigger_kind: String,
    trigger_schema_id: Option<String>,
    trigger_schema_version: Option<i32>,
    trigger_memory_id: Option<uuid::Uuid>,
    tool_ids: Vec<String>,
    prompt: String,
    hard_memory_ids: Vec<uuid::Uuid>,
}

#[derive(Debug, Clone, Copy)]
enum WakeWrite<'a> {
    Explicit(Option<&'a GoalWakeConfigWrite>),
    CarryFrom(GoalId),
}

pub(crate) async fn create_goal_atomic(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &CreateGoalAtomicRequest<'_>,
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

async fn insert_or_replay_goal(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    draft: &GoalDraft,
    expected_prior: Option<GoalId>,
    context: GoalAtomicContext<'_>,
    wake_write: WakeWrite<'_>,
) -> Result<InsertedGoal, StorageError> {
    validate_goal_schema(context, draft)?;
    let owner = draft.owner();
    let (owner_kind, owner_id) = owner.columns();
    let existing: Option<ExistingGoalRow> = sqlx::query_as(
        "SELECT g.goal_id, ce.seq
           FROM proxima_core.goals g
           JOIN proxima_core.change_event ce ON ce.entity_goal_id = g.goal_id
          WHERE g.idempotency_key = md5($1::text || ':' || $2::text || ':' || $3)
          ORDER BY ce.seq ASC
          LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(&draft.request_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;

    if let Some(existing) = existing {
        let body_matches =
            existing_goal_body_matches(tx, existing.goal_id, draft, expected_prior, wake_write)
                .await?;
        if body_matches && authorship_matches(tx, existing.goal_id, &draft.authorship).await? {
            return Ok(InsertedGoal {
                goal_id: GoalId::new(existing.goal_id),
                change_event_seq: existing.seq,
                idempotent_replay: true,
            });
        }
        return Err(idempotency_conflict(&draft.request_id));
    }

    let goal_id = uuid::Uuid::now_v7();
    let change_seq = uuid::Uuid::now_v7();
    insert_goal_row(tx, draft, goal_id, expected_prior).await?;
    insert_goal_sidecar(
        tx,
        sidecars,
        context,
        draft,
        GoalId::new(goal_id),
        expected_prior,
    )
    .await?;
    insert_goal_dependency_edges(tx, context, draft, goal_id).await?;
    write_goal_wake_config(tx, context, GoalId::new(goal_id), wake_write).await?;
    insert_goal_change_event(tx, draft, goal_id, change_seq, expected_prior).await?;
    Ok(InsertedGoal {
        goal_id: GoalId::new(goal_id),
        change_event_seq: change_seq,
        idempotent_replay: false,
    })
}

fn validate_goal_schema(
    context: GoalAtomicContext<'_>,
    draft: &GoalDraft,
) -> Result<(), StorageError> {
    context
        .registry
        .lookup_payload(&draft.schema_id, draft.schema_version, PayloadKind::Goal)
        .ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "unregistered GoalPayload schema {} v{}",
                draft.schema_id.as_str(),
                draft.schema_version.into_inner(),
            ))
        })?;
    Ok(())
}

async fn insert_goal_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    context: GoalAtomicContext<'_>,
    draft: &GoalDraft,
    goal_id: GoalId,
    source_goal_id: Option<GoalId>,
) -> Result<(), StorageError> {
    let Some(sidecar_table) = context
        .registry
        .lookup_payload(&draft.schema_id, draft.schema_version, PayloadKind::Goal)
        .and_then(|schema| schema.sidecar_table.as_deref())
    else {
        return Ok(());
    };
    if let Some(payload) = &draft.sidecar_payload {
        if payload.kind != PayloadKind::Goal
            || payload.schema_id != draft.schema_id
            || payload.schema_version != draft.schema_version
        {
            return Err(StorageError::ConstraintViolation(format!(
                "Goal sidecar payload drift for {} v{} table {sidecar_table}",
                draft.schema_id.as_str(),
                draft.schema_version.into_inner(),
            )));
        }
        sidecars.insert_goal_sidecar(tx, goal_id, payload).await?;
        return Ok(());
    }

    if let Some(source_goal_id) = source_goal_id {
        let key = PgSidecarKey::new(
            PayloadKind::Goal,
            draft.schema_id.clone(),
            draft.schema_version,
        );
        sidecars
            .copy_goal_sidecar(tx, key, goal_id, source_goal_id)
            .await?;
        return Ok(());
    }

    Err(StorageError::ConstraintViolation(format!(
        "missing typed Goal sidecar payload for {} v{} table {sidecar_table}",
        draft.schema_id.as_str(),
        draft.schema_version.into_inner(),
    )))
}

struct CreateGoalReplayExpectation<'a> {
    goal_id: GoalId,
    target_self_perspective_id: MemoryId,
    evidence: &'a [EvidenceTarget],
    evidence_authorship_kind: EdgeAuthorshipKind,
    author_self_perspective_id: Option<MemoryId>,
    wake_write: WakeWrite<'a>,
    expected_prior: Option<GoalId>,
    request_id: &'a str,
}

async fn ensure_create_goal_replay_side_effects_match(
    tx: &mut Transaction<'_, Postgres>,
    expected: CreateGoalReplayExpectation<'_>,
) -> Result<(), StorageError> {
    if !goal_self_assignment_matches(tx, expected.goal_id, expected.target_self_perspective_id)
        .await?
    {
        return Err(idempotency_conflict(expected.request_id));
    }
    if !goal_evidence_edges_match(
        tx,
        expected.goal_id,
        expected.evidence,
        expected.evidence_authorship_kind,
    )
    .await?
    {
        return Err(idempotency_conflict(expected.request_id));
    }
    let Some(lifecycle_memory_id) =
        lifecycle_memory_for_goal(tx, expected.goal_id, GoalLifecycleFact::Activated).await?
    else {
        return Err(idempotency_conflict(expected.request_id));
    };
    if !lifecycle_author_edge_matches(tx, lifecycle_memory_id, expected.author_self_perspective_id)
        .await?
    {
        return Err(idempotency_conflict(expected.request_id));
    }
    if !goal_wake_matches(
        tx,
        expected.goal_id,
        expected.wake_write,
        expected.expected_prior,
    )
    .await?
    {
        return Err(idempotency_conflict(expected.request_id));
    }
    Ok(())
}

async fn goal_self_assignment_matches(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    target_self_perspective_id: MemoryId,
) -> Result<bool, StorageError> {
    let rows: Vec<(Option<uuid::Uuid>,)> = sqlx::query_as(
        "SELECT target_memory_id
           FROM proxima_core.edges
          WHERE source_goal_id = $1
            AND relation = $2
          ORDER BY edge_id ASC",
    )
    .bind(goal_id.into_inner())
    .bind(proxima_core::relation::CORE_INSPIRES_RELATION)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(rows.len() == 1 && rows[0].0 == Some(target_self_perspective_id.into_inner()))
}

async fn goal_evidence_edges_match(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    evidence: &[EvidenceTarget],
    authorship_kind: EdgeAuthorshipKind,
) -> Result<bool, StorageError> {
    type EvidenceEdgeTuple = (
        String,
        String,
        Option<uuid::Uuid>,
        EntityKind,
        Option<uuid::Uuid>,
        EdgeAuthorshipKind,
    );

    let rows: Vec<EvidenceEdgeTuple> = sqlx::query_as(
        "SELECT relation, relation_class::text, source_goal_id, target_kind,
                target_memory_id, authorship_kind
           FROM proxima_core.edges
          WHERE source_goal_id = $1
            AND relation = $2
          ORDER BY edge_id ASC",
    )
    .bind(goal_id.into_inner())
    .bind(CORE_MOTIVATED_BY_RELATION)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    let stored = rows.into_iter().collect::<HashSet<_>>();
    let requested = evidence
        .iter()
        .map(|target| {
            (
                CORE_MOTIVATED_BY_RELATION.to_string(),
                "Structural".to_string(),
                Some(goal_id.into_inner()),
                target.kind,
                Some(target.memory_id.into_inner()),
                authorship_kind,
            )
        })
        .collect::<HashSet<_>>();
    Ok(stored == requested)
}

async fn lifecycle_author_edge_matches(
    tx: &mut Transaction<'_, Postgres>,
    lifecycle_memory_id: MemoryId,
    author_self_perspective_id: Option<MemoryId>,
) -> Result<bool, StorageError> {
    let rows: Vec<(Option<uuid::Uuid>,)> = sqlx::query_as(
        "SELECT source_memory_id
           FROM proxima_core.edges
          WHERE target_memory_id = $1
            AND relation = $2
          ORDER BY edge_id ASC",
    )
    .bind(lifecycle_memory_id.into_inner())
    .bind(CORE_AUTHORED_RELATION)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    let expected = author_self_perspective_id.map(MemoryId::into_inner);
    match expected {
        Some(expected) => Ok(rows.len() == 1 && rows[0].0 == Some(expected)),
        None => Ok(rows.is_empty()),
    }
}

fn idempotency_conflict(request_id: &str) -> StorageError {
    StorageError::ConstraintViolation(format!("idempotency_conflict:{request_id}"))
}

async fn existing_goal_body_matches(
    tx: &mut Transaction<'_, Postgres>,
    existing_goal_id: uuid::Uuid,
    draft: &GoalDraft,
    expected_prior: Option<GoalId>,
    wake_write: WakeWrite<'_>,
) -> Result<bool, StorageError> {
    let row: GoalBodyRow = sqlx::query_as(
        "SELECT schema_id, schema_version, title, text, payload,
                state, supersedes
           FROM proxima_core.goals
          WHERE goal_id = $1",
    )
    .bind(existing_goal_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    let dependencies = dependency_goal_ids(tx, GoalId::new(existing_goal_id)).await?;
    let existing_dependencies: HashSet<GoalId> = dependencies.into_iter().collect();
    let draft_dependencies: HashSet<GoalId> = draft
        .topology
        .dependencies()
        .iter()
        .map(|dependency| dependency.goal_id())
        .collect();
    Ok(row.schema_id == draft.schema_id.as_str()
        && row.schema_version == draft.schema_version.into_inner().cast_signed()
        && row.title == draft.title
        && row.text == draft.text
        && row.payload == draft.payload
        && row.state == draft.state
        && row.supersedes == expected_prior.map(GoalId::into_inner)
        && existing_dependencies == draft_dependencies
        && goal_wake_matches(
            tx,
            GoalId::new(existing_goal_id),
            wake_write,
            expected_prior,
        )
        .await?)
}

async fn authorship_matches(
    tx: &mut Transaction<'_, Postgres>,
    existing_goal_id: uuid::Uuid,
    authorship: &GoalAuthorship,
) -> Result<bool, StorageError> {
    let row: AuthorshipRow = sqlx::query_as(
        "SELECT authorship_kind, authorship_origin, authorship_operator_id,
                authorship_tool_id, operator_kind, input_contract_id, model_id, prompt_version
           FROM proxima_core.goals
          WHERE goal_id = $1",
    )
    .bind(existing_goal_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    let existing = AuthorshipColumns {
        authorship_kind: row.authorship_kind,
        authorship_origin: row.authorship_origin,
        authorship_operator_id: row.authorship_operator_id,
        authorship_tool_id: row.authorship_tool_id,
        operator_kind: row.operator_kind,
        input_contract_id: row.input_contract_id,
        model_id: row.model_id,
        prompt_version: row.prompt_version,
    };
    Ok(existing == authorship_columns(authorship))
}

async fn insert_goal_row(
    tx: &mut Transaction<'_, Postgres>,
    draft: &GoalDraft,
    goal_id: uuid::Uuid,
    supersedes: Option<GoalId>,
) -> Result<(), StorageError> {
    if supersedes.is_none() && draft.state != GoalState::Active {
        return Err(StorageError::ConstraintViolation(
            "root goal rows must be Active".into(),
        ));
    }
    let owner = draft.owner();
    let (owner_kind, owner_id) = owner.columns();
    let authorship = authorship_columns(&draft.authorship);
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, schema_id, schema_version, owner_kind, owner_id,
             title, text, payload, state, supersedes,
             authorship_kind, authorship_origin, authorship_operator_id,
             authorship_tool_id, operator_kind, input_contract_id, model_id, prompt_version,
             request_id, idempotency_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                 $11, $12, $13, $14, $15, $16, $17, $18, $19,
                 md5($4::text || ':' || $5::text || ':' || $19))",
    )
    .bind(goal_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(&draft.title)
    .bind(&draft.text)
    .bind(&draft.payload)
    .bind(draft.state)
    .bind(supersedes.map(GoalId::into_inner))
    .bind(authorship.authorship_kind)
    .bind(authorship.authorship_origin)
    .bind(authorship.authorship_operator_id)
    .bind(authorship.authorship_tool_id)
    .bind(authorship.operator_kind)
    .bind(authorship.input_contract_id)
    .bind(authorship.model_id)
    .bind(authorship.prompt_version)
    .bind(&draft.request_id)
    .execute(&mut **tx)
    .await
    .map_err(map_goal_insert_err)?;
    Ok(())
}

fn map_goal_insert_err(err: sqlx::Error) -> StorageError {
    if let sqlx::Error::Database(db) = &err
        && db.is_unique_violation()
        && db.constraint() == Some("goals_supersedes_unique")
    {
        return StorageError::Conflict("stale goal head".into());
    }
    map_err(err)
}

async fn insert_goal_dependency_edges(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    draft: &GoalDraft,
    goal_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let owner = draft.owner();
    let relation = resolve_relation(context, proxima_core::relation::CORE_DEPENDS_ON_RELATION)?;
    for dependency in draft.topology.dependencies() {
        let dependency_id = dependency.goal_id();
        validate_active_head(tx, &owner, dependency_id).await?;
        let edge = EdgeDraft {
            edge_id: uuid::Uuid::now_v7(),
            relation,
            source_kind: EntityKind::Goal,
            source_memory_id: None,
            source_goal_id: Some(goal_id),
            source_fact_entity_id: None,
            target_kind: EntityKind::Goal,
            target_memory_id: None,
            target_goal_id: Some(dependency_id.into_inner()),
            target_fact_entity_id: None,
            authorship_kind: EdgeAuthorshipKind::Engine,
            authorship_owner_memory_id: None,
            owner: &owner,
        };
        append_edge_in_tx(tx.as_mut(), &edge).await?;
    }
    Ok(())
}

async fn write_goal_wake_config(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    goal_id: GoalId,
    wake_write: WakeWrite<'_>,
) -> Result<(), StorageError> {
    match wake_write {
        WakeWrite::Explicit(Some(config)) => {
            validate_wake_config_storage(tx, context, config).await?;
            insert_goal_wake_config(tx, goal_id, config).await
        }
        WakeWrite::Explicit(None) => Ok(()),
        WakeWrite::CarryFrom(source_goal_id) => {
            sqlx::query(
                "INSERT INTO proxima_core.goal_wake_config
                    (goal_id, trigger_kind, trigger_schema_id, trigger_schema_version,
                     trigger_memory_id, tool_ids, prompt, hard_memory_ids)
                 SELECT $1, trigger_kind, trigger_schema_id, trigger_schema_version,
                        trigger_memory_id, tool_ids, prompt, hard_memory_ids
                   FROM proxima_core.goal_wake_config
                  WHERE goal_id = $2",
            )
            .bind(goal_id.into_inner())
            .bind(source_goal_id.into_inner())
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
            Ok(())
        }
    }
}

async fn validate_wake_config_storage(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    config: &GoalWakeConfigWrite,
) -> Result<(), StorageError> {
    match config.trigger() {
        proxima_core::GoalWakeTrigger::FactSchema {
            schema_id,
            schema_version,
        } => {
            context
                .registry
                .lookup_payload(schema_id, *schema_version, PayloadKind::Fact)
                .ok_or_else(|| {
                    StorageError::ConstraintViolation(format!(
                        "unregistered wake trigger Fact schema {} v{}",
                        schema_id.as_str(),
                        schema_version.into_inner()
                    ))
                })?;
        }
        proxima_core::GoalWakeTrigger::FactMemory { memory_id } => {
            validate_wake_memory_exists(tx, *memory_id, Some(EntityKind::Fact)).await?;
        }
    }
    for tool_id in config.tool_ids() {
        GoalWakeToolId::parse(tool_id.as_str(), context.registry).map_err(|err| {
            StorageError::ConstraintViolation(format!(
                "invalid wake tool id {}: {}",
                tool_id.as_str(),
                err.message
            ))
        })?;
    }
    let mut seen = std::collections::HashSet::with_capacity(config.hard_memory_ids().len());
    for memory_id in config.hard_memory_ids() {
        if !seen.insert(*memory_id) {
            return Err(StorageError::ConstraintViolation(
                "duplicate wake hard memory".into(),
            ));
        }
        validate_wake_memory_exists(tx, *memory_id, None).await?;
    }
    Ok(())
}

async fn validate_wake_memory_exists(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    expected: Option<EntityKind>,
) -> Result<(), StorageError> {
    let row: Option<(Option<EntityKind>,)> = sqlx::query_as(
        "SELECT kind FROM proxima_core.memories WHERE memory_id = $1 AND tombstoned_at IS NULL",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    let Some((stored_kind,)) = row else {
        return Err(StorageError::ConstraintViolation(
            "wake memory does not exist".into(),
        ));
    };
    let kind = stored_kind.unwrap_or(EntityKind::Fact);
    if expected.is_some_and(|expected| expected != kind) {
        return Err(StorageError::ConstraintViolation(
            "wake trigger memory must be a Fact".into(),
        ));
    }
    Ok(())
}

async fn insert_goal_wake_config(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    config: &GoalWakeConfigWrite,
) -> Result<(), StorageError> {
    let shape = wake_shape_from_config(config);
    sqlx::query(
        "INSERT INTO proxima_core.goal_wake_config
            (goal_id, trigger_kind, trigger_schema_id, trigger_schema_version,
             trigger_memory_id, tool_ids, prompt, hard_memory_ids)
         VALUES ($1, $2::proxima_core.goal_wake_trigger_kind, $3, $4, $5, $6, $7, $8)",
    )
    .bind(goal_id.into_inner())
    .bind(&shape.trigger_kind)
    .bind(&shape.trigger_schema_id)
    .bind(shape.trigger_schema_version)
    .bind(shape.trigger_memory_id)
    .bind(&shape.tool_ids)
    .bind(&shape.prompt)
    .bind(&shape.hard_memory_ids)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn goal_wake_matches(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    wake_write: WakeWrite<'_>,
    expected_prior: Option<GoalId>,
) -> Result<bool, StorageError> {
    let expected = match wake_write {
        WakeWrite::Explicit(config) => config.map(wake_shape_from_config),
        WakeWrite::CarryFrom(source_goal_id) => load_wake_shape(tx, source_goal_id).await?,
    };
    let _ = expected_prior;
    Ok(load_wake_shape(tx, goal_id).await? == expected)
}

async fn load_wake_shape(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
) -> Result<Option<WakeConfigShape>, StorageError> {
    let row: Option<WakeConfigRow> = sqlx::query_as(
        "SELECT trigger_kind::text AS trigger_kind,
                trigger_schema_id,
                trigger_schema_version,
                trigger_memory_id,
                tool_ids,
                prompt,
                hard_memory_ids
           FROM proxima_core.goal_wake_config
          WHERE goal_id = $1",
    )
    .bind(goal_id.into_inner())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(row.map(|row| WakeConfigShape {
        trigger_kind: row.trigger_kind,
        trigger_schema_id: row.trigger_schema_id,
        trigger_schema_version: row.trigger_schema_version,
        trigger_memory_id: row.trigger_memory_id,
        tool_ids: row.tool_ids,
        prompt: row.prompt,
        hard_memory_ids: row.hard_memory_ids,
    }))
}

fn wake_shape_from_config(config: &GoalWakeConfigWrite) -> WakeConfigShape {
    let (trigger_kind, trigger_schema_id, trigger_schema_version, trigger_memory_id) =
        match config.trigger() {
            GoalWakeTrigger::FactSchema {
                schema_id,
                schema_version,
            } => (
                "fact_schema".to_string(),
                Some(schema_id.as_str().to_string()),
                Some(schema_version.into_inner().cast_signed()),
                None,
            ),
            GoalWakeTrigger::FactMemory { memory_id } => (
                "fact_memory".to_string(),
                None,
                None,
                Some(memory_id.into_inner()),
            ),
        };
    WakeConfigShape {
        trigger_kind,
        trigger_schema_id,
        trigger_schema_version,
        trigger_memory_id,
        tool_ids: config
            .tool_ids()
            .iter()
            .map(|tool| tool.as_str().to_string())
            .collect(),
        prompt: config.prompt().to_string(),
        hard_memory_ids: config
            .hard_memory_ids()
            .iter()
            .map(|memory_id| memory_id.into_inner())
            .collect(),
    }
}

async fn insert_goal_change_event(
    tx: &mut Transaction<'_, Postgres>,
    draft: &GoalDraft,
    goal_id: uuid::Uuid,
    change_seq: uuid::Uuid,
    supersedes_goal_id: Option<GoalId>,
) -> Result<(), StorageError> {
    let owner = draft.owner();
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_kind, owner_id,
             kind, entity_kind, entity_goal_id, entity_schema_id,
             entity_schema_version, supersedes_goal_id)
         VALUES ($1, $2, $3, 'EntityAppend', 'Goal', $4, $5, $6, $7)",
    )
    .bind(change_seq)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(goal_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(supersedes_goal_id.map(GoalId::into_inner))
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn load_prior_goal(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    goal_id: GoalId,
) -> Result<StoredGoal, StorageError> {
    let (owner_kind, owner_id) = owner.columns();
    let row: Option<StoredGoalRow> = sqlx::query_as(
        "SELECT schema_id, schema_version, title, text, payload, state
           FROM proxima_core.goals
          WHERE goal_id = $1
            AND owner_kind = $2
            AND owner_id IS NOT DISTINCT FROM $3",
    )
    .bind(goal_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    let Some(row) = row else {
        return Err(StorageError::NotFound);
    };
    Ok(StoredGoal {
        schema_id: SchemaId::new(row.schema_id),
        schema_version: SchemaVersion::new(row.schema_version.cast_unsigned()),
        title: row.title,
        text: row.text,
        payload: row.payload,
        state: row.state,
        assignment: goal_assignment_target(tx, goal_id).await?,
        dependencies: dependency_goal_ids(tx, goal_id).await?,
    })
}

async fn dependency_goal_ids(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
) -> Result<Vec<GoalId>, StorageError> {
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT target_goal_id
           FROM proxima_core.edges
          WHERE source_goal_id = $1
            AND relation = 'core/depends-on'
            AND target_goal_id IS NOT NULL
          ORDER BY target_goal_id",
    )
    .bind(goal_id.into_inner())
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().map(|(id,)| GoalId::new(id)).collect())
}

async fn goal_assignment_target(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
) -> Result<MemoryId, StorageError> {
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT target_memory_id
           FROM proxima_core.edges
          WHERE source_goal_id = $1
            AND relation = 'core/inspires'
            AND target_memory_id IS NOT NULL
          ORDER BY created_at ASC
          LIMIT 1",
    )
    .bind(goal_id.into_inner())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    row.map(|(id,)| MemoryId::new(id))
        .ok_or_else(|| StorageError::ConstraintViolation("goal assignment edge missing".into()))
}

async fn validate_active_head(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    goal_id: GoalId,
) -> Result<(), StorageError> {
    let prior = load_prior_goal(tx, owner, goal_id).await?;
    if prior.state != GoalState::Active {
        return Err(StorageError::ConstraintViolation(
            "parent_goal must be Active".into(),
        ));
    }
    let (owner_kind, owner_id) = owner.columns();
    let newer_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.goals
              WHERE supersedes = $1
                AND owner_kind = $2
                AND owner_id IS NOT DISTINCT FROM $3
         )",
    )
    .bind(goal_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    if newer_exists {
        Err(StorageError::Conflict(
            "parent_goal is not current head".into(),
        ))
    } else {
        Ok(())
    }
}

fn validate_goal_transition(prior: GoalState, next: GoalState) -> Result<(), StorageError> {
    match (prior, next) {
        (
            GoalState::Active,
            GoalState::Active | GoalState::Paused | GoalState::Achieved | GoalState::Abandoned,
        )
        | (GoalState::Paused, GoalState::Active) => Ok(()),
        _ => Err(StorageError::ConstraintViolation(format!(
            "invalid goal transition: {prior:?} -> {next:?}",
        ))),
    }
}

fn draft_from_stored(
    owner: &Owner,
    stored: &StoredGoal,
    state: GoalState,
    supersedes: Option<GoalId>,
    authorship: GoalAuthorship,
    request_id: &str,
) -> GoalDraft {
    GoalDraft {
        principal: *owner,
        schema_id: stored.schema_id.clone(),
        schema_version: stored.schema_version,
        title: stored.title.clone(),
        text: stored.text.clone(),
        payload: stored.payload.clone(),
        sidecar_payload: None,
        state,
        topology: proxima_core::GoalTopologyWrite::new(
            proxima_core::GoalAssignmentTarget::perspective(stored.assignment),
            stored
                .dependencies
                .iter()
                .copied()
                .map(proxima_core::GoalDependencyRef::new)
                .collect(),
            Vec::new(),
        )
        .expect("stored topology has unique dependencies"),
        wake: None,
        supersedes_goal_id: supersedes,
        authorship,
        request_id: request_id.to_string(),
    }
}

struct DraftFromPayload<'a> {
    owner: &'a Owner,
    payload: &'a GoalPayloadWrite,
    state: GoalState,
    assignment: MemoryId,
    dependencies: Vec<GoalId>,
    supersedes: Option<GoalId>,
    authorship: GoalAuthorship,
    request_id: &'a str,
}

fn draft_from_payload(input: DraftFromPayload<'_>) -> GoalDraft {
    GoalDraft {
        principal: *input.owner,
        schema_id: input.payload.schema_id.clone(),
        schema_version: input.payload.schema_version,
        title: input.payload.title.clone(),
        text: input.payload.text.clone(),
        payload: input.payload.payload.clone(),
        sidecar_payload: input.payload.sidecar_payload.clone(),
        state: input.state,
        topology: proxima_core::GoalTopologyWrite::new(
            proxima_core::GoalAssignmentTarget::perspective(input.assignment),
            input
                .dependencies
                .into_iter()
                .map(proxima_core::GoalDependencyRef::new)
                .collect(),
            Vec::new(),
        )
        .expect("stored topology has unique dependencies"),
        wake: None,
        supersedes_goal_id: input.supersedes,
        authorship: input.authorship,
        request_id: input.request_id.to_string(),
    }
}

fn child_draft(
    owner: &Owner,
    parent_goal_id: GoalId,
    topology: &proxima_core::GoalTopologyWrite,
    authorship: &GoalAuthorship,
    child: &ChildGoalDraft,
) -> Result<GoalDraft, StorageError> {
    let mut dependencies = topology.dependencies().to_vec();
    dependencies.push(proxima_core::GoalDependencyRef::new(parent_goal_id));
    let child_topology = proxima_core::GoalTopologyWrite::new(
        topology.assignment(),
        dependencies,
        child.evidence.clone(),
    )
    .map_err(|err| StorageError::ConstraintViolation(err.message))?;
    Ok(GoalDraft::active_from_payload_write(
        *owner,
        child.payload.clone(),
        child_topology,
        child.wake.clone(),
        authorship.clone(),
        child.request_id.clone(),
    ))
}

fn validate_operator_goal_evidence(
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

async fn validate_evidence_in_owner(
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
        let row: Option<EvidenceRow> = sqlx::query_as(
            "SELECT m.kind
               FROM proxima_core.memories m
              WHERE m.memory_id = $1
                AND m.tombstoned_at IS NULL",
        )
        .bind(item.memory_id().into_inner())
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_err)?;
        let Some(row) = row else {
            return Err(StorageError::ConstraintViolation(
                "evidence does not exist".into(),
            ));
        };
        let kind = row.kind.unwrap_or(EntityKind::Fact);
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

async fn outgoing_motivated_by_evidence(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    goal_id: GoalId,
) -> Result<Vec<EvidenceTarget>, StorageError> {
    let (owner_kind, owner_id) = owner.columns();
    let rows: Vec<(EntityKind, uuid::Uuid)> = sqlx::query_as(
        "SELECT target_kind, target_memory_id
           FROM proxima_core.edges e
          WHERE relation = $1
            AND source_goal_id = $2
            AND e.owner_kind = $3
            AND e.owner_id IS NOT DISTINCT FROM $4
            AND target_memory_id IS NOT NULL
          ORDER BY created_at ASC",
    )
    .bind(CORE_MOTIVATED_BY_RELATION)
    .bind(goal_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(|(kind, memory_id)| EvidenceTarget {
            kind,
            memory_id: MemoryId::new(memory_id),
        })
        .collect())
}

async fn emit_lifecycle_fact(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    owner: &Owner,
    goal_id: GoalId,
    lifecycle: GoalLifecycleFact,
) -> Result<MemoryId, StorageError> {
    let now = time::OffsetDateTime::now_utc();
    match lifecycle {
        GoalLifecycleFact::Activated => {
            let payload = GoalActivatedV1 {
                goal_id: goal_id.into_inner(),
                transitioned_at: now,
            };
            ingest_lifecycle_fact(tx, context, owner, &payload).await
        }
        GoalLifecycleFact::Paused => {
            let payload = GoalPausedV1 {
                goal_id: goal_id.into_inner(),
                transitioned_at: now,
            };
            ingest_lifecycle_fact(tx, context, owner, &payload).await
        }
        GoalLifecycleFact::Achieved => {
            let payload = GoalAchievedV1 {
                goal_id: goal_id.into_inner(),
                transitioned_at: now,
            };
            ingest_lifecycle_fact(tx, context, owner, &payload).await
        }
        GoalLifecycleFact::Abandoned => {
            let payload = GoalAbandonedV1 {
                goal_id: goal_id.into_inner(),
                transitioned_at: now,
            };
            ingest_lifecycle_fact(tx, context, owner, &payload).await
        }
    }
}

async fn ingest_lifecycle_fact<T>(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    owner: &Owner,
    payload: &T,
) -> Result<MemoryId, StorageError>
where
    T: GoalLifecyclePayload,
{
    let now = time::OffsetDateTime::now_utc();
    let draft = FactWriteCommand {
        schema_id: SchemaId::new(T::SCHEMA_ID.to_string()),
        schema_version: SchemaVersion::new(T::SCHEMA_VERSION),
        payload: payload.receipt_key(),
        rendered_text: Some(payload.render()),
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new(LIFECYCLE_SOURCE_ID),
            source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
        citation: None,
    };
    let outcome = ingest_fact_command_in_tx(tx, owner, &draft, context.embedding_model_id).await?;
    if !outcome.idempotent_replay {
        insert_lifecycle_sidecar(tx, outcome.memory_id, payload).await?;
    }
    Ok(outcome.memory_id)
}

async fn insert_lifecycle_sidecar<T>(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &T,
) -> Result<(), StorageError>
where
    T: GoalLifecyclePayload,
{
    let table = T::SIDECAR_TABLE;
    let sql = format!(
        "INSERT INTO {table} (memory_id, goal_id, transitioned_at)
         VALUES ($1, $2, $3)"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(memory_id.into_inner())
        .bind(payload.goal_id())
        .bind(payload.transitioned_at())
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(())
}

async fn lifecycle_outcome(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    context: GoalAtomicContext<'_>,
    inserted: InsertedGoal,
    lifecycle: GoalLifecycleFact,
    assignment: MemoryId,
) -> Result<GoalWriteOutcome, StorageError> {
    if inserted.idempotent_replay {
        return replay_goal_outcome(
            tx,
            inserted,
            lifecycle,
            &[proxima_core::relation::CORE_INSPIRES_RELATION],
        )
        .await;
    }
    let lifecycle_memory_id =
        Some(emit_lifecycle_fact(tx, context, owner, inserted.goal_id, lifecycle).await?);
    let mut edge_ids = Vec::new();
    edge_ids
        .push(append_goal_to_self_edge(tx, context, owner, inserted.goal_id, assignment).await?);
    if let Some(lifecycle_id) = lifecycle_memory_id
        && let Some(edge_id) =
            append_lifecycle_authored_edge(tx, context, owner, lifecycle_id).await?
    {
        edge_ids.push(edge_id);
    }
    Ok(GoalWriteOutcome {
        goal_id: inserted.goal_id,
        change_event_seq: inserted.change_event_seq,
        lifecycle_memory_id,
        edge_ids,
        idempotent_replay: false,
    })
}

async fn replay_goal_outcome(
    tx: &mut Transaction<'_, Postgres>,
    inserted: InsertedGoal,
    lifecycle: GoalLifecycleFact,
    source_goal_relations: &[&str],
) -> Result<GoalWriteOutcome, StorageError> {
    let lifecycle_memory_id = lifecycle_memory_for_goal(tx, inserted.goal_id, lifecycle).await?;
    let mut edge_ids =
        edge_ids_for_goal_relations(tx, inserted.goal_id, source_goal_relations).await?;
    if let Some(memory_id) = lifecycle_memory_id {
        edge_ids.extend(edge_ids_for_lifecycle_memory(tx, memory_id).await?);
    }
    Ok(GoalWriteOutcome {
        goal_id: inserted.goal_id,
        change_event_seq: inserted.change_event_seq,
        lifecycle_memory_id,
        edge_ids,
        idempotent_replay: true,
    })
}

async fn lifecycle_memory_for_goal(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    lifecycle: GoalLifecycleFact,
) -> Result<Option<MemoryId>, StorageError> {
    let table = match lifecycle {
        GoalLifecycleFact::Activated => "proxima_core.goal_activated_v1",
        GoalLifecycleFact::Paused => "proxima_core.goal_paused_v1",
        GoalLifecycleFact::Achieved => "proxima_core.goal_achieved_v1",
        GoalLifecycleFact::Abandoned => "proxima_core.goal_abandoned_v1",
    };
    let sql = format!("SELECT memory_id FROM {table} WHERE goal_id = $1 LIMIT 1");
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(goal_id.into_inner())
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(row.map(|(id,)| MemoryId::new(id)))
}

async fn edge_ids_for_goal_relations(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    relations: &[&str],
) -> Result<Vec<uuid::Uuid>, StorageError> {
    if relations.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT edge_id
           FROM proxima_core.edges
          WHERE source_goal_id = $1
            AND relation = ANY($2)
          ORDER BY created_at ASC, edge_id ASC",
    )
    .bind(goal_id.into_inner())
    .bind(
        relations
            .iter()
            .map(|relation| (*relation).to_string())
            .collect::<Vec<_>>(),
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

async fn edge_ids_for_lifecycle_memory(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
) -> Result<Vec<uuid::Uuid>, StorageError> {
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT edge_id
           FROM proxima_core.edges
          WHERE source_memory_id = $1 OR target_memory_id = $1
          ORDER BY created_at ASC, edge_id ASC",
    )
    .bind(memory_id.into_inner())
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

async fn append_goal_to_self_edge(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    owner: &Owner,
    goal_id: GoalId,
    self_memory_id: MemoryId,
) -> Result<uuid::Uuid, StorageError> {
    let relation = resolve_relation(context, proxima_core::relation::CORE_INSPIRES_RELATION)?;
    let edge_id = uuid::Uuid::now_v7();
    let draft = EdgeDraft {
        edge_id,
        relation,
        source_kind: EntityKind::Goal,
        source_memory_id: None,
        source_goal_id: Some(goal_id.into_inner()),
        source_fact_entity_id: None,
        target_kind: EntityKind::Perspective,
        target_memory_id: Some(self_memory_id.into_inner()),
        target_goal_id: None,
        target_fact_entity_id: None,
        authorship_kind: EdgeAuthorshipKind::PerspectiveGoalLink,
        authorship_owner_memory_id: Some(self_memory_id.into_inner()),
        owner,
    };
    append_edge_in_tx(tx, &draft).await?;
    Ok(edge_id)
}

fn motivated_by_authorship_kind(authorship: &GoalAuthorship) -> EdgeAuthorshipKind {
    match authorship {
        GoalAuthorship::System(SystemOrigin::Operator { .. }) => {
            EdgeAuthorshipKind::OperatorAtoGoal
        }
        GoalAuthorship::User => EdgeAuthorshipKind::User,
        GoalAuthorship::System(SystemOrigin::Tool { .. }) => EdgeAuthorshipKind::Engine,
        GoalAuthorship::External => EdgeAuthorshipKind::ExternalAgent,
    }
}

async fn append_motivated_by_edges(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    owner: &Owner,
    goal_id: GoalId,
    evidence: &[EvidenceTarget],
    authorship_kind: EdgeAuthorshipKind,
) -> Result<Vec<uuid::Uuid>, StorageError> {
    let relation = resolve_relation(context, CORE_MOTIVATED_BY_RELATION)?;
    let mut edge_ids = Vec::with_capacity(evidence.len());
    for target in evidence {
        let edge_id = uuid::Uuid::now_v7();
        let draft = EdgeDraft {
            edge_id,
            relation,
            source_kind: EntityKind::Goal,
            source_memory_id: None,
            source_goal_id: Some(goal_id.into_inner()),
            source_fact_entity_id: None,
            target_kind: target.kind,
            target_memory_id: Some(target.memory_id.into_inner()),
            target_goal_id: None,
            target_fact_entity_id: None,
            authorship_kind,
            authorship_owner_memory_id: None,
            owner,
        };
        append_edge_in_tx(tx, &draft).await?;
        edge_ids.push(edge_id);
    }
    Ok(edge_ids)
}

async fn append_lifecycle_authored_edge(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    owner: &Owner,
    lifecycle_memory_id: MemoryId,
) -> Result<Option<uuid::Uuid>, StorageError> {
    let Some(self_id) = context.author_self_perspective_id else {
        return Ok(None);
    };
    let relation = resolve_relation(context, CORE_AUTHORED_RELATION)?;
    let edge_id = uuid::Uuid::now_v7();
    let draft = EdgeDraft {
        edge_id,
        relation,
        source_kind: EntityKind::Perspective,
        source_memory_id: Some(self_id.into_inner()),
        source_goal_id: None,
        source_fact_entity_id: None,
        target_kind: EntityKind::Fact,
        target_memory_id: Some(lifecycle_memory_id.into_inner()),
        target_goal_id: None,
        target_fact_entity_id: None,
        authorship_kind: EdgeAuthorshipKind::Engine,
        authorship_owner_memory_id: None,
        owner,
    };
    append_edge_in_tx(tx, &draft).await?;
    Ok(Some(edge_id))
}

async fn append_lifecycle_derived_from_edges(
    tx: &mut Transaction<'_, Postgres>,
    context: GoalAtomicContext<'_>,
    owner: &Owner,
    lifecycle_memory_id: MemoryId,
    evidence: &[EvidenceTarget],
) -> Result<Vec<uuid::Uuid>, StorageError> {
    let relation = resolve_relation(context, CORE_DERIVED_FROM_RELATION)?;
    let mut edge_ids = Vec::new();
    for target in evidence {
        if target.kind != EntityKind::Fact {
            continue;
        }
        let edge_id = uuid::Uuid::now_v7();
        let draft = EdgeDraft {
            edge_id,
            relation,
            source_kind: EntityKind::Fact,
            source_memory_id: Some(lifecycle_memory_id.into_inner()),
            source_goal_id: None,
            source_fact_entity_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(target.memory_id.into_inner()),
            target_goal_id: None,
            target_fact_entity_id: None,
            authorship_kind: EdgeAuthorshipKind::Engine,
            authorship_owner_memory_id: None,
            owner,
        };
        append_edge_in_tx(tx, &draft).await?;
        edge_ids.push(edge_id);
    }
    Ok(edge_ids)
}

fn resolve_relation<'a>(
    context: GoalAtomicContext<'a>,
    relation: &str,
) -> Result<RegisteredRelation<'a>, StorageError> {
    context.registry.resolve_relation(relation).ok_or_else(|| {
        StorageError::Internal(format!("relation {relation} not registered in goal atom"))
    })
}
