//! Code workspace run review, decision, and merge flow helpers.

use std::path::Path;
use std::process::Stdio;

use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::{
    CORE_DERIVED_FROM_RELATION, FactPayload, FlavorRegistry, MemoryId, Owner, SchemaId,
    SchemaVersion, SourceBatchId, SourceId,
};
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use sqlx::{PgPool, Row};
use tokio::process::Command;
use uuid::Uuid;

use crate::payloads::{
    WorkspaceDecision, WorkspaceDecisionV1, WorkspaceReviewFinding, WorkspaceReviewVerdict,
};
use crate::repos::{
    WorkspaceDecisionRecord, WorkspaceMergeOutcome, WorkspaceReviewRecord, WorkspaceRunDiff,
    WorkspaceRunRecord, get_repo, owner_columns_pub,
};

const WORKSPACE_DECISION_SOURCE_ID: &str = "proxima-code/workspace-decision";
const WORKSPACE_DECISION_OBJECT_SCHEMA: &str = "proxima-code/workspace-decision-object-v1";
const WORKSPACE_DECISION_WHOLE_SCHEMA: &str = "proxima-code/workspace-decision-whole-v1";
const WORKSPACE_RUN_DIFF_MAX_BYTES: usize = 96 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceFlowError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("storage error: {0}")]
    Storage(#[from] proxima_core::StorageError),
    #[error("repo not found: {repo_id}")]
    RepoNotFound { repo_id: Uuid },
    #[error("workspace run not found: {memory_id}")]
    RunNotFound { memory_id: Uuid },
    #[error("workspace run has no approved latest review: {memory_id}")]
    ApprovedReviewRequired { memory_id: Uuid },
    #[error("workspace run has a later workspace decision: {memory_id}")]
    LaterWorkspaceDecision { memory_id: Uuid },
    #[error("repo {repo_id} has no target branch")]
    MissingTargetBranch { repo_id: Uuid },
    #[error("git command failed: {command}: {stderr}")]
    Git { command: String, stderr: String },
    #[error("invalid workspace review verdict: {value}")]
    InvalidReviewVerdict { value: String },
    #[error("invalid workspace decision: {value}")]
    InvalidDecision { value: String },
    #[error("invalid sidecar row: {message}")]
    InvalidSidecar { message: String },
}

