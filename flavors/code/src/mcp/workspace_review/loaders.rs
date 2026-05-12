// Workspace review database loading functions
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use proxima_core::mcp::{McpToolCtx, McpToolError};
use proxima_core::{MemoryId, relation::CORE_DERIVED_FROM_RELATION};

use crate::payloads::{
    ExecutionRequestV1, WorkspaceDecision, WorkspaceDecisionV1, WorkspaceReviewV1,
    WorkspaceReviewVerdict,
};
use proxima_core::FactPayload;

use super::types::{LoadedWorkspaceDecision, LoadedWorkspaceReview};
use crate::mcp::sql::{map_storage, owner_principal};

/// Load and validate a workspace run memory.
///
/// # Errors
///
/// Returns an error if the memory is not found, not visible, or not a valid workspace run.
pub async fn load_workspace_run(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<(), McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let row: Option<(String, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT COALESCE(m.kind, 'Fact') AS kind, m.schema_id, r.memory_id
         FROM proxima_core.memories m
         LEFT JOIN proxima_code.workspace_run_v1 r USING (memory_id)
         WHERE m.memory_id = $1
           AND m.owner_principal_kind = $2
           AND m.owner_principal_id = $3",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_storage)?;
    let Some((kind, schema_id, sidecar_memory_id)) = row else {
        return Err(McpToolError::InvalidInput(format!(
            "workspace_run_memory is not visible: {}",
            memory_id.into_inner()
        )));
    };
    if kind != "Fact" || schema_id != "proxima-code/workspace-run-v1" {
        return Err(McpToolError::InvalidInput(
            "workspace_run_memory must be a proxima-code/workspace-run-v1 Fact".into(),
        ));
    }
    if sidecar_memory_id.is_none() {
        return Err(McpToolError::InvalidInput(
            "workspace_run_memory sidecar row is missing".into(),
        ));
    }
    Ok(())
}

/// Load a workspace review from the database.
///
/// # Errors
///
/// Returns an error if the review is not found or database query fails.
#[allow(clippy::too_many_lines)]
pub async fn load_workspace_review(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<LoadedWorkspaceReview, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        String,
        Option<Uuid>,
        Option<Uuid>,
        Option<String>,
        Option<i32>,
        Option<String>,
        Option<serde_json::Value>,
        Option<String>,
        Option<String>,
        Option<time::OffsetDateTime>,
    )> = sqlx::query_as(
        "SELECT COALESCE(m.kind, 'Fact') AS kind,
                m.schema_id,
                r.workspace_run_memory_id,
                r.execution_request_memory_id,
                r.verdict,
                r.round_index,
                r.summary,
                r.findings_json,
                r.correction_instructions,
                r.verification_summary,
                r.reviewed_at
         FROM proxima_core.memories m
         LEFT JOIN proxima_code.workspace_review_v1 r USING (memory_id)
         WHERE m.memory_id = $1
           AND m.owner_principal_kind = $2
           AND m.owner_principal_id = $3",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_storage)?;
    let Some((
        kind,
        schema_id,
        workspace_run_memory_id,
        execution_request_memory_id,
        verdict,
        round_index,
        summary,
        findings_json,
        correction_instructions,
        verification_summary,
        reviewed_at,
    )) = row
    else {
        return Err(McpToolError::InvalidInput(format!(
            "workspace_review_memory is not visible: {}",
            memory_id.into_inner()
        )));
    };
    if kind != "Fact" || schema_id != WorkspaceReviewV1::SCHEMA_ID {
        return Err(McpToolError::InvalidInput(
            "workspace_review_memory must be a proxima-code/workspace-review-v1 Fact".into(),
        ));
    }
    let (
        Some(workspace_run_memory_id),
        Some(execution_request_memory_id),
        Some(verdict),
        Some(round_index),
        Some(summary),
        Some(findings_json),
        Some(reviewed_at),
    ) = (
        workspace_run_memory_id,
        execution_request_memory_id,
        verdict,
        round_index,
        summary,
        findings_json,
        reviewed_at,
    )
    else {
        return Err(McpToolError::InvalidInput(
            "workspace_review_memory sidecar row is missing".into(),
        ));
    };
    let verdict = match verdict.as_str() {
        "approved" => WorkspaceReviewVerdict::Approved,
        "rejected" => WorkspaceReviewVerdict::Rejected,
        "needs_user" => WorkspaceReviewVerdict::NeedsUser,
        other => {
            return Err(McpToolError::InvalidInput(format!(
                "workspace_review_memory has invalid verdict: {other}"
            )));
        }
    };
    let findings = serde_json::from_value(findings_json)
        .map_err(|err| McpToolError::InvalidInput(format!("findings_json: {err}")))?;
    let round_index = u32::try_from(round_index).map_err(|_| {
        McpToolError::InvalidInput("workspace_review_memory has invalid round_index".into())
    })?;
    Ok(LoadedWorkspaceReview {
        memory_id,
        execution_request_memory_id: MemoryId::new(execution_request_memory_id),
        payload: WorkspaceReviewV1 {
            workspace_run_memory_id,
            execution_request_memory_id,
            verdict,
            round_index,
            summary,
            findings,
            correction_instructions,
            verification_summary,
            reviewed_at,
        },
    })
}

/// Load a workspace decision from the database.
///
/// # Errors
///
/// Returns an error if the decision is not found or database query fails.
pub async fn load_workspace_decision(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    memory_id: MemoryId,
) -> Result<LoadedWorkspaceDecision, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        String,
        Option<Uuid>,
        Option<String>,
        Option<time::OffsetDateTime>,
        Option<String>,
        Option<Uuid>,
    )> = sqlx::query_as(
        "SELECT COALESCE(m.kind, 'Fact') AS kind,
                m.schema_id,
                d.workspace_run_memory_id,
                d.decision,
                d.decided_at,
                d.reason_text,
                d.decided_by_owner_id
         FROM proxima_core.memories m
         LEFT JOIN proxima_code.workspace_decision_v1 d USING (memory_id)
         WHERE m.memory_id = $1
           AND m.owner_principal_kind = $2
           AND m.owner_principal_id = $3",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_storage)?;
    let Some((
        kind,
        schema_id,
        workspace_run_memory_id,
        decision,
        decided_at,
        reason_text,
        decided_by_owner_id,
    )) = row
    else {
        return Err(McpToolError::InvalidInput(format!(
            "workspace_decision_memory is not visible: {}",
            memory_id.into_inner()
        )));
    };
    if kind != "Fact" || schema_id != WorkspaceDecisionV1::SCHEMA_ID {
        return Err(McpToolError::InvalidInput(
            "workspace_decision_memory must be a proxima-code/workspace-decision-v1 Fact".into(),
        ));
    }
    let (
        Some(workspace_run_memory_id),
        Some(decision),
        Some(decided_at),
        Some(decided_by_owner_id),
    ) = (
        workspace_run_memory_id,
        decision,
        decided_at,
        decided_by_owner_id,
    )
    else {
        return Err(McpToolError::InvalidInput(
            "workspace_decision_memory sidecar row is missing".into(),
        ));
    };
    let decision = match decision.as_str() {
        "rejected" => WorkspaceDecision::Rejected,
        "retry_requested" => WorkspaceDecision::RetryRequested,
        "accepted" => WorkspaceDecision::Accepted,
        "merged" => WorkspaceDecision::Merged,
        other => {
            return Err(McpToolError::InvalidInput(format!(
                "workspace_decision_memory has invalid decision: {other}"
            )));
        }
    };
    Ok(LoadedWorkspaceDecision {
        memory_id,
        payload: WorkspaceDecisionV1 {
            workspace_run_memory_id,
            decision,
            decided_at,
            reason_text,
            decided_by_owner_id,
        },
    })
}

