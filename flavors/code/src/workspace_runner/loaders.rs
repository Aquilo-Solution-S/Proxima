use proxima_core::{
    CORE_DERIVED_FROM_RELATION, FactPayload, MemoryId, Owner, WorkspaceRunnerError,
};
use serde::de::DeserializeOwned;
use serde_json::json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::payloads::{ExecutionRequestV1, WorkspaceDecisionV1, WorkspaceReviewV1, WorkspaceRunV1};
use crate::repos::owner_columns_pub;

use super::RunnerRepoRow;

pub(super) fn parse_payload<T: DeserializeOwned>(
    payload: &serde_json::Value,
    schema_id: &str,
) -> Result<T, WorkspaceRunnerError> {
    serde_json::from_value(payload.clone()).map_err(|err| {
        WorkspaceRunnerError::PrepareFailed(format!("decode {schema_id} payload: {err}"))
    })
}

#[derive(Debug, Clone)]
pub(super) struct LoadedExecutionRequest {
    pub(super) memory_id: MemoryId,
    payload: ExecutionRequestV1,
}

impl LoadedExecutionRequest {
    pub(super) fn to_json(&self) -> serde_json::Value {
        json!({
            "memory_id": self.memory_id.into_inner().to_string(),
            "payload": self.payload,
        })
    }
}

pub(super) async fn load_execution_request(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
) -> Result<LoadedExecutionRequest, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let row = sqlx::query(
        "SELECT COALESCE(m.kind, 'Fact') AS kind,
                m.schema_id,
                r.repo_id,
                r.title,
                r.instructions,
                r.request_key
         FROM proxima_core.memories m
         LEFT JOIN proxima_code.execution_request_v1 r USING (memory_id)
         WHERE m.memory_id = $1
           AND m.owner_principal_kind = $2
           AND m.owner_principal_id = $3",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("load execution request: {err}")))?;
    let Some(row) = row else {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "execution request not found: {}",
            memory_id.into_inner()
        )));
    };
    let kind: String = row.try_get("kind").map_err(map_sqlx_internal)?;
    let schema_id: String = row.try_get("schema_id").map_err(map_sqlx_internal)?;
    if kind != "Fact" || schema_id != ExecutionRequestV1::SCHEMA_ID {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "memory {} is not an execution request",
            memory_id.into_inner()
        )));
    }
    let repo_id: Option<Uuid> = row.try_get("repo_id").map_err(map_sqlx_internal)?;
    let title: Option<String> = row.try_get("title").map_err(map_sqlx_internal)?;
    let instructions: Option<String> = row.try_get("instructions").map_err(map_sqlx_internal)?;
    let request_key: Option<String> = row.try_get("request_key").map_err(map_sqlx_internal)?;
    let (Some(repo_id), Some(title), Some(instructions), Some(request_key)) =
        (repo_id, title, instructions, request_key)
    else {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "execution request sidecar missing: {}",
            memory_id.into_inner()
        )));
    };
    Ok(LoadedExecutionRequest {
        memory_id,
        payload: ExecutionRequestV1 {
            repo_id,
            title,
            instructions,
            request_key,
        },
    })
}

#[derive(Debug, Clone)]
pub(super) struct LoadedWorkspaceRun {
    pub(super) memory_id: MemoryId,
    payload: WorkspaceRunV1,
}

impl std::ops::Deref for LoadedWorkspaceRun {
    type Target = WorkspaceRunV1;

    fn deref(&self) -> &Self::Target {
        &self.payload
    }
}

