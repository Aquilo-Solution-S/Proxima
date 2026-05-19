use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::payloads::{
    VerificationArtifactRefsV1, VerificationEvidenceStatus, VerificationEvidenceV1,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct BrowserAssertion {
    pub selector: String,
    pub expected_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeterministicCheck {
    FileExists {
        path: String,
    },
    Command {
        command: Vec<String>,
        timeout_ms: u64,
    },
    BrowserSmoke {
        entrypoint: String,
        assertions: Vec<BrowserAssertion>,
    },
    DiffScope {
        allowed_prefixes: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub criterion_key: String,
    pub status: VerificationEvidenceStatus,
    pub summary: String,
    pub artifact_refs: VerificationArtifactRefsV1,
}

impl CheckOutcome {
    #[must_use]
    pub fn into_evidence(
        self,
        workspace_run_memory_id: uuid::Uuid,
        execution_request_memory_id: uuid::Uuid,
    ) -> VerificationEvidenceV1 {
        VerificationEvidenceV1 {
            workspace_run_memory_id,
            execution_request_memory_id,
            criterion_key: self.criterion_key,
            status: self.status,
            summary: self.summary,
            artifact_refs: self.artifact_refs,
        }
    }
}

pub async fn run_check(
    worktree: &Path,
    criterion_key: &str,
    check: &DeterministicCheck,
) -> CheckOutcome {
    match check {
        DeterministicCheck::FileExists { path } => file_exists(worktree, criterion_key, path),
        DeterministicCheck::Command {
            command,
            timeout_ms,
        } => command_check(worktree, criterion_key, command, *timeout_ms).await,
        DeterministicCheck::BrowserSmoke { entrypoint, .. } => {
            browser_smoke(worktree, criterion_key, entrypoint)
        }
        DeterministicCheck::DiffScope { allowed_prefixes } => {
            diff_scope(worktree, criterion_key, allowed_prefixes).await
        }
    }
}

fn file_exists(worktree: &Path, criterion_key: &str, path: &str) -> CheckOutcome {
    let relative = clean_relative_path(path);
    let exists = relative
        .as_ref()
        .is_some_and(|path| worktree.join(path).is_file());
    CheckOutcome {
        criterion_key: criterion_key.into(),
        status: if exists {
            VerificationEvidenceStatus::Passed
        } else {
            VerificationEvidenceStatus::Failed
        },
        summary: if exists {
            format!("file exists: {path}")
        } else {
            format!("file missing: {path}")
        },
        artifact_refs: VerificationArtifactRefsV1 {
            path: Some(path.into()),
            ..Default::default()
        },
    }
}

async fn command_check(
    worktree: &Path,
    criterion_key: &str,
    command: &[String],
    timeout_ms: u64,
) -> CheckOutcome {
    if command.is_empty() {
        return CheckOutcome {
            criterion_key: criterion_key.into(),
            status: VerificationEvidenceStatus::Failed,
            summary: "command check has empty argv".into(),
            artifact_refs: VerificationArtifactRefsV1 {
                command: command.to_vec(),
                ..Default::default()
            },
        };
    }
    let mut cmd = Command::new(&command[0]);
    cmd.args(&command[1..]).current_dir(worktree);
    let timeout = Duration::from_millis(timeout_ms.max(1));
    let result = tokio::time::timeout(timeout, cmd.output()).await;
    match result {
        Ok(Ok(output)) if output.status.success() => CheckOutcome {
            criterion_key: criterion_key.into(),
            status: VerificationEvidenceStatus::Passed,
            summary: format!("command passed: {}", command.join(" ")),
            artifact_refs: VerificationArtifactRefsV1 {
                command: command.to_vec(),
                stdout_tail: Some(tail(&output.stdout)),
                stderr_tail: Some(tail(&output.stderr)),
                ..Default::default()
            },
        },
        Ok(Ok(output)) => CheckOutcome {
            criterion_key: criterion_key.into(),
            status: VerificationEvidenceStatus::Failed,
            summary: format!("command failed: {}", command.join(" ")),
            artifact_refs: VerificationArtifactRefsV1 {
                command: command.to_vec(),
                exit_code: output.status.code(),
                stdout_tail: Some(tail(&output.stdout)),
                stderr_tail: Some(tail(&output.stderr)),
                ..Default::default()
            },
        },
        Ok(Err(err)) => CheckOutcome {
            criterion_key: criterion_key.into(),
            status: VerificationEvidenceStatus::Failed,
            summary: format!("command failed to start: {err}"),
            artifact_refs: VerificationArtifactRefsV1 {
                command: command.to_vec(),
                ..Default::default()
            },
        },
        Err(_) => CheckOutcome {
            criterion_key: criterion_key.into(),
            status: VerificationEvidenceStatus::Failed,
            summary: format!("command timed out after {timeout_ms}ms"),
            artifact_refs: VerificationArtifactRefsV1 {
                command: command.to_vec(),
                ..Default::default()
            },
        },
    }
}

fn browser_smoke(worktree: &Path, criterion_key: &str, entrypoint: &str) -> CheckOutcome {
    let local_file = clean_relative_path(entrypoint)
        .as_ref()
        .is_some_and(|path| worktree.join(path).is_file());
    let local_url = entrypoint.starts_with("http://127.0.0.1")
        || entrypoint.starts_with("http://localhost")
        || entrypoint.starts_with("file://");
    let passed = local_file || local_url;
    CheckOutcome {
        criterion_key: criterion_key.into(),
        status: if passed {
            VerificationEvidenceStatus::Passed
        } else {
            VerificationEvidenceStatus::Failed
        },
        summary: if passed {
            format!("browser smoke entrypoint is local: {entrypoint}")
        } else {
            format!("browser smoke entrypoint is not local or missing: {entrypoint}")
        },
        artifact_refs: VerificationArtifactRefsV1 {
            entrypoint: Some(entrypoint.into()),
            ..Default::default()
        },
    }
}

async fn diff_scope(
    worktree: &Path,
    criterion_key: &str,
    allowed_prefixes: &[String],
) -> CheckOutcome {
    let output = Command::new("git")
        .args(["diff", "--name-only", "main"])
        .current_dir(worktree)
        .output()
        .await;
    let Ok(output) = output else {
        return CheckOutcome {
            criterion_key: criterion_key.into(),
            status: VerificationEvidenceStatus::Failed,
            summary: "git diff scope check failed to start".into(),
            artifact_refs: VerificationArtifactRefsV1 {
                allowed_prefixes: allowed_prefixes.to_vec(),
                ..Default::default()
            },
        };
    };
    let files = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let passed = output.status.success()
        && files.iter().all(|file| {
            allowed_prefixes
                .iter()
                .any(|prefix| file == prefix || file.starts_with(prefix))
        });
    CheckOutcome {
        criterion_key: criterion_key.into(),
        status: if passed {
            VerificationEvidenceStatus::Passed
        } else {
            VerificationEvidenceStatus::Failed
        },
        summary: if passed {
            "changed files are within allowed prefixes".into()
        } else {
            "changed files exceed allowed prefixes".into()
        },
        artifact_refs: VerificationArtifactRefsV1 {
            allowed_prefixes: allowed_prefixes.to_vec(),
            changed_files: files,
            ..Default::default()
        },
    }
}

fn clean_relative_path(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        None
    } else {
        Some(path.to_path_buf())
    }
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let chars = text.chars().collect::<Vec<_>>();
    let start = chars.len().saturating_sub(4000);
    chars[start..].iter().collect()
}
