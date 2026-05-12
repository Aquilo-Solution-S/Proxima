//! `workspace_shell`: bounded `bash -lc` execution, cwd-jailed.

use std::process::Stdio;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::{WorkspaceCtx, WorkspaceToolError};

const DEFAULT_TIMEOUT_MS: u32 = 30_000;
const MAX_TIMEOUT_MS: u32 = 120_000;
const STREAM_CAP: usize = 32 * 1024;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShellArgs {
    pub command: String,
    #[serde(default)]
    pub timeout_ms: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ShellResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stdout_truncated: bool,
    pub stderr: String,
    pub stderr_truncated: bool,
    pub duration_ms: u64,
    pub timed_out: bool,
}

/// Run a bounded shell command in the workspace root.
///
/// # Errors
///
/// Returns [`WorkspaceToolError`] when args are invalid, the process cannot
/// be spawned, pipes cannot be read, or process I/O fails.
pub async fn run(args: Value, ctx: &WorkspaceCtx) -> Result<Value, WorkspaceToolError> {
    let args: ShellArgs =
        serde_json::from_value(args).map_err(|e| WorkspaceToolError::InvalidArgs(e.to_string()))?;
    let timeout_ms = args
        .timeout_ms
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .min(MAX_TIMEOUT_MS);

    let mut cmd = Command::new("bash");
    cmd.arg("-lc")
        .arg(&args.command)
        .current_dir(&ctx.workspace_root)
        .env_clear()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    for key in ["PATH", "HOME", "USER", "LANG", "TERM"] {
        if let Ok(value) = std::env::var(key) {
            cmd.env(key, value);
        }
    }

    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| WorkspaceToolError::Io("stdout pipe unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| WorkspaceToolError::Io("stderr pipe unavailable".into()))?;

    let read_stdout = tokio::spawn(read_capped(stdout));
    let read_stderr = tokio::spawn(read_capped(stderr));
    let timeout = Duration::from_millis(u64::from(timeout_ms));

    let (exit_code, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => (status.code().unwrap_or(-1), false),
        Ok(Err(e)) => return Err(WorkspaceToolError::Io(e.to_string())),
        Err(_) => {
            let _ = child.kill().await;
            (-1, true)
        }
    };

    let stdout = read_stdout
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let stderr = read_stderr
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    Ok(json!(ShellResult {
        exit_code,
        stdout: String::from_utf8_lossy(&stdout.bytes).into_owned(),
        stdout_truncated: stdout.truncated,
        stderr: String::from_utf8_lossy(&stderr.bytes).into_owned(),
        stderr_truncated: stderr.truncated,
        duration_ms,
        timed_out,
    }))
}

struct CappedRead {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_capped<R>(mut reader: R) -> CappedRead
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(8 * 1024);
    let mut truncated = false;
    let mut tmp = [0_u8; 4096];
    loop {
        let Ok(n) = reader.read(&mut tmp).await else {
            truncated = true;
            break;
        };
        if n == 0 {
            break;
        }
        if bytes.len() < STREAM_CAP {
            let take = (STREAM_CAP - bytes.len()).min(n);
            bytes.extend_from_slice(&tmp[..take]);
            if take < n {
                truncated = true;
            }
        } else {
            truncated = true;
        }
    }
    CappedRead { bytes, truncated }
}

#[must_use]
pub fn args_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(ShellArgs)).unwrap_or(Value::Null)
}