pub(super) async fn load_execution_request_for_run(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
) -> Result<LoadedExecutionRequest, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let request_id: Option<Uuid> = sqlx::query_scalar(
        "WITH RECURSIVE ancestry(memory_id, depth, path) AS (
             SELECT e.target_memory_id, 1, ARRAY[$4::uuid, e.target_memory_id]
             FROM proxima_core.edges e
             WHERE e.owner_principal_kind = $1
               AND e.owner_principal_id = $2
               AND e.relation = $3
               AND e.source_kind = 'Fact'
               AND e.source_memory_id = $4
               AND e.target_kind = 'Fact'
               AND e.target_memory_id IS NOT NULL
             UNION ALL
             SELECT e.target_memory_id, a.depth + 1, a.path || e.target_memory_id
             FROM ancestry a
             JOIN proxima_core.edges e
               ON e.owner_principal_kind = $1
              AND e.owner_principal_id = $2
              AND e.relation = $3
              AND e.source_kind = 'Fact'
              AND e.source_memory_id = a.memory_id
              AND e.target_kind = 'Fact'
              AND e.target_memory_id IS NOT NULL
             WHERE NOT e.target_memory_id = ANY(a.path)
         )
         SELECT a.memory_id
         FROM ancestry a
         JOIN proxima_core.memories m
           ON m.memory_id = a.memory_id
          AND m.owner_principal_kind = $1
          AND m.owner_principal_id = $2
         WHERE m.schema_id = $5
         ORDER BY a.depth DESC, a.memory_id DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(run_memory_id.into_inner())
    .bind(ExecutionRequestV1::SCHEMA_ID)
    .fetch_optional(pool)
    .await
    .map_err(|err| {
        WorkspaceRunnerError::Internal(format!("find execution request for run: {err}"))
    })?;
    let Some(request_id) = request_id else {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "workspace run has no derived-from execution request: {}",
            run_memory_id.into_inner()
        )));
    };
    load_execution_request(pool, owner, MemoryId::new(request_id)).await
}

pub(super) async fn load_continuation_workspace_run_for_request(
    pool: &PgPool,
    owner: &Owner,
    request_memory_id: MemoryId,
) -> Result<Option<LoadedWorkspaceRun>, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let run_id: Option<Uuid> = sqlx::query_scalar(
        "WITH RECURSIVE ancestry(memory_id, depth, path) AS (
             SELECT e.target_memory_id, 1, ARRAY[$4::uuid, e.target_memory_id]
             FROM proxima_core.edges e
             WHERE e.owner_principal_kind = $1
               AND e.owner_principal_id = $2
               AND e.relation = $3
               AND e.source_kind = 'Fact'
               AND e.source_memory_id = $4
               AND e.target_kind = 'Fact'
               AND e.target_memory_id IS NOT NULL
             UNION ALL
             SELECT e.target_memory_id, a.depth + 1, a.path || e.target_memory_id
             FROM ancestry a
             JOIN proxima_core.edges e
               ON e.owner_principal_kind = $1
              AND e.owner_principal_id = $2
              AND e.relation = $3
              AND e.source_kind = 'Fact'
              AND e.source_memory_id = a.memory_id
              AND e.target_kind = 'Fact'
              AND e.target_memory_id IS NOT NULL
             WHERE NOT e.target_memory_id = ANY(a.path)
         )
         SELECT a.memory_id
         FROM ancestry a
         JOIN proxima_core.memories m
           ON m.memory_id = a.memory_id
          AND m.owner_principal_kind = $1
          AND m.owner_principal_id = $2
         WHERE m.schema_id = $5
         ORDER BY a.depth, a.memory_id DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(request_memory_id.into_inner())
    .bind(WorkspaceRunV1::SCHEMA_ID)
    .fetch_optional(pool)
    .await
    .map_err(|err| {
        WorkspaceRunnerError::Internal(format!("find continuation workspace run: {err}"))
    })?;
    match run_id {
        Some(run_id) => {
            let memory_id = MemoryId::new(run_id);
            let payload = load_workspace_run(pool, owner, memory_id).await?;
            Ok(Some(LoadedWorkspaceRun { memory_id, payload }))
        }
        None => Ok(None),
    }
}

