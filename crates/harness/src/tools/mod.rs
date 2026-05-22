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
use proxima_core::verbs::schema::PayloadKind;

pub mod strict_inventory;
pub mod strict_schema;
pub mod substrate_dispatch;
pub mod workspace;

/// Resolved binding per tool in the active palette.
#[derive(Clone)]
pub enum ToolBinding {
    Substrate(SubstrateToolBinding),
    TypedEmit {
        internal: SubstrateToolBinding,
        schema_id: String,
        schema_version: u32,
        kind: PayloadKind,
    },
    Workspace(workspace::WorkspaceToolName),
}

impl std::fmt::Debug for ToolBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Substrate(s) => f.debug_tuple("Substrate").field(&s.canonical_name).finish(),
            Self::TypedEmit {
                internal,
                schema_id,
                schema_version,
                kind,
            } => f
                .debug_struct("TypedEmit")
                .field("internal", &internal.canonical_name)
                .field("schema_id", schema_id)
                .field("schema_version", schema_version)
                .field("kind", kind)
                .finish(),
            Self::Workspace(w) => f.debug_tuple("Workspace").field(w).finish(),
        }
    }
}

/// Resolved environment for workspace-tool dispatch.
#[derive(Debug, Clone)]
pub struct WorkspaceCtx {
    /// Host path of the prepared per-wake clone. `text_editor` / `list_files`
    /// jail against this; `shell` uses it only as the docker-exec cwd.
    pub workspace_root: PathBuf,
    /// `Some` when the wake runs inside a per-wake observation container;
    /// `workspace_shell` then routes commands through `docker exec`. `None`
    /// is the host escape hatch (`PROXIMA_WORKSPACE_SANDBOX=host`).
    pub sandbox_session: Option<workspace::sandbox::WorkspaceSandboxSession>,
}
