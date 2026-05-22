//! Workspace tools: cwd-jailed to a prepared worktree.

use async_trait::async_trait;
use serde_json::Value;

use super::WorkspaceCtx;

pub mod list_files;
pub mod sandbox;
pub mod shell;
pub mod text_editor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceToolName {
    Shell,
    TextEditor,
    ListFiles,
}

impl WorkspaceToolName {
    #[must_use]
    pub fn canonical(self) -> &'static str {
        match self {
            Self::Shell => "workspace_shell",
            Self::TextEditor => "workspace_text_editor",
            Self::ListFiles => "workspace_list_files",
        }
    }

    #[must_use]
    pub fn from_canonical(s: &str) -> Option<Self> {
        match s {
            "workspace_shell" => Some(Self::Shell),
            "workspace_text_editor" => Some(Self::TextEditor),
            "workspace_list_files" => Some(Self::ListFiles),
            _ => None,
        }
    }

    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::Shell => "Run a bounded bash command in the prepared workspace.",
            Self::TextEditor => "View or edit text files inside the prepared workspace.",
            Self::ListFiles => "List files and directories inside the prepared workspace.",
        }
    }

    #[must_use]
    pub fn input_schema(self) -> Value {
        match self {
            Self::Shell => shell::args_schema(),
            Self::TextEditor => text_editor::args_schema(),
            Self::ListFiles => list_files::args_schema(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceToolError {
    #[error("invalid args: {0}")]
    InvalidArgs(String),
    #[error("path escapes workspace root: {0}")]
    PathEscape(String),
    #[error("io: {0}")]
    Io(String),
    #[error("timeout after {ms} ms")]
    Timeout { ms: u64 },
}

#[async_trait]
pub trait WorkspaceTool: Send + Sync {
    fn name(&self) -> WorkspaceToolName;
    fn description(&self) -> &'static str;
    fn args_schema(&self) -> Value;

    async fn run(&self, args: Value, ctx: &WorkspaceCtx) -> Result<Value, WorkspaceToolError>;
}

/// Dispatch one workspace tool by canonical enum.
///
/// # Errors
///
/// Returns [`WorkspaceToolError`] when the selected tool receives invalid
/// arguments, touches a jailed path, hits I/O failure, or times out.
pub async fn dispatch(
    name: WorkspaceToolName,
    args: Value,
    ctx: &WorkspaceCtx,
) -> Result<Value, WorkspaceToolError> {
    match name {
        WorkspaceToolName::Shell => shell::run(args, ctx).await,
        WorkspaceToolName::TextEditor => text_editor::run(args, ctx).await,
        WorkspaceToolName::ListFiles => list_files::run(args, ctx).await,
    }
}

/// Cwd-jail check. Resolves `requested` against `root` and rejects
/// anything that escapes the root, including `..` and symlinks that
/// point outside.
///
/// Existing leaves are canonicalized as full paths, so
/// `escape.rs -> /tmp/outside/secret.rs` is rejected before any read.
/// Missing leaves are checked by canonicalizing their parent, since
/// the leaf may not exist yet.
pub(crate) fn jail_path(
    root: &std::path::Path,
    requested: &str,
) -> Result<std::path::PathBuf, WorkspaceToolError> {
    let p = std::path::Path::new(requested);
    if p.is_absolute() {
        return Err(WorkspaceToolError::PathEscape(requested.into()));
    }

    let mut acc = root.to_path_buf();
    for c in p.components() {
        match c {
            std::path::Component::Normal(s) => acc.push(s),
            std::path::Component::CurDir => {}
            _ => return Err(WorkspaceToolError::PathEscape(requested.into())),
        }
    }

    let canon_root = root
        .canonicalize()
        .map_err(|e| WorkspaceToolError::Io(e.to_string()))?;
    if std::fs::symlink_metadata(&acc).is_ok() {
        let canon_acc = acc
            .canonicalize()
            .map_err(|_| WorkspaceToolError::PathEscape(requested.into()))?;
        if !canon_acc.starts_with(&canon_root) {
            return Err(WorkspaceToolError::PathEscape(requested.into()));
        }
        return Ok(canon_acc);
    }

    let parent = acc.parent().unwrap_or(acc.as_path());
    let canon_parent = parent
        .canonicalize()
        .map_err(|_| WorkspaceToolError::PathEscape(requested.into()))?;
    if !canon_parent.starts_with(&canon_root) {
        return Err(WorkspaceToolError::PathEscape(requested.into()));
    }
    let final_path = match acc.file_name() {
        Some(name) => canon_parent.join(name),
        None => canon_parent,
    };
    Ok(final_path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::{WorkspaceToolError, jail_path};
    use std::fs;
    use std::os::unix::fs::symlink;

    fn tmp_root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn accepts_relative_inside_root() {
        let root = tmp_root();
        fs::write(root.path().join("a.rs"), b"").unwrap();
        let p = jail_path(root.path(), "a.rs").unwrap();
        assert!(p.starts_with(root.path().canonicalize().unwrap()));
    }

    #[test]
    fn rejects_absolute() {
        let root = tmp_root();
        assert!(matches!(
            jail_path(root.path(), "/etc/passwd"),
            Err(WorkspaceToolError::PathEscape(_))
        ));
    }

    #[test]
    fn rejects_parent_component() {
        let root = tmp_root();
        assert!(matches!(
            jail_path(root.path(), "../outside.rs"),
            Err(WorkspaceToolError::PathEscape(_))
        ));
    }

    #[test]
    fn rejects_symlink_to_outside_existing_leaf() {
        let root = tmp_root();
        let outside = tmp_root();
        let target = outside.path().join("secret.rs");
        fs::write(&target, b"").unwrap();
        symlink(&target, root.path().join("escape.rs")).unwrap();
        assert!(matches!(
            jail_path(root.path(), "escape.rs"),
            Err(WorkspaceToolError::PathEscape(_))
        ));
    }

    #[test]
    fn rejects_new_file_below_symlinked_dir() {
        let root = tmp_root();
        let outside = tmp_root();
        symlink(outside.path(), root.path().join("link")).unwrap();
        assert!(matches!(
            jail_path(root.path(), "link/new.rs"),
            Err(WorkspaceToolError::PathEscape(_))
        ));
    }
}