#[allow(clippy::too_many_lines)]
pub(super) async fn load_goal_context_for_request(
    pool: &PgPool,
    owner: &Owner,
    request_memory_id: MemoryId,
) -> Result<Option<serde_json::Value>, WorkspaceRunnerError> {
    let goal_tables_exist: bool = sqlx::query_scalar(
        "SELECT to_regclass('proxima_goal.goal_activated_v1') IS NOT NULL
             AND to_regclass('proxima_core.goals') IS NOT NULL",
    )
    .fetch_one(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("check goal tables: {err}")))?;
    if !goal_tables_exist {
        return Ok(None);
    }

    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let row = sqlx::query(
        "WITH RECURSIVE ancestry(memory_id, depth, path) AS (
             SELECT e.target_memory_id, 1, ARRAY[$4::uuid, e.target_memory_id]
             FROM proxima_core.edges e
             WHERE e.owner_principal_kind = $1
               AND e.owner_principal_id = $2
               AND e.relation = $3
               AND e.source_kind = 'Fact'
               AND e.source_memory_id = $4
               AND e.target_kind = 'Fact'
               AND e.target_memory_id IS NOT NULL
             UNION ALL
             SELECT e.target_memory_id, a.depth + 1, a.path || e.target_memory_id
             FROM ancestry a
             JOIN proxima_core.edges e
               ON e.owner_principal_kind = $1
              AND e.owner_principal_id = $2
              AND e.relation = $3
              AND e.source_kind = 'Fact'
              AND e.source_memory_id = a.memory_id
              AND e.target_kind = 'Fact'
              AND e.target_memory_id IS NOT NULL
             WHERE NOT e.target_memory_id = ANY(a.path)
         ),
         activated AS (
             SELECT a.memory_id,
                    g.goal_id,
                    g.schema_id,
                    g.title,
                    g.accepted_at,
                    g.evidence_count
             FROM ancestry a
             JOIN proxima_core.memories m
               ON m.memory_id = a.memory_id
              AND m.owner_principal_kind = $1
              AND m.owner_principal_id = $2
              AND m.schema_id = 'proxima-goal/goal-activated-v1'
             JOIN proxima_goal.goal_activated_v1 g
               ON g.memory_id = a.memory_id
             ORDER BY a.depth, a.memory_id DESC
             LIMIT 1
         ),
         goal_lineage(goal_id, depth, path) AS (
             SELECT goal_id, 0, ARRAY[goal_id]
             FROM activated
             UNION ALL
             SELECT child.goal_id, gl.depth + 1, gl.path || child.goal_id
             FROM goal_lineage gl
             JOIN proxima_core.goals child
               ON child.supersedes = gl.goal_id
              AND child.owner_principal_kind = $1
              AND child.owner_principal_id = $2
             WHERE NOT child.goal_id = ANY(gl.path)
         )
         SELECT a.memory_id AS activated_memory_id,
                a.goal_id AS activated_goal_id,
                a.schema_id AS activated_schema_id,
                a.title AS activated_title,
                a.accepted_at,
                a.evidence_count,
                gh.goal_id AS head_goal_id,
                gh.schema_id AS head_schema_id,
                gh.schema_version AS head_schema_version,
                gh.title AS head_title,
                gh.text AS head_text,
                gh.state AS head_state,
                gh.supersedes AS head_supersedes,
                gh.created_at AS head_created_at
         FROM activated a
         JOIN goal_lineage gl ON true
         JOIN proxima_core.goals gh ON gh.goal_id = gl.goal_id
         ORDER BY gl.depth DESC, gh.created_at DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(request_memory_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("load active goal context: {err}")))?;

    let Some(row) = row else {
        return Ok(None);
    };
    let activated_memory_id: Uuid = row
        .try_get("activated_memory_id")
        .map_err(map_sqlx_internal)?;
    let activated_goal_id: Uuid = row
        .try_get("activated_goal_id")
        .map_err(map_sqlx_internal)?;
    let activated_schema_id: String = row
        .try_get("activated_schema_id")
        .map_err(map_sqlx_internal)?;
    let activated_title: String = row.try_get("activated_title").map_err(map_sqlx_internal)?;
    let accepted_at: time::OffsetDateTime =
        row.try_get("accepted_at").map_err(map_sqlx_internal)?;
    let evidence_count: i32 = row.try_get("evidence_count").map_err(map_sqlx_internal)?;
    let head_goal_id: Uuid = row.try_get("head_goal_id").map_err(map_sqlx_internal)?;
    let head_schema_id: String = row.try_get("head_schema_id").map_err(map_sqlx_internal)?;
    let head_schema_version: i32 = row
        .try_get("head_schema_version")
        .map_err(map_sqlx_internal)?;
    let head_title: String = row.try_get("head_title").map_err(map_sqlx_internal)?;
    let head_text: String = row.try_get("head_text").map_err(map_sqlx_internal)?;
    let head_state: String = row.try_get("head_state").map_err(map_sqlx_internal)?;
    let head_supersedes: Option<Uuid> =
        row.try_get("head_supersedes").map_err(map_sqlx_internal)?;
    let head_created_at: time::OffsetDateTime =
        row.try_get("head_created_at").map_err(map_sqlx_internal)?;
    Ok(Some(json!({
        "activated_memory_id": activated_memory_id.to_string(),
        "activated": {
            "goal_id": activated_goal_id.to_string(),
            "schema_id": activated_schema_id,
            "title": activated_title,
            "accepted_at": accepted_at,
            "evidence_count": evidence_count,
        },
        "head": {
            "goal_id": head_goal_id.to_string(),
            "schema_id": head_schema_id,
            "schema_version": head_schema_version,
            "title": head_title,
            "text": head_text,
            "state": head_state,
            "supersedes": head_supersedes.map(|id| id.to_string()),
            "created_at": head_created_at,
        },
    })))
}

