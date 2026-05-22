//! `InterventionStore` — Postgres data access for the wake-intervention
//! decision tool.
//!
//! Like `ApprovalStore`, `InterventionStore` is its own capability trait,
//! so its `PgStorage` impl lives here next to the verb bodies. The
//! `core/emit_intervention_decision` tool in `proxima-core` calls these
//! through `Storage`'s supertrait bound.
//!
//! `emit_intervention_decision_atomic` reuses [`ingest_event_in_tx`] and
//! [`append_edge_in_tx`] so the Fact materialization, the typed sidecar
//! row, and the provenance edges all land in one transaction.

use async_trait::async_trait;
use proxima_core::{
    CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION, EdgeAuthorshipKind,
    EmitInterventionDecisionInput, EntityKind, FlavorRegistryFrozen, InterventionDecisionEmitOutcome,
    InterventionDecisionV1, InterventionStore, LoadedInterventionRequest, MemoryId, Owner,
    OwnerPrincipalKind, Principal, StorageError, intervention_decision_fact_event_draft,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::PgStorage;
use crate::error::map_err;
use crate::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use crate::verbs::event_ingest::ingest_event_in_tx;

#[async_trait]
impl InterventionStore for PgStorage {
    async fn load_intervention_request(
        &self,
        owner: &Owner,
        memory_id: MemoryId,
    ) -> Result<Option<LoadedInterventionRequest>, StorageError> {
        load_intervention_request(self.pool(), owner, memory_id).await
    }

    async fn existing_intervention_decision(
        &self,
        owner: &Owner,
        intervention_request_memory_id: MemoryId,
        idempotency_key: &str,
    ) -> Result<Option<MemoryId>, StorageError> {
        existing_intervention_decision(
            self.pool(),
            owner,
            intervention_request_memory_id,
            idempotency_key,
        )
        .await
    }

    async fn is_intervention_supervisor(
        &self,
        owner: &Owner,
        caller_self: MemoryId,
        target_personality_instance_id: uuid::Uuid,
    ) -> Result<bool, StorageError> {
        is_intervention_supervisor(self.pool(), owner, caller_self, target_personality_instance_id)
            .await
    }

    async fn prior_continue_grant_rounds(
        &self,
        owner: &Owner,
        intervention_request_memory_id: MemoryId,
    ) -> Result<i64, StorageError> {
        prior_continue_grant_rounds(self.pool(), owner, intervention_request_memory_id).await
    }

    async fn emit_intervention_decision_atomic(
        &self,
        registry: &FlavorRegistryFrozen,
        input: &EmitInterventionDecisionInput,
    ) -> Result<InterventionDecisionEmitOutcome, StorageError> {
        emit_intervention_decision_atomic(self.pool(), registry, input).await
    }
}

fn owner_columns(owner: &Owner) -> (OwnerPrincipalKind, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(user) => (OwnerPrincipalKind::User, user.into_inner()),
        Principal::Group(group) => (OwnerPrincipalKind::Group, group.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

async fn load_intervention_request(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
) -> Result<Option<LoadedInterventionRequest>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let row: Option<(uuid::Uuid, uuid::Uuid, i32, i32)> = sqlx::query_as(
        "SELECT b.memory_id, b.target_intervention_personality_instance_id,
                b.intervention_extension_rounds, b.intervention_hard_cap_rounds
           FROM proxima_core.intervention_requested_v1 b
           JOIN proxima_core.memories m USING (memory_id)
          WHERE b.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.map(
        |(
            memory_id,
            target_intervention_personality_instance_id,
            intervention_extension_rounds,
            intervention_hard_cap_rounds,
        )| LoadedInterventionRequest {
            memory_id: MemoryId::new(memory_id),
            target_intervention_personality_instance_id,
            intervention_extension_rounds,
            intervention_hard_cap_rounds,
        },
    ))
}

async fn existing_intervention_decision(
    pool: &PgPool,
    owner: &Owner,
    intervention_request_memory_id: MemoryId,
    idempotency_key: &str,
) -> Result<Option<MemoryId>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let row: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT d.memory_id
           FROM proxima_core.intervention_decision_v1 d
           JOIN proxima_core.memories m USING (memory_id)
          WHERE d.intervention_request_memory_id = $1
            AND d.idempotency_key = $2
            AND m.owner_principal_kind = $3
            AND m.owner_principal_id = $4
            AND m.owner_org_id = $5",
    )
    .bind(intervention_request_memory_id.into_inner())
    .bind(idempotency_key)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.map(MemoryId::new))
}