/// Load the latest rejected review for a workspace run.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub async fn load_latest_rejected_review_for_run(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    workspace_run_memory_id: MemoryId,
) -> Result<Option<LoadedWorkspaceReview>, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let memory_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT r.memory_id
         FROM proxima_code.workspace_review_v1 r
         JOIN proxima_core.memories m USING (memory_id)
         WHERE m.owner_principal_kind = $1
           AND m.owner_principal_id = $2
           AND r.workspace_run_memory_id = $3
           AND r.verdict = 'rejected'
         ORDER BY r.created_at DESC, r.memory_id DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(workspace_run_memory_id.into_inner())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_storage)?;
    match memory_id {
        Some(memory_id) => load_workspace_review(tx, ctx, MemoryId::new(memory_id))
            .await
            .map(Some),
        None => Ok(None),
    }
}

/// Find the execution request for a workspace run via derived-from edges.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub async fn find_execution_request_for_run(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    workspace_run_memory_id: MemoryId,
) -> Result<MemoryId, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let request: Option<Uuid> = sqlx::query_scalar(
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
    .bind(workspace_run_memory_id.into_inner())
    .bind(ExecutionRequestV1::SCHEMA_ID)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_storage)?;
    request.map(MemoryId::new).ok_or_else(|| {
        McpToolError::InvalidInput(
            "workspace_run_memory has no derived-from execution request".into(),
        )
    })
}

/// Count veto rounds for an execution request.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub async fn veto_count_for_request(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &McpToolCtx,
    execution_request_memory_id: MemoryId,
) -> Result<i64, McpToolError> {
    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let count: i64 = sqlx::query_scalar(
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
    .fetch_one(&mut **tx)
    .await
    .map_err(map_storage)?;
    Ok(count)
}
