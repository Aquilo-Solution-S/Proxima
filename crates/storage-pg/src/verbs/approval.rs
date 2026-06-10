//! `ApprovalStore` — Postgres data access for the core approval gate.
//!
//! Unlike the `Storage` verbs (whose `impl` lives in `lib.rs`),
//! `ApprovalStore` is its own capability trait, so its `PgStorage` impl
//! lives here next to the verb bodies. The approval tools in
//! `proxima-core` call these through `Storage`'s supertrait bound.
//!
//! The three `emit_*_atomic` verbs reuse [`ingest_event_in_tx`] and
//! [`append_edge_in_tx`] so the Fact materialization, the typed sidecar
//! row, and the provenance edges all land in one transaction.

use async_trait::async_trait;
use proxima_core::{
    ApprovalDecisionEmitOutcome, ApprovalDecisionV1, ApprovalPolicyEmitOutcome, ApprovalPolicyV1,
    ApprovalStore, ApprovalTargetKind, ApprovalTargetRef, ApprovalVoteEmitOutcome,
    ApprovalVoteRecord, ApprovalVoteV1, CORE_DERIVED_FROM_RELATION,
    CORE_HAS_APPROVAL_DECISION_RELATION, CORE_HAS_APPROVAL_POLICY_RELATION, CORE_VOTES_ON_RELATION,
    EdgeAuthorshipKind, EmitApprovalDecisionInput, EmitApprovalPolicyInput, EmitApprovalVoteInput,
    EntityKind, FlavorRegistryFrozen, MemoryId, Owner, OwnerPrincipalKind, Principal, StorageError,
    approval_fact_event_draft,
};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;

use crate::PgStorage;
use crate::error::map_err;
use crate::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use crate::verbs::event_ingest::ingest_event_in_tx;

#[async_trait]
impl ApprovalStore for PgStorage {
    async fn approval_target_kind(
        &self,
        owner: &Owner,
        target: &ApprovalTargetRef,
    ) -> Result<Option<String>, StorageError> {
        approval_target_kind(self.pool(), owner, target).await
    }

    async fn load_approval_policy(
        &self,
        owner: &Owner,
        policy_memory_id: MemoryId,
    ) -> Result<Option<ApprovalPolicyV1>, StorageError> {
        load_approval_policy(self.pool(), owner, policy_memory_id).await
    }

    async fn load_approval_votes(
        &self,
        owner: &Owner,
        policy_memory_id: MemoryId,
    ) -> Result<Vec<ApprovalVoteRecord>, StorageError> {
        load_approval_votes(self.pool(), owner, policy_memory_id).await
    }

    async fn emit_approval_policy_atomic(
        &self,
        registry: &FlavorRegistryFrozen,
        input: &EmitApprovalPolicyInput,
    ) -> Result<ApprovalPolicyEmitOutcome, StorageError> {
        emit_approval_policy_atomic(self.pool(), registry, input).await
    }

    async fn emit_approval_vote_atomic(
        &self,
        registry: &FlavorRegistryFrozen,
        input: &EmitApprovalVoteInput,
    ) -> Result<ApprovalVoteEmitOutcome, StorageError> {
        emit_approval_vote_atomic(self.pool(), registry, input).await
    }

    async fn emit_approval_decision_atomic(
        &self,
        registry: &FlavorRegistryFrozen,
        input: &EmitApprovalDecisionInput,
    ) -> Result<ApprovalDecisionEmitOutcome, StorageError> {
        emit_approval_decision_atomic(self.pool(), registry, input).await
    }
}

/// Decoded `approval_policy_v1` join row.
type ApprovalPolicyRow = (
    uuid::Uuid,
    ApprovalTargetKind,
    Option<uuid::Uuid>,
    Option<uuid::Uuid>,
    String,
    String,
    serde_json::Value,
    serde_json::Value,
    String,
    OffsetDateTime,
);

/// Decoded latest-per-voter `approval_vote_v1` join row.
type ApprovalVoteRow = (
    uuid::Uuid,
    uuid::Uuid,
    String,
    proxima_core::ApprovalVoterKind,
    Option<String>,
    Option<uuid::Uuid>,
    Option<uuid::Uuid>,
    Option<uuid::Uuid>,
    proxima_core::ApprovalVoteVerdict,
    String,
    String,
    OffsetDateTime,
);

