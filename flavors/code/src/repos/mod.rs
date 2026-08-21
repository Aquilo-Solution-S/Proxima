//! Repository registry and ingestion-run state.

pub mod erase;
pub mod records;
pub mod registry;
mod rows;
pub mod runs;
pub mod scope;

pub use erase::{erase_footprint, erase_repo, reference_closure_sql};
pub use records::{
    RepoEraseReceipt, RepoIngestionRun, RepoRecord, RepoRegistryError, RunStage, RunStatus,
    StageCounters,
};
pub use registry::{
    get_repo, list_repos, list_repos_page, register_repo, set_repo_scope, set_repo_target_branch,
    update_cursor,
};
pub use scope::{MAX_SCOPE_GLOB_LEN, MAX_SCOPE_GLOBS, RepoScope, ScopeError, ScopeMatcher};
