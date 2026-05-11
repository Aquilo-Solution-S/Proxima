use std::collections::BTreeSet;
use std::path::{Component, Path};
use std::process::Stdio;
use std::time::Instant;

use proxima_core::{WorkspacePrepareInput, WorkspaceRunnerError};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::process::Command;
use uuid::Uuid;

use super::RunnerRepoRow;

const MAX_PRELOADED_FILES: usize = 3;
const MAX_PRELOADED_FILE_BYTES: u64 = 24 * 1024;
const MAX_PRELOADED_TOTAL_BYTES: u64 = 48 * 1024;
const TOOL_OUTPUT_TAIL_BYTES: usize = 4 * 1024;

pub(super) async fn build_workspace_context(
    input: &WorkspacePrepareInput<'_>,
    repo_id: Uuid,
    repo: &RunnerRepoRow,
    target_branch: &str,
    parent_sha: &str,
    branch_name: &str,
    worktree_path: &Path,
    tooling: serde_json::Value,
) -> Result<serde_json::Value, WorkspaceRunnerError> {
    let instructions = input
        .triggering_memory_payload
        .get("instructions")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let mentioned_paths = extract_mentioned_paths(instructions);
    let preloaded_files = preload_mentioned_files(worktree_path, &mentioned_paths).await?;
    Ok(json!({
        "repo_id": repo_id.to_string(),
        "canonical_path": repo.canonical_path,
        "target_branch": target_branch,
        "worktree_path": worktree_path.to_string_lossy(),
        "branch_name": branch_name,
        "parent_sha": parent_sha,
        "request_memory_id": input.triggering_memory_id.into_inner().to_string(),
        "request_key": input
            .triggering_memory_payload
            .get("request_key")
            .and_then(serde_json::Value::as_str),
        "tooling": tooling,
        "mentioned_paths": mentioned_paths,
        "preloaded_files": preloaded_files,
    }))
}

pub(super) async fn hydrate_workspace_tooling(
    worktree_path: &Path,
    pnpm_store_root: &Path,
    pnpm_executable: &Path,
) -> serde_json::Value {
    let pnpm_lock = worktree_path.join("pnpm-lock.yaml");
    if tokio::fs::metadata(&pnpm_lock).await.is_err() {
        return json!({
            "frontend": {
                "pnpm": {
                    "status": "skipped",
                    "reason": "no_pnpm_lock",
                },
            },
        });
    }

    let started = Instant::now();
    let store_dir = pnpm_store_root.to_string_lossy().to_string();
    if let Err(err) = tokio::fs::create_dir_all(pnpm_store_root).await {
        return json!({
            "frontend": {
                "pnpm": {
                    "status": "failed",
                    "reason": "create_store_dir_failed",
                    "store_dir": store_dir,
                    "duration_ms": duration_ms(started),
                    "stderr_tail": err.to_string(),
                },
            },
        });
    }

    let output = Command::new(pnpm_executable)
        .args([
            "install",
            "--frozen-lockfile",
            "--prefer-offline",
            "--store-dir",
            &store_dir,
        ])
        .current_dir(worktree_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;
    match output {
        Ok(output) => json!({
            "frontend": {
                "pnpm": {
                    "status": if output.status.success() { "succeeded" } else { "failed" },
                    "command": [
                        "pnpm",
                        "install",
                        "--frozen-lockfile",
                        "--prefer-offline",
                        "--store-dir",
                        store_dir,
                    ],
                    "store_dir": store_dir,
                    "exit_code": output.status.code(),
                    "duration_ms": duration_ms(started),
                    "stdout_tail": utf8_tail(&output.stdout, TOOL_OUTPUT_TAIL_BYTES),
                    "stderr_tail": utf8_tail(&output.stderr, TOOL_OUTPUT_TAIL_BYTES),
                },
            },
        }),
        Err(err) => json!({
            "frontend": {
                "pnpm": {
                    "status": "failed",
                    "reason": "spawn_failed",
                    "command": [
                        "pnpm",
                        "install",
                        "--frozen-lockfile",
                        "--prefer-offline",
                        "--store-dir",
                        store_dir,
                    ],
                    "store_dir": store_dir,
                    "duration_ms": duration_ms(started),
                    "stderr_tail": err.to_string(),
                },
            },
        }),
    }
}

fn duration_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn utf8_tail(bytes: &[u8], limit: usize) -> String {
    let start = bytes.len().saturating_sub(limit);
    String::from_utf8_lossy(&bytes[start..]).trim().to_string()
}

fn extract_mentioned_paths(instructions: &str) -> Vec<String> {
    let mut paths = BTreeSet::new();
    let mut rest = instructions;
    while let Some(start) = rest.find('`') {
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('`') else {
            break;
        };
        maybe_insert_path(&mut paths, after_start[..end].trim());
        rest = &after_start[end + 1..];
    }

    for token in instructions.split_whitespace() {
        let candidate = token.trim_matches(|ch: char| {
            matches!(
                ch,
                ',' | '.' | ':' | ';' | '!' | '?' | ')' | '(' | '[' | ']' | '{' | '}' | '"' | '\''
            )
        });
        maybe_insert_path(&mut paths, candidate);
    }

    paths.into_iter().take(64).collect()
}

fn maybe_insert_path(paths: &mut BTreeSet<String>, candidate: &str) {
    if candidate.is_empty()
        || candidate.starts_with('/')
        || candidate.starts_with('-')
        || candidate.contains("://")
        || candidate.contains('\n')
        || candidate.len() > 240
    {
        return;
    }
    let looks_like_path = candidate.contains('/')
        || Path::new(candidate)
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|ext| (1..=8).contains(&ext.len()));
    if !looks_like_path || !is_safe_relative_path(candidate) {
        return;
    }
    paths.insert(candidate.to_string());
}

