#![allow(dead_code, unused_imports)]
//! Repository registry and ingestion-run state.

pub mod records;
pub mod registry;
mod rows;
pub mod runs;
pub mod scope;

pub use records::{
    RepoEraseReceipt, RepoIngestionRun, RepoRecord, RepoRegistryError, RunStage, RunStatus,
    StageCounters,
};
pub use registry::{
    delete_repo, erase_repo, get_repo, infer_missing_target_branch, list_repos, list_repos_page,
    register_repo, set_repo_scope, set_repo_target_branch, update_cursor,
};
pub use runs::{
    advance_stage, begin_run, get_active_run, get_run, mark_failed, mark_succeeded, start_run,
    start_run_with_created, sweep_orphaned_runs,
};
pub use scope::{MAX_SCOPE_GLOB_LEN, MAX_SCOPE_GLOBS, RepoScope, ScopeError, ScopeMatcher};
