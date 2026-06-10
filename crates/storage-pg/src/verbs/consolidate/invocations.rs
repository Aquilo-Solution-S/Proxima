use proxima_core::personality::{
    ListWakeInvocationsRequest, PersonalityInstanceId, WakeInvocationFinalize,
    WakeInvocationLogDraft, WakeInvocationLogRow, WakeInvocationRow, WakeInvocationStart,
    WakeInvocationStatus,
};
use proxima_core::{
    CORE_DERIVED_FROM_RELATION, InterventionContinueCandidate, MemoryId, Owner, OwnerPrincipalKind,
    StorageError,
};
use sqlx::PgPool;
use sqlx::Row;

use super::parse::owner_from_parts;
use super::rows::owner_columns;
use crate::error::map_err;

pub async fn advance_wake_cursor(
    pool: &PgPool,
    owner: &Owner,
    instance: PersonalityInstanceId,
    last_considered_seq: uuid::Uuid,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    sqlx::query!(
        r"UPDATE proxima_core.personality_wake_cursor
         SET last_considered_seq = GREATEST(last_considered_seq, $1), updated_at = now()
         WHERE owner_principal_kind = $2
           AND owner_principal_id = $3
           AND owner_org_id = $4
           AND personality_instance_id = $5",
        last_considered_seq,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        instance.into_inner(),
    )
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

pub async fn try_begin_wake_invocation(
    pool: &PgPool,
    owner: &Owner,
    instance: PersonalityInstanceId,
    wake_entry_id: uuid::Uuid,
    change_event_seq: uuid::Uuid,
) -> Result<bool, StorageError> {
    start_wake_invocation(
        pool,
        &WakeInvocationStart {
            invocation_id: uuid::Uuid::now_v7(),
            owner: owner.clone(),
            personality_instance_id: instance,
            wake_entry_id,
            change_event_seq,
            wake_token: uuid::Uuid::nil(),
            resolved_inference_target_ref: String::new(),
            continuation: None,
        },
    )
    .await
}

pub async fn start_wake_invocation(
    pool: &PgPool,
    start: &WakeInvocationStart,
) -> Result<bool, StorageError> {
    let owner = &start.owner;
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let (continuation_intervention_decision_memory_id, continuation_original_invocation_id) = start
        .continuation
        .as_ref()
        .map_or((None, None), |continuation| {
            (
                Some(continuation.intervention_decision_memory_id),
                Some(continuation.original_invocation_id),
            )
        });
    let conflict_clause = if continuation_intervention_decision_memory_id.is_some() {
        "ON CONFLICT (continuation_intervention_decision_memory_id)
             WHERE continuation_intervention_decision_memory_id IS NOT NULL
         DO NOTHING"
    } else {
        "ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id,
                      personality_instance_id, wake_entry_id, change_event_seq)
             WHERE continuation_intervention_decision_memory_id IS NULL
         DO NOTHING"
    };
    let sql = format!(
        r"INSERT INTO proxima_core.personality_wake_invocations
            (owner_principal_kind, owner_principal_id, owner_org_id,
             personality_instance_id, wake_entry_id, change_event_seq, invocation_id,
             status, started_at, wake_token,
             resolved_inference_target_ref, continuation_intervention_decision_memory_id,
             continuation_original_invocation_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'running', now(), $8, $9, $10, $11)
         {conflict_clause}
         RETURNING invocation_id"
    );
    let inserted = sqlx::query_scalar::<_, uuid::Uuid>(&sql)
        .bind(owner_kind as OwnerPrincipalKind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(start.personality_instance_id.into_inner())
        .bind(start.wake_entry_id)
        .bind(start.change_event_seq)
        .bind(start.invocation_id)
        .bind(start.wake_token)
        .bind(&start.resolved_inference_target_ref)
        .bind(continuation_intervention_decision_memory_id)
        .bind(continuation_original_invocation_id)
        .fetch_optional(pool)
        .await
        .map_err(map_err)?;
    Ok(inserted.is_some())
}

#[allow(clippy::too_many_arguments)]
pub async fn finish_wake_invocation(
    pool: &PgPool,
    owner: &Owner,
    instance: PersonalityInstanceId,
    wake_entry_id: uuid::Uuid,
    change_event_seq: uuid::Uuid,
    status: WakeInvocationStatus,
    turn_count: u16,
    cost_usd: f64,
) -> Result<(), StorageError> {
    finalize_wake_invocation(
        pool,
        &WakeInvocationFinalize {
            owner: owner.clone(),
            personality_instance_id: instance,
            wake_entry_id,
            change_event_seq,
            status,
            turn_count: Some(turn_count),
            cost_usd: Some(cost_usd),
            failure_reason: None,
            exit_code: None,
            duration_ms: None,
            stdout_tail: None,
            stderr_tail: None,
            stdout_truncated: false,
            stderr_truncated: false,
            invocation_id: uuid::Uuid::nil(),
        },
    )
    .await
}

pub async fn finalize_wake_invocation(
    pool: &PgPool,
    finalize: &WakeInvocationFinalize,
) -> Result<(), StorageError> {
    let owner = &finalize.owner;
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    sqlx::query(
        r"UPDATE proxima_core.personality_wake_invocations
         SET status = $1,
             finished_at = now(),
             turn_count = COALESCE($2, turn_count),
             cost_usd = COALESCE($3::float8, cost_usd),
             failure_reason = $4,
             exit_code = $5,
             duration_ms = $6,
             stdout_tail = $7,
             stderr_tail = $8,
             stdout_truncated = $9,
             stderr_truncated = $10
         WHERE owner_principal_kind = $11
           AND owner_principal_id = $12
           AND owner_org_id = $13
           AND personality_instance_id = $14
           AND wake_entry_id = $15
           AND change_event_seq = $16
           AND ($17::uuid = '00000000-0000-0000-0000-000000000000'::uuid OR invocation_id = $17)",
    )
    .bind(finalize.status)
    .bind(finalize.turn_count.map(i32::from))
    .bind(finalize.cost_usd)
    .bind(&finalize.failure_reason)
    .bind(finalize.exit_code)
    .bind(finalize.duration_ms.and_then(|v| i64::try_from(v).ok()))
    .bind(&finalize.stdout_tail)
    .bind(&finalize.stderr_tail)
    .bind(finalize.stdout_truncated)
    .bind(finalize.stderr_truncated)
    .bind(owner_kind as OwnerPrincipalKind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(finalize.personality_instance_id.into_inner())
    .bind(finalize.wake_entry_id)
    .bind(finalize.change_event_seq)
    .bind(finalize.invocation_id)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

pub async fn append_wake_invocation_log(
    pool: &PgPool,
    log: &WakeInvocationLogDraft,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&log.owner);
    sqlx::query(
        "INSERT INTO proxima_core.personality_wake_invocation_logs
            (owner_principal_kind, owner_principal_id, owner_org_id,
             personality_instance_id, wake_entry_id, change_event_seq, invocation_id,
             phase, tool_id, status, duration_ms, message_tail)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(owner_kind as OwnerPrincipalKind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(log.personality_instance_id.into_inner())
    .bind(log.wake_entry_id)
    .bind(log.change_event_seq)
    .bind(log.invocation_id)
    .bind(&log.phase)
    .bind(&log.tool_id)
    .bind(log.status)
    .bind(log.duration_ms.and_then(|v| i64::try_from(v).ok()))
    .bind(&log.message_tail)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

pub async fn list_wake_invocations(
    pool: &PgPool,
    req: &ListWakeInvocationsRequest,
) -> Result<Vec<WakeInvocationRow>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    let limit = i64::from(req.limit.clamp(1, 100));
    let rows = sqlx::query(
        r"SELECT i.owner_principal_kind,
                  i.owner_principal_id, i.owner_org_id,
                  i.invocation_id, i.personality_instance_id, i.wake_entry_id,
                  e.label AS wake_entry_label,
                  i.change_event_seq,
                  i.status,
                  i.started_at, i.finished_at,
                  i.turn_count, i.cost_usd::float8 AS cost_usd,
                  i.resolved_inference_target_ref, i.failure_reason,
                  i.exit_code, i.duration_ms, i.stdout_tail, i.stderr_tail,
                  i.stdout_truncated, i.stderr_truncated,
                  i.continuation_intervention_decision_memory_id,
                  i.continuation_original_invocation_id
             FROM proxima_core.personality_wake_invocations i
             JOIN proxima_core.personality_wake_entries e
               ON e.owner_principal_kind = i.owner_principal_kind
              AND e.owner_principal_id = i.owner_principal_id
              AND e.owner_org_id = i.owner_org_id
              AND e.personality_instance_id = i.personality_instance_id
              AND e.wake_entry_id = i.wake_entry_id
             WHERE i.owner_principal_kind = $1
               AND i.owner_principal_id = $2
               AND i.owner_org_id = $3
               AND i.personality_instance_id = $4
               AND ($5::uuid IS NULL OR i.wake_entry_id = $5)
               AND ($6::uuid IS NULL OR i.change_event_seq = $6)
               AND ($7::uuid IS NULL OR EXISTS (
                    SELECT 1
                      FROM proxima_core.change_event c
                     WHERE c.owner_principal_kind = i.owner_principal_kind
                       AND c.owner_principal_id = i.owner_principal_id
                       AND c.owner_org_id = i.owner_org_id
                       AND c.seq = i.change_event_seq
                       AND c.entity_memory_id = $7
               ))
             ORDER BY i.started_at DESC
             LIMIT $8",
    )
    .bind(owner_kind as OwnerPrincipalKind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(req.personality_instance_id.into_inner())
    .bind(req.wake_entry_id)
    .bind(req.change_event_seq)
    .bind(
        req.triggering_memory_id
            .map(proxima_core::MemoryId::into_inner),
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let logs = fetch_invocation_logs(pool, row.get("invocation_id")).await?;
        let owner_principal_kind: OwnerPrincipalKind = row.get("owner_principal_kind");
        let owner_principal_id: uuid::Uuid = row.get("owner_principal_id");
        let owner_org_id: uuid::Uuid = row.get("owner_org_id");
        let personality_instance_id: uuid::Uuid = row.get("personality_instance_id");
        let wake_entry_id: uuid::Uuid = row.get("wake_entry_id");
        let change_event_seq: uuid::Uuid = row.get("change_event_seq");
        out.push(WakeInvocationRow {
            invocation_id: row.get("invocation_id"),
            owner: owner_from_parts(owner_principal_kind, owner_principal_id, owner_org_id),
            personality_instance_id: PersonalityInstanceId::new(personality_instance_id),
            wake_entry_id,
            wake_entry_label: row.get("wake_entry_label"),
            change_event_seq,
            status: row.get("status"),
            started_at: row.get("started_at"),
            finished_at: row.get("finished_at"),
            turn_count: u16::try_from(row.get::<i32, _>("turn_count")).unwrap_or(0),
            cost_usd: row.get("cost_usd"),
            resolved_inference_target_ref: row.get("resolved_inference_target_ref"),
            failure_reason: row.get("failure_reason"),
            exit_code: row.get("exit_code"),
            duration_ms: row
                .get::<Option<i64>, _>("duration_ms")
                .and_then(|v| u64::try_from(v).ok()),
            stdout_tail: row.get("stdout_tail"),
            stderr_tail: row.get("stderr_tail"),
            stdout_truncated: row.get("stdout_truncated"),
            stderr_truncated: row.get("stderr_truncated"),
            continuation_intervention_decision_memory_id: row
                .get("continuation_intervention_decision_memory_id"),
            continuation_original_invocation_id: row.get("continuation_original_invocation_id"),
            logs,
        });
    }
    Ok(out)
}

async fn fetch_invocation_logs(
    pool: &PgPool,
    invocation_id: uuid::Uuid,
) -> Result<Vec<WakeInvocationLogRow>, StorageError> {
    let rows = sqlx::query(
        r"SELECT log_seq, at, phase, tool_id,
                  status,
                  duration_ms, message_tail
             FROM proxima_core.personality_wake_invocation_logs
             WHERE invocation_id = $1
             ORDER BY log_seq ASC",
    )
    .bind(invocation_id)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(|log| WakeInvocationLogRow {
            log_seq: i64::from(log.get::<i32, _>("log_seq")),
            at: log.get("at"),
            phase: log.get("phase"),
            tool_id: log.get("tool_id"),
            status: log.get("status"),
            duration_ms: log
                .get::<Option<i64>, _>("duration_ms")
                .and_then(|v| u64::try_from(v).ok()),
            message_tail: log.get("message_tail"),
        })
        .collect())
}

pub async fn load_intervention_continue_candidate(
    pool: &PgPool,
    owner: &Owner,
    decision_memory_id: MemoryId,
) -> Result<Option<InterventionContinueCandidate>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let row = sqlx::query(
        r"SELECT d.memory_id AS decision_memory_id,
                  d.intervention_request_memory_id,
                  r.original_invocation_id,
                  r.original_wake_entry_id,
                  r.original_personality_instance_id,
                  r.original_change_event_seq,
                  r.triggering_memory_id,
                  r.wake_trace_memory_id,
                  d.grant_rounds,
                  d.rationale
             FROM proxima_core.intervention_decision_v1 d
             JOIN proxima_core.memories dm
               ON dm.memory_id = d.memory_id
             JOIN proxima_core.intervention_requested_v1 r
               ON r.memory_id = d.intervention_request_memory_id
             JOIN proxima_core.edges e
               ON e.relation = $5
              AND e.source_memory_id = d.memory_id
              AND e.target_memory_id = r.memory_id
              AND e.owner_principal_kind = dm.owner_principal_kind
              AND e.owner_principal_id = dm.owner_principal_id
              AND e.owner_org_id = dm.owner_org_id
            WHERE d.memory_id = $1
              AND d.decision = 'continue'
              AND d.grant_rounds IS NOT NULL
              AND dm.owner_principal_kind = $2
              AND dm.owner_principal_id = $3
              AND dm.owner_org_id = $4",
    )
    .bind(decision_memory_id.into_inner())
    .bind(owner_kind as OwnerPrincipalKind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(CORE_DERIVED_FROM_RELATION)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    row.map(|row| {
        let grant_rounds = row
            .get::<i32, _>("grant_rounds")
            .try_into()
            .map_err(|_| StorageError::Internal("grant_rounds outside u16 range".into()))?;
        Ok(InterventionContinueCandidate {
            intervention_decision_memory_id: MemoryId::new(row.get("decision_memory_id")),
            intervention_request_memory_id: MemoryId::new(
                row.get("intervention_request_memory_id"),
            ),
            original_invocation_id: row.get("original_invocation_id"),
            original_wake_entry_id: row.get("original_wake_entry_id"),
            original_personality_instance_id: PersonalityInstanceId::new(
                row.get("original_personality_instance_id"),
            ),
            original_change_event_seq: row.get("original_change_event_seq"),
            original_triggering_memory_id: MemoryId::new(row.get("triggering_memory_id")),
            wake_trace_memory_id: MemoryId::new(row.get("wake_trace_memory_id")),
            grant_rounds,
            rationale: row.get("rationale"),
        })
    })
    .transpose()
}
