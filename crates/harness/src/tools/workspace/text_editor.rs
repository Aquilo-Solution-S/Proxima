//! `workspace_text_editor`: `view` | `create` | `str_replace` | `insert`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::fs;

use super::{WorkspaceCtx, WorkspaceToolError, jail_path};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TextEditorArgs {
    #[schemars(
        description = "Workspace text-editor operation: `view`, `create`, `str_replace`, or `insert`."
    )]
    pub op: TextEditorOp,
    #[schemars(
        description = "Relative path inside the prepared workspace. Absolute paths, `..`, and symlink escapes are rejected."
    )]
    pub path: String,
    #[serde(default)]
    #[schemars(
        description = "Optional 1-based inclusive line range for `view`, as `[start, end]`. Omit or null to view the whole file."
    )]
    pub view_range: Option<[u32; 2]>,
    #[serde(default)]
    #[schemars(
        description = "Optional full file text for `create`; required only when `op` is `create`."
    )]
    pub file_text: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional exact text to replace for `str_replace`; required only when `op` is `str_replace` and must match once."
    )]
    pub old_str: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional replacement or insertion text. Required for `str_replace` and `insert`; omit or null for other ops."
    )]
    pub new_str: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional 0-based insertion line for `insert`; required only when `op` is `insert`."
    )]
    pub insert_line: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextEditorOp {
    View,
    Create,
    StrReplace,
    Insert,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TextEditorResult {
    pub op: &'static str,
    pub path: String,
    pub line_count: u32,
    pub content: Option<String>,
}

/// Execute one text-editor operation inside the workspace root.
///
/// # Errors
///
/// Returns [`WorkspaceToolError`] when args are invalid, a path escapes the
/// workspace, I/O fails, or `str_replace` does not match exactly once.
pub async fn run(args: Value, ctx: &WorkspaceCtx) -> Result<Value, WorkspaceToolError> {
    let parsed: TextEditorArgs =
        serde_json::from_value(args).map_err(|e| WorkspaceToolError::InvalidArgs(e.to_string()))?;
    match parsed.op {
        TextEditorOp::View => view(ctx, &parsed.path, parsed.view_range).await,
        TextEditorOp::Create => {
            let file_text = required_field(parsed.file_text, "file_text", "create")?;
            create(ctx, &parsed.path, &file_text).await
        }
        TextEditorOp::StrReplace => {
            let old_str = required_field(parsed.old_str, "old_str", "str_replace")?;
            let new_str = required_field(parsed.new_str, "new_str", "str_replace")?;
            str_replace(ctx, &parsed.path, &old_str, &new_str).await
        }
        TextEditorOp::Insert => {
            let insert_line = parsed.insert_line.ok_or_else(|| {
                WorkspaceToolError::InvalidArgs(
                    "insert_line is required for workspace_text_editor op insert".into(),
                )
            })?;
            let new_str = required_field(parsed.new_str, "new_str", "insert")?;
            insert(ctx, &parsed.path, insert_line, &new_str).await
        }
    }
}

fn required_field(
    value: Option<String>,
    field: &str,
    op: &str,
) -> Result<String, WorkspaceToolError> {
    value.ok_or_else(|| {
        WorkspaceToolError::InvalidArgs(format!(
            "{field} is required for workspace_text_editor op {op}"
        ))
    })
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
    let line_count = line_count(&content);
    let content = match view_range {
        Some([start, end]) => {
            if start == 0 || end < start {
                return Err(WorkspaceToolError::InvalidArgs(
                    "view_range must be [start, end] with start >= 1 and end >= start".into(),
                ));
            }
            content
                .lines()
                .skip((start - 1) as usize)
                .take((end - start + 1) as usize)
                .collect::<Vec<_>>()
                .join("\n")
        }
        None => content,
    };
    Ok(json!(TextEditorResult {
        op: "view",
        path: path.into(),
        line_count,
        content: Some(content),
    }))
}

async fn create(
    ctx: &WorkspaceCtx,
    path: &str,
    file_text: &str,
) -> Result<Value, WorkspaceToolError> {
    let p = jail_path(&ctx.workspace_root, path)?;
    fs::write(&p, file_text)
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    Ok(json!(TextEditorResult {
        op: "create",
        path: path.into(),
        line_count: line_count(file_text),
        content: None,
    }))
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
    Ok(json!(TextEditorResult {
        op: "str_replace",
        path: path.into(),
        line_count: line_count(&replaced),
        content: None,
    }))
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
    let inserted = if new_str.ends_with('\n') {
        format!("{prefix}{new_str}{suffix}")
    } else {
        format!("{prefix}{new_str}\n{suffix}")
    };
    fs::write(&p, &inserted)
        .await
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    Ok(json!(TextEditorResult {
        op: "insert",
        path: path.into(),
        line_count: line_count(&inserted),
        content: None,
    }))
}

fn line_count(content: &str) -> u32 {
    u32::try_from(content.lines().count()).unwrap_or(u32::MAX)
}

#[must_use]
pub fn args_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(TextEditorArgs)).unwrap_or(Value::Null)
}
