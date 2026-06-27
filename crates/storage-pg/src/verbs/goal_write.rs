//! Core Goal storage atoms.

use std::collections::HashSet;

use proxima_core::goal::payloads::{
    GoalAbandonedV1, GoalAchievedV1, GoalActivatedV1, GoalPausedV1,
};
use proxima_core::goal::relations::CORE_MOTIVATED_BY_RELATION;
use proxima_core::relation::{CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION};
use proxima_core::verbs::event_ingest::EventDraft;
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, DecomposedGoalOutcome, GoalAtomicContext, GoalAuthorship, GoalDraft,
    GoalEvidenceRef, GoalLifecycleFact, GoalPayloadWrite, GoalState, GoalWriteOutcome,
    ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    EdgeAuthorshipKind, EntityKind, FactPayload, GoalId, MemoryId, Owner, OwnerPrincipalKind,
    RegisteredRelation, SchemaId, SchemaVersion, SourceBatchId, SourceId, StorageError,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::authorship::{AuthorshipColumns, authorship_columns};
use crate::error::{internal, map_err};
use crate::sidecars::{PgSidecarKey, PgSidecarRegistryFrozen};
use crate::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use crate::verbs::entity_owner::insert_entity_owner_home;
use crate::verbs::event_ingest::ingest_event_in_tx;

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
    parent_goal_ids: Vec<GoalId>,
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
    model_id: Option<String>,
    prompt_version: Option<String>,
    personality_instance_id: Option<uuid::Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
struct EvidenceRow {
    kind: Option<EntityKind>,
    owner_principal_kind: Option<OwnerPrincipalKind>,
    owner_principal_id: Option<uuid::Uuid>,
}

#[derive(Debug, Clone, Copy)]
struct EvidenceTarget {
    kind: EntityKind,
    memory_id: MemoryId,
}

