# Task 3.3 — `workspace_text_editor`

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: `crates/harness/src/tools/workspace/text_editor.rs`
- Create: `crates/harness/tests/workspace_text_editor.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/harness/tests/workspace_text_editor.rs`:

```rust
use proxima_harness::tools::WorkspaceCtx;
use proxima_harness::tools::workspace::text_editor;
use serde_json::json;
use tempfile::tempdir;

fn ctx(root: std::path::PathBuf) -> WorkspaceCtx {
    WorkspaceCtx { workspace_root: root }
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
```

Run: `cargo test -p proxima-harness --test workspace_text_editor`
Expected: FAIL — unimplemented.

- [ ] **Step 2: Implement `text_editor::run`**

Replace `crates/harness/src/tools/workspace/text_editor.rs`:

```rust
//! workspace_text_editor: view | create | str_replace | insert,
//! cwd-jailed.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::fs;

use super::{WorkspaceCtx, WorkspaceToolError, jail_path};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum TextEditorArgs {
    View {
        path: String,
        #[serde(default)]
        view_range: Option<[u32; 2]>,
    },
    Create {
        path: String,
        file_text: String,
    },
    StrReplace {
        path: String,
        old_str: String,
        new_str: String,
    },
    Insert {
        path: String,
        insert_line: u32,
        new_str: String,
    },
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TextEditorResult {
    pub op: &'static str,
    pub path: String,
    pub line_count: u32,
    pub content: Option<String>,
}

pub async fn run(args: Value, ctx: &WorkspaceCtx) -> Result<Value, WorkspaceToolError> {
    let parsed: TextEditorArgs = serde_json::from_value(args)
        .map_err(|e| WorkspaceToolError::InvalidArgs(e.to_string()))?;
    match parsed {
        TextEditorArgs::View { path, view_range } => view(ctx, &path, view_range).await,
        TextEditorArgs::Create { path, file_text } => create(ctx, &path, &file_text).await,
        TextEditorArgs::StrReplace {
            path,
            old_str,
            new_str,
        } => str_replace(ctx, &path, &old_str, &new_str).await,
        TextEditorArgs::Insert {
            path,
            insert_line,
            new_str,
        } => insert(ctx, &path, insert_line, &new_str).await,
    }
}

async fn view(
    ctx: &WorkspaceCtx,
    path: &str,
    view_range: Option<[u32; 2]>,
) -> Result<Value, WorkspaceToolError> {
    let p = jail_path(&ctx.workspace_root, path)?;
    let content = fs::read_to_string(&p)
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let line_count = u32::try_from(content.lines().count()).unwrap_or(u32::MAX);
    let trimmed = match view_range {
        Some([start, end]) => content
            .lines()
            .skip((start.saturating_sub(1)) as usize)
            .take((end.saturating_sub(start.saturating_sub(1))) as usize)
            .collect::<Vec<_>>()
            .join("\n"),
        None => content,
    };
    Ok(json!({"op":"view","path":path,"line_count":line_count,"content":trimmed}))
}

async fn create(
    ctx: &WorkspaceCtx,
    path: &str,
    file_text: &str,
) -> Result<Value, WorkspaceToolError> {
    let p = jail_path(&ctx.workspace_root, path)?;
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    }
    fs::write(&p, file_text)
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let line_count = u32::try_from(file_text.lines().count()).unwrap_or(u32::MAX);
    Ok(json!({"op":"create","path":path,"line_count":line_count}))
}

async fn str_replace(
    ctx: &WorkspaceCtx,
    path: &str,
    old_str: &str,
    new_str: &str,
) -> Result<Value, WorkspaceToolError> {
    let p = jail_path(&ctx.workspace_root, path)?;
    let content = fs::read_to_string(&p)
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let occurrences = content.matches(old_str).count();
    if occurrences == 0 {
        return Err(WorkspaceToolError::InvalidArgs(format!(
            "old_str not found in {path}"
        )));
    }
    if occurrences > 1 {
        return Err(WorkspaceToolError::InvalidArgs(format!(
            "old_str not unique in {path} (found {occurrences} occurrences)"
        )));
    }
    let replaced = content.replacen(old_str, new_str, 1);
    fs::write(&p, &replaced)
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let line_count = u32::try_from(replaced.lines().count()).unwrap_or(u32::MAX);
    Ok(json!({"op":"str_replace","path":path,"line_count":line_count}))
}

async fn insert(
    ctx: &WorkspaceCtx,
    path: &str,
    insert_line: u32,
    new_str: &str,
) -> Result<Value, WorkspaceToolError> {
    let p = jail_path(&ctx.workspace_root, path)?;
    let content = fs::read_to_string(&p)
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let mut lines: Vec<&str> = content.split_inclusive('\n').collect();
    let idx = (insert_line as usize).min(lines.len());
    let prefix: String = lines.drain(..idx).collect();
    let suffix: String = lines.into_iter().collect();
    let needs_nl = !new_str.ends_with('\n');
    let inserted = if needs_nl {
        format!("{prefix}{new_str}\n{suffix}")
    } else {
        format!("{prefix}{new_str}{suffix}")
    };
    fs::write(&p, &inserted)
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let line_count = u32::try_from(inserted.lines().count()).unwrap_or(u32::MAX);
    Ok(json!({"op":"insert","path":path,"line_count":line_count}))
}

#[must_use]
pub fn args_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(TextEditorArgs)).unwrap_or(Value::Null)
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p proxima-harness --test workspace_text_editor`
Expected: all 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/harness/src/tools/workspace/text_editor.rs crates/harness/tests/workspace_text_editor.rs
git commit -m "harness: workspace_text_editor with cwd-jail and unique-match enforcement"
```

