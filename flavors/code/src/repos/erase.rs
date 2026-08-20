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
/// engineer who worked on it.
/// `every_declared_surface_is_reached_by_the_repo_erase_or_named_as_an_exemption`
/// in `tests` is what keeps this list and that exemption honest against the
/// contract.
///
/// `development_perspective_v1.repo_id` is NULLABLE, and `repo_id = $1`
/// therefore never matches a NULL one. That is the intended reading, not an
/// oversight: the payload documents `None` as "cross-repo observations", so
/// a NULL-repo perspective belongs to the owner the way the self-model rows
/// do and no single repository's erase is entitled to it. It goes with the
/// owner, through the compliance erase.
/// `a_perspective_about_no_particular_repo_survives_a_repo_erase` pins it,
/// because a nullable column silently excluded from a sweep is otherwise
/// indistinguishable from a bug.
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

/// Every `proxima_code` row that references an erased admission through a
/// column other than its own `t`, and the admissions behind those rows.
///
/// Nine such columns exist and every one of them is `NO ACTION`. The repo
/// sweep above reaches a row only through the row's OWN `repo_id` (or,
/// for the two work-item sidecars, through a work item of this repo), and
/// nothing anywhere constrains a row's `repo_id` to agree with the repo of
/// the memory it points at. So repo B may hold an `execution_result_v1`
/// naming repo A's `work_requested_v1` memory: erasing A sweeps A's rows,
/// B's row survives with a pointer to a memory about to go, and the
/// substrate delete does not leak — it RAISES, and the whole erase rolls
/// back. That is not a hypothetical; it reproduces on an ordinary
/// deployment.
///
/// The semantics chosen here: A REFERENCE TO ERASED DATA IS ITSELF ERASED,
/// wherever it lives. The alternatives are worse. Keeping the row is not
/// available — the database refuses it. Nulling the column would leave a
/// test result that reports on nothing and an assignment pointing at
/// nobody, which is a fact about the erased work item preserved in the
/// negative. Refusing the erase would let one repository's stray pointer
/// veto another repository's erasure, which is the inverse of what an
/// erase is for.
///
/// Run to a fixpoint by the caller: these rows carry admissions of their
/// own, and a row erased here may in turn be referenced by a third. The
/// loop terminates because every iteration that returns anything has
/// deleted at least one row from a set that only shrinks.
///
/// `the_reference_closure_covers_every_non_t_foreign_key_into_memory` pins
/// the column list against `pg_constraint` rather than against this
/// comment.
const CLOSE_DANGLING_REFERENCES_SQL: &str = "\
WITH erased AS (
    SELECT unnest($1::uuid[]) AS t
),
d_acceptance_criteria AS (
    DELETE FROM proxima_code.acceptance_criteria_v1
     WHERE work_item_memory_id IN (SELECT t FROM erased)
    RETURNING t
),
d_acceptance_summary AS (
    DELETE FROM proxima_code.acceptance_summary_v1
     WHERE work_item_memory_id IN (SELECT t FROM erased)
    RETURNING t
),
d_acceptance_verification AS (
    DELETE FROM proxima_code.acceptance_verification_v1
     WHERE work_item_memory_id IN (SELECT t FROM erased)
        OR verifier_memory_id IN (SELECT t FROM erased)
    RETURNING t
),
d_execution_plan AS (
    DELETE FROM proxima_code.execution_plan_v1
     WHERE goal_activated_memory_id IN (SELECT t FROM erased)
    RETURNING t
),
d_execution_result AS (
    DELETE FROM proxima_code.execution_result_v1
     WHERE work_requested_memory_id IN (SELECT t FROM erased)
    RETURNING t
),
d_test_result AS (
    DELETE FROM proxima_code.test_result_v1
     WHERE test_requested_memory_id IN (SELECT t FROM erased)
    RETURNING t
),
d_work_assignment AS (
    DELETE FROM proxima_code.work_assignment_v1
     WHERE work_item_memory_id IN (SELECT t FROM erased)
        OR target_perspective_memory_id IN (SELECT t FROM erased)
    RETURNING t
)
SELECT t FROM d_acceptance_criteria
UNION SELECT t FROM d_acceptance_summary
UNION SELECT t FROM d_acceptance_verification
UNION SELECT t FROM d_execution_plan
UNION SELECT t FROM d_execution_result
UNION SELECT t FROM d_test_result
UNION SELECT t FROM d_work_assignment";

