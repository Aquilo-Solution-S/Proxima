//! Phase 1d: `LocalCliGooseAdapter` — shells `goose run --recipe ...`
//! per spec lines 410-432. Maps subprocess exit + a small stderr scan
//! to [`TargetOutcomeKind`].
//!
//! - Recipe params are JSON-serialised and passed as `--params K=V`.
//! - `--max-turns` is the WakeEntry-level override; `max_rounds = 0`
//!   omits the flag and leaves Goose uncapped by turn count.
//! - `--no-session` keeps wake runs ephemeral/non-resumable.
//! - Env is cleared and only the engine-supplied vars + `PATH` flow
//!   through, so inherited dev-shell creds don't leak into the LLM loop.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

use super::{
    TargetAdapter, TargetAdapterError, TargetInvocation, TargetOutcome, TargetOutcomeKind,
};

const STREAM_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

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
        let (logger, log_open_error) = SessionLogger::open(invocation.session_log_path.as_deref())
            .await
            .unwrap_or_else(|err| {
                (
                    SessionLogger::disabled(),
                    Some(format!("open session log: {err}")),
                )
            });
        let logger = Arc::new(logger);
        logger
            .write(serde_json::json!({
                "record": "start",
                "invocation_id": invocation.invocation_id,
                "personality_instance_id": invocation.personality_instance_id,
                "wake_entry_id": invocation.wake_entry_id,
                "change_event_seq": invocation.change_event_seq,
                "cwd": invocation.cwd,
                "recipe_path": invocation.recipe_path,
                "max_rounds": invocation.max_rounds,
                "argv": redacted_argv(&invocation),
                "param_keys": sorted_keys(&invocation.params),
                "env_keys": sorted_keys(&invocation.env),
                "session_log_open_error": log_open_error.clone(),
            }))
            .await;

        let mut cmd = Command::new(&self.binary);
        cmd.arg("run").arg("--recipe").arg(&invocation.recipe_path);

        for (key, value) in &invocation.params {
            let serialized = serde_json::to_string(value).unwrap_or_default();
            cmd.arg("--params").arg(format!("{key}={serialized}"));
        }
        if invocation.max_rounds > 0 {
            cmd.arg("--max-turns")
                .arg(invocation.max_rounds.to_string());
        }
        cmd.arg("--no-session");
        cmd.arg("--output-format").arg("stream-json");
        cmd.arg("--debug");
        if let Some(cwd) = invocation.cwd.as_ref() {
            cmd.current_dir(cwd);
        }

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
        let mut child = cmd
            .spawn()
            .map_err(|e| TargetAdapterError::SpawnFailed { source: e })?;
        let stdout = child.stdout.take().ok_or_else(|| TargetAdapterError::Io {
            source: std::io::Error::other("child stdout not piped"),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| TargetAdapterError::Io {
            source: std::io::Error::other("child stderr not piped"),
        })?;
        let stdout_capture = Arc::new(StreamCapture::default());
        let stderr_capture = Arc::new(StreamCapture::default());
        let stdout_task = tokio::spawn(read_stream(
            "stdout",
            BufReader::new(stdout),
            Arc::clone(&logger),
            Arc::clone(&stdout_capture),
        ));
        let stderr_task = tokio::spawn(read_stream(
            "stderr",
            BufReader::new(stderr),
            Arc::clone(&logger),
            Arc::clone(&stderr_capture),
        ));

        let status = match timeout(invocation.timeout, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(e)) => return Err(TargetAdapterError::Io { source: e }),
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let stream_drain_errors = stream_drain_errors([
                    drain_stream_task("stdout", stdout_task).await,
                    drain_stream_task("stderr", stderr_task).await,
                ]);
                let stdout_full = stdout_capture.snapshot().await;
                let stderr_full = stderr_capture.snapshot().await;
                let (_, stdout_truncated) = tail_lines(&stdout_full, 80);
                let (_, stderr_truncated) = tail_lines(&stderr_full, 80);
                logger
                    .write(serde_json::json!({
                        "record": "finish",
                        "outcome": "timeout",
                        "turn_count": null,
                        "exit_code": null,
                        "duration_ms": duration_ms,
                        "stdout_truncated": stdout_truncated,
                        "stderr_truncated": stderr_truncated,
                        "stream_drain_errors": stream_drain_errors,
                    }))
                    .await;
                return Err(TargetAdapterError::Timeout {
                    timeout: invocation.timeout,
                });
            }
        };
        let stream_drain_errors = stream_drain_errors([
            drain_stream_task("stdout", stdout_task).await,
            drain_stream_task("stderr", stderr_task).await,
        ]);
        let stdout_full = stdout_capture.snapshot().await;
        let mut stderr_full = stderr_capture.snapshot().await;
        append_stream_drain_errors(&mut stderr_full, &stream_drain_errors);

        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (stdout_tail, stdout_truncated) = tail_lines(&stdout_full, 80);
        let (stderr_tail, stderr_truncated) = tail_lines(&stderr_full, 80);
        let turn_count = parse_turn_count(&stderr_full).or_else(|| parse_turn_count(&stdout_full));

        let truncated =
            output_indicates_turn_limit(&stdout_full) || output_indicates_turn_limit(&stderr_full);
        let kind = if status.success() {
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
        logger
            .write(serde_json::json!({
                "record": "finish",
                "outcome": target_outcome_kind_str(kind),
                "turn_count": turn_count,
                "exit_code": status.code(),
                "duration_ms": duration_ms,
                "stdout_truncated": stdout_truncated,
                "stderr_truncated": stderr_truncated,
            }))
            .await;

        Ok(TargetOutcome {
            kind,
            turn_count,
            exit_code: status.code(),
            duration_ms,
            stdout_tail,
            stderr_tail,
            stdout_truncated,
            stderr_truncated,
            session_log_error: log_open_error,
        })
    }
}

