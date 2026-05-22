//! Git-clone workspace runner for wake fire path.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::process::Command;
use uuid::Uuid;

use crate::engine::Engine;
use crate::personality::workspace::{
    WorkspaceFinalizeInput, WorkspaceOutcome, WorkspacePrepareInput, WorkspacePreparedRun,
    WorkspaceRunnerError,
};
use crate::personality::{WakeWorkspaceBinding, WakeWorkspaceFinalize};
use crate::wake::context::WakeContext;
use crate::workspace_run::{
    CORE_WORKSPACE_RUN_SOURCE_ID, CoreWorkspaceDiffFile, CoreWorkspaceDiffStat,
    CoreWorkspaceRunPersistInput, CoreWorkspaceRunV1,
};
use crate::{EntityKind, MemoryId, SourceBatchId, SourceId};

use super::input::FireWakeEntryInput;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum CoreWorkspaceRunnerState {
    /// A per-wake full `git clone` of the repo. Unlike a linked worktree,
    /// a clone has a real `.git` directory, so `git` functions inside a
    /// container bind-mounting only the clone. Changes return to the real
    /// repo as a fetched branch; the real working tree is never touched.
    GitClone {
        /// Canonical path of the real repository.
        repo_path: String,
        /// The disposable per-wake clone the harness runs against.
        staging_dir: String,
        /// `proxima/wake/<invocation_id>` — the branch changes land on.
        wake_branch: String,
        /// The ref the wake was based on (e.g. `HEAD`).
        base_ref: String,
        /// The resolved commit `base_ref` pointed at when the wake started.
        base_sha: String,
        finalize: WakeWorkspaceFinalize,
    },
}

pub(super) async fn prepare_registered_workspace_runner(
    engine: &Engine,
    prepare_input: WorkspacePrepareInput<'_>,
    flavor_id: &str,
) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
    let runner = engine
        .registry()
        .workspace_runner(flavor_id)
        .ok_or_else(|| {
            WorkspaceRunnerError::TriggerNotEligible(format!(
                "workspace_no_runner_for_flavor:{flavor_id}"
            ))
        })?;

    if !engine
        .registry()
        .is_workspace_trigger(prepare_input.triggering_memory_schema_id)
    {
        return Err(WorkspaceRunnerError::TriggerNotEligible(format!(
            "workspace_trigger_not_eligible:{}",
            prepare_input.triggering_memory_schema_id
        )));
    }

    runner.prepare(prepare_input).await
}

pub(super) async fn prepare_workspace_binding(
    engine: &Engine,
    input: WorkspacePrepareInput<'_>,
    binding: &WakeWorkspaceBinding,
) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
    match binding {
        WakeWorkspaceBinding::GitWorktree {
            repo_path,
            base_ref,
            finalize,
            worktrees_root,
        } => prepare_core_git_clone(input, repo_path, base_ref, *finalize, worktrees_root).await,
        WakeWorkspaceBinding::RegisteredRunner { flavor_id } => {
            prepare_registered_workspace_runner(engine, input, flavor_id).await
        }
    }
}