/// Take a row lock on the admissions about to be erased.
///
/// Not an optimisation and not a formality. `READ COMMITTED` gives every
/// statement its own snapshot, so the sweep, the closure and the substrate
/// delete are three snapshots with two gaps between them, and a referencing
/// row committed in either gap aborts the erase after the work is done.
/// `FOR UPDATE` on the referenced `memory` rows closes both gaps for real:
/// inserting a row with a foreign key takes `FOR KEY SHARE` on the row it
/// references, and `FOR UPDATE` conflicts with `FOR KEY SHARE`, so a
/// concurrent writer that would have created a dangling pointer blocks
/// until this transaction commits and then fails its own foreign key —
/// which is the correct outcome, in its own transaction rather than ours.
const LOCK_ERASED_ADMISSIONS_SQL: &str = "\
SELECT t FROM proxima_core.memory WHERE t = ANY($1::uuid[]) FOR UPDATE";

/// The repo row, locked.
///
/// `FOR UPDATE` here serializes two erases of the same repository against
/// each other, and blocks an ingestion run starting against a repository
/// that is being erased — `repo_ingestion_runs` carries a foreign key to
/// this row, so starting a run needs `FOR KEY SHARE` on it. The sidecar
/// tables do NOT reference `repos`, so this lock is not what makes the
/// erase safe against a concurrent sidecar write;
/// [`LOCK_ERASED_ADMISSIONS_SQL`] is.
const REPO_EXISTS_SQL: &str = "\
SELECT repo_id FROM proxima_code.repos
 WHERE owner_kind = $1 AND owner_id = $2 AND repo_id = $3
   FOR UPDATE";

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

    let swept: Vec<Uuid> = sqlx::query_scalar(DELETE_REPO_ROWS_SQL)
        .bind(repo_id)
        .fetch_all(&mut *tx)
        .await?;

    let ts = close_dangling_references(&mut tx, swept).await?;

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

/// The reference-closure statement, for the test that asks `pg_constraint`
/// whether its column list is still the schema's.
#[must_use]
pub fn reference_closure_sql() -> &'static str {
    CLOSE_DANGLING_REFERENCES_SQL
}

/// Grow `swept` into the full set of admissions this erase must delete, by
/// deleting every flavor row that points at one of them and adding that
/// row's own admission to the set.
///
/// Each round locks its frontier's `memory` rows before it deletes anything
/// that references them, so the round's own snapshot gap is closed; the
/// locks accumulate for the life of the transaction, so by the time the
/// last round returns nothing, every admission in the answer is held
/// against concurrent referencing writes.
///
/// Termination: a round either returns nothing, or has deleted at least one
/// row. Rows are only deleted, never created, so the rounds are bounded by
/// the number of rows in the flavor's schema.
async fn close_dangling_references(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    swept: Vec<Uuid>,
) -> Result<Vec<Uuid>, RepoRegistryError> {
    let mut all: Vec<Uuid> = swept;
    let mut seen: std::collections::HashSet<Uuid> = all.iter().copied().collect();
    let mut frontier = all.clone();
    while !frontier.is_empty() {
        sqlx::query(LOCK_ERASED_ADMISSIONS_SQL)
            .bind(&frontier)
            .execute(&mut **tx)
            .await?;
        let reached: Vec<Uuid> = sqlx::query_scalar(CLOSE_DANGLING_REFERENCES_SQL)
            .bind(&frontier)
            .fetch_all(&mut **tx)
            .await?;
        frontier = reached.into_iter().filter(|t| seen.insert(*t)).collect();
        all.extend_from_slice(&frontier);
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::{CLOSE_DANGLING_REFERENCES_SQL, DELETE_REPO_ROWS_SQL, DELETE_REPO_SQL};
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

    /// Through the contract's own accessor, not a hand-rolled union of two
    /// of its fields. The hand-rolled version could not see
    /// `kernel_surfaces` or the projection surface, which is exactly the
    /// blindness this whole test exists to remove.
    fn every_declared_surface() -> impl Iterator<Item = Surface> {
        CODE_FLAVOR_CONTRACT.all_surfaces()
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
            .chain(tables_deleted_by(CLOSE_DANGLING_REFERENCES_SQL))
            .filter(|table| !declared.contains(table))
            .collect();
        stray.sort_unstable();
        assert!(
            stray.is_empty(),
            "the repo erase deletes from tables the contract does not declare: {stray:?}"
        );
    }

    /// The closure may only reach tables the sweep already reaches.
    ///
    /// It exists to catch a row the sweep's `repo_id` filter missed, in a
    /// table the sweep already knows about. A table appearing ONLY in the
    /// closure would mean a whole surface whose erasure depends on someone
    /// else pointing at it, which is not a rule anyone could state.
    #[test]
    fn the_reference_closure_reaches_no_table_the_sweep_does_not() {
        let swept = tables_deleted_by(DELETE_REPO_ROWS_SQL);
        let mut only_in_closure: Vec<&str> = tables_deleted_by(CLOSE_DANGLING_REFERENCES_SQL)
            .into_iter()
            .filter(|table| !swept.contains(table))
            .collect();
        only_in_closure.sort_unstable();
        assert!(
            only_in_closure.is_empty(),
            "the reference closure deletes from tables the repo sweep never touches: \
             {only_in_closure:?}"
        );
    }
}