fn owner_columns(owner: &Owner) -> (OwnerPrincipalKind, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(user) => (OwnerPrincipalKind::User, user.into_inner()),
        Principal::Group(group) => (OwnerPrincipalKind::Group, group.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

async fn approval_target_kind(
    pool: &PgPool,
    owner: &Owner,
    target: &ApprovalTargetRef,
) -> Result<Option<String>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    if target.kind == ApprovalTargetKind::Goal {
        let goal_id = target
            .goal_id
            .ok_or_else(|| StorageError::Internal("goal approval target missing goal_id".into()))?;
        let row: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT goal_id FROM proxima_core.goals
             WHERE goal_id = $1
               AND owner_principal_kind = $2
               AND owner_principal_id = $3
               AND owner_org_id = $4",
        )
        .bind(goal_id)
        .bind(owner_kind)
        .bind(owner_id)
        .bind(owner_org_id)
        .fetch_optional(pool)
        .await
        .map_err(map_err)?;
        return Ok(row.map(|_| "goal".to_string()));
    }
    let memory_id = target
        .memory_id
        .ok_or_else(|| StorageError::Internal("memory approval target missing memory_id".into()))?;
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT CASE WHEN event_id IS NOT NULL THEN 'Fact' ELSE kind::text END
           FROM proxima_core.memories
         WHERE memory_id = $1
           AND owner_principal_kind = $2
           AND owner_principal_id = $3
           AND owner_org_id = $4",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.map(|(kind,)| kind))
}

async fn load_approval_policy(
    pool: &PgPool,
    owner: &Owner,
    policy_memory_id: MemoryId,
) -> Result<Option<ApprovalPolicyV1>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let row: Option<ApprovalPolicyRow> = sqlx::query_as(
        "SELECT p.memory_id, p.target_kind, p.target_memory_id, p.target_goal_id,
                p.title, p.summary, p.eligible_voters_json, p.requirements_json,
                p.idempotency_key, p.created_at
           FROM proxima_core.approval_policy_v1 p
           JOIN proxima_core.memories m USING (memory_id)
          WHERE p.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4",
    )
    .bind(policy_memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    let Some((
        _,
        target_kind,
        target_memory_id,
        target_goal_id,
        title,
        summary,
        eligible_voters_json,
        requirements_json,
        idempotency_key,
        created_at,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(ApprovalPolicyV1 {
        target: ApprovalTargetRef {
            kind: target_kind,
            memory_id: target_memory_id,
            goal_id: target_goal_id,
        },
        title,
        summary,
        eligible_voters: serde_json::from_value(eligible_voters_json)
            .map_err(|err| StorageError::Internal(format!("decode eligible voters: {err}")))?,
        requirements: serde_json::from_value(requirements_json)
            .map_err(|err| StorageError::Internal(format!("decode requirements: {err}")))?,
        idempotency_key,
        created_at,
    }))
}

async fn load_approval_votes(
    pool: &PgPool,
    owner: &Owner,
    policy_memory_id: MemoryId,
) -> Result<Vec<ApprovalVoteRecord>, StorageError> {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let rows: Vec<ApprovalVoteRow> = sqlx::query_as(
        "SELECT DISTINCT ON (v.voter_key)
                v.memory_id, v.policy_memory_id, v.voter_key, v.voter_kind,
                v.role, v.personality_instance_id, v.self_perspective_memory_id,
                v.master_token_id, v.verdict, v.rationale, v.idempotency_key, v.voted_at
           FROM proxima_core.approval_vote_v1 v
           JOIN proxima_core.memories m USING (memory_id)
          WHERE v.policy_memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND m.owner_org_id = $4
          ORDER BY v.voter_key, v.voted_at DESC, v.memory_id DESC",
    )
    .bind(policy_memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner_org_id)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(
            |(
                memory_id,
                policy_memory_id,
                voter_key,
                voter_kind,
                role,
                personality_instance_id,
                self_perspective_memory_id,
                master_token_id,
                verdict,
                rationale,
                idempotency_key,
                voted_at,
            )| ApprovalVoteRecord {
                memory_id: MemoryId::new(memory_id),
                payload: ApprovalVoteV1 {
                    policy_memory_id,
                    voter_key,
                    voter_kind,
                    role,
                    personality_instance_id,
                    self_perspective_memory_id,
                    master_token_id,
                    verdict,
                    rationale,
                    idempotency_key,
                    voted_at,
                },
            },
        )
        .collect())
}