#[derive(Debug)]
struct SessionLogger {
    file: tokio::sync::Mutex<Option<tokio::fs::File>>,
}

impl SessionLogger {
    async fn open(path: Option<&Path>) -> Result<(Self, Option<String>), std::io::Error> {
        let Some(path) = path else {
            return Ok((Self::disabled(), None));
        };
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .await?;
        Ok((
            Self {
                file: tokio::sync::Mutex::new(Some(file)),
            },
            None,
        ))
    }

    fn disabled() -> Self {
        Self {
            file: tokio::sync::Mutex::new(None),
        }
    }

    async fn write(&self, record: serde_json::Value) {
        let mut guard = self.file.lock().await;
        let Some(file) = guard.as_mut() else {
            return;
        };
        let Ok(mut line) = serde_json::to_vec(&record) else {
            return;
        };
        line.push(b'\n');
        let _ = file.write_all(&line).await;
        let _ = file.flush().await;
    }
}

#[derive(Debug, Default)]
struct StreamCapture {
    text: tokio::sync::Mutex<String>,
}

impl StreamCapture {
    async fn push_line(&self, line: &str) {
        let mut text = self.text.lock().await;
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(line);
    }

    async fn snapshot(&self) -> String {
        self.text.lock().await.clone()
    }
}

async fn read_stream<R>(
    record: &'static str,
    reader: R,
    logger: Arc<SessionLogger>,
    capture: Arc<StreamCapture>,
) -> Result<(), std::io::Error>
where
    R: AsyncBufRead + Unpin,
{
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await? {
        capture.push_line(&line).await;
        let parsed = serde_json::from_str::<serde_json::Value>(&line).ok();
        logger
            .write(serde_json::json!({
                "record": record,
                "line": line,
                "parsed": parsed,
            }))
            .await;
    }
    Ok(())
}