/// List workspace runs for a repo, newest first.
///
/// # Errors
/// Returns `WorkspaceFlowError` on database or sidecar decode failures.
pub async fn list_workspace_runs(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
    limit: u32,
) -> Result<Vec<WorkspaceRunRecord>, WorkspaceFlowError> {
    let limit = i64::from(limit.clamp(1, 100));
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let rows = sqlx::query(
        "SELECT r.memory_id,
                r.wake_invocation_id,
                r.repo_id,
                req.title AS execution_request_title,
                r.target_branch,
                r.worktree_path,
                r.branch_name,
                r.parent_sha,
                r.head_sha,
                r.diff_stat_json,
                r.exit_code,
                r.stdout_tail,
                r.stderr_tail,
                r.duration_ms,
                r.created_at
         FROM proxima_code.workspace_run_v1 r
         JOIN proxima_core.memories m USING (memory_id)
         LEFT JOIN proxima_core.edges request_edge
           ON request_edge.owner_principal_kind = m.owner_principal_kind
          AND request_edge.owner_principal_id = m.owner_principal_id
          AND request_edge.relation = $4
          AND request_edge.source_kind = 'Fact'
          AND request_edge.source_memory_id = r.memory_id
          AND request_edge.target_kind = 'Fact'
         LEFT JOIN proxima_code.execution_request_v1 req
           ON req.memory_id = request_edge.target_memory_id
         WHERE m.owner_principal_kind = $1
           AND m.owner_principal_id = $2
           AND r.repo_id = $3
         ORDER BY r.created_at DESC, r.memory_id DESC
         LIMIT $5",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(repo_id)
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let mut records = Vec::with_capacity(rows.len());
    for row in rows {
        let memory_id: Uuid = row.try_get("memory_id")?;
        records.push(WorkspaceRunRecord {
            memory_id,
            wake_invocation_id: row.try_get("wake_invocation_id")?,
            repo_id: row.try_get("repo_id")?,
            execution_request_title: row.try_get("execution_request_title")?,
            target_branch: row.try_get("target_branch")?,
            worktree_path: row.try_get("worktree_path")?,
            branch_name: row.try_get("branch_name")?,
            parent_sha: row.try_get("parent_sha")?,
            head_sha: row.try_get("head_sha")?,
            diff_stat_json: serde_json::from_value(row.try_get("diff_stat_json")?).map_err(
                |err| WorkspaceFlowError::InvalidSidecar {
                    message: format!("workspace_run_v1.diff_stat_json: {err}"),
                },
            )?,
            exit_code: row.try_get("exit_code")?,
            stdout_tail: row.try_get("stdout_tail")?,
            stderr_tail: row.try_get("stderr_tail")?,
            duration_ms: row
                .try_get::<Option<i64>, _>("duration_ms")?
                .and_then(|value| u64::try_from(value).ok()),
            created_at: row.try_get("created_at")?,
            latest_review: latest_review(pool, owner, MemoryId::new(memory_id)).await?,
            latest_decision: latest_decision(pool, owner, MemoryId::new(memory_id)).await?,
        });
    }
    Ok(records)
}

/// List reviews for one workspace run, newest first.
///
/// # Errors
/// Returns `WorkspaceFlowError` on database or sidecar decode failures.
pub async fn list_workspace_reviews(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
) -> Result<Vec<WorkspaceReviewRecord>, WorkspaceFlowError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let rows = sqlx::query(
        "SELECT r.memory_id,
                r.workspace_run_memory_id,
                r.execution_request_memory_id,
                r.verdict,
                r.round_index,
                r.summary,
                r.findings_json,
                r.correction_instructions,
                r.verification_summary,
                r.reviewed_at,
                r.created_at
         FROM proxima_code.workspace_review_v1 r
         JOIN proxima_core.memories m USING (memory_id)
         WHERE m.owner_principal_kind = $1
           AND m.owner_principal_id = $2
           AND r.workspace_run_memory_id = $3
         ORDER BY r.created_at DESC, r.memory_id DESC",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(run_memory_id.into_inner())
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| review_record_from_row(&row))
        .collect()
}

/// Return a bounded unified diff for one workspace run.
///
/// # Errors
/// Returns `WorkspaceFlowError` if the run is not visible or git diff fails.
pub async fn get_workspace_run_diff(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
) -> Result<WorkspaceRunDiff, WorkspaceFlowError> {
    let run = load_run(pool, owner, run_memory_id).await?;
    let repo_path = Path::new(&run.worktree_path);
    let range = format!("{}..{}", run.parent_sha, run.head_sha);
    let stat = git_output(repo_path, &["diff", "--stat", &range]).await?;
    let files = git_output(repo_path, &["diff", "--name-only", &range])
        .await?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect();
    let patch = git_output(repo_path, &["diff", "--unified=80", &range]).await?;
    let (patch, patch_truncated) = truncate_utf8(patch, WORKSPACE_RUN_DIFF_MAX_BYTES);
    Ok(WorkspaceRunDiff {
        range,
        stat,
        files,
        patch,
        patch_truncated,
        max_patch_bytes: WORKSPACE_RUN_DIFF_MAX_BYTES,
    })
}

