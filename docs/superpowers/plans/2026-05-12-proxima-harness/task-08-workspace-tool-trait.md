# Task 3.1 — `WorkspaceTool` trait + registry

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: `crates/harness/src/tools/mod.rs`
- Create: `crates/harness/src/tools/workspace/mod.rs`

- [ ] **Step 1: Define `ToolBinding` and the workspace trait**

Replace `crates/harness/src/tools/mod.rs`:

```rust
//! Tool surfaces the harness exposes to the model.
//!
//! Three sources:
//! - **Substrate**: wake-visible substrate tools resolved by the
//!   `HarnessSubstrateBridge`; includes registered MCP descriptors
//!   and personality substrate-pack tools.
//! - **Flavor**: same shape as substrate; the harness doesn't
//!   distinguish them at the dispatch layer.
//! - **Workspace**: Rust impls in `workspace/`; cwd-jailed to the
//!   prepared worktree.

use std::path::PathBuf;

use proxima_core::harness::SubstrateToolBinding;

pub mod substrate_dispatch;
pub mod workspace;

/// Resolved binding per tool in the active palette.
#[derive(Clone)]
pub enum ToolBinding {
    Substrate(SubstrateToolBinding),
    Workspace(workspace::WorkspaceToolName),
}

impl std::fmt::Debug for ToolBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Substrate(s) => f.debug_tuple("Substrate").field(&s.canonical_name).finish(),
            Self::Workspace(w) => f.debug_tuple("Workspace").field(w).finish(),
        }
    }
}

/// Resolved environment for workspace-tool dispatch.
#[derive(Debug, Clone)]
pub struct WorkspaceCtx {
    pub workspace_root: PathBuf,
}
```

- [ ] **Step 2: Create `workspace/mod.rs` with the trait and stub for the three tools**

```rust
//! Workspace tools: cwd-jailed to a prepared worktree.

pub mod list_files;
pub mod shell;
pub mod text_editor;

use serde_json::Value;

use super::WorkspaceCtx;

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

    pub fn from_canonical(s: &str) -> Option<Self> {
        match s {
            "workspace_shell" => Some(Self::Shell),
            "workspace_text_editor" => Some(Self::TextEditor),
            "workspace_list_files" => Some(Self::ListFiles),
            _ => None,
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
/// the leaf may not exist yet. Without the parent check, `link/new.rs`
/// where `link -> /tmp/outside` would slip through: full-path
/// canonicalization fails on the missing leaf, a lexical fallback still
/// starts with `root`, and `fs::write` follows the symlink out.
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
```

- [ ] **Step 3: Create stubs that compile**

Create `crates/harness/src/tools/workspace/shell.rs`:
```rust
//! workspace_shell — implemented in Task 3.2.
use serde_json::Value;
use super::{WorkspaceCtx, WorkspaceToolError};
pub async fn run(_args: Value, _ctx: &WorkspaceCtx) -> Result<Value, WorkspaceToolError> {
    Err(WorkspaceToolError::Io("unimplemented".into()))
}
```

Same shape for `crates/harness/src/tools/workspace/text_editor.rs` and `crates/harness/src/tools/workspace/list_files.rs`.

Create `crates/harness/src/tools/substrate_dispatch.rs`:
```rust
//! Substrate dispatch — implemented in Task 4.2.
```

- [ ] **Step 4: Add `jail_path` unit tests**

Append to `crates/harness/src/tools/workspace/mod.rs`:

```rust
#[cfg(all(test, unix))]
mod tests {
    use super::{jail_path, WorkspaceToolError};
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

    /// Regression: a new file below a symlinked directory must be
    /// rejected. The leaf doesn't exist yet, so canonicalizing the
    /// full path fails — the parent-canonicalize fix catches it.
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
```

Add to `crates/harness/Cargo.toml` `[dev-dependencies]` if missing: `tempfile = "3"`.

- [ ] **Step 5: Verify build and tests**

Run: `cargo build -p proxima-harness && cargo test -p proxima-harness tools::workspace`
Expected: builds clean; all five `jail_path` tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/harness/src/tools crates/harness/Cargo.toml
git commit -m "harness: workspace tool trait + cwd-jail helper"
```
