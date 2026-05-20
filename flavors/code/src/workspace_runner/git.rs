use std::path::Path;
use std::process::Stdio;

use proxima_core::{Owner, WorkspaceRunnerError};
use serde_json::json;
use tokio::process::Command;

use crate::payloads::{WorkspaceDiffFile, WorkspaceDiffStat, WorkspaceRunV1};

const REVIEW_DIFF_MAX_BYTES: usize = 96 * 1024;

pub(super) async fn ensure_worktree_head(
    worktree_path: &Path,
    branch_name: &str,
    head_sha: &str,
) -> Result<(), WorkspaceRunnerError> {
    if tokio::fs::metadata(worktree_path).await.is_err() {
        return Err(WorkspaceRunnerError::PrepareFailed(format!(
            "workspace worktree missing: {}",
            worktree_path.display()
        )));
    }
    if let Err(stderr) = git_output(worktree_path, &["checkout", branch_name]).await {
        git_output(worktree_path, &["checkout", head_sha])
            .await
            .map_err(|head_stderr| {
                WorkspaceRunnerError::PrepareFailed(format!(
                    "checkout {branch_name} failed: {stderr}; checkout {head_sha} failed: {head_stderr}"
                ))
            })?;
    }
    git_output(worktree_path, &["reset", "--hard", head_sha])
        .await
        .map_err(|stderr| {
            WorkspaceRunnerError::PrepareFailed(format!("reset worktree: {stderr}"))
        })?;
    Ok(())
}

pub(super) async fn build_review_diff_context(
    worktree_path: &Path,
    run: &WorkspaceRunV1,
) -> Result<serde_json::Value, WorkspaceRunnerError> {
    let range = format!("{}..{}", run.parent_sha, run.head_sha);
    let stat = git_output(worktree_path, &["diff", "--stat", &range])
        .await
        .map_err(|stderr| WorkspaceRunnerError::PrepareFailed(format!("diff --stat: {stderr}")))?;
    let name_only = git_output(worktree_path, &["diff", "--name-only", &range])
        .await
        .map_err(|stderr| {
            WorkspaceRunnerError::PrepareFailed(format!("diff --name-only: {stderr}"))
        })?;
    let patch = git_output(worktree_path, &["diff", "--unified=80", &range])
        .await
        .map_err(|stderr| {
            WorkspaceRunnerError::PrepareFailed(format!("diff --unified=80: {stderr}"))
        })?;
    let (patch, patch_truncated) = truncate_utf8(patch, REVIEW_DIFF_MAX_BYTES);
    Ok(json!({
        "range": range,
        "stat": stat,
        "name_only": name_only
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>(),
        "patch": patch,
        "patch_truncated": patch_truncated,
        "max_patch_bytes": REVIEW_DIFF_MAX_BYTES,
    }))
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

pub(super) async fn git_output(cwd: &Path, args: &[&str]) -> Result<String, String> {
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

pub(super) async fn commit_all_candidate(
    worktree: &Path,
    triggering_memory_id: proxima_core::MemoryId,
    invocation_id: uuid::Uuid,
) -> Result<Option<String>, WorkspaceRunnerError> {
    let status = git_output(worktree, &["status", "--porcelain"])
        .await
        .map_err(|stderr| {
            WorkspaceRunnerError::FinalizeFailed(format!("git status --porcelain: {stderr}"))
        })?;
    if status.trim().is_empty() {
        return Ok(None);
    }
    git_output(worktree, &["add", "-A"])
        .await
        .map_err(|stderr| WorkspaceRunnerError::FinalizeFailed(format!("git add -A: {stderr}")))?;
    let triggering_memory = triggering_memory_id.into_inner().to_string();
    let invocation = invocation_id.to_string();
    git_output(
        worktree,
        &[
            "-c",
            "user.name=Proxima Worker",
            "-c",
            "user.email=worker@proxima.local",
            "commit",
            "-m",
            "proxima worker candidate",
            "-m",
            &format!("Triggering-Memory: {triggering_memory}"),
            "-m",
            &format!("Wake-Invocation: {invocation}"),
        ],
    )
    .await
    .map_err(|stderr| {
        WorkspaceRunnerError::FinalizeFailed(format!("git commit candidate: {stderr}"))
    })?;
    let head_sha = git_output(worktree, &["rev-parse", "HEAD"])
        .await
        .map_err(|stderr| {
            WorkspaceRunnerError::FinalizeFailed(format!("rev-parse committed HEAD: {stderr}"))
        })?;
    Ok(Some(head_sha))
}

pub(super) async fn diff_stat(
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

pub(super) fn owner_component(owner: &Owner) -> String {
    match &owner.principal {
        proxima_core::Principal::User(user) => user.into_inner().to_string(),
        proxima_core::Principal::Group(group) => group.into_inner().to_string(),
    }
}