/// Append a user workspace decision Fact and sidecar row.
///
/// # Errors
/// Returns `WorkspaceFlowError` if the run is not visible or persistence fails.
pub async fn emit_workspace_decision(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
    decision: WorkspaceDecision,
    reason: Option<&str>,
) -> Result<MemoryId, WorkspaceFlowError> {
    load_run(pool, owner, run_memory_id).await?;
    let decided_by_owner_id = match &owner.principal {
        proxima_core::Principal::User(user) => user.into_inner(),
        proxima_core::Principal::Group(group) => group.into_inner(),
    };
    let payload = WorkspaceDecisionV1 {
        workspace_run_memory_id: run_memory_id.into_inner(),
        decision,
        decided_at: time::OffsetDateTime::now_utc(),
        reason_text: reason
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        decided_by_owner_id,
    };
    ingest_workspace_decision(pool, owner, &payload).await
}

/// Fast-forward the run target branch after an approved review and append
/// a `merged` decision Fact.
///
/// # Errors
/// Returns `WorkspaceFlowError` if policy checks, git checks, or persistence fail.
pub async fn merge_workspace_run(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
) -> Result<WorkspaceMergeOutcome, WorkspaceFlowError> {
    let run = load_run(pool, owner, run_memory_id).await?;
    let latest_review = latest_review(pool, owner, run_memory_id).await?.ok_or(
        WorkspaceFlowError::ApprovedReviewRequired {
            memory_id: run_memory_id.into_inner(),
        },
    )?;
    if latest_review.verdict != WorkspaceReviewVerdict::Approved {
        return Err(WorkspaceFlowError::ApprovedReviewRequired {
            memory_id: run_memory_id.into_inner(),
        });
    }
    if has_workspace_decision_after(pool, owner, run_memory_id, latest_review.created_at).await? {
        return Err(WorkspaceFlowError::LaterWorkspaceDecision {
            memory_id: run_memory_id.into_inner(),
        });
    }
    let repo = get_repo(pool, owner, run.repo_id)
        .await
        .map_err(|err| match err {
            crate::repos::RepoRegistryError::Database(err) => WorkspaceFlowError::Database(err),
            other => WorkspaceFlowError::InvalidSidecar {
                message: other.to_string(),
            },
        })?
        .ok_or(WorkspaceFlowError::RepoNotFound {
            repo_id: run.repo_id,
        })?;
    let target_branch = repo
        .target_branch
        .clone()
        .filter(|branch| !branch.trim().is_empty())
        .ok_or(WorkspaceFlowError::MissingTargetBranch {
            repo_id: run.repo_id,
        })?;
    let repo_path = Path::new(&repo.canonical_path);
    let old_target_sha = git_output(
        repo_path,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{target_branch}"),
        ],
    )
    .await?;
    git_status(
        repo_path,
        &[
            "merge-base",
            "--is-ancestor",
            &run.parent_sha,
            &old_target_sha,
        ],
    )
    .await?;
    git_status(
        repo_path,
        &[
            "merge-base",
            "--is-ancestor",
            &old_target_sha,
            &run.head_sha,
        ],
    )
    .await?;
    git_output(repo_path, &["checkout", &target_branch]).await?;
    git_output(repo_path, &["merge", "--ff-only", &run.head_sha]).await?;
    let new_target_sha = git_output(repo_path, &["rev-parse", "--verify", "HEAD"]).await?;
    let decision_memory_id =
        emit_workspace_decision(pool, owner, run_memory_id, WorkspaceDecision::Merged, None)
            .await?;
    Ok(WorkspaceMergeOutcome {
        run_memory_id: run_memory_id.into_inner(),
        decision_memory_id: decision_memory_id.into_inner(),
        repo_id: run.repo_id,
        target_branch,
        old_target_sha,
        new_target_sha,
    })
}

#[derive(Debug)]
struct LoadedRun {
    repo_id: Uuid,
    worktree_path: String,
    parent_sha: String,
    head_sha: String,
}