async fn emit_approval_policy_atomic(
    pool: &PgPool,
    registry: &FlavorRegistryFrozen,
    input: &EmitApprovalPolicyInput,
) -> Result<ApprovalPolicyEmitOutcome, StorageError> {
    let draft = approval_fact_event_draft(&input.owner, &input.payload)?;
    let mut tx = pool.begin().await.map_err(map_err)?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    let mut target_edge_id = None;
    if !outcome.idempotent_replay {
        insert_policy_sidecar(&mut tx, outcome.memory_id, &input.payload).await?;
        let (source_kind, source_memory_id, source_goal_id) =
            target_edge_source(&input.payload.target);
        target_edge_id = Some(
            append_edge(
                &mut tx,
                registry,
                CORE_HAS_APPROVAL_POLICY_RELATION,
                &input.owner,
                source_kind,
                source_memory_id,
                source_goal_id,
                EntityKind::Fact,
                Some(outcome.memory_id.into_inner()),
                None,
                input.edge_authorship,
                input.authorship_owner,
            )
            .await?,
        );
    }
    tx.commit().await.map_err(map_err)?;
    Ok(ApprovalPolicyEmitOutcome {
        memory_id: outcome.memory_id,
        target_edge_id,
        idempotent_replay: outcome.idempotent_replay,
    })
}

async fn emit_approval_vote_atomic(
    pool: &PgPool,
    registry: &FlavorRegistryFrozen,
    input: &EmitApprovalVoteInput,
) -> Result<ApprovalVoteEmitOutcome, StorageError> {
    let draft = approval_fact_event_draft(&input.owner, &input.payload)?;
    let mut tx = pool.begin().await.map_err(map_err)?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    let mut vote_edge_id = None;
    if !outcome.idempotent_replay {
        insert_vote_sidecar(&mut tx, outcome.memory_id, &input.payload).await?;
        vote_edge_id = Some(
            append_edge(
                &mut tx,
                registry,
                CORE_VOTES_ON_RELATION,
                &input.owner,
                EntityKind::Fact,
                Some(outcome.memory_id.into_inner()),
                None,
                EntityKind::Fact,
                Some(input.policy_memory_id.into_inner()),
                None,
                input.edge_authorship,
                input.authorship_owner,
            )
            .await?,
        );
    }
    tx.commit().await.map_err(map_err)?;
    Ok(ApprovalVoteEmitOutcome {
        memory_id: outcome.memory_id,
        vote_edge_id,
        idempotent_replay: outcome.idempotent_replay,
    })
}

async fn emit_approval_decision_atomic(
    pool: &PgPool,
    registry: &FlavorRegistryFrozen,
    input: &EmitApprovalDecisionInput,
) -> Result<ApprovalDecisionEmitOutcome, StorageError> {
    let draft = approval_fact_event_draft(&input.owner, &input.payload)?;
    let mut tx = pool.begin().await.map_err(map_err)?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    let mut edge_ids = Vec::new();
    if !outcome.idempotent_replay {
        insert_decision_sidecar(&mut tx, outcome.memory_id, &input.payload).await?;
        let decision_memory_id = outcome.memory_id.into_inner();
        let (source_kind, source_memory_id, source_goal_id) =
            target_edge_source(&input.payload.target);
        edge_ids.push(
            append_edge(
                &mut tx,
                registry,
                CORE_HAS_APPROVAL_DECISION_RELATION,
                &input.owner,
                source_kind,
                source_memory_id,
                source_goal_id,
                EntityKind::Fact,
                Some(decision_memory_id),
                None,
                EdgeAuthorshipKind::Engine,
                input.authorship_owner,
            )
            .await?,
        );
        edge_ids.push(
            append_edge(
                &mut tx,
                registry,
                CORE_DERIVED_FROM_RELATION,
                &input.owner,
                EntityKind::Fact,
                Some(decision_memory_id),
                None,
                EntityKind::Fact,
                Some(input.policy_memory_id.into_inner()),
                None,
                EdgeAuthorshipKind::Engine,
                input.authorship_owner,
            )
            .await?,
        );
        for vote in &input.payload.counted_votes {
            edge_ids.push(
                append_edge(
                    &mut tx,
                    registry,
                    CORE_DERIVED_FROM_RELATION,
                    &input.owner,
                    EntityKind::Fact,
                    Some(decision_memory_id),
                    None,
                    EntityKind::Fact,
                    Some(vote.vote_memory_id),
                    None,
                    EdgeAuthorshipKind::Engine,
                    input.authorship_owner,
                )
                .await?,
            );
        }
    }
    tx.commit().await.map_err(map_err)?;
    Ok(ApprovalDecisionEmitOutcome {
        memory_id: outcome.memory_id,
        edge_ids,
        idempotent_replay: outcome.idempotent_replay,
    })
}

