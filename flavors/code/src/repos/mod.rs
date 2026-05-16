//! Repository registry and ingestion-run state.

pub mod records;
pub mod registry;
mod rows;
pub mod runs;

pub use records::{
    RepoEraseReceipt, RepoIngestionRun, RepoRecord, RepoRegistryError, RunStage, RunStatus,
    StageCounters, WorkspaceDecisionRecord, WorkspaceMergeOutcome, WorkspaceReviewRecord,
    WorkspaceRunDiff, WorkspaceRunRecord,
};
pub use registry::{
    delete_repo, erase_repo, get_repo, infer_missing_target_branch, list_repos, register_repo,
    set_repo_target_branch, update_cursor,
};
pub use runs::{
    advance_stage, begin_run, get_active_run, get_run, mark_failed, mark_succeeded, start_run,
    start_run_with_created, sweep_orphaned_runs,
};

use proxima_core::{Owner, OwnerPrincipalKind, Principal};

/// Encode `Owner` into the three column values used by the `repos` table.
pub(crate) fn owner_columns(owner: &Owner) -> (OwnerPrincipalKind, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => (OwnerPrincipalKind::User, u.into_inner()),
        Principal::Group(g) => (OwnerPrincipalKind::Group, g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

#[doc(hidden)]
#[must_use]
pub fn owner_columns_pub(owner: &Owner) -> (OwnerPrincipalKind, uuid::Uuid, uuid::Uuid) {
    owner_columns(owner)
}