async fn load_run(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
) -> Result<LoadedRun, WorkspaceFlowError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let row = sqlx::query(
        "SELECT r.repo_id, r.worktree_path, r.parent_sha, r.head_sha
         FROM proxima_code.workspace_run_v1 r
         JOIN proxima_core.memories m USING (memory_id)
         WHERE m.owner_principal_kind = $1
           AND m.owner_principal_id = $2
           AND r.memory_id = $3",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(run_memory_id.into_inner())
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Err(WorkspaceFlowError::RunNotFound {
            memory_id: run_memory_id.into_inner(),
        });
    };
    Ok(LoadedRun {
        repo_id: row.try_get("repo_id")?,
        worktree_path: row.try_get("worktree_path")?,
        parent_sha: row.try_get("parent_sha")?,
        head_sha: row.try_get("head_sha")?,
    })
}

async fn latest_review(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
) -> Result<Option<WorkspaceReviewRecord>, WorkspaceFlowError> {
    Ok(list_workspace_reviews(pool, owner, run_memory_id)
        .await?
        .into_iter()
        .next())
}

async fn latest_decision(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
) -> Result<Option<WorkspaceDecisionRecord>, WorkspaceFlowError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let row = sqlx::query(
        "SELECT d.memory_id,
                d.workspace_run_memory_id,
                d.decision,
                d.decided_at,
                d.reason_text,
                d.decided_by_owner_id
         FROM proxima_code.workspace_decision_v1 d
         JOIN proxima_core.memories m USING (memory_id)
         WHERE m.owner_principal_kind = $1
           AND m.owner_principal_id = $2
           AND d.workspace_run_memory_id = $3
         ORDER BY d.decided_at DESC, d.memory_id DESC
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(run_memory_id.into_inner())
    .fetch_optional(pool)
    .await?;
    row.map(|row| decision_record_from_row(&row)).transpose()
}

async fn has_workspace_decision_after(
    pool: &PgPool,
    owner: &Owner,
    run_memory_id: MemoryId,
    created_at: time::OffsetDateTime,
) -> Result<bool, WorkspaceFlowError> {
    let (owner_kind, owner_principal_id, _) = owner_columns_pub(owner);
    let exists = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
             FROM proxima_code.workspace_decision_v1 d
             JOIN proxima_core.memories m USING (memory_id)
             WHERE m.owner_principal_kind = $1
               AND m.owner_principal_id = $2
               AND d.workspace_run_memory_id = $3
               AND d.decided_at > $4
         )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(run_memory_id.into_inner())
    .bind(created_at)
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

fn review_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<WorkspaceReviewRecord, WorkspaceFlowError> {
    let verdict: String = row.try_get("verdict")?;
    let round_index: i32 = row.try_get("round_index")?;
    let findings: serde_json::Value = row.try_get("findings_json")?;
    Ok(WorkspaceReviewRecord {
        memory_id: row.try_get("memory_id")?,
        workspace_run_memory_id: row.try_get("workspace_run_memory_id")?,
        execution_request_memory_id: row.try_get("execution_request_memory_id")?,
        verdict: parse_verdict(&verdict)?,
        round_index: u32::try_from(round_index).map_err(|_| {
            WorkspaceFlowError::InvalidSidecar {
                message: format!("negative review round_index: {round_index}"),
            }
        })?,
        summary: row.try_get("summary")?,
        findings: serde_json::from_value::<Vec<WorkspaceReviewFinding>>(findings).map_err(
            |err| WorkspaceFlowError::InvalidSidecar {
                message: format!("workspace_review_v1.findings_json: {err}"),
            },
        )?,
        correction_instructions: row.try_get("correction_instructions")?,
        verification_summary: row.try_get("verification_summary")?,
        reviewed_at: row.try_get("reviewed_at")?,
        created_at: row.try_get("created_at")?,
    })
}

fn decision_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<WorkspaceDecisionRecord, WorkspaceFlowError> {
    let decision: String = row.try_get("decision")?;
    Ok(WorkspaceDecisionRecord {
        memory_id: row.try_get("memory_id")?,
        workspace_run_memory_id: row.try_get("workspace_run_memory_id")?,
        decision: parse_decision(&decision)?,
        decided_at: row.try_get("decided_at")?,
        reason_text: row.try_get("reason_text")?,
        decided_by_owner_id: row.try_get("decided_by_owner_id")?,
    })
}

