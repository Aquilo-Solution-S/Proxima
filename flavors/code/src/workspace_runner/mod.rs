//! Code-flavor workspace runner.
//!
//! Core owns only wake dispatch and the runner trait. This module owns
//! repo, branch, worktree, and workspace-run Fact semantics.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::{
    FactPayload, MemoryId, Owner, SchemaId, SchemaVersion, SourceBatchId, SourceId,
    WorkspaceFinalizeInput, WorkspacePrepareInput, WorkspacePreparedRun, WorkspaceRunRecord,
    WorkspaceRunner, WorkspaceRunnerError,
};
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::process::Command;
use uuid::Uuid;

use crate::payloads::{WorkspaceDiffFile, WorkspaceDiffStat, WorkspaceRunV1};
use crate::repos::owner_columns_pub;

pub const WORKSPACE_RUNNER_SOURCE_ID: &str = "proxima-code/workspace-runner";
pub const WORKSPACE_RUN_OBJECT_SCHEMA: &str = "proxima-code/workspace-run-object-v1";
pub const WORKSPACE_RUN_WHOLE_SCHEMA: &str = "proxima-code/workspace-run-whole-v1";

#[derive(Debug, Default, Clone)]
pub struct CodeWorkspaceRunner {
    pool: Option<PgPool>,
    worktrees_root: Option<PathBuf>,
}

impl CodeWorkspaceRunner {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool: Some(pool),
            worktrees_root: None,
        }
    }

    #[must_use]
    pub fn with_worktrees_root(mut self, root: PathBuf) -> Self {
        self.worktrees_root = Some(root);
        self
    }

    fn pool(&self) -> Result<&PgPool, WorkspaceRunnerError> {
        self.pool
            .as_ref()
            .ok_or(WorkspaceRunnerError::Unimplemented)
    }

    fn worktrees_root(&self) -> PathBuf {
        self.worktrees_root.clone().unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map_or_else(std::env::temp_dir, PathBuf::from)
                .join(".proxima")
                .join("worktrees")
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PreparedState {
    repo_id: Uuid,
    canonical_path: String,
    target_branch: String,
    branch_name: String,
    parent_sha: String,
    worktree_path: String,
}

#[derive(Debug, sqlx::FromRow)]
struct RunnerRepoRow {
    canonical_path: String,
    target_branch: Option<String>,
}

#[async_trait::async_trait]
impl WorkspaceRunner for CodeWorkspaceRunner {
    async fn prepare(
        &self,
        input: WorkspacePrepareInput<'_>,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
        let pool = self.pool()?;
        let repo_id = repo_id_from_payload(input.triggering_memory_payload)?;
        let mut repo = load_repo(pool, input.owner, repo_id).await?;
        if repo.target_branch.is_none() {
            let inferred = crate::repos::infer_missing_target_branch(pool, input.owner, repo_id)
                .await
                .map_err(|err| {
                    WorkspaceRunnerError::PrepareFailed(format!(
                        "repo {repo_id} target_branch inference failed: {err}"
                    ))
                })?;
            repo.target_branch = inferred.target_branch;
        }
        let target_branch = repo.target_branch.ok_or_else(|| {
            WorkspaceRunnerError::PrepareFailed(format!("repo {repo_id} has no target_branch"))
        })?;
        let parent_sha = git_output(
            Path::new(&repo.canonical_path),
            &["rev-parse", &target_branch],
        )
        .await
        .map_err(|stderr| {
            WorkspaceRunnerError::PrepareFailed(format!(
                "target branch {target_branch} invalid: {stderr}"
            ))
        })?;
        let branch_name = format!("proxima/wake/{}", input.invocation_id);
        let owner_component = owner_component(input.owner);
        let worktree_path = self
            .worktrees_root()
            .join(owner_component)
            .join(input.invocation_id.to_string());
        if let Some(parent) = worktree_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|err| {
                WorkspaceRunnerError::PrepareFailed(format!("create worktree parent: {err}"))
            })?;
        }
        let worktree_arg = worktree_path.to_string_lossy().to_string();
        git_output(
            Path::new(&repo.canonical_path),
            &[
                "worktree",
                "add",
                "-b",
                &branch_name,
                &worktree_arg,
                &parent_sha,
            ],
        )
        .await
        .map_err(|stderr| WorkspaceRunnerError::PrepareFailed(format!("worktree add: {stderr}")))?;

        let state = PreparedState {
            repo_id,
            canonical_path: repo.canonical_path,
            target_branch,
            branch_name,
            parent_sha,
            worktree_path: worktree_arg,
        };
        let runner_state = serde_json::to_value(&state).map_err(|err| {
            WorkspaceRunnerError::Internal(format!("serialize runner state: {err}"))
        })?;
        Ok(WorkspacePreparedRun {
            work_dir: worktree_path,
            effective_recipe_path: input.effective_recipe_path.to_path_buf(),
            runner_state,
        })
    }

    async fn finalize(
        &self,
        input: WorkspaceFinalizeInput<'_>,
    ) -> Result<WorkspaceRunRecord, WorkspaceRunnerError> {
        let pool = self.pool()?;
        let state: PreparedState = serde_json::from_value(input.prepared.runner_state.clone())
            .map_err(|err| {
                WorkspaceRunnerError::FinalizeFailed(format!("decode runner state: {err}"))
            })?;
        let worktree = Path::new(&state.worktree_path);
        let head_sha = git_output(worktree, &["rev-parse", "HEAD"])
            .await
            .map_err(|stderr| {
                WorkspaceRunnerError::FinalizeFailed(format!("rev-parse HEAD: {stderr}"))
            })?;
        let diff_stat = diff_stat(worktree, &state.parent_sha, &head_sha).await?;
        let payload = WorkspaceRunV1 {
            wake_invocation_id: input.invocation_id,
            repo_id: state.repo_id,
            target_branch: state.target_branch,
            worktree_path: state.worktree_path,
            branch_name: state.branch_name,
            parent_sha: state.parent_sha,
            head_sha: head_sha.clone(),
            diff_stat_json: diff_stat,
            exit_code: input.outcome.exit_code,
            stdout_tail: input.outcome.stdout_tail.clone(),
            stderr_tail: input.outcome.stderr_tail.clone(),
            duration_ms: input.outcome.duration_ms,
        };
        let memory_id = ingest_workspace_run(pool, &payload, input).await?;
        Ok(WorkspaceRunRecord {
            primary_memory_id: Some(memory_id),
        })
    }
}

