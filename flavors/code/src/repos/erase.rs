//! Erase one registered repository.
//!
//! This used to live in `crates/storage-pg/src/verbs/code_repo_erase.rs`:
//! the core spine held a hardcoded list of nine `proxima_code` tables (five
//! of the flavor's sixteen sidecars) and a hand-written duplicate of the
//! kernel's own erase that reached seven `proxima_core` tables and stopped
//! there. Both halves were wrong in the same way — a table added to either
//! side was silently not erased — and the flavor is the only place that
//! knows what "one repository's rows" means.
//!
//! The split is now: the flavor deletes the flavor's rows and names the
//! admissions behind them; [`erase_memory_series`] deletes the substrate.
//! Neither half enumerates the other's tables.

use proxima_core::Owner;
use proxima_storage_pg::verbs::forget::erase_memory_series;
use sqlx::PgPool;
use uuid::Uuid;

use super::records::{RepoEraseReceipt, RepoRegistryError};
use crate::store::CodeFlavorStore;

/// Delete every `proxima_code` row filed under one repo, returning the
/// admissions they named.
///
/// One statement on purpose. Data-modifying CTEs all read the same snapshot,
/// so `work_items` still sees the work-item rows that the same statement is
/// deleting — which is what lets `acceptance_criteria_v1` and
/// `acceptance_verification_v1`, the two sidecars that carry no `repo_id` of
/// their own, be reached through the work item they belong to. Both hold a
/// plain (non-cascading) foreign key into `proxima_core.memory`, so leaving
/// them behind would not leak a row, it would ABORT the substrate erase.
///
/// Detail tables are absent by design: `acceptance_criterion_v1`,
/// `code_chunk_call_v1`, `execution_plan_item_v1` and
/// `test_requested_criterion_v1` cascade from the sidecar above them, and
/// the contract declares exactly that (`EraseRule::Cascade`). Naming them
/// here would be a second, weaker statement of a constraint the database
/// already enforces.
///
/// `commit_summarizer_self_v1` and `engineer_self_v1` are absent for the
/// opposite reason: they are the owner's self-model, carry no `repo_id`,
/// and outlive any one repository. Erasing a repo must not delete the
/// engineer who worked on it. `repo_completeness` in `tests` is what keeps
/// this list and that exemption honest against the contract.
const DELETE_REPO_ROWS_SQL: &str = "\
WITH work_items AS (
    SELECT t FROM proxima_code.work_requested_v1 WHERE repo_id = $1
    UNION
    SELECT t FROM proxima_code.test_requested_v1 WHERE repo_id = $1
),
d_commit AS (
    DELETE FROM proxima_code.commit_v1 WHERE repo_id = $1 RETURNING t
),
d_commit_summary AS (
    DELETE FROM proxima_code.commit_summary_v1 WHERE repo_id = $1 RETURNING t
),
d_code_chunk AS (
    DELETE FROM proxima_code.code_chunk_v1 WHERE repo_id = $1 RETURNING t
),
d_file_revision AS (
    DELETE FROM proxima_code.file_revision_v1 WHERE repo_id = $1 RETURNING t
),
d_work_requested AS (
    DELETE FROM proxima_code.work_requested_v1 WHERE repo_id = $1 RETURNING t
),
d_test_requested AS (
    DELETE FROM proxima_code.test_requested_v1 WHERE repo_id = $1 RETURNING t
),
d_execution_result AS (
    DELETE FROM proxima_code.execution_result_v1 WHERE repo_id = $1 RETURNING t
),
d_test_result AS (
    DELETE FROM proxima_code.test_result_v1 WHERE repo_id = $1 RETURNING t
),
d_execution_plan AS (
    DELETE FROM proxima_code.execution_plan_v1 WHERE repo_id = $1 RETURNING t
),
d_acceptance_summary AS (
    DELETE FROM proxima_code.acceptance_summary_v1 WHERE repo_id = $1 RETURNING t
),
d_development_perspective AS (
    DELETE FROM proxima_code.development_perspective_v1 WHERE repo_id = $1 RETURNING t
),
d_work_assignment AS (
    DELETE FROM proxima_code.work_assignment_v1 WHERE repo_id = $1 RETURNING t
),
d_acceptance_criteria AS (
    DELETE FROM proxima_code.acceptance_criteria_v1
     WHERE work_item_memory_id IN (SELECT t FROM work_items)
    RETURNING t
),
d_acceptance_verification AS (
    DELETE FROM proxima_code.acceptance_verification_v1
     WHERE work_item_memory_id IN (SELECT t FROM work_items)
    RETURNING t
)
SELECT t FROM d_commit
UNION SELECT t FROM d_commit_summary
UNION SELECT t FROM d_code_chunk
UNION SELECT t FROM d_file_revision
UNION SELECT t FROM d_work_requested
UNION SELECT t FROM d_test_requested
UNION SELECT t FROM d_execution_result
UNION SELECT t FROM d_test_result
UNION SELECT t FROM d_execution_plan
UNION SELECT t FROM d_acceptance_summary
UNION SELECT t FROM d_development_perspective
UNION SELECT t FROM d_work_assignment
UNION SELECT t FROM d_acceptance_criteria
UNION SELECT t FROM d_acceptance_verification";

const REPO_EXISTS_SQL: &str = "\
SELECT repo_id FROM proxima_code.repos
 WHERE owner_kind = $1 AND owner_id = $2 AND repo_id = $3";

/// `repo_ingestion_runs` is absent here too: `runs_repo_fk` cascades.
const DELETE_REPO_SQL: &str = "\
DELETE FROM proxima_code.repos
 WHERE owner_kind = $1 AND owner_id = $2 AND repo_id = $3";

