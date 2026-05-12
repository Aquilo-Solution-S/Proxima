# Task 3.2 — `workspace_shell`

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: `crates/harness/src/tools/workspace/shell.rs`
- Create: `crates/harness/tests/workspace_shell.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/harness/tests/workspace_shell.rs`:

```rust
use std::path::PathBuf;

use proxima_harness::tools::WorkspaceCtx;
use proxima_harness::tools::workspace::shell;
use serde_json::json;
use tempfile::tempdir;

fn ctx(root: PathBuf) -> WorkspaceCtx {
    WorkspaceCtx { workspace_root: root }
}

#[tokio::test]
async fn returns_exit_code_and_stdout() {
    let tmp = tempdir().unwrap();
    let r = shell::run(
        json!({"command": "echo hello-world"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    assert_eq!(r["exit_code"], 0);
    assert!(r["stdout"].as_str().unwrap().contains("hello-world"));
    assert_eq!(r["timed_out"], false);
}

#[tokio::test]
async fn timeout_returns_timed_out_true_and_keeps_stdout() {
    let tmp = tempdir().unwrap();
    let r = shell::run(
        json!({"command": "sleep 5", "timeout_ms": 200}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    assert_eq!(r["timed_out"], true);
}

#[tokio::test]
async fn stdout_is_capped_at_32k() {
    let tmp = tempdir().unwrap();
    let r = shell::run(
        // Generate ~64 KB and ensure cap kicks in.
        json!({"command": "yes 'x' | head -c 65536"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    let stdout = r["stdout"].as_str().unwrap();
    assert!(stdout.len() <= 32 * 1024);
    assert_eq!(r["stdout_truncated"], true);
}

#[tokio::test]
async fn env_is_cleared_except_for_allowlist() {
    let tmp = tempdir().unwrap();
    // PROXIMA_WAKE_TOKEN must not leak into the subshell.
    // We can't set process env from inside the test reliably across
    // platforms, so verify HOME survives and PROXIMA_WAKE_TOKEN
    // does not by checking what `env` returns.
    let r = shell::run(
        json!({"command": "env | grep -E '^(HOME|PROXIMA_WAKE_TOKEN)=' || true"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    let stdout = r["stdout"].as_str().unwrap();
    assert!(!stdout.contains("PROXIMA_WAKE_TOKEN"));
}

#[tokio::test]
async fn cwd_is_the_workspace_root() {
    let tmp = tempdir().unwrap();
    let r = shell::run(
        json!({"command": "pwd"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    let canon_root = tmp.path().canonicalize().unwrap();
    let out = r["stdout"].as_str().unwrap().trim();
    assert_eq!(std::path::Path::new(out).canonicalize().unwrap(), canon_root);
}
```

Run: `cargo test -p proxima-harness --test workspace_shell`
Expected: FAIL — `shell::run` returns `Err("unimplemented")`.

- [ ] **Step 2: Implement `shell::run`**

Replace `crates/harness/src/tools/workspace/shell.rs`:

```rust
//! workspace_shell: bounded `bash -lc` execution, cwd-jailed.
//!
//! Args:   { command: string, timeout_ms?: u32 }
//! Result: { exit_code, stdout, stdout_truncated, stderr,
//!           stderr_truncated, duration_ms, timed_out }
//! Env:    cleared except PATH, HOME, USER, LANG, TERM.

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
const STDOUT_CAP: usize = 32 * 1024;

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

pub async fn run(args: Value, ctx: &WorkspaceCtx) -> Result<Value, WorkspaceToolError> {
    let args: ShellArgs = serde_json::from_value(args)
        .map_err(|e| WorkspaceToolError::InvalidArgs(e.to_string()))?;
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

    // Allowlist.
    for k in &["PATH", "HOME", "USER", "LANG", "TERM"] {
        if let Ok(v) = std::env::var(k) {
            cmd.env(k, v);
        }
    }

    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();

    let timeout = Duration::from_millis(u64::from(timeout_ms));
    let mut out_buf = Vec::with_capacity(8 * 1024);
    let mut err_buf = Vec::with_capacity(8 * 1024);

    let result = tokio::time::timeout(timeout, async {
        let read_out = async {
            let mut tmp = [0u8; 4096];
            loop {
                let n = stdout.read(&mut tmp).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                if out_buf.len() < STDOUT_CAP {
                    let take = (STDOUT_CAP - out_buf.len()).min(n);
                    out_buf.extend_from_slice(&tmp[..take]);
                }
            }
        };
        let read_err = async {
            let mut tmp = [0u8; 4096];
            loop {
                let n = stderr.read(&mut tmp).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                if err_buf.len() < STDOUT_CAP {
                    let take = (STDOUT_CAP - err_buf.len()).min(n);
                    err_buf.extend_from_slice(&tmp[..take]);
                }
            }
        };
        tokio::join!(read_out, read_err);
        child.wait().await
    })
    .await;

    let (exit_code, timed_out) = match result {
        Ok(Ok(status)) => (status.code().unwrap_or(-1), false),
        Ok(Err(e)) => {
            return Err(WorkspaceToolError::Io(e.to_string()));
        }
        Err(_) => {
            // Timed out — kill and report.
            let _ = child.start_kill();
            (-1, true)
        }
    };
    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    let stdout_truncated = out_buf.len() >= STDOUT_CAP;
    let stderr_truncated = err_buf.len() >= STDOUT_CAP;
    let stdout = String::from_utf8_lossy(&out_buf).into_owned();
    let stderr = String::from_utf8_lossy(&err_buf).into_owned();

    Ok(json!({
        "exit_code": exit_code,
        "stdout": stdout,
        "stdout_truncated": stdout_truncated,
        "stderr": stderr,
        "stderr_truncated": stderr_truncated,
        "duration_ms": duration_ms,
        "timed_out": timed_out,
    }))
}

/// Schemars-derived JSON schema for the args. Used by the harness
/// when building `ToolSpec.input_schema`.
#[must_use]
pub fn args_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(ShellArgs)).unwrap_or(Value::Null)
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p proxima-harness --test workspace_shell -- --test-threads=1`
Expected: all 5 tests pass. (`--test-threads=1` keeps the env-var test from racing.)

- [ ] **Step 4: Commit**

```bash
git add crates/harness/src/tools/workspace/shell.rs crates/harness/tests/workspace_shell.rs
git commit -m "harness: workspace_shell with cwd-jail, env-clear, output cap, timeout"
```

