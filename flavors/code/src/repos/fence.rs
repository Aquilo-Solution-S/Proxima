//! The repository lifecycle fence.
//!
//! A repository is a scope the substrate cannot see. Every repo-scoped
//! sidecar carries a bare `repo_id uuid` with no foreign key into
//! `proxima_code.repos`, so the `FOR UPDATE` an erase takes on the repo row
//! constrains only what references that row — `repo_ingestion_runs` — and
//! nothing else. Two ingests of the same repository share the owner fence
//! and the one constant source fence (`proxima-code/local-git`) with every
//! other repository, so neither fence separates repository A's erase from
//! repository B's write, and neither serializes A's erase against A's own
//! write. That gap is what let an ingest commit a fresh admission after the
//! erase had computed and swept its footprint: a successful erase, and rows
//! left behind for a repository that no longer exists.
//!
//! The fence closes it in its own advisory-lock namespace. The key spans
//! owner kind, owner id and repo id — the owner portion repeated for the
//! reason it is repeated on the source fence, so a repo id that somehow
//! appeared under two owners never serializes them, and the kind for the
//! reason the owner fence carries it. Repository ids are NEVER hashed into
//! the Memory `t`/handle namespace: a repository is not an admission, and
//! aliasing the two would make a repo id collide with a series handle.
//!
//! # Lock order
//!
//! ```text
//! owner fence -> source fence -> REPO FENCE -> handle / lifecycle `t` -> rows
//! ```
//!
//! [`super::erase::erase_repo`] takes this fence EXCLUSIVELY before it reads
//! anything — fence-before-select, so its footprint is exact by construction
//! rather than by re-checking afterwards. Every transaction that writes a
//! Memory, sidecar, admission or run row carrying a `repo_id` takes it in the
//! same transaction, before its handle/`t` locks, and re-asks whether the
//! repository is still registered while holding it. Under the fence that
//! question has one answer for the life of the transaction: an erase that
//! committed first is visible, and an erase that has not is waiting.
//!
//! Different repositories take different keys and never wait on each other.
//!
//! # Two acquisition paths, one key
//!
//! A flavor-owned transaction ([`super::erase`], [`super::runs`]) takes the
//! fence with [`lock_repo_fence_exclusive_tx`] / [`lock_repo_fence_shared_tx`],
//! which spell `pg_advisory_xact_lock[_shared]` directly. A Memory admission
//! runs in the Engine's write session, which owns its transaction and names
//! its locks by i64 key rather than by statement, so [`fence_repo_admission`]
//! resolves the same key through [`REPO_FENCE_KEY_SQL`] first and hands it to
//! `UnitOfWork::advisory_xact_lock_shared`. Same key, same mode, same lane:
//! two writers into one repository do not wait on each other, and neither
//! path is a weaker fence than the other.

use proxima_core::{Owner, StorageError, UnitOfWork};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::records::RepoRegistryError;

/// The fence key, as `PostgreSQL` computes it. `$1` owner kind, `$2` owner
/// id, `$3` repo id.
///
/// Written once and repeated verbatim inside the three statements below —
/// `the_three_fence_statements_hash_one_key` fails if any of them drifts,
/// because a fence whose two acquisition paths hash different keys is not a
/// fence.
#[cfg(test)]
const REPO_FENCE_KEY_EXPR: &str =
    "hashtextextended('proxima-code-repo-fence:' || $1 || ':' || $2::text || ':' || $3::text, 0)";

/// The key alone, for the callers that must hand it to
/// `UnitOfWork::advisory_xact_lock_shared` instead of taking the lock
/// themselves.
const REPO_FENCE_KEY_SQL: &str = "\
SELECT hashtextextended('proxima-code-repo-fence:' || $1 || ':' || $2::text || ':' || $3::text, 0)";

const LOCK_REPO_FENCE_SHARED_SQL: &str = "\
SELECT pg_advisory_xact_lock_shared(
           hashtextextended('proxima-code-repo-fence:' || $1 || ':' || $2::text || ':' || $3::text, 0)
       )";

const LOCK_REPO_FENCE_EXCLUSIVE_SQL: &str = "\
SELECT pg_advisory_xact_lock(
           hashtextextended('proxima-code-repo-fence:' || $1 || ':' || $2::text || ':' || $3::text, 0)
       )";

/// Is this repository still registered for this owner?
///
/// Deliberately not `FOR SHARE`: the fence, not a row lock, is what holds
/// the answer still, and a row lock here would put `repos` into the write
/// lane's order twice.
const REPO_REGISTERED_SQL: &str = "\
SELECT EXISTS(
    SELECT 1
      FROM proxima_code.repos
     WHERE owner_kind = $1 AND owner_id = $2 AND repo_id = $3
)";

