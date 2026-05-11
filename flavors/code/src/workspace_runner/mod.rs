//! Code-flavor workspace runner.
//!
//! Core owns only wake dispatch and the runner trait. This module owns
//! repo, branch, worktree, and workspace-run Fact semantics.

mod context;
mod git;
mod ingest;
mod loaders;
mod prepare;

use std::path::PathBuf;

use proxima_core::WorkspaceRunnerError;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

pub const WORKSPACE_RUNNER_SOURCE_ID: &str = "proxima-code/workspace-runner";
pub const WORKSPACE_RUN_OBJECT_SCHEMA: &str = "proxima-code/workspace-run-object-v1";
pub const WORKSPACE_RUN_WHOLE_SCHEMA: &str = "proxima-code/workspace-run-whole-v1";

#[derive(Debug, Default, Clone)]
pub struct CodeWorkspaceRunner {
    pool: Option<PgPool>,
    worktrees_root: Option<PathBuf>,
    pnpm_store_root: Option<PathBuf>,
    pnpm_executable: Option<PathBuf>,
}

impl CodeWorkspaceRunner {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool: Some(pool),
            worktrees_root: None,
            pnpm_store_root: None,
            pnpm_executable: None,
        }
    }

    #[must_use]
    pub fn with_worktrees_root(mut self, root: PathBuf) -> Self {
        self.worktrees_root = Some(root);
        self
    }

    #[must_use]
    pub fn with_pnpm_store_root(mut self, root: PathBuf) -> Self {
        self.pnpm_store_root = Some(root);
        self
    }

    #[must_use]
    pub fn with_pnpm_executable(mut self, executable: PathBuf) -> Self {
        self.pnpm_executable = Some(executable);
        self
    }

    pub(super) fn pool(&self) -> Result<&PgPool, WorkspaceRunnerError> {
        self.pool
            .as_ref()
            .ok_or(WorkspaceRunnerError::Unimplemented)
    }

    pub(super) fn worktrees_root(&self) -> PathBuf {
        self.worktrees_root.clone().unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map_or_else(std::env::temp_dir, PathBuf::from)
                .join(".proxima")
                .join("worktrees")
        })
    }

    pub(super) fn pnpm_store_root(&self) -> PathBuf {
        self.pnpm_store_root.clone().unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map_or_else(std::env::temp_dir, PathBuf::from)
                .join(".proxima")
                .join("pnpm-store")
        })
    }

    pub(super) fn pnpm_executable(&self) -> PathBuf {
        self.pnpm_executable
            .clone()
            .unwrap_or_else(|| PathBuf::from("pnpm"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PreparedState {
    pub(super) repo_id: Uuid,
    pub(super) canonical_path: String,
    pub(super) target_branch: String,
    pub(super) branch_name: String,
    pub(super) parent_sha: String,
    pub(super) worktree_path: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct RunnerRepoRow {
    pub(super) canonical_path: String,
    pub(super) target_branch: Option<String>,
}