/// The edge source-end for an approval target: a goal target sources
/// from `source_goal_id`, a memory target from `source_memory_id`.
fn target_edge_source(
    target: &ApprovalTargetRef,
) -> (EntityKind, Option<uuid::Uuid>, Option<uuid::Uuid>) {
    match target.kind {
        ApprovalTargetKind::Goal => (EntityKind::Goal, None, target.goal_id),
        _ => (target.kind.entity_kind(), target.memory_id, None),
    }
}

#[allow(clippy::too_many_arguments)]
async fn append_edge(
    tx: &mut Transaction<'_, Postgres>,
    registry: &FlavorRegistryFrozen,
    relation_id: &str,
    owner: &Owner,
    source_kind: EntityKind,
    source_memory_id: Option<uuid::Uuid>,
    source_goal_id: Option<uuid::Uuid>,
    target_kind: EntityKind,
    target_memory_id: Option<uuid::Uuid>,
    target_goal_id: Option<uuid::Uuid>,
    authorship_kind: EdgeAuthorshipKind,
    authorship_owner: Option<MemoryId>,
) -> Result<uuid::Uuid, StorageError> {
    let relation = registry
        .resolve_relation(relation_id)
        .ok_or_else(|| StorageError::Internal(format!("relation {relation_id} not registered")))?;
    let edge_id = uuid::Uuid::now_v7();
    append_edge_in_tx(
        tx.as_mut(),
        &EdgeDraft {
            edge_id,
            relation,
            source_kind,
            source_memory_id,
            source_goal_id,
            target_kind,
            target_memory_id,
            target_goal_id,
            authorship_kind,
            authorship_owner_memory_id: authorship_owner.map(MemoryId::into_inner),
            owner,
        },
        None,
    )
    .await?;
    Ok(edge_id)
}

async fn insert_policy_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ApprovalPolicyV1,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO proxima_core.approval_policy_v1
            (memory_id, target_kind, target_memory_id, target_goal_id,
             title, summary, eligible_voters_json, requirements_json,
             idempotency_key, created_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(memory_id.into_inner())
    .bind(payload.target.kind)
    .bind(payload.target.memory_id)
    .bind(payload.target.goal_id)
    .bind(&payload.title)
    .bind(&payload.summary)
    .bind(serde_json::to_value(&payload.eligible_voters).map_err(json_err)?)
    .bind(serde_json::to_value(&payload.requirements).map_err(json_err)?)
    .bind(&payload.idempotency_key)
    .bind(payload.created_at)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn insert_vote_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ApprovalVoteV1,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO proxima_core.approval_vote_v1
            (memory_id, policy_memory_id, voter_key, voter_kind, role,
             personality_instance_id, self_perspective_memory_id, master_token_id,
             verdict, rationale, idempotency_key, voted_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(memory_id.into_inner())
    .bind(payload.policy_memory_id)
    .bind(&payload.voter_key)
    .bind(payload.voter_kind)
    .bind(payload.role.as_deref())
    .bind(payload.personality_instance_id)
    .bind(payload.self_perspective_memory_id)
    .bind(payload.master_token_id)
    .bind(payload.verdict)
    .bind(&payload.rationale)
    .bind(&payload.idempotency_key)
    .bind(payload.voted_at)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn insert_decision_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    memory_id: MemoryId,
    payload: &ApprovalDecisionV1,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO proxima_core.approval_decision_v1
            (memory_id, policy_memory_id, target_kind, target_memory_id,
             target_goal_id, decision, reason, counted_votes_json,
             idempotency_key, decided_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(memory_id.into_inner())
    .bind(payload.policy_memory_id)
    .bind(payload.target.kind)
    .bind(payload.target.memory_id)
    .bind(payload.target.goal_id)
    .bind(payload.decision)
    .bind(&payload.reason)
    .bind(serde_json::to_value(&payload.counted_votes).map_err(json_err)?)
    .bind(&payload.idempotency_key)
    .bind(payload.decided_at)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// `map_err` hands ownership of the error, so by-value is required here.
#[allow(clippy::needless_pass_by_value)]
fn json_err(err: serde_json::Error) -> StorageError {
    StorageError::Internal(format!("serialize approval sidecar json: {err}"))
}
