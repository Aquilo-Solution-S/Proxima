//! `workspace_list_files`: cwd-rooted listing of file entries.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{WorkspaceCtx, WorkspaceToolError, jail_path};

const ENTRY_CAP: usize = 500;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListFilesArgs {
    #[serde(default = "default_path")]
    #[schemars(
        description = "Relative directory path inside the prepared workspace. Defaults to `.`; absolute paths and `..` are rejected."
    )]
    pub path: String,
    #[serde(default)]
    #[schemars(description = "Whether to include dotfiles and `.git` entries. Defaults to false.")]
    pub include_hidden: bool,
    #[serde(default = "default_recursive")]
    #[schemars(
        description = "Whether to recurse into subdirectories. Defaults to false; output is capped at 500 entries."
    )]
    pub recursive: bool,
}

fn default_path() -> String {
    ".".into()
}

const fn default_recursive() -> bool {
    false
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListEntry {
    pub path: String,
    pub kind: &'static str,
    pub size_bytes: Option<u64>,
}

/// List workspace entries under a jailed path.
///
/// # Errors
///
/// Returns [`WorkspaceToolError`] when args are invalid, a path escapes the
/// workspace, or directory traversal fails.
#[expect(
    clippy::unused_async,
    reason = "workspace tools share an async dispatch signature"
)]
pub async fn run(args: Value, ctx: &WorkspaceCtx) -> Result<Value, WorkspaceToolError> {
    let args: ListFilesArgs =
        serde_json::from_value(args).map_err(|e| WorkspaceToolError::InvalidArgs(e.to_string()))?;
    let canon_root = ctx
        .workspace_root
        .canonicalize()
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    let base = jail_path(&ctx.workspace_root, &args.path)?;
    let mut out = Vec::new();
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
    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !include_hidden && (name_str == ".git" || name_str.starts_with('.')) {
            continue;
        }
        if out.len() >= ENTRY_CAP {
            *truncated = true;
            break;
        }

        let file_type = entry.file_type()?;
        let kind = if file_type.is_dir() {
            "dir"
        } else if file_type.is_symlink() {
            "symlink"
        } else {
            "file"
        };
        let entry_path = entry.path();
        let rel = entry_path
            .strip_prefix(root)
            .unwrap_or(&entry_path)
            .to_string_lossy()
            .into_owned();
        let size_bytes = if file_type.is_file() {
            entry.metadata().ok().map(|m| m.len())
        } else {
            None
        };
        out.push(ListEntry {
            path: rel,
            kind,
            size_bytes,
        });
        if recursive && file_type.is_dir() {
            walk(&entry_path, root, include_hidden, recursive, out, truncated)?;
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