/// Prepare a disposable per-wake **full clone** of the repo.
///
/// A linked git worktree shares its parent's object store and its `.git`
/// is only a pointer file — `git` does not work inside a container that
/// bind-mounts the worktree alone. A `git clone --local` produces a real
/// `.git` directory (hardlinked objects, near-instant, no network), so the
/// clone is self-contained and container-portable.
pub(super) async fn prepare_core_git_clone(
    input: WorkspacePrepareInput<'_>,
    repo_path: &str,
    base_ref: &str,
    finalize: WakeWorkspaceFinalize,
    clones_root: &Option<String>,
) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
    let repo = std::fs::canonicalize(repo_path).map_err(|err| {
        WorkspaceRunnerError::PrepareFailed(format!("canonicalize repo_path {repo_path}: {err}"))
    })?;
    let base_sha = git_output(&repo, &["rev-parse", base_ref])
        .await
        .map_err(|stderr| {
            WorkspaceRunnerError::PrepareFailed(format!("rev-parse {base_ref}: {stderr}"))
        })?;
    let root = clones_root
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(default_core_wake_clones_root);
    let staging_path = root.join(input.invocation_id.to_string());
    let wake_branch = format!("proxima/wake/{}", input.invocation_id);
    clone_repo_to_staging(&repo, &staging_path, &wake_branch, &base_sha)
        .await
        .map_err(WorkspaceRunnerError::PrepareFailed)?;
    let staging_arg = staging_path.to_string_lossy().to_string();

    let state = CoreWorkspaceRunnerState::GitClone {
        repo_path: repo.to_string_lossy().to_string(),
        staging_dir: staging_arg.clone(),
        wake_branch: wake_branch.clone(),
        base_ref: base_ref.to_string(),
        base_sha: base_sha.clone(),
        finalize,
    };
    let workspace_context = json!({
        "mode": "core_git_clone",
        "repo_path": repo.to_string_lossy(),
        "repo_handle": repo.to_string_lossy(),
        "staging_dir": staging_arg,
        "wake_branch": wake_branch,
        "base_ref": base_ref,
        "base_sha": base_sha,
        "finalize": finalize.as_str(),
        "triggering_memory_schema_id": input.triggering_memory_schema_id,
        "triggering_memory_id": input.triggering_memory_id.into_inner().to_string(),
        "is_continuation": input.is_continuation,
    });
    let runner_state = serde_json::to_value(state).map_err(|err| {
        WorkspaceRunnerError::Internal(format!("serialize core workspace state: {err}"))
    })?;
    Ok(WorkspacePreparedRun {
        work_dir: staging_path,
        workspace_context: Some(workspace_context),
        runner_state,
    })
}

/// `git clone --local` the repo into a fresh `staging_path`, then position
/// it on `wake_branch` at exactly `base_sha`.
pub(super) async fn clone_repo_to_staging(
    repo: &Path,
    staging_path: &Path,
    wake_branch: &str,
    base_sha: &str,
) -> Result<(), String> {
    if let Some(parent) = staging_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create staging parent: {err}"))?;
    }
    // `git clone` requires the target not to exist; clear any stale dir
    // (e.g. a clone left behind by a crashed prior run).
    if staging_path.exists() {
        std::fs::remove_dir_all(staging_path)
            .map_err(|err| format!("clear stale staging dir: {err}"))?;
    }
    let repo_arg = repo.to_string_lossy().to_string();
    let staging_arg = staging_path.to_string_lossy().to_string();
    git_output(repo, &["clone", "--local", &repo_arg, &staging_arg])
        .await
        .map_err(|stderr| format!("git clone --local: {stderr}"))?;
    git_output(staging_path, &["checkout", "-B", wake_branch, base_sha])
        .await
        .map_err(|stderr| format!("git checkout -B {wake_branch}: {stderr}"))?;
    Ok(())
}

pub(super) fn default_core_wake_clones_root() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join(".proxima")
        .join("wake-clones")
        .join("core")
}

