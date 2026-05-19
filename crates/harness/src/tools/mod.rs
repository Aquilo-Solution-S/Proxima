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

pub mod strict_inventory;
pub mod strict_schema;
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
