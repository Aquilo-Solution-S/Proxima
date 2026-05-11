use proxima_core::personality::{
    ListWakeInvocationsRequest, PersonalityInstanceId, WakeInvocationFinalize,
    WakeInvocationLogDraft, WakeInvocationLogRow, WakeInvocationRow, WakeInvocationStart,
    WakeInvocationStatus,
};
use proxima_core::{Owner, StorageError};
use sqlx::PgPool;

use super::parse::{owner_from_parts, parse_wake_invocation_status};
use super::rows::owner_columns;
use crate::error::map_err;

pub async fn advance_wake_cursor(
    pool: &PgPool,
    owner: &Owner,
    instance: PersonalityInstanceId,
    last_considered_seq: uuid::Uuid,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    sqlx::query(
        "UPDATE proxima_core.personality_wake_cursor
         SET last_considered_seq = GREATEST(last_considered_seq, $1), updated_at = now()
         WHERE owner_principal_kind = $2
           AND owner_principal_id = $3
           AND owner_org_id = $4
           AND personality_instance_id = $5",
    )
    .bind(last_considered_seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(instance.into_inner())
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
            owner: owner.clone(),
            personality_instance_id: instance,
            wake_entry_id,
            change_event_seq,
            wake_token: uuid::Uuid::nil(),
            recipe_sha256: String::new(),
            resolved_inference_target_ref: String::new(),
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
    let inserted: Option<(uuid::Uuid,)> = sqlx::query_as(
        "INSERT INTO proxima_core.personality_wake_invocations
            (owner_principal_kind, owner_principal_id, owner_org_id,
             personality_instance_id, wake_entry_id, change_event_seq,
             status, started_at, wake_token, recipe_sha256,
             resolved_inference_target_ref)
         VALUES ($1, $2, $3, $4, $5, $6, 'running', now(), $7, $8, $9)
         ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id,
                      personality_instance_id, wake_entry_id, change_event_seq)
         DO NOTHING
         RETURNING change_event_seq",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(start.personality_instance_id.into_inner())
    .bind(start.wake_entry_id)
    .bind(start.change_event_seq)
    .bind(start.wake_token)
    .bind(&start.recipe_sha256)
    .bind(&start.resolved_inference_target_ref)
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
        "UPDATE proxima_core.personality_wake_invocations
         SET status = $1,
             finished_at = now(),
             turn_count = COALESCE($2, turn_count),
             cost_usd = COALESCE($3, cost_usd),
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
           AND change_event_seq = $16",
    )
    .bind(finalize.status.as_str())
    .bind(finalize.turn_count.map(i32::from))
    .bind(finalize.cost_usd)
    .bind(&finalize.failure_reason)
    .bind(finalize.exit_code)
    .bind(finalize.duration_ms.and_then(|v| i64::try_from(v).ok()))
    .bind(&finalize.stdout_tail)
    .bind(&finalize.stderr_tail)
    .bind(finalize.stdout_truncated)
    .bind(finalize.stderr_truncated)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(finalize.personality_instance_id.into_inner())
    .bind(finalize.wake_entry_id)
    .bind(finalize.change_event_seq)
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
             personality_instance_id, wake_entry_id, change_event_seq,
             phase, tool_id, status, duration_ms, message_tail)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(log.personality_instance_id.into_inner())
    .bind(log.wake_entry_id)
    .bind(log.change_event_seq)
    .bind(&log.phase)
    .bind(&log.tool_id)
    .bind(&log.status)
    .bind(log.duration_ms.and_then(|v| i64::try_from(v).ok()))
    .bind(&log.message_tail)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct WakeInvocationRowDb {
    owner_principal_kind: String,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
    personality_instance_id: uuid::Uuid,
    wake_entry_id: uuid::Uuid,
    wake_entry_label: String,
    change_event_seq: uuid::Uuid,
    status: String,
    started_at: time::OffsetDateTime,
    finished_at: Option<time::OffsetDateTime>,
    turn_count: i32,
    cost_usd: f64,
    recipe_sha256: Option<String>,
    resolved_inference_target_ref: Option<String>,
    failure_reason: Option<String>,
    exit_code: Option<i32>,
    duration_ms: Option<i64>,
    stdout_tail: Option<String>,
    stderr_tail: Option<String>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

#[derive(sqlx::FromRow)]
struct WakeInvocationLogRowDb {
    log_seq: i32,
    at: time::OffsetDateTime,
    phase: String,
    tool_id: Option<String>,
    status: String,
    duration_ms: Option<i64>,
    message_tail: Option<String>,
}

pub async fn list_wake_invocations(
    pool: &PgPool,
    req: &ListWakeInvocationsRequest,
) -> Result<Vec<WakeInvocationRow>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    let limit = i64::from(req.limit.clamp(1, 100));
    let rows: Vec<WakeInvocationRowDb> = sqlx::query_as(
        "SELECT i.owner_principal_kind, i.owner_principal_id, i.owner_org_id,
                i.personality_instance_id, i.wake_entry_id, e.label AS wake_entry_label,
                i.change_event_seq, i.status, i.started_at, i.finished_at,
                i.turn_count, i.cost_usd::float8 AS cost_usd, i.recipe_sha256,
                i.resolved_inference_target_ref, i.failure_reason,
                i.exit_code, i.duration_ms, i.stdout_tail, i.stderr_tail,
                i.stdout_truncated, i.stderr_truncated
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
         ORDER BY i.started_at DESC
         LIMIT $6",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(req.personality_instance_id.into_inner())
    .bind(req.wake_entry_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let logs: Vec<WakeInvocationLogRowDb> = sqlx::query_as(
            "SELECT log_seq, at, phase, tool_id, status, duration_ms, message_tail
             FROM proxima_core.personality_wake_invocation_logs
             WHERE owner_principal_kind = $1
               AND owner_principal_id = $2
               AND owner_org_id = $3
               AND personality_instance_id = $4
               AND wake_entry_id = $5
               AND change_event_seq = $6
             ORDER BY log_seq ASC",
        )
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(row.personality_instance_id)
        .bind(row.wake_entry_id)
        .bind(row.change_event_seq)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;
        out.push(WakeInvocationRow {
            owner: owner_from_parts(
                &row.owner_principal_kind,
                row.owner_principal_id,
                row.owner_org_id,
            ),
            personality_instance_id: PersonalityInstanceId::new(row.personality_instance_id),
            wake_entry_id: row.wake_entry_id,
            wake_entry_label: row.wake_entry_label,
            change_event_seq: row.change_event_seq,
            status: parse_wake_invocation_status(&row.status),
            started_at: row.started_at,
            finished_at: row.finished_at,
            turn_count: u16::try_from(row.turn_count).unwrap_or(0),
            cost_usd: row.cost_usd,
            recipe_sha256: row.recipe_sha256,
            resolved_inference_target_ref: row.resolved_inference_target_ref,
            failure_reason: row.failure_reason,
            exit_code: row.exit_code,
            duration_ms: row.duration_ms.and_then(|v| u64::try_from(v).ok()),
            stdout_tail: row.stdout_tail,
            stderr_tail: row.stderr_tail,
            stdout_truncated: row.stdout_truncated,
            stderr_truncated: row.stderr_truncated,
            logs: logs
                .into_iter()
                .map(|log| WakeInvocationLogRow {
                    log_seq: i64::from(log.log_seq),
                    at: log.at,
                    phase: log.phase,
                    tool_id: log.tool_id,
                    status: log.status,
                    duration_ms: log.duration_ms.and_then(|v| u64::try_from(v).ok()),
                    message_tail: log.message_tail,
                })
                .collect(),
        });
    }
    Ok(out)
}
