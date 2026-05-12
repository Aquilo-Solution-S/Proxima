use std::path::PathBuf;

use proxima_harness::tools::WorkspaceCtx;
use proxima_harness::tools::workspace::shell;
use serde_json::json;
use tempfile::tempdir;

fn ctx(root: PathBuf) -> WorkspaceCtx {
    WorkspaceCtx {
        workspace_root: root,
    }
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
async fn timeout_returns_timed_out_true() {
    let tmp = tempdir().unwrap();
    let r = shell::run(
        json!({"command": "sleep 5", "timeout_ms": 200}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    assert_eq!(r["timed_out"], true);
    assert_eq!(r["exit_code"], -1);
}

#[tokio::test]
async fn stdout_is_capped_at_32k() {
    let tmp = tempdir().unwrap();
    let r = shell::run(
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
async fn stdout_exactly_at_cap_is_not_truncated() {
    let tmp = tempdir().unwrap();
    let r = shell::run(
        json!({"command": "yes 'x' | head -c 32768"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    let stdout = r["stdout"].as_str().unwrap();
    assert_eq!(stdout.len(), 32 * 1024);
    assert_eq!(r["stdout_truncated"], false);
}

#[tokio::test]
async fn env_is_cleared_except_for_allowlist() {
    let tmp = tempdir().unwrap();
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
    let r = shell::run(json!({"command": "pwd"}), &ctx(tmp.path().to_path_buf()))
        .await
        .unwrap();
    let canon_root = tmp.path().canonicalize().unwrap();
    let out = r["stdout"].as_str().unwrap().trim();
    assert_eq!(
        std::path::Path::new(out).canonicalize().unwrap(),
        canon_root
    );
}
