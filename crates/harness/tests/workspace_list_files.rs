use proxima_harness::tools::WorkspaceCtx;
use proxima_harness::tools::workspace::list_files;
use serde_json::json;
use tempfile::tempdir;

fn ctx(root: std::path::PathBuf) -> WorkspaceCtx {
    WorkspaceCtx {
        workspace_root: root,
    }
}

#[tokio::test]
async fn lists_top_level() {
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "x").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub/b.txt"), "y").unwrap();
    let r = list_files::run(json!({"path":"."}), &ctx(tmp.path().to_path_buf()))
        .await
        .unwrap();
    let entries = r["entries"].as_array().unwrap();
    let names: Vec<&str> = entries.iter().filter_map(|e| e["path"].as_str()).collect();
    assert!(names.contains(&"a.txt"));
    assert!(names.contains(&"sub"));
    assert_eq!(r["truncated"], false);
}

#[tokio::test]
async fn skips_hidden_dot_git_by_default() {
    let tmp = tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    std::fs::write(tmp.path().join("a.txt"), "x").unwrap();
    let r = list_files::run(json!({"path":"."}), &ctx(tmp.path().to_path_buf()))
        .await
        .unwrap();
    let names: Vec<&str> = r["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["path"].as_str())
        .collect();
    assert!(!names.iter().any(|n| n.starts_with(".git")));
    assert_eq!(r["truncated"], false);
}

#[tokio::test]
async fn recursive_listing_is_capped_at_500_entries() {
    let tmp = tempdir().unwrap();
    let sub = tmp.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    for i in 0..550 {
        std::fs::write(sub.join(format!("{i:03}.txt")), "x").unwrap();
    }

    let r = list_files::run(
        json!({"path":".", "recursive": true}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();

    assert_eq!(r["entries"].as_array().unwrap().len(), 500);
    assert_eq!(r["truncated"], true);
}

#[tokio::test]
async fn include_hidden_lists_dotfiles() {
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join(".env"), "x").unwrap();
    let r = list_files::run(
        json!({"path":".", "include_hidden": true}),
        &ctx(tmp.path().to_path_buf()),
    )
    .await
    .unwrap();
    let names: Vec<&str> = r["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["path"].as_str())
        .collect();
    assert!(names.contains(&".env"));
}

#[tokio::test]
async fn path_traversal_rejected() {
    let tmp = tempdir().unwrap();
    let err = list_files::run(json!({"path":"../"}), &ctx(tmp.path().to_path_buf()))
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("escapes"));
}