pub(super) fn goal_close_candidate(
    active_goal: Option<&serde_json::Value>,
    latest_review: Option<&serde_json::Value>,
    decision_memory_id: MemoryId,
) -> serde_json::Value {
    if latest_review
        .and_then(|review| review.get("verdict"))
        .and_then(serde_json::Value::as_str)
        != Some("approved")
    {
        return json!({
            "status": "skipped",
            "reason": "latest review is not approved",
        });
    }
    let Some(active_goal) = active_goal else {
        return json!({
            "status": "skipped",
            "reason": "no originating Active Goal found",
        });
    };
    let Some(head) = active_goal.get("head") else {
        return json!({
            "status": "skipped",
            "reason": "originating Goal context has no head",
        });
    };
    let state = head
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if state != "Active" {
        return json!({
            "status": "skipped",
            "reason": format!("originating Goal head is not Active: {state}"),
        });
    }
    let Some(goal_id) = head.get("goal_id").and_then(serde_json::Value::as_str) else {
        return json!({
            "status": "skipped",
            "reason": "originating Goal head id is missing",
        });
    };
    json!({
        "status": "ready",
        "goal_id": goal_id,
        "evidence_memory_ids": [decision_memory_id.into_inner().to_string()],
    })
}

pub(super) fn recipe_declares_title(recipe_bytes: &[u8], title: &str) -> bool {
    std::str::from_utf8(recipe_bytes).is_ok_and(|recipe| {
        recipe
            .lines()
            .any(|line| line.trim() == format!("title: {title}"))
    })
}