pub(crate) async fn create_goal_atomic(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    req: &CreateGoalAtomicRequest<'_>,
) -> Result<GoalWriteOutcome, StorageError> {
    let mut tx = pool.begin().await.map_err(internal)?;
    let evidence = validate_evidence_in_owner(&mut tx, &req.draft.owner(), &req.evidence).await?;
    let inserted = insert_or_replay_goal(&mut tx, sidecars, &req.draft, None, req.context).await?;
    let outcome = if inserted.idempotent_replay {
        ensure_create_goal_replay_side_effects_match(
            &mut tx,
            inserted.goal_id,
            req.target_self_perspective_id,
            &evidence,
            req.context.author_self_perspective_id,
            &req.draft.request_id,
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
                req.target_self_perspective_id,
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
                EdgeAuthorshipKind::ExternalAgent,
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
    let mut tx = pool.begin().await.map_err(internal)?;
    let prior = load_prior_goal(&mut tx, &req.owner, req.prior_goal_id).await?;
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
    )
    .await?;
    let lifecycle = GoalLifecycleFact::for_state(req.next_state);
    let outcome = lifecycle_outcome(&mut tx, &req.owner, req.context, inserted, lifecycle).await?;
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
    let prior = load_prior_goal(&mut tx, &req.owner, req.prior_goal_id).await?;
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
    )
    .await?;
    let outcome = if inserted.idempotent_replay {
        replay_goal_outcome(
            &mut tx,
            inserted,
            GoalLifecycleFact::Achieved,
            &[CORE_MOTIVATED_BY_RELATION, CORE_DERIVED_FROM_RELATION],
        )
        .await?
    } else {
        let lifecycle_memory_id = Some(
            emit_lifecycle_fact(
                &mut tx,
                req.context,
                &req.owner,
                inserted.goal_id,
                GoalLifecycleFact::Achieved,
            )
            .await?,
        );
        let mut edge_ids = Vec::new();
        edge_ids.extend(
            append_motivated_by_edges(
                &mut tx,
                req.context,
                &req.owner,
                inserted.goal_id,
                &evidence,
                EdgeAuthorshipKind::Engine,
            )
            .await?,
        );
        if let Some(lifecycle_id) = lifecycle_memory_id {
            if let Some(edge_id) =
                append_lifecycle_authored_edge(&mut tx, req.context, &req.owner, lifecycle_id)
                    .await?
            {
                edge_ids.push(edge_id);
            }
            edge_ids.extend(
                append_lifecycle_derived_from_edges(
                    &mut tx,
                    req.context,
                    &req.owner,
                    lifecycle_id,
                    &evidence,
                )
                .await?,
            );
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
    let draft = draft_from_payload(
        &req.owner,
        &req.replacement,
        GoalState::Active,
        prior.parent_goal_ids,
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
    )
    .await?;
    let outcome = if inserted.idempotent_replay {
        replay_goal_outcome(
            &mut tx,
            inserted,
            GoalLifecycleFact::Activated,
            &[CORE_MOTIVATED_BY_RELATION],
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
        edge_ids.extend(
            append_motivated_by_edges(
                &mut tx,
                req.context,
                &req.owner,
                inserted.goal_id,
                &evidence,
                EdgeAuthorshipKind::User,
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
        let draft = child_draft(&req.owner, req.parent_goal_id, &req.authorship, child);
        let inserted = insert_or_replay_goal(&mut tx, sidecars, &draft, None, req.context).await?;
        let outcome = if inserted.idempotent_replay {
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
                    req.target_self_perspective_id,
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
                    EdgeAuthorshipKind::ExternalAgent,
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
) -> Result<InsertedGoal, StorageError> {
    validate_goal_schema(context, draft)?;
    let owner = draft.owner();
    let (owner_kind, owner_principal_id) = owner.columns();
    let existing: Option<ExistingGoalRow> = sqlx::query_as(
        "SELECT g.goal_id, ce.seq
           FROM proxima_core.goals g
           JOIN proxima_core.change_event ce ON ce.entity_goal_id = g.goal_id
          WHERE g.idempotency_key = md5($1::text || ':' || $2::text || ':' || $3)
          ORDER BY ce.seq ASC
          LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(&draft.request_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;

    if let Some(existing) = existing {
        let body_matches =
            existing_goal_body_matches(tx, existing.goal_id, draft, expected_prior).await?;
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
    insert_goal_parents(tx, draft, goal_id).await?;
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

async fn ensure_create_goal_replay_side_effects_match(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
    target_self_perspective_id: MemoryId,
    evidence: &[EvidenceTarget],
    author_self_perspective_id: Option<MemoryId>,
    request_id: &str,
) -> Result<(), StorageError> {
    if !goal_self_assignment_matches(tx, goal_id, target_self_perspective_id).await? {
        return Err(idempotency_conflict(request_id));
    }
    if !goal_evidence_edges_match(tx, goal_id, evidence).await? {
        return Err(idempotency_conflict(request_id));
    }
    let Some(lifecycle_memory_id) =
        lifecycle_memory_for_goal(tx, goal_id, GoalLifecycleFact::Activated).await?
    else {
        return Err(idempotency_conflict(request_id));
    };
    if !lifecycle_author_edge_matches(tx, lifecycle_memory_id, author_self_perspective_id).await? {
        return Err(idempotency_conflict(request_id));
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
) -> Result<bool, StorageError> {
    let rows: Vec<(EntityKind, uuid::Uuid)> = sqlx::query_as(
        "SELECT target_kind, target_memory_id
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
        .map(|target| (target.kind, target.memory_id.into_inner()))
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
    let parents = parent_goal_ids(tx, GoalId::new(existing_goal_id)).await?;
    let existing_parents: HashSet<GoalId> = parents.into_iter().collect();
    let draft_parents: HashSet<GoalId> = draft.parent_goal_ids.iter().copied().collect();
    Ok(row.schema_id == draft.schema_id.as_str()
        && row.schema_version == draft.schema_version.into_inner().cast_signed()
        && row.title == draft.title
        && row.text == draft.text
        && row.payload == draft.payload
        && row.state == draft.state
        && row.supersedes == expected_prior.map(GoalId::into_inner)
        && existing_parents == draft_parents)
}

async fn authorship_matches(
    tx: &mut Transaction<'_, Postgres>,
    existing_goal_id: uuid::Uuid,
    authorship: &GoalAuthorship,
) -> Result<bool, StorageError> {
    let row: AuthorshipRow = sqlx::query_as(
        "SELECT authorship_kind, authorship_origin, authorship_operator_id,
                authorship_tool_id, operator_kind, model_id, prompt_version,
                personality_instance_id
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
        model_id: row.model_id,
        prompt_version: row.prompt_version,
        personality_instance_id: row.personality_instance_id,
    };
    Ok(existing == authorship_columns(authorship))
}

async fn insert_goal_row(
    tx: &mut Transaction<'_, Postgres>,
    draft: &GoalDraft,
    goal_id: uuid::Uuid,
    supersedes: Option<GoalId>,
) -> Result<(), StorageError> {
    let owner = draft.owner();
    let (owner_kind, owner_principal_id) = owner.columns();
    let authorship = authorship_columns(&draft.authorship);
    // NOTE: $4 (owner_kind) and $5 (owner_id) are bound but intentionally absent
    // from the column list — the goal row no longer stores its owner. They feed
    // ONLY the computed idempotency_key, whose formula MUST stay byte-identical
    // to the replay lookup and the 0007 backfill: md5(owner_kind || ':' ||
    // owner_id || ':' || request_id). Do not renumber without updating both.
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, schema_id, schema_version, title, text, payload, state, supersedes,
             authorship_kind, authorship_origin, authorship_operator_id,
             authorship_tool_id, operator_kind, model_id, prompt_version,
             personality_instance_id, request_id, idempotency_key)
         VALUES ($1, $2, $3, $6, $7, $8, $9, $10,
                 $11, $12, $13, $14, $15, $16, $17, $18, $19,
                 md5($4::text || ':' || $5::text || ':' || $19))",
    )
    .bind(goal_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(owner_kind)
    .bind(owner_principal_id)
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
    .bind(authorship.model_id)
    .bind(authorship.prompt_version)
    .bind(authorship.personality_instance_id)
    .bind(&draft.request_id)
    .execute(&mut **tx)
    .await
    .map_err(map_goal_insert_err)?;
    insert_entity_owner_home(tx, goal_id, &owner, authorship.personality_instance_id).await?;
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

async fn insert_goal_parents(
    tx: &mut Transaction<'_, Postgres>,
    draft: &GoalDraft,
    goal_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let owner = draft.owner();
    for parent_id in &draft.parent_goal_ids {
        validate_parent_owner(tx, &owner, *parent_id).await?;
        sqlx::query(
            "INSERT INTO proxima_core.goal_parents (goal_id, parent_goal_id)
             VALUES ($1, $2)",
        )
        .bind(goal_id)
        .bind(parent_id.into_inner())
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }
    Ok(())
}

async fn insert_goal_change_event(
    tx: &mut Transaction<'_, Postgres>,
    draft: &GoalDraft,
    goal_id: uuid::Uuid,
    change_seq: uuid::Uuid,
    supersedes_goal_id: Option<GoalId>,
) -> Result<(), StorageError> {
    let owner = draft.owner();
    let (owner_kind, owner_principal_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id,
             kind, entity_kind, entity_goal_id, entity_schema_id,
             entity_schema_version, supersedes_goal_id)
         VALUES ($1, $2, $3, 'EntityAppend', 'Goal', $4, $5, $6, $7)",
    )
    .bind(change_seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
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
    let (owner_kind, owner_principal_id) = owner.columns();
    let row: Option<StoredGoalRow> = sqlx::query_as(
        "SELECT schema_id, schema_version, title, text, payload, state
           FROM proxima_core.goals
          WHERE goal_id = $1
            AND EXISTS (
                SELECT 1
                  FROM proxima_core.entity_owner eo
                 WHERE eo.entity_id = goal_id
                   AND eo.owner_principal_kind = $2
                   AND eo.owner_principal_id = $3
                   AND eo.is_home
            )",
    )
    .bind(goal_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
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
        parent_goal_ids: parent_goal_ids(tx, goal_id).await?,
    })
}

async fn parent_goal_ids(
    tx: &mut Transaction<'_, Postgres>,
    goal_id: GoalId,
) -> Result<Vec<GoalId>, StorageError> {
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT parent_goal_id
           FROM proxima_core.goal_parents
          WHERE goal_id = $1
          ORDER BY parent_goal_id",
    )
    .bind(goal_id.into_inner())
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().map(|(id,)| GoalId::new(id)).collect())
}

async fn validate_parent_owner(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    parent_id: GoalId,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id) = owner.columns();
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.goals
              WHERE goal_id = $1
                AND EXISTS (
                    SELECT 1
                      FROM proxima_core.entity_owner eo
                     WHERE eo.entity_id = goal_id
                       AND eo.owner_principal_kind = $2
                       AND eo.owner_principal_id = $3
                       AND eo.is_home
                )
         )",
    )
    .bind(parent_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    if exists {
        Ok(())
    } else {
        Err(StorageError::ConstraintViolation(
            "goal parent crosses Owner boundary or does not exist".into(),
        ))
    }
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
    let (owner_kind, owner_principal_id) = owner.columns();
    let newer_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.goals
              WHERE supersedes = $1
                AND EXISTS (
                    SELECT 1
                      FROM proxima_core.entity_owner eo
                     WHERE eo.entity_id = goal_id
                       AND eo.owner_principal_kind = $2
                       AND eo.owner_principal_id = $3
                       AND eo.is_home
                )
         )",
    )
    .bind(goal_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
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

fn draft_from_stored(
    owner: &Owner,
    stored: &StoredGoal,
    state: GoalState,
    supersedes: Option<GoalId>,
    authorship: GoalAuthorship,
    request_id: &str,
) -> GoalDraft {
    GoalDraft {
        principal: owner.clone(),
        schema_id: stored.schema_id.clone(),
        schema_version: stored.schema_version,
        title: stored.title.clone(),
        text: stored.text.clone(),
        payload: stored.payload.clone(),
        sidecar_payload: None,
        state,
        parent_goal_ids: stored.parent_goal_ids.clone(),
        supersedes_goal_id: supersedes,
        authorship,
        request_id: request_id.to_string(),
    }
}

fn draft_from_payload(
    owner: &Owner,
    payload: &GoalPayloadWrite,
    state: GoalState,
    parent_goal_ids: Vec<GoalId>,
    supersedes: Option<GoalId>,
    authorship: GoalAuthorship,
    request_id: &str,
) -> GoalDraft {
    GoalDraft {
        principal: owner.clone(),
        schema_id: payload.schema_id.clone(),
        schema_version: payload.schema_version,
        title: payload.title.clone(),
        text: payload.text.clone(),
        payload: payload.payload.clone(),
        sidecar_payload: payload.sidecar_payload.clone(),
        state,
        parent_goal_ids,
        supersedes_goal_id: supersedes,
        authorship,
        request_id: request_id.to_string(),
    }
}

fn child_draft(
    owner: &Owner,
    parent_goal_id: GoalId,
    authorship: &GoalAuthorship,
    child: &ChildGoalDraft,
) -> GoalDraft {
    draft_from_payload(
        owner,
        &child.payload,
        GoalState::Active,
        vec![parent_goal_id],
        None,
        authorship.clone(),
        child.request_id.as_str(),
    )
}

async fn validate_evidence_in_owner(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    evidence: &[GoalEvidenceRef],
) -> Result<Vec<EvidenceTarget>, StorageError> {
    let (owner_kind, owner_principal_id) = owner.columns();
    let mut seen = HashSet::with_capacity(evidence.len());
    let mut out = Vec::with_capacity(evidence.len());
    for item in evidence {
        if !seen.insert(item.memory_id) {
            return Err(StorageError::ConstraintViolation(
                "duplicate goal evidence".into(),
            ));
        }
        let row: Option<EvidenceRow> = sqlx::query_as(
            "SELECT m.kind, home_owner.owner_principal_kind, home_owner.owner_principal_id
               FROM proxima_core.memories m
               LEFT JOIN proxima_core.entity_owner home_owner
                 ON home_owner.entity_id = m.memory_id
                AND home_owner.is_home
              WHERE m.memory_id = $1",
        )
        .bind(item.memory_id.into_inner())
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_err)?;
        let Some(row) = row else {
            return Err(StorageError::NotFound);
        };
        if let (Some(row_owner_kind), Some(row_owner_principal_id)) =
            (row.owner_principal_kind, row.owner_principal_id)
            && (row_owner_kind != owner_kind || row_owner_principal_id != owner_principal_id)
        {
            return Err(StorageError::ConstraintViolation(
                "evidence crosses Owner boundary".into(),
            ));
        }
        let kind = row.kind.unwrap_or(EntityKind::Fact);
        match kind {
            EntityKind::Fact | EntityKind::Abstraction => out.push(EvidenceTarget {
                kind,
                memory_id: item.memory_id,
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
    let (owner_kind, owner_principal_id) = owner.columns();
    let rows: Vec<(EntityKind, uuid::Uuid)> = sqlx::query_as(
        "SELECT target_kind, target_memory_id
           FROM proxima_core.edges e
          WHERE relation = $1
            AND source_goal_id = $2
            AND EXISTS (
                SELECT 1
                  FROM proxima_core.entity_owner eo
                 WHERE eo.entity_id = e.source_goal_id
                   AND eo.owner_principal_kind = $3
                   AND eo.owner_principal_id = $4
                   AND eo.is_home
            )
            AND target_memory_id IS NOT NULL
          ORDER BY created_at ASC",
    )
    .bind(CORE_MOTIVATED_BY_RELATION)
    .bind(goal_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
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
    let draft = EventDraft {
        source_id: SourceId::new(LIFECYCLE_SOURCE_ID),
        source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
        principal: owner.clone(),
        author_personality_instance_id: None,
        schema_id: SchemaId::new(T::SCHEMA_ID.to_string()),
        schema_version: SchemaVersion::new(T::SCHEMA_VERSION),
        payload: payload.event_key(),
        rendered_text: Some(payload.render()),
        observed_at: now,
        occurred_at: now,
        citation: None,
    };
    let outcome = ingest_event_in_tx(tx, &draft, context.embedding_model_id).await?;
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
    sqlx::query(&sql)
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
) -> Result<GoalWriteOutcome, StorageError> {
    if inserted.idempotent_replay {
        return replay_goal_outcome(tx, inserted, lifecycle, &[]).await;
    }
    let lifecycle_memory_id =
        Some(emit_lifecycle_fact(tx, context, owner, inserted.goal_id, lifecycle).await?);
    let mut edge_ids = Vec::new();
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
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(&sql)
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
        authorship_kind: EdgeAuthorshipKind::ExternalAgent,
        authorship_owner_memory_id: Some(self_memory_id.into_inner()),
        owner,
    };
    append_edge_in_tx(tx, &draft).await?;
    Ok(edge_id)
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