fn repo_id_from_payload(payload: &serde_json::Value) -> Result<Uuid, WorkspaceRunnerError> {
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

async fn load_repo(
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

async fn git_output(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn diff_stat(
    worktree: &Path,
    parent_sha: &str,
    head_sha: &str,
) -> Result<WorkspaceDiffStat, WorkspaceRunnerError> {
    let range = format!("{parent_sha}..{head_sha}");
    let raw = git_output(worktree, &["diff", "--numstat", &range])
        .await
        .map_err(|stderr| {
            WorkspaceRunnerError::FinalizeFailed(format!("diff numstat: {stderr}"))
        })?;
    let mut files = Vec::new();
    let mut insertions = 0u64;
    let mut deletions = 0u64;
    for line in raw.lines() {
        let mut parts = line.splitn(3, '\t');
        let added = parts.next().unwrap_or("0");
        let deleted = parts.next().unwrap_or("0");
        let path = parts.next().unwrap_or("").to_string();
        let added_n = added.parse::<u64>().unwrap_or(0);
        let deleted_n = deleted.parse::<u64>().unwrap_or(0);
        insertions = insertions.saturating_add(added_n);
        deletions = deletions.saturating_add(deleted_n);
        files.push(WorkspaceDiffFile {
            path,
            insertions: added_n,
            deletions: deleted_n,
        });
    }
    Ok(WorkspaceDiffStat {
        files_changed: u64::try_from(files.len()).unwrap_or(u64::MAX),
        insertions,
        deletions,
        files,
    })
}

#[allow(clippy::too_many_lines)]
async fn ingest_workspace_run(
    pool: &PgPool,
    payload: &WorkspaceRunV1,
    input: WorkspaceFinalizeInput<'_>,
) -> Result<MemoryId, WorkspaceRunnerError> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes).map_err(|err| {
        WorkspaceRunnerError::FinalizeFailed(format!("serialize workspace run: {err}"))
    })?;
    let content_hash = blake3::hash(&payload_bytes);
    let observed_at = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new(WORKSPACE_RUNNER_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: input.owner.clone(),
        schema_id: SchemaId::new(WorkspaceRunV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(WorkspaceRunV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: observed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(WORKSPACE_RUN_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(WORKSPACE_RUN_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    };

    let mut tx = pool
        .begin()
        .await
        .map_err(|err| WorkspaceRunnerError::Internal(format!("begin workspace tx: {err}")))?;
    let outcome = ingest_event_in_tx(&mut tx, &draft)
        .await
        .map_err(|err| WorkspaceRunnerError::FinalizeFailed(format!("event ingest: {err}")))?;
    if !outcome.idempotent_replay {
        sqlx::query(
            "INSERT INTO proxima_code.workspace_run_v1
                (memory_id, wake_invocation_id, repo_id, target_branch,
                 worktree_path, branch_name, parent_sha, head_sha,
                 diff_stat_json, exit_code, stdout_tail, stderr_tail, duration_ms)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(outcome.memory_id.into_inner())
        .bind(payload.wake_invocation_id)
        .bind(payload.repo_id)
        .bind(&payload.target_branch)
        .bind(&payload.worktree_path)
        .bind(&payload.branch_name)
        .bind(&payload.parent_sha)
        .bind(&payload.head_sha)
        .bind(
            serde_json::to_value(&payload.diff_stat_json).map_err(|err| {
                WorkspaceRunnerError::FinalizeFailed(format!("serialize diff stat: {err}"))
            })?,
        )
        .bind(payload.exit_code)
        .bind(payload.stdout_tail.as_deref())
        .bind(payload.stderr_tail.as_deref())
        .bind(payload.duration_ms.and_then(|v| i64::try_from(v).ok()))
        .execute(&mut *tx)
        .await
        .map_err(|err| WorkspaceRunnerError::FinalizeFailed(format!("insert sidecar: {err}")))?;

        let authored = EdgeDraft {
            edge_id: Uuid::now_v7(),
            relation: input.authored_relation,
            source_kind: "Perspective",
            source_memory_id: Some(input.root_perspective_memory_id.into_inner()),
            source_goal_id: None,
            target_kind: "Fact",
            target_memory_id: Some(outcome.memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: "Engine",
            authorship_owner_memory_id: None,
            owner: input.owner,
        };
        append_edge_in_tx(&mut tx, &authored, None)
            .await
            .map_err(|err| {
                WorkspaceRunnerError::FinalizeFailed(format!("append authored edge: {err}"))
            })?;

        let derived = EdgeDraft {
            edge_id: Uuid::now_v7(),
            relation: input.derived_from_relation,
            source_kind: "Fact",
            source_memory_id: Some(outcome.memory_id.into_inner()),
            source_goal_id: None,
            target_kind: "Fact",
            target_memory_id: Some(input.triggering_memory_id.into_inner()),
            target_goal_id: None,
            authorship_kind: "EventSource",
            authorship_owner_memory_id: None,
            owner: input.owner,
        };
        append_edge_in_tx(&mut tx, &derived, None)
            .await
            .map_err(|err| {
                WorkspaceRunnerError::FinalizeFailed(format!("append derived edge: {err}"))
            })?;
    }
    tx.commit()
        .await
        .map_err(|err| WorkspaceRunnerError::Internal(format!("commit workspace tx: {err}")))?;
    Ok(outcome.memory_id)
}

fn owner_component(owner: &Owner) -> String {
    match &owner.principal {
        proxima_core::Principal::User(user) => user.into_inner().to_string(),
        proxima_core::Principal::Group(group) => group.into_inner().to_string(),
    }
}
