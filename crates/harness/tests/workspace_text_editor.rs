use proxima_harness::tools::WorkspaceCtx;
use proxima_harness::tools::workspace::text_editor;
use serde_json::json;
use tempfile::tempdir;

fn ctx(root: std::path::PathBuf) -> WorkspaceCtx {
    WorkspaceCtx {
        workspace_root: root,
        sandbox_session: None,
    }
}

#[tokio::test]
async fn create_writes_file_and_returns_summary() {
    let tmp = tempdir().unwrap();
    let r = text_editor::run(
        json!({"op":"create","path":"a.txt","file_text":"hello\nworld\n"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    assert_eq!(r["op"], "create");
    assert_eq!(r["line_count"], 2);
    let content = std::fs::read_to_string(tmp.path().join("a.txt")).unwrap();
    assert_eq!(content, "hello\nworld\n");
}

#[tokio::test]
async fn view_returns_lines() {
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "1\n2\n3\n").unwrap();
    let r = text_editor::run(
        json!({"op":"view","path":"a.txt"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    assert_eq!(r["content"], "1\n2\n3\n");
}

#[tokio::test]
async fn view_range_returns_selected_lines() {
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "1\n2\n3\n").unwrap();
    let r = text_editor::run(
        json!({"op":"view","path":"a.txt","view_range":[2,3]}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    assert_eq!(r["content"], "2\n3");
}

#[tokio::test]
async fn str_replace_errors_when_old_str_not_unique() {
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "x\nx\n").unwrap();
    let err = text_editor::run(
        json!({"op":"str_replace","path":"a.txt","old_str":"x","new_str":"y"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("not unique") || msg.contains("multiple"));
}

#[tokio::test]
async fn path_traversal_dot_dot_rejected() {
    let tmp = tempdir().unwrap();
    let err = text_editor::run(
        json!({"op":"view","path":"../etc/passwd"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap_err();
    assert!(format!("{err}").contains("escapes"));
}

#[tokio::test]
async fn absolute_path_rejected() {
    let tmp = tempdir().unwrap();
    let err = text_editor::run(
        json!({"op":"view","path":"/etc/passwd"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap_err();
    assert!(format!("{err}").contains("escapes"));
}

#[tokio::test]
async fn insert_at_line_works() {
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "a\nb\nc\n").unwrap();
    let _ = text_editor::run(
        json!({"op":"insert","path":"a.txt","insert_line":1,"new_str":"INSERTED"}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(tmp.path().join("a.txt")).unwrap();
    assert_eq!(content, "a\nINSERTED\nb\nc\n");
}