/// Erase one registered repo's code-flavor rows and the admissions behind
/// them.
///
/// Cold objects are enqueued, not destroyed. A version of a repo memory that
/// was cooled has its bytes in the object store, and destroying them from
/// inside this transaction would lose them outright on a rollback — so the
/// erase leaves a durable `cold_purge_pending` row per object and
/// `proxima-mcp maintain-retention --retry-cold-object-purges` (the lane
/// that already exists for exactly this) destroys them. The receipt reports
/// how many, so a caller can see the queue it just added to. The version
/// this replaced never deleted the `cooled` rows at all, which leaked both
/// the locator and the bytes.
///
/// # Errors
/// Returns `RepoRegistryError::NotFound` if the repo is not registered for
/// `owner`; otherwise returns database/storage errors from the transaction.
pub async fn erase_repo(
    store: &CodeFlavorStore,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<RepoEraseReceipt, RepoRegistryError> {
    let (kind, principal_id) = owner.columns();
    let pool: &PgPool = store.pool();
    let mut tx = pool.begin().await?;

    let exists: Option<(Uuid,)> = sqlx::query_as(REPO_EXISTS_SQL)
        .bind(kind)
        .bind(principal_id)
        .bind(repo_id)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        return Err(RepoRegistryError::NotFound { repo_id });
    }

    let ts: Vec<Uuid> = sqlx::query_scalar(DELETE_REPO_ROWS_SQL)
        .bind(repo_id)
        .fetch_all(&mut *tx)
        .await?;

    let (memories_deleted, cold_purge) =
        erase_memory_series(&mut tx, store.sidecars(), owner, &ts).await?;

    let repo_record_deleted = sqlx::query(DELETE_REPO_SQL)
        .bind(kind)
        .bind(principal_id)
        .bind(repo_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    tx.commit().await?;

    Ok(RepoEraseReceipt {
        repo_id,
        completed_at: time::OffsetDateTime::now_utc(),
        memories_deleted,
        cold_objects_pending: cold_purge.object_keys().len() as u64,
        repo_record_deleted,
    })
}

#[cfg(test)]
mod tests {
    use super::{DELETE_REPO_ROWS_SQL, DELETE_REPO_SQL};
    use crate::contract::CODE_FLAVOR_CONTRACT;
    use proxima_core::flavor::{EraseRule, Surface};
    use std::collections::BTreeSet;

    /// The owner's self-model. Both carry no `repo_id`, are written once per
    /// owner, and are the target of `work_assignment_v1`: erasing a repo
    /// must not delete the engineer who worked on it.
    const OWNER_SCOPED: &[&str] = &[
        "proxima_code.commit_summarizer_self_v1",
        "proxima_code.engineer_self_v1",
    ];

    /// Reached by the repo row itself: `runs_repo_fk` is
    /// `ON DELETE CASCADE` from `proxima_code.repos`.
    const CASCADES_FROM_THE_REPO_ROW: &[&str] = &["proxima_code.repo_ingestion_runs"];

    fn every_declared_surface() -> impl Iterator<Item = &'static Surface> {
        CODE_FLAVOR_CONTRACT
            .schemas
            .iter()
            .flat_map(|schema| schema.surfaces.iter())
            .chain(CODE_FLAVOR_CONTRACT.state_surfaces.iter())
    }

    fn tables_deleted_by(sql: &'static str) -> BTreeSet<&'static str> {
        sql.lines()
            .filter_map(|line| line.trim().strip_prefix("DELETE FROM "))
            .filter_map(|rest| rest.split_whitespace().next())
            .collect()
    }

    /// The point of the whole move: a table added to this flavor and not to
    /// the sweep is a table `proxima-code_erase_repo` silently leaves
    /// behind. The core spine could not run this test — it did not know
    /// what the flavor declared, which is exactly how the version this
    /// replaced came to reach five of sixteen sidecars.
    #[test]
    fn every_declared_surface_is_reached_by_the_repo_erase_or_named_as_an_exemption() {
        let swept = tables_deleted_by(DELETE_REPO_ROWS_SQL);
        assert_eq!(
            swept.len(),
            14,
            "the row sweep should delete from fourteen tables, found {swept:?}"
        );
        let repo_row = tables_deleted_by(DELETE_REPO_SQL);

        let mut unreached: Vec<&str> = every_declared_surface()
            .filter(|surface| {
                let table = surface.table;
                !(swept.contains(table)
                    || repo_row.contains(table)
                    || OWNER_SCOPED.contains(&table)
                    || CASCADES_FROM_THE_REPO_ROW.contains(&table)
                    || matches!(surface.erase, EraseRule::Cascade { .. }))
            })
            .map(|surface| surface.table)
            .collect();
        unreached.sort_unstable();
        unreached.dedup();
        assert!(
            unreached.is_empty(),
            "these declared surfaces are erased by nothing when a repo is erased: {unreached:?} \
             — add them to the sweep, or add them to an exemption list with a reason"
        );
    }

    #[test]
    fn the_erase_names_no_table_the_contract_does_not_declare() {
        let declared: BTreeSet<&str> = every_declared_surface()
            .map(|surface| surface.table)
            .collect();
        let mut stray: Vec<&str> = tables_deleted_by(DELETE_REPO_ROWS_SQL)
            .union(&tables_deleted_by(DELETE_REPO_SQL))
            .copied()
            .filter(|table| !declared.contains(table))
            .collect();
        stray.sort_unstable();
        assert!(
            stray.is_empty(),
            "the repo erase deletes from tables the contract does not declare: {stray:?}"
        );
    }
}
