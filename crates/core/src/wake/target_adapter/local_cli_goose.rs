//! Phase 1d: `LocalCliGooseAdapter` — shells `goose run --recipe ...`
//! per spec lines 410-432. Maps subprocess exit + a small stderr scan
//! to [`TargetOutcomeKind`].
//!
//! - Recipe params are JSON-serialised and passed as `--params K=V`.
//! - `--max-turns` is the WakeEntry-level override (spec §"max_rounds is
//!   the only WakeEntry-level limit override").
//! - `--no-session` keeps wake runs ephemeral/non-resumable.
//! - Env is cleared and only the engine-supplied vars + `PATH` flow
//!   through, so inherited dev-shell creds don't leak into the LLM loop.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Instant;

use async_trait::async_trait;
use tokio::process::Command;
use tokio::time::timeout;

use super::{
    TargetAdapter, TargetAdapterError, TargetInvocation, TargetOutcome, TargetOutcomeKind,
};

#[derive(Debug, Clone)]
pub struct LocalCliGooseAdapter {
    binary: PathBuf,
}

impl LocalCliGooseAdapter {
    #[must_use]
    pub fn new(binary: PathBuf) -> Self {
        Self { binary }
    }
}

#[async_trait]
impl TargetAdapter for LocalCliGooseAdapter {
    async fn run(&self, invocation: TargetInvocation) -> Result<TargetOutcome, TargetAdapterError> {
        let mut cmd = Command::new(&self.binary);
        cmd.arg("run").arg("--recipe").arg(&invocation.recipe_path);

        for (key, value) in &invocation.params {
            let serialized = serde_json::to_string(value).unwrap_or_default();
            cmd.arg("--params").arg(format!("{key}={serialized}"));
        }
        cmd.arg("--max-turns")
            .arg(invocation.max_rounds.to_string());
        cmd.arg("--no-session");

        // Clear inherited env, then apply only what the engine specified.
        cmd.env_clear();
        for (k, v) in &invocation.env {
            cmd.env(k, v);
        }
        // PATH is required for goose to find its own subprocess deps.
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", path);
        }

        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let started = Instant::now();
        let child = cmd
            .spawn()
            .map_err(|e| TargetAdapterError::SpawnFailed { source: e })?;
        let output = match timeout(invocation.timeout, child.wait_with_output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return Err(TargetAdapterError::Io { source: e }),
            Err(_) => {
                return Err(TargetAdapterError::Timeout {
                    timeout: invocation.timeout,
                });
            }
        };

        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let stdout_full = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr_full = String::from_utf8_lossy(&output.stderr).into_owned();
        let (stdout_tail, stdout_truncated) = tail_lines(&stdout_full, 80);
        let (stderr_tail, stderr_truncated) = tail_lines(&stderr_full, 80);
        let turn_count = parse_turn_count(&stderr_full).or_else(|| parse_turn_count(&stdout_full));

        let truncated =
            stderr_full.contains("turn limit") || stderr_full.contains("--max-turns reached");
        let kind = if output.status.success() {
            if truncated {
                TargetOutcomeKind::Truncated
            } else {
                TargetOutcomeKind::Succeeded
            }
        } else if truncated {
            TargetOutcomeKind::Truncated
        } else {
            TargetOutcomeKind::Failed
        };

        Ok(TargetOutcome {
            kind,
            turn_count,
            exit_code: output.status.code(),
            duration_ms,
            stdout_tail,
            stderr_tail,
            stdout_truncated,
            stderr_truncated,
        })
    }
}

fn tail_lines(s: &str, n: usize) -> (String, bool) {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    (lines[start..].join("\n"), start > 0)
}

fn parse_turn_count(s: &str) -> Option<i32> {
    let re = regex::Regex::new(r"(?:completed|after|reached)\s+(\d+)\s+turns?").ok()?;
    let caps = re.captures(s)?;
    caps.get(1)?.as_str().parse().ok()
}
