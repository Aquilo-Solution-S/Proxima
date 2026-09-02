//! Repository registry and ingestion-run state.

pub mod erase;
pub mod fence;
pub mod records;
pub mod registry;
mod rows;
pub mod runs;
pub mod scope;

pub use erase::erase_repo;
#[cfg(any(test, debug_assertions))]
pub use erase::{erase_footprint, reference_closure_sql};
/// The declared lifecycle scope. `CODE_REPO_SCOPE` is what a repo-scoped
/// payload names and what an integration-test barrier hands to
/// `proxima::flavor::lock_scope_fence_exclusive_tx` — the same key the
/// production paths take, not a hand-copied one.
pub use fence::{CODE_REPO_SCOPE, CODE_REPO_SCOPE_DECL};
pub use records::{
    RepoEraseReceipt, RepoIngestionRun, RepoRecord, RepoRegistryError, RunStage, RunStatus,
    StageCounters,
};
pub use registry::{
    get_repo, list_repos, list_repos_page, register_repo, set_repo_scope, set_repo_target_branch,
    update_cursor,
};
pub use scope::{MAX_SCOPE_GLOB_LEN, MAX_SCOPE_GLOBS, RepoScope, ScopeError, ScopeMatcher};