/// Take the repository fence shared, in a flavor-owned transaction.
///
/// The admission side of the fence: several writers into one repository may
/// hold it together, and an erase of that repository may not.
///
/// # Errors
///
/// Database errors from the lock.
pub async fn lock_repo_fence_shared_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<(), RepoRegistryError> {
    let (kind, owner_id) = owner.columns();
    sqlx::query(LOCK_REPO_FENCE_SHARED_SQL)
        .bind(kind.as_str())
        .bind(owner_id)
        .bind(repo_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Take the repository fence exclusively, in a flavor-owned transaction.
///
/// The erase side. Taken BEFORE the erase reads anything it intends to
/// delete, so the footprint it computes is the whole of what this repository
/// can ever hold: a later writer waits here, and an earlier one has already
/// committed and is in the footprint.
///
/// # Errors
///
/// Database errors from the lock.
pub async fn lock_repo_fence_exclusive_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<(), RepoRegistryError> {
    let (kind, owner_id) = owner.columns();
    sqlx::query(LOCK_REPO_FENCE_EXCLUSIVE_SQL)
        .bind(kind.as_str())
        .bind(owner_id)
        .bind(repo_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Whether `repo_id` is still registered for `owner`, asked in a
/// flavor-owned transaction.
///
/// # Errors
///
/// Database errors from the read.
pub(crate) async fn repo_registered_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<bool, RepoRegistryError> {
    let (kind, owner_id) = owner.columns();
    Ok(sqlx::query_scalar(REPO_REGISTERED_SQL)
        .bind(kind)
        .bind(owner_id)
        .bind(repo_id)
        .fetch_one(&mut **tx)
        .await?)
}

/// Fence a Memory admission that carries `repo_id`, inside the Engine's own
/// write transaction.
///
/// Three steps, and the order is the whole of the guarantee:
///
/// 1. resolve the fence key — a pure `hashtextextended` on the pool, before
///    the write session opens, so no connection is held across it;
/// 2. take the fence SHARED in the write transaction, before any handle or
///    `t` lock, which is what keeps a racing erase from closing a cycle with
///    this writer rather than queueing behind it. Shared because the thing
///    to exclude is the erase, not the other writers into the same
///    repository;
/// 3. ask, WHILE HOLDING IT, whether the repository is still registered.
///
/// Step 3 runs on the pool rather than in the write transaction because the
/// write-session port offers no way to read a flavor state table from inside
/// it — and it does not need to. `proxima_code.repos` is read at READ
/// COMMITTED, so it reports every erase that committed before step 2 and, by
/// construction, no erase can commit between step 2 and this transaction's
/// own commit. A vanished row is therefore a settled fact, not a race, and
/// the answer is a refusal.
///
/// The cost is one pool connection held briefly alongside the write
/// transaction's own. Write concurrency in this flavor is bounded well below
/// the pool (one active ingestion run per repository, agent-paced tool
/// writes), and the failure mode of exhausting it is an acquire timeout, not
/// a stall.
///
/// # Errors
///
/// [`RepoRegistryError::NotFound`] when the repository is no longer
/// registered for `owner` — it was never registered, or an erase took the
/// fence first. Database/storage errors from the key resolution, the lock,
/// or the read.
pub(crate) async fn fence_repo_admission(
    pool: &PgPool,
    uow: &mut UnitOfWork<'_>,
    owner: Owner,
    repo_id: Uuid,
) -> Result<(), RepoRegistryError> {
    let (kind, owner_id) = owner.columns();
    let key: i64 = sqlx::query_scalar(REPO_FENCE_KEY_SQL)
        .bind(kind.as_str())
        .bind(owner_id)
        .bind(repo_id)
        .fetch_one(pool)
        .await?;
    uow.advisory_xact_lock_shared(key)
        .await
        .map_err(|err| RepoRegistryError::Storage(StorageError::Internal(err.to_string())))?;
    let registered: bool = sqlx::query_scalar(REPO_REGISTERED_SQL)
        .bind(kind)
        .bind(owner_id)
        .bind(repo_id)
        .fetch_one(pool)
        .await?;
    if !registered {
        return Err(RepoRegistryError::NotFound { repo_id });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        LOCK_REPO_FENCE_EXCLUSIVE_SQL, LOCK_REPO_FENCE_SHARED_SQL, REPO_FENCE_KEY_EXPR,
        REPO_FENCE_KEY_SQL,
    };

    /// One key, three statements. The shared and exclusive locks and the
    /// bare key resolution must hash the same string, or the two acquisition
    /// paths take two different locks and the fence stops being one.
    #[test]
    fn the_three_fence_statements_hash_one_key() {
        // Whitespace is the only thing the multi-line spellings add.
        let collapse = |sql: &str| sql.split_whitespace().collect::<Vec<_>>().join(" ");
        let expr = collapse(REPO_FENCE_KEY_EXPR);
        for sql in [
            REPO_FENCE_KEY_SQL,
            LOCK_REPO_FENCE_SHARED_SQL,
            LOCK_REPO_FENCE_EXCLUSIVE_SQL,
        ] {
            assert!(
                collapse(sql).contains(&expr),
                "fence statement does not hash the one fence key:\n{sql}"
            );
        }
    }

    /// The namespace is its own. A repository id hashed into the Memory
    /// `t`/handle or owner lanes would make a repository collide with an
    /// admission that happens to share its uuid.
    #[test]
    fn the_fence_namespace_is_not_an_existing_one() {
        for existing in [
            "proxima-owner-fence:",
            "proxima-source-fence:",
            "proxima-forget:",
        ] {
            assert!(
                !REPO_FENCE_KEY_EXPR.contains(existing),
                "the repo fence must not share the {existing} namespace"
            );
        }
        assert!(REPO_FENCE_KEY_EXPR.contains("proxima-code-repo-fence:"));
    }
}
