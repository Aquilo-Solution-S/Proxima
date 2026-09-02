//! Erase one registered repository.
//!
//! The flavor is the only place that knows what "one repository's rows"
//! means, so the split is: the flavor deletes the flavor's rows and names
//! the admissions behind them; [`erase_memory_series`] deletes the
//! substrate. Neither half enumerates the other's tables.
//!
//! The two halves reach their tables differently, and the difference is the
//! point. [`erase_memory_series`] iterates the declared surfaces it is
//! handed, so a surface added to the declaration is reached without
//! touching it. This file is the flavor's own inverse and names its tables
//! by hand: the statements below are `&'static str` constants spelling every
//! `proxima_code` relation a repo's rows live in.
//!
//! A hand-written inverse is only as good as what checks it, so both
//! directions are pinned against the contract:
//! `every_declared_surface_is_reached_by_the_repo_erase_or_named_as_an_exemption`
//! fails on a surface the contract declares and these statements miss,
//! unless it is listed as an exemption with a reason, and
//! `the_erase_names_no_table_the_contract_does_not_declare` fails on a table
//! these statements name and the contract does not.

use proxima_core::{Owner, StorageError};
use proxima_storage_pg::verbs::forget::{
    admissions_outside_owner, erase_memory_series, expand_series_for_erase,
    lock_admissions_for_erase,
};
use proxima_storage_pg::{MAX_TRANSACTION_ATTEMPTS, is_transient_conflict};
use sqlx::PgPool;
use std::collections::{BTreeMap, BTreeSet};
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
/// a perspective SERIES that never named this repository belongs to the
/// owner the way the self-model rows do, and no single repository's erase is
/// entitled to it. It goes with the owner, through the owner erase.
///
/// The unit is the series, not the version. A perspective whose current
/// version says `None` but whose history was filed under this repository IS
/// this repository's, and goes: the footprint expands every admission it
/// finds to the whole series (that is what
/// [`expand_series_for_erase`] is for), because keeping one version of a
/// series and erasing another is not a state the substrate can be left in —
/// the head would point at a missing row. Saying "a NULL `repo_id`
/// survives" full stop would therefore be a promise about rows, made in the
/// language of series, and false in exactly the case where it matters.
/// `a_perspective_about_no_particular_repo_survives_a_repo_erase` pins the
/// first half and
/// `a_perspective_that_dropped_its_repo_id_still_goes_with_the_repo` pins
/// the second, because a nullable column silently excluded from a sweep is
/// otherwise indistinguishable from a bug.
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

/// [`DELETE_REPO_ROWS_SQL`] asked instead of performed.
///
/// The erase now computes its whole footprint before it deletes anything,
/// so the first step has to be a question. The two statements name the same
/// fourteen tables through the same predicates and
/// `the_finder_and_the_sweep_ask_the_same_question` holds them to it; the
/// runtime check in [`erase_repo`] holds them to it again, on real rows, by
/// refusing to proceed if the sweep deletes a row the finder never saw.
const FIND_REPO_ROWS_SQL: &str = "\
WITH work_items AS (
    SELECT t FROM proxima_code.work_requested_v1 WHERE repo_id = $1
    UNION
    SELECT t FROM proxima_code.test_requested_v1 WHERE repo_id = $1
)
SELECT 'commit_v1' AS src, t FROM proxima_code.commit_v1 WHERE repo_id = $1
UNION ALL
SELECT 'commit_summary_v1', t FROM proxima_code.commit_summary_v1 WHERE repo_id = $1
UNION ALL
SELECT 'code_chunk_v1', t FROM proxima_code.code_chunk_v1 WHERE repo_id = $1
UNION ALL
SELECT 'file_revision_v1', t FROM proxima_code.file_revision_v1 WHERE repo_id = $1
UNION ALL
SELECT 'work_requested_v1', t FROM proxima_code.work_requested_v1 WHERE repo_id = $1
UNION ALL
SELECT 'test_requested_v1', t FROM proxima_code.test_requested_v1 WHERE repo_id = $1
UNION ALL
SELECT 'execution_result_v1', t FROM proxima_code.execution_result_v1 WHERE repo_id = $1
UNION ALL
SELECT 'test_result_v1', t FROM proxima_code.test_result_v1 WHERE repo_id = $1
UNION ALL
SELECT 'execution_plan_v1', t FROM proxima_code.execution_plan_v1 WHERE repo_id = $1
UNION ALL
SELECT 'acceptance_summary_v1', t FROM proxima_code.acceptance_summary_v1 WHERE repo_id = $1
UNION ALL
SELECT 'development_perspective_v1', t
  FROM proxima_code.development_perspective_v1 WHERE repo_id = $1
