//! Erase one registered repository.
//!
//! The flavor is the only place that knows what "one repository's rows"
//! means; the substrate is the only place that knows how to erase a series.
//! So the split is: [`FIND_REPO_ROWS_SQL`] names the admissions filed under
//! the repository, and [`UnitOfWork::erase_own_series`] erases them, a page
//! at a time (docs/13 §Flavor-scoped erase). The verb expands each to its
//! whole series, erases every row that references the erase set through a
//! foreign key, refuses a footprint that reaches another owner or another
//! flavor's schema, and takes its own locks, fences and transaction. The
//! flavor then retires the registry row under the `code-repo` scope fence
//! ([`retire_repo`]). No storage lock, owner fence or platform transaction
//! is spelled here.
//!
//! The finder names its tables by hand, so it is pinned against the
//! contract in both directions:
//! `every_declared_surface_is_reached_by_the_repo_erase_or_named_as_an_exemption`
//! fails on a surface the contract declares and the finder misses, unless
//! it is listed as an exemption with a reason, and
//! `the_erase_names_no_table_the_contract_does_not_declare` fails on a table
//! the finder names and the contract does not.
//!
//! [`UnitOfWork::erase_own_series`]: proxima_core::UnitOfWork::erase_own_series

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use proxima::flavor::{
    EraseMode, MAX_ERASE_SERIES_PER_CALL, SeriesEraseError, SeriesEraseReceipt,
    SeriesEraseRefusalKind, SeriesSelection,
};
use proxima_core::{AuthzContext, Engine, MemoryId, Owner, StorageError};
use proxima_storage_pg::{
    MAX_TRANSACTION_ATTEMPTS, begin_compatible_owner_transaction, is_transient_conflict,
};
use uuid::Uuid;

use super::records::{RepoEraseReceipt, RepoRegistryError};
use crate::store::CodeFlavorStore;

/// Every `proxima_code` row filed under one repo, oldest admission first,
/// at most `$2`, with the table it was found in.
///
/// Fourteen tables. `work_requested_v1` and `test_requested_v1` are the
/// work items; `acceptance_criteria_v1` and `acceptance_verification_v1`
/// carry no `repo_id` of their own and are found through the work item they
/// belong to. The verb's reference closure would reach them anyway — each
/// holds a foreign key into the work item's admission — but naming them
/// here keeps "what is this repository's" a question the flavor answers,
/// not a side effect of a constraint.
///
/// Detail tables are absent by design: `acceptance_criterion_v1`,
/// `code_chunk_call_v1`, `execution_plan_item_v1` and
/// `test_requested_criterion_v1` cascade from the sidecar above them, and
/// the contract declares exactly that (`EraseRule::Cascade`).
///
/// `commit_summarizer_self_v1` and `engineer_self_v1` are absent for the
/// opposite reason: they are the owner's self-model, carry no `repo_id`,
/// and outlive any one repository. Erasing a repo must not delete the
/// engineer who worked on it.
///
/// `development_perspective_v1.repo_id` is NULLABLE, and `repo_id = $1`
/// therefore never matches a NULL one. The payload documents `None` as
/// "cross-repo observations", so a perspective SERIES that never named this
/// repository belongs to the owner and goes with the owner erase. The unit
/// is the series, not the version: a perspective whose current version says
/// `None` but whose history was filed under this repository IS this
/// repository's, and goes, because the verb erases every series it is
/// handed whole.
/// `a_perspective_about_no_particular_repo_survives_a_repo_erase` pins the
/// first half and
/// `a_perspective_that_dropped_its_repo_id_still_goes_with_the_repo` pins
/// the second.
///
/// Under owner RLS the finder sees only this owner's rows. An admission
/// transferred away keeps its `repo_id` (the transfer moves `owner_id` and
/// nothing else), so without RLS the finder reaches it and the verb refuses
/// it as another owner's; with RLS it is the destination's and the finder
/// never sees it. Either way no other owner's row is erased.
const FIND_REPO_ROWS_SQL: &str = "\
WITH work_items AS (
    SELECT t FROM proxima_code.work_requested_v1 WHERE repo_id = $1
    UNION
    SELECT t FROM proxima_code.test_requested_v1 WHERE repo_id = $1
)
SELECT src, t FROM (
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
 WHERE work_item_memory_id IN (SELECT t FROM work_items)
) AS found
ORDER BY t
LIMIT $2";