#[allow(clippy::too_many_lines)]
pub(super) async fn load_workspace_run(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
) -> Result<WorkspaceRunV1, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let row = sqlx::query(
        "SELECT COALESCE(m.kind, 'Fact') AS kind,
                m.schema_id,
                r.wake_invocation_id,
                r.repo_id,
                r.target_branch,
                r.worktree_path,
                r.branch_name,
                r.parent_sha,
                r.head_sha,
                r.diff_stat_json,
                r.exit_code,
                r.stdout_tail,
                r.stderr_tail,
                r.duration_ms
         FROM proxima_core.memories m
         LEFT JOIN proxima_code.workspace_run_v1 r USING (memory_id)
         WHERE m.memory_id = $1
           AND m.owner_principal_kind = $2
           AND m.owner_principal_id = $3",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("load workspace run: {err}")))?;
    let Some(row) = row else {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "workspace run not found: {}",
            memory_id.into_inner()
        )));
    };
    let kind: String = row.try_get("kind").map_err(map_sqlx_internal)?;
    let schema_id: String = row.try_get("schema_id").map_err(map_sqlx_internal)?;
    if kind != "Fact" || schema_id != WorkspaceRunV1::SCHEMA_ID {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "memory {} is not a workspace run",
            memory_id.into_inner()
        )));
    }
    let wake_invocation_id: Option<Uuid> = row
        .try_get("wake_invocation_id")
        .map_err(map_sqlx_internal)?;
    let repo_id: Option<Uuid> = row.try_get("repo_id").map_err(map_sqlx_internal)?;
    let target_branch: Option<String> = row.try_get("target_branch").map_err(map_sqlx_internal)?;
    let worktree_path: Option<String> = row.try_get("worktree_path").map_err(map_sqlx_internal)?;
    let branch_name: Option<String> = row.try_get("branch_name").map_err(map_sqlx_internal)?;
    let parent_sha: Option<String> = row.try_get("parent_sha").map_err(map_sqlx_internal)?;
    let head_sha: Option<String> = row.try_get("head_sha").map_err(map_sqlx_internal)?;
    let diff_stat_json: Option<serde_json::Value> =
        row.try_get("diff_stat_json").map_err(map_sqlx_internal)?;
    let exit_code: Option<i32> = row.try_get("exit_code").map_err(map_sqlx_internal)?;
    let stdout_tail: Option<String> = row.try_get("stdout_tail").map_err(map_sqlx_internal)?;
    let stderr_tail: Option<String> = row.try_get("stderr_tail").map_err(map_sqlx_internal)?;
    let duration_ms_raw: Option<i64> = row.try_get("duration_ms").map_err(map_sqlx_internal)?;
    let (
        Some(wake_invocation_id),
        Some(repo_id),
        Some(target_branch),
        Some(worktree_path),
        Some(branch_name),
        Some(parent_sha),
        Some(head_sha),
        Some(diff_stat_json),
    ) = (
        wake_invocation_id,
        repo_id,
        target_branch,
        worktree_path,
        branch_name,
        parent_sha,
        head_sha,
        diff_stat_json,
    )
    else {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "workspace run sidecar missing: {}",
            memory_id.into_inner()
        )));
    };
    let diff_stat_json = serde_json::from_value(diff_stat_json).map_err(|err| {
        WorkspaceRunnerError::PrepareFailed(format!("decode workspace run diff_stat_json: {err}"))
    })?;
    let duration_ms = duration_ms_raw.and_then(|value| u64::try_from(value).ok());
    Ok(WorkspaceRunV1 {
        wake_invocation_id,
        repo_id,
        target_branch,
        worktree_path,
        branch_name,
        parent_sha,
        head_sha,
        diff_stat_json,
        exit_code,
        stdout_tail,
        stderr_tail,
        duration_ms,
    })
}

pub(super) async fn load_latest_review_for_run(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
) -> Result<Option<serde_json::Value>, WorkspaceRunnerError> {
    let mut reviews = load_reviews_for_run(pool, owner, run_memory_id).await?;
    Ok(reviews.pop())
}

pub(super) async fn load_latest_rejected_review_for_run(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
) -> Result<Option<serde_json::Value>, WorkspaceRunnerError> {
    let mut reviews = load_review_rows(
        pool,
        owner,
        "r.workspace_run_memory_id = $4 AND r.verdict = 'rejected'",
        run_memory_id.into_inner(),
    )
    .await?;
    Ok(reviews.pop())
}

pub(super) async fn load_reviews_for_request(
    pool: &PgPool,
    owner: &Owner,
    request_memory_id: MemoryId,
) -> Result<Vec<serde_json::Value>, WorkspaceRunnerError> {
    load_review_rows(
        pool,
        owner,
        "r.execution_request_memory_id = $4",
        request_memory_id.into_inner(),
    )
    .await
}

async fn load_reviews_for_run(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
) -> Result<Vec<serde_json::Value>, WorkspaceRunnerError> {
    load_review_rows(
        pool,
        owner,
        "r.workspace_run_memory_id = $4",
        run_memory_id.into_inner(),
    )
    .await
}

async fn load_review_rows(
    pool: &PgPool,
    owner: &Owner,
    predicate: &str,
    predicate_id: Uuid,
) -> Result<Vec<serde_json::Value>, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let sql = format!(
        "SELECT r.memory_id,
                r.workspace_run_memory_id,
                r.execution_request_memory_id,
                r.verdict,
                r.round_index,
                r.summary,
                r.findings_json,
                r.correction_instructions,
                r.verification_summary,
                r.reviewed_at
         FROM proxima_code.workspace_review_v1 r
         JOIN proxima_core.memories m USING (memory_id)
         WHERE m.owner_principal_kind = $1
           AND m.owner_principal_id = $2
           AND m.schema_id = $3
           AND {predicate}
         ORDER BY r.created_at, r.memory_id"
    );
    let rows = sqlx::query(&sql)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(WorkspaceReviewV1::SCHEMA_ID)
        .bind(predicate_id)
        .fetch_all(pool)
        .await
        .map_err(|err| WorkspaceRunnerError::Internal(format!("load reviews: {err}")))?;
    rows.into_iter()
        .map(|row| review_row_to_json(&row))
        .collect::<Result<Vec<_>, _>>()
}