UNION ALL
SELECT 'work_assignment_v1', t FROM proxima_code.work_assignment_v1 WHERE repo_id = $1
UNION ALL
SELECT 'acceptance_criteria_v1', t
  FROM proxima_code.acceptance_criteria_v1
 WHERE work_item_memory_id IN (SELECT t FROM work_items)
UNION ALL
SELECT 'acceptance_verification_v1', t
  FROM proxima_code.acceptance_verification_v1
 WHERE work_item_memory_id IN (SELECT t FROM work_items)";

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
/// One pass, over the CLOSED footprint. These rows carry admissions of
/// their own and a row deleted here may in turn be referenced by a third,
/// so the set has to be a fixpoint — but the fixpoint is reached by
/// [`FIND_DANGLING_REFERENCES_SQL`], which asks the same question without
/// deleting anything, so that the whole set can be locked in one statement
/// before any of it is touched. Deleting in rounds and locking per round
/// was the earlier shape and it was wrong twice over: it left an
/// arbitrarily long window between locking one round and asking for the
/// next, and it could not see the versions the series expansion adds.
///
/// `the_reference_closure_covers_every_non_t_foreign_key_into_memory` pins
/// the column list of both statements against `pg_constraint` rather than
/// against this comment.
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

/// [`CLOSE_DANGLING_REFERENCES_SQL`] asked instead of performed, with the
/// table each answer came from.
///
/// The table name is not decoration: a row found here may belong to a
/// DIFFERENT principal — a flavor table's foreign key names
/// `proxima_core.memory (t)` and nothing constrains whose memory that is,
/// and an admission that changed hands leaves exactly this shape behind.
/// Following such a reference would delete one principal's rows on another
/// principal's authority, so [`erase_repo`] refuses instead, and the
/// refusal has to be able to say which rows.
const FIND_DANGLING_REFERENCES_SQL: &str = "\
WITH erased AS (
    SELECT unnest($1::uuid[]) AS t
)
SELECT 'acceptance_criteria_v1' AS src, t
  FROM proxima_code.acceptance_criteria_v1
 WHERE work_item_memory_id IN (SELECT t FROM erased)
UNION ALL
SELECT 'acceptance_summary_v1', t
  FROM proxima_code.acceptance_summary_v1
 WHERE work_item_memory_id IN (SELECT t FROM erased)
UNION ALL
SELECT 'acceptance_verification_v1', t
  FROM proxima_code.acceptance_verification_v1
 WHERE work_item_memory_id IN (SELECT t FROM erased)
    OR verifier_memory_id IN (SELECT t FROM erased)
UNION ALL
SELECT 'execution_plan_v1', t
  FROM proxima_code.execution_plan_v1
 WHERE goal_activated_memory_id IN (SELECT t FROM erased)
UNION ALL
SELECT 'execution_result_v1', t
  FROM proxima_code.execution_result_v1
 WHERE work_requested_memory_id IN (SELECT t FROM erased)
UNION ALL
SELECT 'test_result_v1', t
  FROM proxima_code.test_result_v1
 WHERE test_requested_memory_id IN (SELECT t FROM erased)
UNION ALL
SELECT 'work_assignment_v1', t
  FROM proxima_code.work_assignment_v1
 WHERE work_item_memory_id IN (SELECT t FROM erased)
    OR target_perspective_memory_id IN (SELECT t FROM erased)";