/// The repo row, locked.
///
/// `FOR UPDATE` serializes two retirements of the same repository, and
/// blocks an ingestion run starting against it — `repo_ingestion_runs`
/// carries a foreign key to this row, so starting a run needs
/// `FOR KEY SHARE` on it. The sidecar tables do NOT reference `repos`, so
/// what separates the retirement from a sidecar write is the `code-repo`
/// scope fence, taken before this.
const REPO_EXISTS_SQL: &str = "\
SELECT repo_id FROM proxima_code.repos
 WHERE owner_kind = $1 AND owner_id = $2 AND repo_id = $3
   FOR UPDATE";

/// `repo_ingestion_runs` is absent here too: `runs_repo_fk` cascades.
const DELETE_REPO_SQL: &str = "\
DELETE FROM proxima_code.repos
 WHERE owner_kind = $1 AND owner_id = $2 AND repo_id = $3";

/// How long the retirement waits for any one lock before giving up on the
/// attempt.
///
/// Five seconds, the figure the erase verb and the migration path use:
/// waiting FOR a lock is not the same as holding one. Without it the wait
/// inherits the pool's five-minute `statement_timeout` and ends in `57014`,
/// which nothing retries. With it the same wait is a `55P03` in five
/// seconds, which IS transient, so the attempt rolls back and comes round
/// again.
const ERASE_LOCK_TIMEOUT_SQL: &str = "SET LOCAL lock_timeout = '5s'";

