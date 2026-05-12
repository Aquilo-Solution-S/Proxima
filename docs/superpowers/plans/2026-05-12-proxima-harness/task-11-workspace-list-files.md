# Task 3.4 — `workspace_list_files`

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: `crates/harness/src/tools/workspace/list_files.rs`
- Create: `crates/harness/tests/workspace_list_files.rs`

- [ ] **Step 1: Write failing tests**

```rust
use proxima_harness::tools::WorkspaceCtx;
use proxima_harness::tools::workspace::list_files;
use serde_json::json;
use tempfile::tempdir;

fn ctx(root: std::path::PathBuf) -> WorkspaceCtx {
    WorkspaceCtx { workspace_root: root }
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
async fn path_traversal_rejected() {
    let tmp = tempdir().unwrap();
    let err = list_files::run(json!({"path":"../"}), &ctx(tmp.path().to_path_buf()))
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("escapes"));
}
```

Run: `cargo test -p proxima-harness --test workspace_list_files`
Expected: FAIL.

- [ ] **Step 2: Implement**

```rust
//! workspace_list_files: cwd-rooted listing of file entries.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{WorkspaceCtx, WorkspaceToolError, jail_path};

const ENTRY_CAP: usize = 500;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListFilesArgs {
    #[serde(default = "default_path")]
    pub path: String,
    #[serde(default)]
    pub include_hidden: bool,
    #[serde(default = "default_recursive")]
    pub recursive: bool,
}

fn default_path() -> String { ".".into() }
fn default_recursive() -> bool { false }

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListEntry {
    pub path: String,
    pub kind: &'static str, // "file" | "dir" | "symlink"
    pub size_bytes: Option<u64>,
}

pub async fn run(args: Value, ctx: &WorkspaceCtx) -> Result<Value, WorkspaceToolError> {
    let args: ListFilesArgs = serde_json::from_value(args)
        .map_err(|e| WorkspaceToolError::InvalidArgs(e.to_string()))?;
    let canon_root = ctx.workspace_root
        .canonicalize()
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let base = jail_path(&ctx.workspace_root, &args.path)?;
    let mut out: Vec<ListEntry> = Vec::new();
    let mut truncated = false;
    walk(
        &base,
        &canon_root,
        args.include_hidden,
        args.recursive,
        &mut out,
        &mut truncated,
    )
    .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    Ok(json!({"entries": out, "truncated": truncated}))
}

fn walk(
    dir: &std::path::Path,
    root: &std::path::Path,
    include_hidden: bool,
    recursive: bool,
    out: &mut Vec<ListEntry>,
    truncated: &mut bool,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !include_hidden && (name_str == ".git" || name_str.starts_with('.')) {
            continue;
        }
        if out.len() >= ENTRY_CAP {
            *truncated = true;
            break;
        }
        let ft = entry.file_type()?;
        let kind = if ft.is_dir() {
            "dir"
        } else if ft.is_symlink() {
            "symlink"
        } else {
            "file"
        };
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(&entry.path())
            .to_string_lossy()
            .into_owned();
        let size_bytes = if ft.is_file() {
            entry.metadata().ok().map(|m| m.len())
        } else {
            None
        };
        out.push(ListEntry { path: rel, kind, size_bytes });
        if recursive && ft.is_dir() {
            walk(
                &entry.path(),
                root,
                include_hidden,
                recursive,
                out,
                truncated,
            )?;
            if *truncated {
                break;
            }
        }
    }
    Ok(())
}

#[must_use]
pub fn args_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(ListFilesArgs)).unwrap_or(Value::Null)
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p proxima-harness --test workspace_list_files`
Expected: all 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/harness/src/tools/workspace/list_files.rs crates/harness/tests/workspace_list_files.rs
git commit -m "harness: workspace_list_files with entry cap"
```