fn review_row_to_json(
    row: &sqlx::postgres::PgRow,
) -> Result<serde_json::Value, WorkspaceRunnerError> {
    let memory_id: Uuid = row.try_get("memory_id").map_err(map_sqlx_internal)?;
    let workspace_run_memory_id: Uuid = row
        .try_get("workspace_run_memory_id")
        .map_err(map_sqlx_internal)?;
    let execution_request_memory_id: Uuid = row
        .try_get("execution_request_memory_id")
        .map_err(map_sqlx_internal)?;
    let verdict: String = row.try_get("verdict").map_err(map_sqlx_internal)?;
    let round_index: i32 = row.try_get("round_index").map_err(map_sqlx_internal)?;
    let summary: String = row.try_get("summary").map_err(map_sqlx_internal)?;
    let findings: serde_json::Value = row.try_get("findings_json").map_err(map_sqlx_internal)?;
    let correction_instructions: Option<String> = row
        .try_get("correction_instructions")
        .map_err(map_sqlx_internal)?;
    let verification_summary: Option<String> = row
        .try_get("verification_summary")
        .map_err(map_sqlx_internal)?;
    let reviewed_at: time::OffsetDateTime =
        row.try_get("reviewed_at").map_err(map_sqlx_internal)?;
    Ok(json!({
        "memory_id": memory_id.to_string(),
        "workspace_run_memory_id": workspace_run_memory_id.to_string(),
        "execution_request_memory_id": execution_request_memory_id.to_string(),
        "verdict": verdict,
        "round_index": round_index,
        "summary": summary,
        "findings": findings,
        "correction_instructions": correction_instructions,
        "verification_summary": verification_summary,
        "reviewed_at": reviewed_at,
    }))
}

pub(super) async fn load_decisions_for_request(
    pool: &PgPool,
    owner: &Owner,
    request_memory_id: MemoryId,
) -> Result<Vec<serde_json::Value>, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let rows = sqlx::query(
        "SELECT d.memory_id,
                d.workspace_run_memory_id,
                d.decision,
                d.decided_at,
                d.reason_text,
                d.decided_by_owner_id
         FROM proxima_code.workspace_decision_v1 d
         JOIN proxima_core.memories dm USING (memory_id)
         JOIN proxima_core.edges e
           ON e.source_kind = 'Fact'
          AND e.source_memory_id = d.workspace_run_memory_id
          AND e.target_kind = 'Fact'
          AND e.target_memory_id = $4
          AND e.relation = $5
          AND e.owner_principal_kind = dm.owner_principal_kind
          AND e.owner_principal_id = dm.owner_principal_id
         WHERE dm.owner_principal_kind = $1
           AND dm.owner_principal_id = $2
           AND dm.schema_id = $3
         ORDER BY d.decided_at, d.memory_id",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(WorkspaceDecisionV1::SCHEMA_ID)
    .bind(request_memory_id.into_inner())
    .bind(CORE_DERIVED_FROM_RELATION)
    .fetch_all(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("load decisions: {err}")))?;
    rows.into_iter()
        .map(|row| decision_row_to_json(&row))
        .collect::<Result<Vec<_>, _>>()
}

fn decision_row_to_json(
    row: &sqlx::postgres::PgRow,
) -> Result<serde_json::Value, WorkspaceRunnerError> {
    let memory_id: Uuid = row.try_get("memory_id").map_err(map_sqlx_internal)?;
    let workspace_run_memory_id: Uuid = row
        .try_get("workspace_run_memory_id")
        .map_err(map_sqlx_internal)?;
    let decision: String = row.try_get("decision").map_err(map_sqlx_internal)?;
    let decided_at: time::OffsetDateTime = row.try_get("decided_at").map_err(map_sqlx_internal)?;
    let reason_text: Option<String> = row.try_get("reason_text").map_err(map_sqlx_internal)?;
    let decided_by_owner_id: Uuid = row
        .try_get("decided_by_owner_id")
        .map_err(map_sqlx_internal)?;
    Ok(json!({
        "memory_id": memory_id.to_string(),
        "workspace_run_memory_id": workspace_run_memory_id.to_string(),
        "decision": decision,
        "decided_at": decided_at,
        "reason_text": reason_text,
        "decided_by_owner_id": decided_by_owner_id.to_string(),
    }))
}

