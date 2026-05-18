//! Atomic `budget_review_requested` persistence.

use proxima_core::{
    BudgetReviewPersistInput, BudgetReviewPersistOutcome, CORE_AUTHORED_RELATION,
    CORE_DERIVED_FROM_RELATION, CORE_RECEIVES_BUDGET_REVIEW_RELATION, EdgeAuthorshipKind,
    EntityKind, FlavorRegistryFrozen, MemoryId, OwnerPrincipalKind, Principal, StorageError,
    budget_review_event_draft,
};
use sqlx::PgPool;

use crate::error::map_err;
use crate::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use crate::verbs::event_ingest::ingest_event_in_tx;

pub async fn persist_budget_review_requested_atomic(
    pool: &PgPool,
    registry: &FlavorRegistryFrozen,
    input: &BudgetReviewPersistInput,
) -> Result<BudgetReviewPersistOutcome, StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;

    let (owner_kind, owner_principal_id) = match &input.owner.principal {
        Principal::User(u) => (OwnerPrincipalKind::User, u.into_inner()),
        Principal::Group(g) => (OwnerPrincipalKind::Group, g.into_inner()),
    };
    let owner_org_id = input.owner.org_id.into_inner();

    if let Some(memory_id) = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT b.memory_id
           FROM proxima_core.budget_review_requested_v1 b
           JOIN proxima_core.memories m USING (memory_id)
          WHERE b.original_invocation_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(input.request.original_invocation_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?
    {
        let change_event_seq = sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT seq
               FROM proxima_core.change_event
              WHERE entity_memory_id = $1
              ORDER BY seq ASC
              LIMIT 1",
        )
        .bind(memory_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_err)?;
        tx.commit().await.map_err(map_err)?;
        return Ok(BudgetReviewPersistOutcome {
            memory_id: MemoryId::new(memory_id),
            change_event_seq,
            idempotent_replay: true,
        });
    }

    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(&input.request, &mut payload_bytes)
        .map_err(|err| StorageError::Internal(format!("serialize budget review: {err}")))?;
    let draft = budget_review_event_draft(
        input.owner.clone(),
        &payload_bytes,
        input.source_batch_id,
        input.source_id.clone(),
        input.request.requested_at,
    );
    let ingest = ingest_event_in_tx(&mut tx, &draft).await?;
    let memory_id = ingest.memory_id.into_inner();

    sqlx::query(
        "INSERT INTO proxima_core.budget_review_requested_v1
            (memory_id, original_invocation_id, original_wake_entry_id,
             original_personality_instance_id, original_change_event_seq,
             triggering_memory_id, wake_trace_memory_id,
             target_budgeter_personality_instance_id, max_rounds, rounds_used,
             budget_extension_rounds, budget_hard_cap_rounds, continued_rounds_used,
             active_goal_ids, progress_contract, requested_at, idempotency_key)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)
         ON CONFLICT (memory_id) DO NOTHING",
    )
    .bind(memory_id)
    .bind(input.request.original_invocation_id)
    .bind(input.request.original_wake_entry_id)
    .bind(input.request.original_personality_instance_id)
    .bind(input.request.original_change_event_seq)
    .bind(input.request.triggering_memory_id)
    .bind(input.request.wake_trace_memory_id)
    .bind(input.request.target_budgeter_personality_instance_id)
    .bind(i32::from(input.request.max_rounds))
    .bind(i32::from(input.request.rounds_used))
    .bind(i32::from(input.request.budget_extension_rounds))
    .bind(i32::from(input.request.budget_hard_cap_rounds))
    .bind(i32::from(input.request.continued_rounds_used))
    .bind(&input.request.active_goal_ids)
    .bind(&input.request.progress_contract)
    .bind(input.request.requested_at)
    .bind(&input.request.idempotency_key)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    let authored_relation = registry
        .resolve_relation(CORE_AUTHORED_RELATION)
        .ok_or_else(|| StorageError::Internal("missing core/authored relation".into()))?;
    append_edge_in_tx(
        tx.as_mut(),
        &EdgeDraft {
            edge_id: uuid::Uuid::now_v7(),
            relation: authored_relation,
            source_kind: EntityKind::Perspective,
            source_memory_id: Some(input.root_perspective_memory_id.into_inner()),
            source_goal_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(memory_id),
            target_goal_id: None,
            authorship_kind: EdgeAuthorshipKind::Engine,
            authorship_owner_memory_id: None,
            owner: &input.owner,
        },
        None,
    )
    .await?;

    let derived_relation = registry
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| StorageError::Internal("missing core/derived-from relation".into()))?;
    for target in [
        input.request.triggering_memory_id,
        input.request.wake_trace_memory_id,
    ] {
        append_edge_in_tx(
            tx.as_mut(),
            &EdgeDraft {
                edge_id: uuid::Uuid::now_v7(),
                relation: derived_relation,
                source_kind: EntityKind::Fact,
                source_memory_id: Some(memory_id),
                source_goal_id: None,
                target_kind: EntityKind::Fact,
                target_memory_id: Some(target),
                target_goal_id: None,
                authorship_kind: EdgeAuthorshipKind::Engine,
                authorship_owner_memory_id: None,
                owner: &input.owner,
            },
            None,
        )
        .await?;
    }

    let budgeter_root: uuid::Uuid = sqlx::query_scalar(
        "SELECT current_root_perspective_memory_id
           FROM proxima_core.personality
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND personality_instance_id = $4
            AND status = 'active'",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(input.request.target_budgeter_personality_instance_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_err)?;
    let budget_relation = registry
        .resolve_relation(CORE_RECEIVES_BUDGET_REVIEW_RELATION)
        .ok_or_else(|| {
            StorageError::Internal("missing core/receives-budget-review relation".into())
        })?;
    append_edge_in_tx(
        tx.as_mut(),
        &EdgeDraft {
            edge_id: uuid::Uuid::now_v7(),
            relation: budget_relation,
            source_kind: EntityKind::Perspective,
            source_memory_id: Some(budgeter_root),
            source_goal_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(memory_id),
            target_goal_id: None,
            authorship_kind: EdgeAuthorshipKind::Engine,
            authorship_owner_memory_id: None,
            owner: &input.owner,
        },
        None,
    )
    .await?;

    tx.commit().await.map_err(map_err)?;
    Ok(BudgetReviewPersistOutcome {
        memory_id: ingest.memory_id,
        change_event_seq: ingest.change_event_seq,
        idempotent_replay: ingest.idempotent_replay,
    })
}