async fn ingest_workspace_decision(
    pool: &PgPool,
    owner: &Owner,
    payload: &WorkspaceDecisionV1,
) -> Result<MemoryId, WorkspaceFlowError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes).map_err(|err| {
        WorkspaceFlowError::InvalidSidecar {
            message: format!("serialize workspace decision: {err}"),
        }
    })?;
    let content_hash = blake3::hash(&payload_bytes);
    let draft = EventDraft {
        source_id: SourceId::new(WORKSPACE_DECISION_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: owner.clone(),
        schema_id: SchemaId::new(WorkspaceDecisionV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(WorkspaceDecisionV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at: time::OffsetDateTime::now_utc(),
        occurred_at: payload.decided_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(WORKSPACE_DECISION_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(WORKSPACE_DECISION_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    let mut tx = pool.begin().await?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    if !outcome.idempotent_replay {
        sqlx::query(
            "INSERT INTO proxima_code.workspace_decision_v1
                (memory_id, workspace_run_memory_id, decision, decided_at,
                 reason_text, decided_by_owner_id)
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(outcome.memory_id.into_inner())
        .bind(payload.workspace_run_memory_id)
        .bind(payload.decision.as_str())
        .bind(payload.decided_at)
        .bind(payload.reason_text.as_deref())
        .bind(payload.decided_by_owner_id)
        .execute(&mut *tx)
        .await?;
        let registry = FlavorRegistry::default().freeze();
        let relation = registry
            .resolve_relation(CORE_DERIVED_FROM_RELATION)
            .ok_or_else(|| WorkspaceFlowError::InvalidSidecar {
                message: "core/derived-from relation not registered".into(),
            })?;
        append_edge_in_tx(
            &mut tx,
            &EdgeDraft {
                edge_id: Uuid::now_v7(),
                relation,
                source_kind: "Fact",
                source_memory_id: Some(outcome.memory_id.into_inner()),
                source_goal_id: None,
                target_kind: "Fact",
                target_memory_id: Some(payload.workspace_run_memory_id),
                target_goal_id: None,
                authorship_kind: "EventSource",
                authorship_owner_memory_id: None,
                owner,
            },
            None,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(outcome.memory_id)
}

fn parse_verdict(value: &str) -> Result<WorkspaceReviewVerdict, WorkspaceFlowError> {
    match value {
        "approved" => Ok(WorkspaceReviewVerdict::Approved),
        "rejected" => Ok(WorkspaceReviewVerdict::Rejected),
        "needs_user" => Ok(WorkspaceReviewVerdict::NeedsUser),
        other => Err(WorkspaceFlowError::InvalidReviewVerdict {
            value: other.to_string(),
        }),
    }
}

fn parse_decision(value: &str) -> Result<WorkspaceDecision, WorkspaceFlowError> {
    match value {
        "rejected" => Ok(WorkspaceDecision::Rejected),
        "retry_requested" => Ok(WorkspaceDecision::RetryRequested),
        "accepted" => Ok(WorkspaceDecision::Accepted),
        "merged" => Ok(WorkspaceDecision::Merged),
        other => Err(WorkspaceFlowError::InvalidDecision {
            value: other.to_string(),
        }),
    }
}

fn truncate_utf8(value: String, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value, false);
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

async fn git_output(cwd: &Path, args: &[&str]) -> Result<String, WorkspaceFlowError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|err| WorkspaceFlowError::Git {
            command: format!("git {}", args.join(" ")),
            stderr: err.to_string(),
        })?;
    if !output.status.success() {
        return Err(WorkspaceFlowError::Git {
            command: format!("git {}", args.join(" ")),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn git_status(cwd: &Path, args: &[&str]) -> Result<(), WorkspaceFlowError> {
    git_output(cwd, args).await.map(drop)
}