/// The repo row, locked.
///
/// `FOR UPDATE` here serializes two erases of the same repository against
/// each other, and blocks an ingestion run starting against a repository
/// that is being erased — `repo_ingestion_runs` carries a foreign key to
/// this row, so starting a run needs `FOR KEY SHARE` on it. The sidecar
/// tables do NOT reference `repos`, so this lock reaches no sidecar write:
/// what makes the erase safe against one is the `code-repo` scope fence
/// (`proxima::flavor::lock_scope_fence_exclusive_tx`), taken before this and
/// held through commit, plus [`lock_admissions_for_erase`] over the
/// footprint.
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
/// `proxima-mcp maintain-storage --retry-cold-object-purges` (the lane
/// that already exists for exactly this) destroys them. The receipt reports
/// how many, so a caller can see the queue it just added to. The version
/// this replaced never deleted the `cooled` rows at all, which leaked both
/// the locator and the bytes.
///
/// A deadlock is retried, not surfaced. The erase takes `FOR UPDATE` on
/// admissions in `t` order and an ordinary writer takes `FOR KEY SHARE` on
/// them in whatever order its own statement produces, so the pair can
/// always be made cyclic and no lock ordering on this side prevents it.
/// `PostgreSQL` then picks the cheapest transaction to abort, which is the
/// one that has not yet written anything — this one. Re-running the whole
/// transaction is the correct response and the only one: by the time the
/// retry starts, the writer that won has committed, so its row is visible
/// to the new attempt's discovery and gets erased with the rest.
///
/// # Errors
/// Returns `RepoRegistryError::NotFound` if the repo is not registered for
/// `owner`, `RepoRegistryError::CrossOwnerReference` if another principal's
/// rows point into this repo; otherwise returns database/storage errors
/// from the transaction.
pub async fn erase_repo(
    store: &CodeFlavorStore,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<RepoEraseReceipt, RepoRegistryError> {
    with_erase_retry(MAX_TRANSACTION_ATTEMPTS, || {
        erase_repo_once(store, owner, repo_id)
    })
    .await
}

/// Re-run a whole erase transaction while it fails transiently, `attempts`
/// times in total.
///
/// Separated from [`erase_repo`] so the loop itself is reachable from a
/// test with an operation that fails on demand. Pinning a retry through the
/// database means arranging a real deadlock at exactly the right moment,
/// which is not something a test can schedule; the consequence, until this
/// existed, was that the budget could be cut to one and every test stayed
/// green. `a_transient_failure_is_retried_within_the_budget_and_not_past_it`
/// is what fails now.
///
/// Nothing about it is test-only: it is the whole retry policy, named.
async fn with_erase_retry<T, F, Fut>(attempts: usize, mut op: F) -> Result<T, RepoRegistryError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, RepoRegistryError>>,
{
    let mut attempt = 1;
    loop {
        match op().await {
            Err(err) if attempt < attempts && is_transient(&err) => attempt += 1,
            outcome => return outcome,
        }
    }
}

/// Whether the whole erase transaction is worth re-running.
fn is_transient(err: &RepoRegistryError) -> bool {
    match err {
        RepoRegistryError::Database(err) => is_transient_conflict(err),
        // `FootprintIncomplete` is here for a reason of its own, not because
        // it resembles a deadlock: the ordinary cause is a row committed in
        // the discovery-to-lock window, and re-discovery is exactly what
        // fixes it. The refusal checks run again on the way, so a
        // CROSS-OWNER row arriving in that window still comes back as
        // `CrossOwnerReference` rather than looping.
        RepoRegistryError::Storage(StorageError::Retryable(_))
        | RepoRegistryError::FootprintIncomplete { .. } => true,
        _ => false,
    }
}

/// How long the erase waits for any one lock before giving up on the
/// attempt.
///
/// Five seconds, the same figure and the same reasoning as the migration
/// path: waiting FOR a lock is not the same as holding one. Without it the
/// erase inherits the pool's five-minute `statement_timeout`, so a
/// long-lived `FOR KEY SHARE` holder turns the lock statement into a
/// five-minute stall ending in `57014` — which is not a transient code, so
/// nothing retries it, and the erase fails hard after five minutes of
/// blocking every writer queued behind it. With the timeout the same
/// situation is a `55P03` in five seconds, which IS transient, so the
/// transaction rolls back (releasing what it held) and comes round again.
const ERASE_LOCK_TIMEOUT_SQL: &str = "SET LOCAL lock_timeout = '5s'";

/// One attempt: begin, compute, lock, delete, commit.
async fn erase_repo_once(
    store: &CodeFlavorStore,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<RepoEraseReceipt, RepoRegistryError> {
    let (kind, principal_id) = owner.columns();
    let pool: &PgPool = store.pool();
    let mut tx = pool.begin().await?;
    // Before anything that takes a lock, which is the very next statement.
    sqlx::query(ERASE_LOCK_TIMEOUT_SQL)
        .execute(&mut *tx)
        .await?;

    // Transfer takes both endpoint owner fences exclusively before it moves
    // any Memory. Holding the source fence shared from before repository
    // discovery through commit makes the footprint one ownership snapshot:
    // a series cannot leave after the owner check and before its flavor rows
    // are deleted.
    proxima_storage_pg::access::owner_columns::lock_owner_fence_shared_tx(&mut tx, owner).await?;

    // The declared `code-repo` scope fence, exclusively, BEFORE the first
    // read of anything this erase intends to delete. The owner fence above
    // is shared with every admission of this owner and the source fence is
    // one lane for every repository, so neither separates this erase from a
    // write into THIS repository; the sidecar tables carry a bare `repo_id`
    // and no foreign key, so the row lock below does not either. Taking the
    // fence here rather than after discovery is the same fence-before-select
    // rule the whole-owner and source-scope erases follow: a same-repository
    // write that has not started waits here, and one that has started
    // committed before the footprint was read and is therefore in it.
    //
    // This is the SAME key the Engine takes shared on every admission of a
    // payload declaring `CODE_REPO_SCOPE` — generated from one declaration,
    // so the two sides cannot drift onto two locks.
    proxima::flavor::lock_scope_fence_exclusive_tx(&mut tx, super::CODE_REPO_SCOPE, owner, repo_id)
        .await?;

    let exists: Option<(Uuid,)> = sqlx::query_as(REPO_EXISTS_SQL)
        .bind(kind)
        .bind(principal_id)
        .bind(repo_id)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        return Err(RepoRegistryError::NotFound { repo_id });
    }

    let ts = erase_footprint(&mut tx, owner, repo_id).await?;
    let held: BTreeSet<Uuid> = ts.iter().copied().collect();

    let swept: Vec<Uuid> = sqlx::query_scalar(DELETE_REPO_ROWS_SQL)
        .bind(repo_id)
        .fetch_all(&mut *tx)
        .await?;
    let closed: Vec<Uuid> = sqlx::query_scalar(CLOSE_DANGLING_REFERENCES_SQL)
        .bind(&ts)
        .fetch_all(&mut *tx)
        .await?;
    // Every row the two deletes reach was in the footprint, or the
    // footprint was not a footprint and the locks are on the wrong rows.
    // Cheap, and the only check that sees the real rows rather than the
    // statements.
    if let Some(missed) = swept.iter().chain(&closed).find(|t| !held.contains(t)) {
        return Err(RepoRegistryError::FootprintIncomplete {
            repo_id,
            memory_id: *missed,
        });
    }

    let (memories_deleted, cold_purge) =
        erase_memory_series(&mut tx, store.sidecars(), store.surfaces(), owner, &ts).await?;

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

/// The statements whose column lists the `pg_constraint` tests check, so
/// that neither the question nor the deletion can drift from the schema.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn reference_closure_sql() -> [&'static str; 2] {
    [FIND_DANGLING_REFERENCES_SQL, CLOSE_DANGLING_REFERENCES_SQL]
}

/// Every admission this repo's erase will delete, locked, before anything
/// is deleted.
///
/// Three things grow the set and they grow each other, so the answer is a
/// JOINT fixpoint and not two passes:
///
/// 1. the repo's own rows, found by `repo_id`;
/// 2. every other version of those rows' series — the substrate erases a
///    series whole ([`expand_series_for_erase`]), and nothing constrains a
///    later version of a handle to name the same repository as the first,
///    so the expansion routinely adds admissions the sweep never saw;
/// 3. every flavor row that POINTS at any admission already in the set,
///    whichever repository it is filed under, plus that row's own
///    admission — which is a new admission, whose series expands, whose
///    versions may be pointed at in turn.
///
/// Computing 1 and 3 without 2 is what made a repo erase abort on an
/// ordinary superseded memory: the sweep found v1, the closure ran over v1
/// and found nothing, the substrate then deleted v2 as part of the series,
/// and v2's referencing row was still there.
///
/// Termination is by `seen`, which only ever grows and is bounded by the
/// number of this owner's admissions — not by "a round deletes a row",
/// which is false here because no round deletes anything, and which was
/// false before too for a cycle's last round.
///
/// The lock is taken ONCE, over the whole answer, for the reason written on
/// [`lock_admissions_for_erase`]: locking per round leaves a window between
/// rounds in which this transaction holds part of the set and is not yet
/// asking for the rest. The ownership question is asked once too, over the
/// whole answer, for a reason of the same shape — see below.
///
/// A retry re-runs all of this, which is what keeps the refusal a refusal:
/// a cross-owner row that arrives during the discovery window is found by
/// the next attempt's fixpoint and refused, rather than retried forever.
///
/// # Errors
/// Returns `RepoRegistryError::CrossOwnerReference` if any admission in the
/// footprint — however it was reached — belongs to another principal;
/// otherwise database/storage errors.
pub async fn erase_footprint(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<Vec<Uuid>, RepoRegistryError> {
    let mut found: Vec<(String, Uuid)> = sqlx::query_as(FIND_REPO_ROWS_SQL)
        .bind(repo_id)
        .fetch_all(&mut **tx)
        .await?;
    let mut seen: BTreeSet<Uuid> = BTreeSet::new();
    // Which row each admission was found through, so a refusal can name
    // something an operator can go and look at. A version added by the
    // series expansion has no flavor row of its own and so no entry; it
    // also cannot be foreign, because the expansion is owner-scoped.
    let mut found_through: BTreeMap<Uuid, String> = BTreeMap::new();

    loop {
        let frontier: Vec<Uuid> = found
            .iter()
            .map(|(table, t)| {
                found_through.entry(*t).or_insert_with(|| table.clone());
                *t
            })
            .filter(|t| !seen.contains(t))
            .collect();
        if frontier.is_empty() {
            break;
        }
        seen.extend(frontier.iter().copied());

        let expanded: Vec<Uuid> = expand_series_for_erase(tx, owner, &frontier).await?;
        found = sqlx::query_as(FIND_DANGLING_REFERENCES_SQL)
            .bind(&frontier)
            .fetch_all(&mut **tx)
            .await?;
        found.extend(expanded.into_iter().map(|t| (String::new(), t)));
    }

    // ONE ownership question, over the WHOLE footprint, and after the
    // fixpoint rather than inside it.
    //
    // Asking it only of the rows reached by following references left the
    // seed leg unguarded, and the seed leg is the ordinary case: a
    // transferred memory keeps the `repo_id` it was written with — nothing
    // in the transfer touches flavor columns, only `owner_id` — so
    // `repo_id = $1` still finds it under the source owner's repo while the
    // admission itself belongs to someone else. Erasing it would destroy the
    // destination's sidecar row on the source's authority and leave their
    // `memory` row stamping a table it is absent from — silently, because
    // the row is in the footprint, so nothing else complains.
    let footprint: Vec<Uuid> = seen.into_iter().collect();
    let foreign = admissions_outside_owner(tx, owner, &footprint).await?;
    if !foreign.is_empty() {
        let blocking: Vec<String> = foreign
            .into_iter()
            .map(|t| match found_through.get(&t) {
                Some(table) if !table.is_empty() => format!("proxima_code.{table} t={t}"),
                _ => format!("t={t}"),
            })
            .collect();
        return Err(RepoRegistryError::CrossOwnerReference { repo_id, blocking });
    }

    // Sorted, because `seen` was a BTreeSet: two erases whose footprints
    // overlap ask for the shared rows in the same order.
    lock_admissions_for_erase(tx, owner, &footprint).await?;
    Ok(footprint)
}

#[cfg(test)]
mod tests {
    use super::{
        CLOSE_DANGLING_REFERENCES_SQL, DELETE_REPO_ROWS_SQL, DELETE_REPO_SQL,
        FIND_DANGLING_REFERENCES_SQL, FIND_REPO_ROWS_SQL, RepoRegistryError,
    };
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

    /// Every `proxima_code` relation the statement names, however it names
    /// it. The `src` labels in the finder are bare table names on purpose,
    /// so they are not mistaken for references here.
    fn tables_named_in(sql: &'static str) -> BTreeSet<&'static str> {
        sql.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | '\''))
            .filter(|token| token.starts_with("proxima_code."))
            .collect()
    }

    /// Every `(table, col)` the statement tests with
    /// `col IN (SELECT t FROM erased)`.
    ///
    /// Pairs, not columns: `work_item_memory_id` names four different
    /// tables, so a set of bare column names would report nine references
    /// as six and let three of them be dropped without a word.
    fn erased_pairs_of(sql: &'static str) -> BTreeSet<(&'static str, &'static str)> {
        let mut table = "";
        let mut pairs = BTreeSet::new();
        for line in sql.lines() {
            let line = line.trim();
            if let Some(named) = line
                .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | '\''))
                .find(|token| token.starts_with("proxima_code."))
            {
                table = named;
            }
            if let Some(rest) = line.strip_suffix(" IN (SELECT t FROM erased)")
                && let Some(column) = rest.rsplit(' ').next()
            {
                pairs.insert((table, column));
            }
        }
        pairs
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

    /// A deadlock is a retry; a refusal is an answer.
    ///
    /// The retry loop wraps the whole erase, so getting this wrong in the
    /// other direction is worse than not retrying at all: a refusal
    /// classified as transient would be re-run three times and then
    /// surfaced anyway, and a cross-owner reference does not stop being a
    /// cross-owner reference on the second attempt.
    #[test]
    fn a_deadlock_is_retried_and_a_refusal_is_not() {
        use super::is_transient;
        use proxima_core::StorageError;
        assert!(is_transient(&RepoRegistryError::Storage(
            StorageError::Retryable("deadlock detected".into())
        )));
        assert!(!is_transient(&RepoRegistryError::CrossOwnerReference {
            repo_id: uuid::Uuid::nil(),
            blocking: vec!["proxima_code.execution_result_v1 t=…".into()],
        }));
        // Not drift until the budget says so: the ordinary cause is an
        // ordinary write landing in the discovery window, and re-discovery
        // is the fix.
        assert!(is_transient(&RepoRegistryError::FootprintIncomplete {
            repo_id: uuid::Uuid::nil(),
            memory_id: uuid::Uuid::nil(),
        }));
        assert!(!is_transient(&RepoRegistryError::NotFound {
            repo_id: uuid::Uuid::nil()
        }));
        const {
            assert!(
                super::MAX_TRANSACTION_ATTEMPTS > 1,
                "a retry budget of one is not a retry"
            );
        }
    }

    /// The loop, run twice.
    ///
    /// The predicate above says what SHOULD be retried; nothing said the
    /// loop ever runs a second attempt. It could be cut to one attempt and
    /// every other test in the workspace stayed green, which makes the
    /// retry — the entire answer to a deadlock — unpinned.
    #[tokio::test]
    async fn a_transient_failure_is_retried_within_the_budget_and_not_past_it() {
        use std::cell::Cell;

        let fails_once = || {
            let calls = Cell::new(0_usize);
            move || {
                let seen = calls.get();
                calls.set(seen + 1);
                async move {
                    if seen == 0 {
                        Err(RepoRegistryError::Storage(
                            proxima_core::StorageError::Retryable("deadlock detected".into()),
                        ))
                    } else {
                        Ok(seen)
                    }
                }
            }
        };

        let one = super::with_erase_retry(1, fails_once()).await;
        assert!(
            matches!(one, Err(RepoRegistryError::Storage(_))),
            "a budget of one attempt must surface the first failure, not swallow it"
        );

        let budgeted = super::with_erase_retry(super::MAX_TRANSACTION_ATTEMPTS, fails_once())
            .await
            .expect("the second attempt succeeds and the loop must reach it");
        assert_eq!(
            budgeted, 1,
            "the value returned is the SECOND attempt's, so the loop really re-ran"
        );

        // And a refusal is answered once, not three times.
        let calls = Cell::new(0_usize);
        let refused = super::with_erase_retry(super::MAX_TRANSACTION_ATTEMPTS, || {
            calls.set(calls.get() + 1);
            async {
                Err::<(), _>(RepoRegistryError::NotFound {
                    repo_id: uuid::Uuid::nil(),
                })
            }
        })
        .await;
        assert!(matches!(refused, Err(RepoRegistryError::NotFound { .. })));
        assert_eq!(calls.get(), 1, "a non-transient answer is not re-asked");
    }

    /// The erase asks before it deletes, which means the question and the
    /// deletion are two statements that must mean the same thing. They are
    /// hand-written, so nothing but this stops a table being added to one
    /// and not the other — and the failure mode is quiet: the sweep deletes
    /// a row the footprint never locked, or the footprint locks a row the
    /// sweep never reaches.
    #[test]
    fn the_finder_and_the_sweep_ask_the_same_question() {
        assert_eq!(
            tables_named_in(FIND_REPO_ROWS_SQL),
            tables_named_in(DELETE_REPO_ROWS_SQL),
            "the repo-row finder and the repo-row sweep name different tables"
        );
        assert_eq!(
            tables_named_in(FIND_DANGLING_REFERENCES_SQL),
            tables_named_in(CLOSE_DANGLING_REFERENCES_SQL),
            "the reference finder and the reference closure name different tables"
        );
        let pairs = erased_pairs_of(FIND_DANGLING_REFERENCES_SQL);
        assert_eq!(
            pairs,
            erased_pairs_of(CLOSE_DANGLING_REFERENCES_SQL),
            "the reference finder and the reference closure test different columns"
        );
        assert_eq!(
            pairs.len(),
            9,
            "nine non-`t` foreign keys into the core admission table exist; found {pairs:?}"
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
            .chain(tables_named_in(FIND_REPO_ROWS_SQL))
            .chain(tables_named_in(FIND_DANGLING_REFERENCES_SQL))
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