async fn drain_stream_task(
    record: &'static str,
    mut task: tokio::task::JoinHandle<Result<(), std::io::Error>>,
) -> Option<String> {
    match timeout(STREAM_DRAIN_TIMEOUT, &mut task).await {
        Ok(Ok(Ok(()))) => None,
        Ok(Ok(Err(source))) => Some(format!("{record} stream read failed: {source}")),
        Ok(Err(err)) => Some(format!("{record} stream task join failed: {err}")),
        Err(_) => {
            task.abort();
            Some(format!(
                "{record} stream did not close within {STREAM_DRAIN_TIMEOUT:?}"
            ))
        }
    }
}

fn stream_drain_errors(items: [Option<String>; 2]) -> Vec<String> {
    items.into_iter().flatten().collect()
}

fn append_stream_drain_errors(stderr: &mut String, errors: &[String]) {
    for error in errors {
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str(error);
    }
}

fn redacted_argv(invocation: &TargetInvocation) -> Vec<String> {
    let mut argv = vec![
        "run".to_string(),
        "--recipe".to_string(),
        invocation.recipe_path.display().to_string(),
    ];
    for key in sorted_keys(&invocation.params) {
        argv.push("--params".to_string());
        argv.push(format!("{key}=<redacted>"));
    }
    if invocation.max_rounds > 0 {
        argv.push("--max-turns".to_string());
        argv.push(invocation.max_rounds.to_string());
    }
    argv.extend([
        "--no-session".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--debug".to_string(),
    ]);
    argv
}

fn sorted_keys<T>(map: &std::collections::HashMap<String, T>) -> Vec<String> {
    let mut keys: Vec<String> = map.keys().cloned().collect();
    keys.sort();
    keys
}

fn target_outcome_kind_str(kind: TargetOutcomeKind) -> &'static str {
    match kind {
        TargetOutcomeKind::Succeeded => "succeeded",
        TargetOutcomeKind::Truncated => "truncated",
        TargetOutcomeKind::Failed => "failed",
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

fn output_indicates_turn_limit(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("turn limit")
        || lower.contains("--max-turns reached")
        || lower.contains("maximum number of actions")
        || lower.contains("max actions")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    #[test]
    fn target_invocation_carries_optional_cwd() {
        let inv_no_cwd = TargetInvocation {
            recipe_path: PathBuf::from("/tmp/nope"),
            params: HashMap::new(),
            max_rounds: 1,
            env: HashMap::new(),
            timeout: Duration::from_secs(1),
            cwd: None,
            session_log_path: None,
            invocation_id: None,
            personality_instance_id: None,
            wake_entry_id: None,
            change_event_seq: None,
        };
        assert!(inv_no_cwd.cwd.is_none());

        let inv_with_cwd = TargetInvocation {
            recipe_path: PathBuf::from("/tmp/nope"),
            params: HashMap::new(),
            max_rounds: 1,
            env: HashMap::new(),
            timeout: Duration::from_secs(1),
            cwd: Some(PathBuf::from("/tmp/some-worktree")),
            session_log_path: None,
            invocation_id: None,
            personality_instance_id: None,
            wake_entry_id: None,
            change_event_seq: None,
        };
        assert_eq!(
            inv_with_cwd.cwd.as_deref(),
            Some(std::path::Path::new("/tmp/some-worktree")),
        );
    }

    #[test]
    fn goose_max_actions_message_counts_as_turn_limit() {
        assert!(output_indicates_turn_limit(
            "I've reached the maximum number of actions I can do without user input."
        ));
    }

    #[tokio::test]
    async fn drain_stream_task_times_out_and_aborts_reader() {
        let task =
            tokio::spawn(async { std::future::pending::<Result<(), std::io::Error>>().await });

        let started = Instant::now();
        let error = drain_stream_task("stdout", task)
            .await
            .expect("drain timeout");

        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(error.contains("stdout stream did not close within"));
    }
}