pub(super) async fn finalize_workspace_runner(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    invocation_id: Uuid,
    prepared: WorkspacePreparedRun,
    outcome: WorkspaceOutcome,
    evidence: CoreSandboxEvidence,
) -> Result<(), String> {
    let Some(binding) = input.wake_entry.workspace_binding.as_ref() else {
        return Err("workspace_binding_required".into());
    };
    if matches!(binding, WakeWorkspaceBinding::GitWorktree { .. }) {
        return finalize_core_workspace_binding(
            engine,
            input,
            wake_context,
            invocation_id,
            prepared,
            outcome,
            evidence,
        )
        .await;
    }
    let WakeWorkspaceBinding::RegisteredRunner { flavor_id } = binding else {
        unreachable!("all core workspace bindings handled above")
    };
    let runner = engine
        .registry()
        .workspace_runner(flavor_id)
        .ok_or_else(|| format!("workspace_no_runner_for_flavor:{flavor_id}"))?;
    let authored_relation = engine
        .registry()
        .resolve_relation(crate::CORE_AUTHORED_RELATION)
        .ok_or_else(|| "missing core/authored relation".to_string())?;
    let derived_from_relation = engine
        .registry()
        .resolve_relation(crate::CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| "missing core/derived-from relation".to_string())?;
    runner
        .finalize(WorkspaceFinalizeInput {
            owner: &input.owner,
            invocation_id,
            wake_entry_id: input.wake_entry.wake_entry_id,
            personality_instance_id: input.personality_instance_id,
            root_perspective_memory_id: MemoryId::new(wake_context.root_perspective.memory_id),
            triggering_memory_id: MemoryId::new(wake_context.triggering_memory.memory_id),
            authored_relation,
            derived_from_relation,
            prepared,
            outcome,
        })
        .await
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[derive(Debug, Clone)]
pub(super) struct CoreGitWorkspaceFinalization {
    pub head_sha: String,
    pub committed: bool,
    pub diff_stat: CoreWorkspaceDiffStat,
}

/// Per-wake observation evidence the fire path collects once the harness
/// returns — sandbox identity and on-disk artifact hashes — to record on
/// the `workspace_run` Fact. Every field is `None` for a host-mode wake.
#[derive(Debug, Clone, Default)]
pub(super) struct CoreSandboxEvidence {
    pub sandbox_image: Option<String>,
    pub sandbox_container: Option<String>,
    pub transcript_blob_hash: Option<String>,
    pub network_log_blob_hash: Option<String>,
}

pub(super) async fn finalize_core_workspace_binding(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    invocation_id: Uuid,
    prepared: WorkspacePreparedRun,
    outcome: WorkspaceOutcome,
    evidence: CoreSandboxEvidence,
) -> Result<(), String> {
    let state: CoreWorkspaceRunnerState = serde_json::from_value(prepared.runner_state)
        .map_err(|err| format!("decode core workspace state: {err}"))?;
    match state {
        CoreWorkspaceRunnerState::GitClone {
            repo_path,
            staging_dir,
            wake_branch,
            base_ref,
            base_sha,
            finalize,
        } => {
            let finalization = finalize_core_git_clone(
                &PathBuf::from(&staging_dir),
                &PathBuf::from(&repo_path),
                &wake_branch,
                &base_sha,
                finalize,
            )
            .await?;
            let run = CoreWorkspaceRunV1 {
                wake_invocation_id: invocation_id,
                wake_entry_id: input.wake_entry.wake_entry_id,
                personality_instance_id: input.personality_instance_id.into_inner(),
                binding_kind: "git_clone".to_string(),
                finalize: finalize.as_str().to_string(),
                repo_path,
                base_ref,
                // `CoreWorkspaceRunV1.worktree_path` / `parent_sha` field
                // names are retained for now — Phase D renames the columns.
                worktree_path: staging_dir,
                branch_name: wake_branch.clone(),
                parent_sha: base_sha,
                head_sha: finalization.head_sha,
                committed: finalization.committed,
                diff_stat_json: finalization.diff_stat,
                exit_code: outcome.exit_code,
                stdout_tail: outcome.stdout_tail,
                stderr_tail: outcome.stderr_tail,
                duration_ms: outcome.duration_ms,
                sandbox_image: evidence.sandbox_image,
                sandbox_container: evidence.sandbox_container,
                wake_branch: Some(wake_branch),
                transcript_blob_hash: evidence.transcript_blob_hash,
                network_log_blob_hash: evidence.network_log_blob_hash,
            };
            let observed_at = time::OffsetDateTime::now_utc();
            engine
                .persist_core_workspace_run_internal(CoreWorkspaceRunPersistInput {
                    owner: input.owner.clone(),
                    root_perspective_memory_id: MemoryId::new(
                        wake_context.root_perspective.memory_id,
                    ),
                    triggering_memory_id: MemoryId::new(wake_context.triggering_memory.memory_id),
                    triggering_memory_kind: workspace_triggering_memory_kind(
                        &wake_context.triggering_memory.kind,
                    )?,
                    run,
                    source_batch_id: SourceBatchId::new(Uuid::now_v7()),
                    source_id: SourceId::new(CORE_WORKSPACE_RUN_SOURCE_ID.to_string()),
                    observed_at,
                })
                .await
                .map_err(|err| format!("persist core workspace run: {err}"))?;
            Ok(())
        }
    }
}

pub(super) fn workspace_triggering_memory_kind(kind: &str) -> Result<EntityKind, String> {
    match kind {
        "Fact" => Ok(EntityKind::Fact),
        "Abstraction" => Ok(EntityKind::Abstraction),
        "Perspective" => Ok(EntityKind::Perspective),
        other => Err(format!(
            "unsupported workspace triggering memory kind: {other}"
        )),
    }
}

/// Finalize a per-wake clone: commit its changes onto the wake branch,
/// fetch that branch back into the real repo, then discard the clone.
///
/// `git fetch` moves commits, not uncommitted state, so a workspace wake
/// always commits. The `finalize` mode no longer decides *whether* to
/// commit — only how the commit is labelled: `LeaveDirty` marks it WIP.
pub(super) async fn finalize_core_git_clone(
    staging: &Path,
    repo: &Path,
    wake_branch: &str,
    base_sha: &str,
    finalize: WakeWorkspaceFinalize,
) -> Result<CoreGitWorkspaceFinalization, String> {
    // 1. Commit any working-tree changes in the staging clone.
    let status = git_output(staging, &["status", "--porcelain"])
        .await
        .map_err(|stderr| format!("git status --porcelain: {stderr}"))?;
    let committed = if status.trim().is_empty() {
        false
    } else {
        git_output(staging, &["add", "-A"])
            .await
            .map_err(|stderr| format!("git add -A: {stderr}"))?;
        let message = match finalize {
            WakeWorkspaceFinalize::CommitAll => {
                "chore(proxima): record wake workspace changes"
            }
            WakeWorkspaceFinalize::LeaveDirty => {
                "chore(proxima): record wake workspace changes [WIP - leave_dirty]"
            }
        };
        git_output(
            staging,
            &[
                "-c",
                "user.name=Proxima Wake",
                "-c",
                "user.email=wake@proxima.local",
                "commit",
                "-m",
                message,
            ],
        )
        .await
        .map_err(|stderr| format!("git commit: {stderr}"))?;
        true
    };

    // 2. Return the wake branch to the real repo. A clone has its own
    //    object store, so unlike a worktree this fetch is required; the
    //    real repo's working tree and current branch are untouched.
    let staging_arg = staging.to_string_lossy().to_string();
    let refspec = format!("{wake_branch}:refs/heads/{wake_branch}");
    git_output(repo, &["fetch", staging_arg.as_str(), refspec.as_str()])
        .await
        .map_err(|stderr| format!("git fetch wake branch: {stderr}"))?;

    // 3. Diff stat of the committed branch against its base, read from the
    //    real repo now that the branch lives there.
    let head_sha = git_output(repo, &["rev-parse", wake_branch])
        .await
        .map_err(|stderr| format!("git rev-parse {wake_branch}: {stderr}"))?;
    let numstat = git_output(repo, &["diff", "--numstat", base_sha, wake_branch])
        .await
        .map_err(|stderr| format!("git diff --numstat {base_sha} {wake_branch}: {stderr}"))?;
    let diff_stat = parse_numstat(&numstat);

    // 4. Discard the disposable clone.
    std::fs::remove_dir_all(staging)
        .map_err(|err| format!("remove staging clone {staging_arg}: {err}"))?;

    Ok(CoreGitWorkspaceFinalization {
        head_sha,
        committed,
        diff_stat,
    })
}

pub(super) fn parse_numstat(numstat: &str) -> CoreWorkspaceDiffStat {
    let mut files = Vec::new();
    let mut insertions = 0_u64;
    let mut deletions = 0_u64;
    for line in numstat.lines().filter(|line| !line.trim().is_empty()) {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let file_insertions = parts[0].parse::<u64>().unwrap_or(0);
        let file_deletions = parts[1].parse::<u64>().unwrap_or(0);
        insertions = insertions.saturating_add(file_insertions);
        deletions = deletions.saturating_add(file_deletions);
        files.push(CoreWorkspaceDiffFile {
            path: parts[2..].join("\t"),
            insertions: file_insertions,
            deletions: file_deletions,
        });
    }
    CoreWorkspaceDiffStat {
        files_changed: files.len() as u64,
        insertions,
        deletions,
        files,
    }
}

pub(super) async fn git_output(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal real repo with one commit; returns `(repo_dir, base_sha)`.
    async fn seed_repo(repo: &Path) -> String {
        git_output(repo, &["init", "-q"]).await.unwrap();
        git_output(repo, &["config", "user.name", "Test"])
            .await
            .unwrap();
        git_output(repo, &["config", "user.email", "test@proxima.local"])
            .await
            .unwrap();
        std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
        git_output(repo, &["add", "-A"]).await.unwrap();
        git_output(repo, &["commit", "-q", "-m", "seed"])
            .await
            .unwrap();
        git_output(repo, &["rev-parse", "HEAD"]).await.unwrap()
    }

    #[tokio::test]
    async fn wake_clone_returns_changes_as_branch_without_touching_real_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let base_sha = seed_repo(&repo).await;
        let real_head_before = git_output(&repo, &["rev-parse", "HEAD"]).await.unwrap();
        let branch = "proxima/wake/test";
        let staging = tmp.path().join("wake-clones").join("test");

        clone_repo_to_staging(&repo, &staging, branch, &base_sha)
            .await
            .unwrap();
        // The clone is a real, self-contained repo positioned on the wake branch.
        assert!(staging.join(".git").is_dir(), "clone has a real .git dir");
        assert!(staging.join("seed.txt").is_file());
        let staging_branch = git_output(&staging, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .unwrap();
        assert_eq!(staging_branch, branch);

        // The personality makes a change inside the clone.
        std::fs::write(staging.join("wake.txt"), "wake output\n").unwrap();

        let finalization =
            finalize_core_git_clone(&staging, &repo, branch, &base_sha, WakeWorkspaceFinalize::CommitAll)
                .await
                .unwrap();

        assert!(finalization.committed);
        assert_eq!(finalization.diff_stat.files_changed, 1);
        assert_eq!(finalization.diff_stat.files[0].path, "wake.txt");

        // The wake branch landed in the real repo, pointing at the commit.
        let fetched = git_output(&repo, &["rev-parse", branch]).await.unwrap();
        assert_eq!(fetched, finalization.head_sha);

        // The real repo's working tree and current branch are untouched.
        assert!(!repo.join("wake.txt").exists(), "real working tree untouched");
        let real_status = git_output(&repo, &["status", "--porcelain"]).await.unwrap();
        assert!(real_status.is_empty(), "real repo working tree stays clean");
        let real_head_after = git_output(&repo, &["rev-parse", "HEAD"]).await.unwrap();
        assert_eq!(real_head_after, real_head_before, "real HEAD unmoved");

        // The disposable clone is discarded.
        assert!(!staging.exists(), "staging clone removed after finalize");
    }

    #[tokio::test]
    async fn wake_clone_with_no_changes_reports_uncommitted() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let base_sha = seed_repo(&repo).await;
        let branch = "proxima/wake/empty";
        let staging = tmp.path().join("clone");

        clone_repo_to_staging(&repo, &staging, branch, &base_sha)
            .await
            .unwrap();
        let finalization =
            finalize_core_git_clone(&staging, &repo, branch, &base_sha, WakeWorkspaceFinalize::CommitAll)
                .await
                .unwrap();

        assert!(!finalization.committed);
        assert_eq!(finalization.head_sha, base_sha, "no commit, head stays at base");
        assert_eq!(finalization.diff_stat.files_changed, 0);
    }

    #[tokio::test]
    async fn wake_clone_leave_dirty_marks_commit_as_wip() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let base_sha = seed_repo(&repo).await;
        let branch = "proxima/wake/wip";
        let staging = tmp.path().join("clone");

        clone_repo_to_staging(&repo, &staging, branch, &base_sha)
            .await
            .unwrap();
        std::fs::write(staging.join("wake.txt"), "draft\n").unwrap();
        finalize_core_git_clone(&staging, &repo, branch, &base_sha, WakeWorkspaceFinalize::LeaveDirty)
            .await
            .unwrap();

        let subject = git_output(&repo, &["log", "-1", "--format=%s", branch])
            .await
            .unwrap();
        assert!(subject.contains("WIP"), "leave_dirty commit subject marks WIP: {subject}");
    }
}