pub(super) async fn veto_count_for_request(
    pool: &PgPool,
    owner: &Owner,
    execution_request_memory_id: MemoryId,
) -> Result<i64, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    sqlx::query_scalar(
        "WITH review_vetoes AS (
             SELECT r.memory_id
             FROM proxima_code.workspace_review_v1 r
             JOIN proxima_core.memories m USING (memory_id)
             WHERE m.owner_principal_kind = $1
               AND m.owner_principal_id = $2
               AND r.execution_request_memory_id = $3
               AND r.verdict = 'rejected'
         ),
         decision_vetoes AS (
             SELECT d.memory_id
             FROM proxima_code.workspace_decision_v1 d
             JOIN proxima_core.memories dm USING (memory_id)
             JOIN proxima_core.edges e
               ON e.source_kind = 'Fact'
              AND e.source_memory_id = d.workspace_run_memory_id
              AND e.target_kind = 'Fact'
              AND e.target_memory_id = $3
              AND e.relation = $4
              AND e.owner_principal_kind = dm.owner_principal_kind
              AND e.owner_principal_id = dm.owner_principal_id
             WHERE dm.owner_principal_kind = $1
               AND dm.owner_principal_id = $2
               AND d.decision = 'retry_requested'
         )
         SELECT (SELECT count(*) FROM review_vetoes)
              + (SELECT count(*) FROM decision_vetoes)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(execution_request_memory_id.into_inner())
    .bind(CORE_DERIVED_FROM_RELATION)
    .fetch_one(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("count vetoes: {err}")))
}

pub(super) async fn load_target_worker_personality_for_request(
    pool: &PgPool,
    owner: &Owner,
    request_memory_id: MemoryId,
) -> Result<Option<Uuid>, WorkspaceRunnerError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    sqlx::query_scalar(
        "SELECT p.personality_instance_id
         FROM proxima_core.edges e
         JOIN proxima_core.personality p
           ON p.current_root_perspective_memory_id = e.source_memory_id
          AND p.owner_principal_kind = e.owner_principal_kind
          AND p.owner_principal_id = e.owner_principal_id
         WHERE e.owner_principal_kind = $1
           AND e.owner_principal_id = $2
           AND e.relation = $3
           AND e.source_kind = 'Perspective'
           AND e.target_kind = 'Fact'
           AND e.target_memory_id = $4
         ORDER BY e.created_at DESC, e.edge_id DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(crate::mcp::CODE_TARGETS_EXECUTION_REQUEST_RELATION)
    .bind(request_memory_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("load target worker: {err}")))
}

#[allow(clippy::needless_pass_by_value)]
fn map_sqlx_internal(err: sqlx::Error) -> WorkspaceRunnerError {
    WorkspaceRunnerError::Internal(err.to_string())
}

pub(super) fn repo_id_from_payload(
    payload: &serde_json::Value,
) -> Result<Uuid, WorkspaceRunnerError> {
    payload
        .get("repo_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            WorkspaceRunnerError::PrepareFailed("triggering payload has no repo_id".into())
        })
        .and_then(|raw| {
            Uuid::parse_str(raw)
                .map_err(|err| WorkspaceRunnerError::PrepareFailed(format!("repo_id: {err}")))
        })
}

pub(super) async fn load_repo(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<RunnerRepoRow, WorkspaceRunnerError> {
    let (kind, principal_id, org_id) = owner_columns_pub(owner);
    sqlx::query_as::<_, RunnerRepoRow>(
        "SELECT canonical_path, target_branch
         FROM proxima_code.repos
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND repo_id = $4",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .fetch_optional(pool)
    .await
    .map_err(|err| WorkspaceRunnerError::Internal(format!("load repo: {err}")))?
    .ok_or_else(|| WorkspaceRunnerError::PrepareFailed(format!("repo not found: {repo_id}")))
}
