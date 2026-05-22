//! `workspace_shell`: bounded shell execution in a prepared workspace.

use std::path::{Path, PathBuf};
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
    let sandbox = ShellSandbox::from_env()?;
    let exec = shell_exec_spec(&args.command, &ctx.workspace_root, &sandbox)?;

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
enum ShellSandbox {
    Host,
    Docker(DockerSandbox),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DockerSandbox {
    docker: String,
    image: String,
    network: String,
    memory: String,
    cpus: String,
    pids_limit: String,
}

impl ShellSandbox {
    fn from_env() -> Result<Self, WorkspaceToolError> {
        let mode = std::env::var("PROXIMA_WORKSPACE_SHELL_SANDBOX").unwrap_or_default();
        match mode.as_str() {
            "" | "host" => Ok(Self::Host),
            "docker" => {
                let image =
                    std::env::var("PROXIMA_WORKSPACE_SHELL_DOCKER_IMAGE").map_err(|_| {
                        WorkspaceToolError::InvalidArgs(
                            "PROXIMA_WORKSPACE_SHELL_DOCKER_IMAGE is required when PROXIMA_WORKSPACE_SHELL_SANDBOX=docker".into(),
                        )
                    })?;
                Ok(Self::Docker(DockerSandbox {
                    docker: std::env::var("PROXIMA_WORKSPACE_SHELL_DOCKER_BIN")
                        .unwrap_or_else(|_| "docker".into()),
                    image,
                    network: std::env::var("PROXIMA_WORKSPACE_SHELL_DOCKER_NETWORK")
                        .unwrap_or_else(|_| "none".into()),
                    memory: std::env::var("PROXIMA_WORKSPACE_SHELL_DOCKER_MEMORY")
                        .unwrap_or_else(|_| "2g".into()),
                    cpus: std::env::var("PROXIMA_WORKSPACE_SHELL_DOCKER_CPUS")
                        .unwrap_or_else(|_| "2".into()),
                    pids_limit: std::env::var("PROXIMA_WORKSPACE_SHELL_DOCKER_PIDS_LIMIT")
                        .unwrap_or_else(|_| "256".into()),
                }))
            }
            other => Err(WorkspaceToolError::InvalidArgs(format!(
                "unsupported PROXIMA_WORKSPACE_SHELL_SANDBOX {other:?}; expected host or docker"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellExecSpec {
    program: String,
    args: Vec<String>,
    env_allowlist: &'static [&'static str],
    sandbox_label: String,
}

fn shell_exec_spec(
    command: &str,
    workspace_root: &Path,
    sandbox: &ShellSandbox,
) -> Result<ShellExecSpec, WorkspaceToolError> {
    match sandbox {
        ShellSandbox::Host => Ok(ShellExecSpec {
            program: "bash".into(),
            args: vec!["-lc".into(), command.into()],
            env_allowlist: &["PATH", "HOME", "USER", "LANG", "TERM"],
            sandbox_label: "host".into(),
        }),
        ShellSandbox::Docker(config) => docker_exec_spec(command, workspace_root, config),
    }
}

fn docker_exec_spec(
    command: &str,
    workspace_root: &Path,
    config: &DockerSandbox,
) -> Result<ShellExecSpec, WorkspaceToolError> {
    let root = workspace_root
        .canonicalize()
        .map_err(|err| WorkspaceToolError::Io(format!("canonicalize workspace root: {err}")))?;
    let mount = bind_mount_arg(&root);
    Ok(ShellExecSpec {
        program: config.docker.clone(),
        args: vec![
            "run".into(),
            "--rm".into(),
            "--init".into(),
            "--pull=never".into(),
            "--network".into(),
            config.network.clone(),
            "--memory".into(),
            config.memory.clone(),
            "--cpus".into(),
            config.cpus.clone(),
            "--pids-limit".into(),
            config.pids_limit.clone(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            "--cap-drop".into(),
            "ALL".into(),
            "--tmpfs".into(),
            "/tmp:rw,nosuid,nodev,size=256m".into(),
            "-e".into(),
            "HOME=/tmp".into(),
            "-e".into(),
            "CI=true".into(),
            "-v".into(),
            mount,
            "-w".into(),
            WORKSPACE_MOUNT.into(),
            config.image.clone(),
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
        sandbox_label: format!("docker:{}", config.image),
    })
}

fn bind_mount_arg(root: &PathBuf) -> String {
    format!("{}:{WORKSPACE_MOUNT}:rw", root.display())
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
    use super::{DockerSandbox, ShellSandbox, WORKSPACE_MOUNT, docker_exec_spec, shell_exec_spec};

    #[test]
    fn host_spec_runs_bash_lc_in_workspace() {
        let root = tempfile::tempdir().unwrap();
        let spec = shell_exec_spec("echo ok", root.path(), &ShellSandbox::Host).unwrap();

        assert_eq!(spec.program, "bash");
        assert_eq!(spec.args, vec!["-lc", "echo ok"]);
        assert_eq!(spec.sandbox_label, "host");
    }

    #[test]
    fn docker_spec_is_networkless_and_mounts_only_workspace() {
        let root = tempfile::tempdir().unwrap();
        let config = DockerSandbox {
            docker: "docker".into(),
            image: "proxima-sandbox:local".into(),
            network: "none".into(),
            memory: "3g".into(),
            cpus: "4".into(),
            pids_limit: "128".into(),
        };

        let spec = docker_exec_spec("cargo test", root.path(), &config).unwrap();

        assert_eq!(spec.program, "docker");
        assert_eq!(spec.sandbox_label, "docker:proxima-sandbox:local");
        assert!(
            spec.args
                .windows(2)
                .any(|pair| pair == ["--network", "none"])
        );
        assert!(
            spec.args
                .windows(2)
                .any(|pair| pair == ["--pull=never", "--network"])
        );
        assert!(
            spec.args
                .windows(2)
                .any(|pair| pair == ["--security-opt", "no-new-privileges"])
        );
        assert!(
            spec.args
                .windows(2)
                .any(|pair| pair == ["--cap-drop", "ALL"])
        );
        assert!(spec.args.windows(2).any(|pair| pair[0] == "-v"
            && pair[1].ends_with(&format!(":{WORKSPACE_MOUNT}:rw"))));
        assert!(
            spec.args
                .windows(2)
                .any(|pair| pair == ["-w", WORKSPACE_MOUNT])
        );
        assert_eq!(spec.args.last().unwrap(), "cargo test");
    }
}