fn is_safe_relative_path(candidate: &str) -> bool {
    let path = Path::new(candidate);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

async fn preload_mentioned_files(
    worktree_path: &Path,
    mentioned_paths: &[String],
) -> Result<serde_json::Value, WorkspaceRunnerError> {
    let mut files = Vec::new();
    let mut omitted = Vec::new();
    let mut total_bytes = 0u64;

    for relative in mentioned_paths {
        let path = Path::new(relative);
        if !is_safe_relative_path(relative) {
            continue;
        }
        let full_path = worktree_path.join(path);
        let metadata = match tokio::fs::metadata(&full_path).await {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => continue,
        };
        let size = metadata.len();
        if files.len() >= MAX_PRELOADED_FILES {
            omitted.push(json!({
                "path": relative,
                "size_bytes": size,
                "reason": "file_count_limit",
            }));
            continue;
        }
        if size > MAX_PRELOADED_FILE_BYTES {
            omitted.push(json!({
                "path": relative,
                "size_bytes": size,
                "reason": "file_too_large",
            }));
            continue;
        }
        if total_bytes.saturating_add(size) > MAX_PRELOADED_TOTAL_BYTES {
            omitted.push(json!({
                "path": relative,
                "size_bytes": size,
                "reason": "total_bytes_limit",
            }));
            continue;
        }
        let bytes = tokio::fs::read(&full_path).await.map_err(|err| {
            WorkspaceRunnerError::PrepareFailed(format!("read mentioned file {relative}: {err}"))
        })?;
        total_bytes = total_bytes.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        files.push(json!({
            "path": relative,
            "size_bytes": size,
            "line_count": String::from_utf8_lossy(&bytes).lines().count(),
            "sha256": hex::encode(Sha256::digest(&bytes)),
            "content": String::from_utf8_lossy(&bytes),
        }));
    }

    Ok(json!({
        "files": files,
        "omitted": omitted,
        "limits": {
            "max_files": MAX_PRELOADED_FILES,
            "max_file_bytes": MAX_PRELOADED_FILE_BYTES,
            "max_total_bytes": MAX_PRELOADED_TOTAL_BYTES,
        },
    }))
}