async fn is_intervention_supervisor(
    pool: &PgPool,
    owner: &Owner,
    caller_self: MemoryId,
    target_personality_instance_id: uuid::Uuid,
) -> Result<bool, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let matched: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT personality_instance_id
           FROM proxima_core.personality
          WHERE current_root_perspective_memory_id = $1
            AND personality_instance_id = $2
            AND owner_principal_kind = $3
            AND owner_principal_id = $4
            AND owner_org_id = $5
            AND status = 'active'",
    )
    .bind(caller_self.into_inner())
    .bind(target_personality_instance_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(matched.is_some())
}

async fn prior_continue_grant_rounds(
    pool: &PgPool,
    owner: &Owner,
    intervention_request_memory_id: MemoryId,
) -> Result<i64, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let prior: Option<i64> = sqlx::query_scalar(
        "SELECT COALESCE(SUM(d.grant_rounds), 0)
           FROM proxima_core.intervention_decision_v1 d
           JOIN proxima_core.memories m USING (memory_id)
          WHERE d.intervention_request_memory_id = $1
            AND d.decision = 'continue'
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(intervention_request_memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_one(pool)
    .await
    .map_err(map_err)?;
    Ok(prior.unwrap_or(0))
}

async fn emit_intervention_decision_atomic(
    pool: &PgPool,
    registry: &FlavorRegistryFrozen,
    input: &EmitInterventionDecisionInput,
) -> Result<InterventionDecisionEmitOutcome, StorageError> {
    let draft = intervention_decision_fact_event_draft(&input.owner, &input.payload)?;
    let mut tx = pool.begin().await.map_err(map_err)?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    if !outcome.idempotent_replay {
        insert_decision_sidecar(&mut tx, outcome.memory_id, &input.payload).await?;
        let decision_memory_id = outcome.memory_id.into_inner();
        // Wake Supervisor `Self` Perspective authored the decision Fact.
        append_edge(
            &mut tx,
            registry,
            CORE_AUTHORED_RELATION,
            &input.owner,
            EntityKind::Perspective,
            input.caller_self.into_inner(),
            EntityKind::Fact,
            decision_memory_id,
            input.caller_self,
        )
        .await?;
        // The decision is derived from the intervention request it answers.
        append_edge(
            &mut tx,
            registry,
            CORE_DERIVED_FROM_RELATION,
            &input.owner,
            EntityKind::Fact,
            decision_memory_id,
            EntityKind::Fact,
            input.payload.intervention_request_memory_id,
            input.caller_self,
        )
        .await?;
    }
    tx.commit().await.map_err(map_err)?;
    Ok(InterventionDecisionEmitOutcome {
        memory_id: outcome.memory_id,
        idempotent_replay: outcome.idempotent_replay,
    })
}

#[allow(clippy::too_many_arguments)]
async fn append_edge(
    tx: &mut Transaction<'_, Postgres>,
    registry: &FlavorRegistryFrozen,
    relation_id: &str,
    owner: &Owner,
    source_kind: EntityKind,
    source_memory_id: uuid::Uuid,
    target_kind: EntityKind,
    target_memory_id: uuid::Uuid,
    authorship_owner: MemoryId,
) -> Result<(), StorageError> {
    let relation = registry.resolve_relation(relation_id).ok_or_else(|| {
        StorageError::Internal(format!("relation {relation_id} not registered"))
    })?;
    append_edge_in_tx(
        tx.as_mut(),
        &EdgeDraft {
            edge_id: uuid::Uuid::now_v7(),
            relation,
            source_kind,
            source_memory_id: Some(source_memory_id),
            source_goal_id: None,
            target_kind,
            target_memory_id: Some(target_memory_id),
            target_goal_id: None,
            authorship_kind: EdgeAuthorshipKind::ExternalAgent,
            authorship_owner_memory_id: Some(authorship_owner.into_inner()),
            owner,
        },
        None,
    )
    .await?;
    Ok(())
}

async fn insert_decision_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &InterventionDecisionV1,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO proxima_core.intervention_decision_v1
            (memory_id, intervention_request_memory_id, decision, grant_rounds,
             redirect_personality_instance_id, rationale, decided_at, idempotency_key)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
         ON CONFLICT (intervention_request_memory_id, idempotency_key) DO NOTHING",
    )
    .bind(memory_id.into_inner())
    .bind(payload.intervention_request_memory_id)
    .bind(payload.decision)
    .bind(payload.grant_rounds.map(i32::from))
    .bind(payload.redirect_personality_instance_id)
    .bind(&payload.rationale)
    .bind(payload.decided_at)
    .bind(&payload.idempotency_key)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}