/// Erase one registered repo's code-flavor rows and the admissions behind
/// them, then its registration.
///
/// Runs on [`proxima_core::UnitOfWork::erase_own_series`], so the verb's
/// gates are this function's: Admin on `owner` (an Ingest-only principal
/// cannot erase a repository), and, inside a tool handler, a tool the code
/// flavor's contract declares destructive.
///
/// Paged. Each page is one erase unit of at most the verb's cap, halved
/// while its reference closure is over the cap; each commits on its own,
/// and re-erasing an erased series is a no-op, so a failure between pages
/// leaves a repository that is partly erased and still registered — the
/// next call finishes it. Cold objects of each page are purged after that
/// page commits; one that fails stays in `cold_purge_pending` for
/// `proxima-mcp maintain-storage --retry-cold-object-purges`.
///
/// The first page runs even when the finder returns nothing, so the verb's
/// Admin gate stands in front of the registry-row delete too.
///
/// A deadlock, a lock-wait timeout, a series replaced mid-erase, or a row
/// the retirement finds under the fence is retried, not surfaced: the whole
/// attempt re-runs from discovery, up to [`MAX_TRANSACTION_ATTEMPTS`]. The
/// erase takes `FOR UPDATE` on admissions and an ordinary writer takes
/// `FOR KEY SHARE` on them in its own order, so the pair can always be made
/// cyclic, and `PostgreSQL` aborts whichever closed the cycle. By the time
/// the retry starts the writer that won has committed, so its row is
/// visible to the new attempt's discovery and gets erased with the rest.
///
/// # Errors
/// `RepoRegistryError::NotFound` if the repo is not registered for `owner`;
/// `CrossOwnerReference` if another principal's rows point into this repo;
/// `EraseRefused` for any other refusal of the verb; `Protocol` for its
/// authorization and declaration faults; `FootprintIncomplete` when writes
/// keep landing past the retry budget; otherwise database/storage errors.
pub async fn erase_repo(
    engine: &Engine,
    authz: &AuthzContext,
    store: &CodeFlavorStore,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<RepoEraseReceipt, RepoRegistryError> {
    let tally = Tally::default();
    let repo_record_deleted = with_erase_retry(MAX_TRANSACTION_ATTEMPTS, || {
        erase_repo_once(engine, authz, store, owner, repo_id, &tally)
    })
    .await?;
    Ok(RepoEraseReceipt {
        repo_id,
        completed_at: time::OffsetDateTime::now_utc(),
        memories_deleted: tally.memories_deleted.load(Ordering::Relaxed),
        cold_objects_pending: tally.cold_objects_pending.load(Ordering::Relaxed),
        repo_record_deleted,
    })
}

/// What the committed pages erased, across attempts.
///
/// Atomics rather than a `&mut`: the retry loop re-borrows it on every
/// attempt, and a page that committed before a later page failed stays
/// erased, so its counts belong in the receipt.
#[derive(Default)]
struct Tally {
    memories_deleted: AtomicU64,
    cold_objects_pending: AtomicU64,
}

impl Tally {
    fn add(&self, receipt: &SeriesEraseReceipt) {
        self.memories_deleted
            .fetch_add(receipt.versions_erased, Ordering::Relaxed);
        self.cold_objects_pending
            .fetch_add(receipt.cold_objects_pending, Ordering::Relaxed);
    }
}

/// Re-run a whole erase attempt while it fails transiently, `attempts`
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

/// Whether the whole erase attempt is worth re-running.
fn is_transient(err: &RepoRegistryError) -> bool {
    match err {
        RepoRegistryError::Database(err) => is_transient_conflict(err),
        // `FootprintIncomplete` is here for a reason of its own, not because
        // it resembles a deadlock: the ordinary cause is a row committed
        // after the last page and before the retirement took the fence, and
        // re-discovery is exactly what fixes it. The verb's refusals run
        // again on the way, so a CROSS-OWNER row arriving in that window
        // still comes back as `CrossOwnerReference` rather than looping.
        RepoRegistryError::Storage(StorageError::Retryable(_))
        | RepoRegistryError::FootprintIncomplete { .. } => true,
        _ => false,
    }
}

/// One attempt: check, erase page by page, retire.
async fn erase_repo_once(
    engine: &Engine,
    authz: &AuthzContext,
    store: &CodeFlavorStore,
    owner: &Owner,
    repo_id: Uuid,
    tally: &Tally,
) -> Result<bool, RepoRegistryError> {
    if super::registry::get_repo(store.pool(), store.owner_scope(), owner, repo_id)
        .await?
        .is_none()
    {
        return Err(RepoRegistryError::NotFound { repo_id });
    }
    let mut page = MAX_ERASE_SERIES_PER_CALL;
    let mut gated = false;
    loop {
        let found = find_repo_rows(store, repo_id, page).await?;
        if found.is_empty() && gated {
            break;
        }
        let mut seeds: Vec<MemoryId> = found.iter().map(|(_, t)| MemoryId::new(*t)).collect();
        seeds.dedup();
        let mut unit = engine.unit_of_work(authz).await?;
        let receipt = match unit
            .erase_own_series(
                crate::contract::FLAVOR_ID,
                *owner,
                SeriesSelection::Ids(seeds),
                EraseMode::Erase,
            )
            .await
        {
            Ok(receipt) => receipt,
            // The page's closure is over the cap: the unit rolled back with
            // nothing deleted, so ask again for less.
            Err(SeriesEraseError::Refused(refusal))
                if refusal.kind == SeriesEraseRefusalKind::OverCap && page > 1 =>
            {
                page /= 2;
                continue;
            }
            Err(err) => return Err(page_error(repo_id, err, &found)),
        };
        unit.commit().await?;
        gated = true;
        tally.add(&receipt);
        // A page the finder named and the verb erased nothing of: a
        // concurrent erase took it (the retry's discovery no longer finds
        // it), or the finder reaches a row no series erase removes (the
        // budget runs out). Looping here would never end in the second case.
        if let Some((_, first)) = found.first()
            && receipt.versions_erased == 0
        {
            return Err(RepoRegistryError::FootprintIncomplete {
                repo_id,
                memory_id: *first,
            });
        }
    }
    retire_repo(store, owner, repo_id).await
}

/// Up to `limit` rows filed under `repo_id`, as this owner sees them.
async fn find_repo_rows(
    store: &CodeFlavorStore,
    repo_id: Uuid,
    limit: usize,
) -> Result<Vec<(String, Uuid)>, RepoRegistryError> {
    let mut tx = begin_compatible_owner_transaction(store.pool(), store.owner_scope()).await?;
    let found = sqlx::query_as(FIND_REPO_ROWS_SQL)
        .bind(repo_id)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(found)
}

/// The verb's answer, in this registry's terms.
///
/// A refused seed is named by the table the finder found it in, so a
/// refusal names something an operator can go and look at; a series the
/// verb reached through a reference is already named by the referencing
/// column.
fn page_error(repo_id: Uuid, err: SeriesEraseError, found: &[(String, Uuid)]) -> RepoRegistryError {
    match err {
        SeriesEraseError::Refused(refusal)
            if refusal.kind == SeriesEraseRefusalKind::CrossOwner =>
        {
            let tables: BTreeMap<Uuid, &str> = found
                .iter()
                .map(|(table, t)| (*t, table.as_str()))
                .collect();
            let blocking = refusal
                .offending
                .into_iter()
                .map(|offender| {
                    match offender
                        .strip_prefix("t=")
                        .and_then(|t| t.parse::<Uuid>().ok())
                        .and_then(|t| tables.get(&t))
                    {
                        Some(table) => format!("proxima_code.{table} {offender}"),
                        None => offender,
                    }
                })
                .collect();
            RepoRegistryError::CrossOwnerReference { repo_id, blocking }
        }
        SeriesEraseError::Refused(refusal) => RepoRegistryError::EraseRefused { repo_id, refusal },
        SeriesEraseError::Retryable(message) => {
            RepoRegistryError::Storage(StorageError::Retryable(message))
        }
        SeriesEraseError::Protocol(err) => RepoRegistryError::Protocol(err),
    }
}

/// Delete the registry row once nothing is filed under it.
///
/// The declared `code-repo` scope fence, exclusively, BEFORE the last read.
/// Every admission of a payload declaring `CODE_REPO_SCOPE` takes the same
/// key shared — generated from one declaration, so the two sides cannot
/// drift onto two locks — so a same-repository write that has not started
/// waits here and then finds the scope unregistered, and one that has
/// started committed before the read below and is found by it. A row found
/// under the fence is a write that landed after the last page: the attempt
/// is refused as `FootprintIncomplete`, and the retry erases it.
async fn retire_repo(
    store: &CodeFlavorStore,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<bool, RepoRegistryError> {
    let (kind, principal_id) = owner.columns();
    let mut tx = begin_compatible_owner_transaction(store.pool(), store.owner_scope()).await?;
    sqlx::query(ERASE_LOCK_TIMEOUT_SQL)
        .execute(&mut *tx)
        .await?;
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
    let left: Vec<(String, Uuid)> = sqlx::query_as(FIND_REPO_ROWS_SQL)
        .bind(repo_id)
        .bind(1_i64)
        .fetch_all(&mut *tx)
        .await?;
    if let Some((_, memory_id)) = left.first() {
        return Err(RepoRegistryError::FootprintIncomplete {
            repo_id,
            memory_id: *memory_id,
        });
    }
    let deleted = sqlx::query(DELETE_REPO_SQL)
        .bind(kind)
        .bind(principal_id)
        .bind(repo_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    tx.commit().await?;
    Ok(deleted)
}

/// The finder, for the catalog tests that hold it to the schema.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn repo_finder_sql() -> &'static str {
    FIND_REPO_ROWS_SQL
}

#[cfg(test)]
mod tests {
    use super::{DELETE_REPO_SQL, FIND_REPO_ROWS_SQL, RepoRegistryError};
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

    /// Every `src` label the finder reports, which is how a refusal names
    /// the table a seed came from.
    fn labels_in(sql: &'static str) -> BTreeSet<&'static str> {
        sql.lines()
            .filter_map(|line| line.trim().strip_prefix("SELECT '"))
            .filter_map(|rest| rest.split('\'').next())
            .collect()
    }

    /// The point of the whole move: a table added to this flavor and not to
    /// the finder is a table `proxima-code_erase_repo` silently leaves
    /// behind. The core spine could not run this test — it did not know
    /// what the flavor declared, which is exactly how the version this
    /// replaced came to reach five of sixteen sidecars.
    #[test]
    fn every_declared_surface_is_reached_by_the_repo_erase_or_named_as_an_exemption() {
        let found = tables_named_in(FIND_REPO_ROWS_SQL);
        assert_eq!(
            found.len(),
            14,
            "the repo-row finder should name fourteen tables, found {found:?}"
        );
        let repo_row = tables_deleted_by(DELETE_REPO_SQL);

        let mut unreached: Vec<&str> = every_declared_surface()
            .filter(|surface| {
                let table = surface.table;
                !(found.contains(table)
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
             — add them to the finder, or add them to an exemption list with a reason"
        );
    }

    /// A refusal names a seed by its finder label, so every label has to be
    /// the table it is found in — a copy-pasted label would send an
    /// operator to the wrong table.
    #[test]
    fn every_finder_label_is_the_table_it_reads() {
        let labels: BTreeSet<String> = labels_in(FIND_REPO_ROWS_SQL)
            .into_iter()
            .map(|label| format!("proxima_code.{label}"))
            .collect();
        let tables: BTreeSet<String> = tables_named_in(FIND_REPO_ROWS_SQL)
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(labels, tables, "finder labels and finder tables disagree");
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
        assert!(!is_transient(&RepoRegistryError::EraseRefused {
            repo_id: uuid::Uuid::nil(),
            refusal: proxima::flavor::SeriesEraseRefusal {
                kind: proxima::flavor::SeriesEraseRefusalKind::UnerasableReference,
                offending: vec!["proxima_x.y.z t=…".into()],
            },
        }));
        assert!(!is_transient(&RepoRegistryError::Protocol(
            proxima_core::ProtocolError::forbidden("not an admin")
        )));
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

    /// A seed the verb refuses as another owner's is named by the table the
    /// finder found it in; a series reached through a reference keeps the
    /// verb's own name for it.
    #[test]
    fn a_cross_owner_refusal_names_the_table_each_seed_came_from() {
        use proxima::flavor::{SeriesEraseError, SeriesEraseRefusal, SeriesEraseRefusalKind};
        let seed = uuid::Uuid::now_v7();
        let joined = uuid::Uuid::now_v7();
        let err = super::page_error(
            uuid::Uuid::nil(),
            SeriesEraseError::Refused(SeriesEraseRefusal {
                kind: SeriesEraseRefusalKind::CrossOwner,
                offending: vec![
                    format!("t={seed}"),
                    format!("proxima_code.execution_result_v1.work_requested_memory_id t={joined}"),
                ],
            }),
            &[("work_requested_v1".to_owned(), seed)],
        );
        let RepoRegistryError::CrossOwnerReference { blocking, .. } = err else {
            panic!("a cross-owner refusal is a CrossOwnerReference, got {err:?}");
        };
        assert_eq!(
            blocking,
            vec![
                format!("proxima_code.work_requested_v1 t={seed}"),
                format!("proxima_code.execution_result_v1.work_requested_memory_id t={joined}"),
            ]
        );
    }

    #[test]
    fn the_erase_names_no_table_the_contract_does_not_declare() {
        let declared: BTreeSet<&str> = every_declared_surface()
            .map(|surface| surface.table)
            .collect();
        let mut stray: Vec<&str> = tables_named_in(FIND_REPO_ROWS_SQL)
            .union(&tables_deleted_by(DELETE_REPO_SQL))
            .copied()
            .filter(|table| !declared.contains(table))
            .collect();
        stray.sort_unstable();
        assert!(
            stray.is_empty(),
            "the repo erase names tables the contract does not declare: {stray:?}"
        );
    }
}
