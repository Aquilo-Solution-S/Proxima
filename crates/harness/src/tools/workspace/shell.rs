//! `workspace_shell`: bounded shell execution in a prepared workspace.

use std::process::Stdio;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::sandbox::{self, WorkspaceSandboxSession};
use super::{WorkspaceCtx, WorkspaceToolError};

const DEFAULT_TIMEOUT_MS: u32 = 30_000;
const MAX_TIMEOUT_MS: u32 = 120_000;
const STREAM_CAP: usize = 32 * 1024;
const WORKSPACE_MOUNT: &str = "/workspace";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShellArgs {
    #[schemars(
        description = "Shell command to run as `bash -lc` in the prepared workspace root. Use relative paths inside the workspace; output is capped."
    )]
    pub command: String,
    #[serde(default)]
    #[schemars(
        description = "Optional timeout in milliseconds. Omit for the default 30000 ms; values above 120000 ms are clamped."
    )]
    pub timeout_ms: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ShellResult {
    pub exit_code: i32,
    pub sandbox: String,
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
    let exec = shell_exec_spec(&args.command, ctx);

    let mut cmd = Command::new(&exec.program);
    cmd.args(&exec.args)
        .current_dir(&ctx.workspace_root)
        .env_clear()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    for key in exec.env_allowlist {
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
        sandbox: exec.sandbox_label,
        stdout: String::from_utf8_lossy(&stdout.bytes).into_owned(),
        stdout_truncated: stdout.truncated,
        stderr: String::from_utf8_lossy(&stderr.bytes).into_owned(),
        stderr_truncated: stderr.truncated,
        duration_ms,
        timed_out,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellExecSpec {
    program: String,
    args: Vec<String>,
    env_allowlist: &'static [&'static str],
    sandbox_label: String,
}

/// Build the exec spec for one shell command.
///
/// With a per-wake sandbox session the command runs via `docker exec` inside
/// the wake's observation container. Without one — the host escape hatch,
/// `PROXIMA_WORKSPACE_SANDBOX=host` — it runs as a host `bash -lc`. The
/// per-command `docker run` path is gone: the per-wake session is the only
/// docker signal, so there is no second source of truth.
fn shell_exec_spec(command: &str, ctx: &WorkspaceCtx) -> ShellExecSpec {
    match &ctx.sandbox_session {
        Some(session) => docker_exec_spec(command, session),
        None => ShellExecSpec {
            program: "bash".into(),
            args: vec!["-lc".into(), command.into()],
            env_allowlist: &["PATH", "HOME", "USER", "LANG", "TERM"],
            sandbox_label: "host".into(),
        },
    }
}

/// `docker exec` into the running per-wake observation container.
fn docker_exec_spec(command: &str, session: &WorkspaceSandboxSession) -> ShellExecSpec {
    ShellExecSpec {
        program: sandbox::docker_bin(),
        args: vec![
            "exec".into(),
            "-w".into(),
            WORKSPACE_MOUNT.into(),
            "-e".into(),
            "HOME=/tmp".into(),
            "-e".into(),
            "CI=true".into(),
            session.container_name.clone(),
            "bash".into(),
            "-lc".into(),
            command.into(),
        ],
        // These variables reach only the Docker CLI, not the container. They
        // allow Docker Desktop or a remote daemon to be selected by the host.
        env_allowlist: &[
            "PATH",
            "HOME",
            "DOCKER_HOST",
            "DOCKER_CONTEXT",
            "DOCKER_CONFIG",
        ],
        sandbox_label: format!("docker:{}", session.image),
    }
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

#[cfg(test)]
mod tests {
    use super::{WORKSPACE_MOUNT, WorkspaceSandboxSession, shell_exec_spec};
    use crate::tools::WorkspaceCtx;

    fn ctx(session: Option<WorkspaceSandboxSession>) -> WorkspaceCtx {
        WorkspaceCtx {
            workspace_root: std::path::PathBuf::from("/tmp/x"),
            sandbox_session: session,
        }
    }

    fn session() -> WorkspaceSandboxSession {
        WorkspaceSandboxSession {
            container_name: "proxima-wake-abc".into(),
            network_name: "proxima-wake-net-abc".into(),
            image: "proxima-workspace-sandbox:local".into(),
            label: "proxima.wake=abc".into(),
        }
    }

    #[test]
    fn host_spec_runs_bash_lc_when_no_sandbox_session() {
        let spec = shell_exec_spec("echo ok", &ctx(None));

        assert_eq!(spec.program, "bash");
        assert_eq!(spec.args, vec!["-lc", "echo ok"]);
        assert_eq!(spec.sandbox_label, "host");
    }

    #[test]
    fn sandbox_spec_runs_docker_exec_into_the_wake_container() {
        let spec = shell_exec_spec("cargo test", &ctx(Some(session())));

        assert_eq!(spec.args.first().unwrap(), "exec");
        assert_eq!(spec.sandbox_label, "docker:proxima-workspace-sandbox:local");
        assert!(
            spec.args
                .windows(2)
                .any(|pair| pair == ["-w", WORKSPACE_MOUNT])
        );
        assert!(spec.args.contains(&"proxima-wake-abc".to_string()));
        assert!(spec.args.windows(2).any(|pair| pair == ["bash", "-lc"]));
        assert_eq!(spec.args.last().unwrap(), "cargo test");
        // The per-command `docker run` path is gone — exec into the
        // already-running per-wake container, never spawn a fresh one.
        assert!(!spec.args.iter().any(|arg| arg == "run"));
        assert!(!spec.args.iter().any(|arg| arg == "--rm"));
    }
}
