use proxima_core::storage_ports::{EmbeddingJobStatusCounts, OwnerWritePermit};
use proxima_core::{
    EmbeddingJobClaim, EmbeddingSpace, MemoryId, Owner, OwnerRefKind, StorageError,
};
use sqlx::{PgConnection, PgExecutor, PgPool};

use crate::error::map_err;
use crate::pg_enums::PgMemoryKind;

use super::{ensure_nonnegative_limit, nonnegative_count};

/// Claim pending jobs across every embedding space, ordered by `job_id`.
///
/// One arm: `status = 'pending'`, minus the Owners the drainer is backing
/// off. Rides `embedding_jobs_pending_claim_idx (job_id) WHERE status =
/// 'pending'`. Locked unclaimed rows release with the statement's
/// transaction. `claimed_at` is what makes a crashed drainer's row
/// recoverable ([`reclaim_stale_embedding_jobs`]); there is no
/// `next_attempt_at` column. Each claim also gets a fencing token so a
/// reclaimed worker cannot complete a successor's claim. The table's unique
/// `(owner_id, entity_id, model_id, dim)` key guarantees at most one job for
/// an entity in this space; callers never need an invocation-sized exclusion
/// list to prevent duplicate work.
const CLAIM_EMBEDDING_JOBS_SQL: &str = "WITH claimed AS (
             SELECT job_id
               FROM proxima_core.embedding_jobs
              WHERE status = 'pending'
                AND owner_id <> ALL($2::uuid[])
              ORDER BY job_id ASC
              FOR UPDATE SKIP LOCKED
              LIMIT $1
         )
         UPDATE proxima_core.embedding_jobs j
            SET status = 'processing',
                claimed_at = now(),
                claim_token = uuidv7()
           FROM claimed, proxima_core.memory m, proxima_core.owners o
          WHERE j.job_id = claimed.job_id
            AND m.t = j.entity_id
            AND o.owner_id = j.owner_id
        RETURNING o.kind AS owner_kind,
                  j.job_id,
                  j.owner_id,
                  m.kind AS entity_kind,
                  j.entity_id,
                  j.model_id,
                  j.dim,
                  j.claim_token";

/// The claim statement, for EXPLAIN-based plan guards. Same cfg gate as
/// `search.rs`'s `*_sql_for_tests` exports.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn claim_embedding_jobs_sql_for_tests() -> &'static str {
    CLAIM_EMBEDDING_JOBS_SQL
}

#[derive(sqlx::FromRow)]
struct EmbeddingJobClaimRow {
    owner_kind: OwnerRefKind,
    job_id: uuid::Uuid,
    owner_id: uuid::Uuid,
    entity_kind: PgMemoryKind,
    entity_id: uuid::Uuid,
    model_id: String,
    dim: i16,
    claim_token: uuid::Uuid,
}

impl TryFrom<EmbeddingJobClaimRow> for EmbeddingJobClaim {
    type Error = StorageError;

    fn try_from(row: EmbeddingJobClaimRow) -> Result<Self, StorageError> {
        Ok(Self {
            job_id: row.job_id,
            owner: row.owner_kind.with_uuid(row.owner_id),
            entity_kind: row.entity_kind.into(),
            entity_id: MemoryId::new(row.entity_id),
            space: crate::pgvector::stored_space(row.model_id, row.dim)?,
            claim_token: row.claim_token,
        })
    }
}

/// Owner-scoped list of Facts with rendered text and no embedding row
/// in `space`.
///
/// # Errors
///
/// Returns `ConstraintViolation` when `limit` is too large for
/// Postgres `bigint`, otherwise maps SQL failures through the shared
/// mapper.
pub async fn list_facts_missing_embedding<'e>(
    pool: impl PgExecutor<'e>,
    owner: &Owner,
    space: &EmbeddingSpace,
    limit: usize,
    non_embeddable_schemas: &[String],
) -> Result<Vec<MemoryId>, StorageError> {
    let owner_id = owner.stored_owner_id();
    let limit = i64::try_from(limit)
        .map_err(|_| StorageError::ConstraintViolation("limit too large".into()))?;
    missing_embedding_ids(pool, owner_id, space, limit, non_embeddable_schemas, false).await
}

async fn missing_embedding_ids<'e>(
    pool: impl PgExecutor<'e>,
    owner_id: uuid::Uuid,
    space: &EmbeddingSpace,
    limit: i64,
    non_embeddable_schemas: &[String],
    exclude_existing_jobs: bool,
) -> Result<Vec<MemoryId>, StorageError> {
    // Chunks are memory rows. A second arm against code_chunk_v1 is a subset of
    // this anti-join and duplicates t.
    let rows = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT m.t
           FROM proxima_core.memory_head h
           JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
          WHERE m.owner_id = $1
            AND NOT (m.schema_id = ANY($4::text[]))
            AND NOT EXISTS (
                SELECT 1 FROM proxima_core.embedding_heads eh
                 WHERE eh.entity_id = m.t AND eh.model_id = $2 AND eh.dim = $6
            )
            AND (NOT $5::boolean OR NOT EXISTS (
                SELECT 1 FROM proxima_core.embedding_jobs j
                 WHERE j.owner_id = m.owner_id
                   AND j.entity_id = m.t
                   AND j.model_id = $2
                   AND j.dim = $6
            ))
          ORDER BY m.t ASC
          LIMIT $3",
    )
    .bind(owner_id)
    .bind(space.model_id())
    .bind(limit)
    .bind(non_embeddable_schemas)
    .bind(exclude_existing_jobs)
    .bind(crate::pgvector::Lane::of(space.dim()).width)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().map(MemoryId::new).collect())
}

/// Atomically claim pending embedding jobs in queue order, across every
/// embedding space, skipping `skip_owners`' jobs.
///
/// Selects `status = 'pending'` rows, `FOR UPDATE SKIP LOCKED`, then sets
/// `processing` and stamps `claimed_at`. Each claim carries its own space: the
/// drainer routes the job's Owner and embeds only when that route still names
/// it. There is no `next_attempt_at`
/// column; a claim a drainer never finishes is recovered by
/// [`reclaim_stale_embedding_jobs`].
///
/// # Errors
///
/// Returns `ConstraintViolation` for negative limits, otherwise maps SQL
/// failures through the shared mapper.
pub async fn claim_pending_embedding_jobs<'e, E: PgExecutor<'e>>(
    pool: E,
    limit: i64,
    skip_owners: &[Owner],
) -> Result<Vec<EmbeddingJobClaim>, StorageError> {
    let limit = ensure_nonnegative_limit(limit)?;
    if limit == 0 {
        return Ok(Vec::new());
    }
    let skip_owner_ids: Vec<uuid::Uuid> = skip_owners
        .iter()
        .copied()
        .map(Owner::stored_owner_id)
        .collect();
    // SQL-POLICY: fixed-fragment — the compile-time claim constant above;
    // every value is bound.
    let rows = sqlx::query_as::<_, EmbeddingJobClaimRow>(CLAIM_EMBEDDING_JOBS_SQL)
        .bind(limit)
        .bind(&skip_owner_ids)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;
    rows.into_iter().map(EmbeddingJobClaim::try_from).collect()
}

/// The predicate every claimed job's transition carries: the claim's token
/// while the job is still `processing`, under the Owner, memory and space it
/// was claimed for. A successor claim, or a transfer of the memory while the
/// provider worked outside any transaction, leaves the statement touching
/// nothing, so a reclaimed drainer cannot settle another drainer's job.
/// Binds `$1..$6` through [`bind_claim_fence`].
macro_rules! claim_fence {
    () => {
        "job_id = $1
            AND claim_token = $2
            AND status = 'processing'
            AND owner_id = $3
            AND entity_id = $4
            AND model_id = $5
            AND dim = $6"
    };
}
pub(super) use claim_fence;

type PgQuery<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;

/// The values [`claim_fence`] matches.
#[derive(Clone, Copy)]
pub(super) struct ClaimFence<'a> {
    pub(super) job_id: uuid::Uuid,
    pub(super) claim_token: uuid::Uuid,
    pub(super) owner: &'a Owner,
    pub(super) entity_id: uuid::Uuid,
    pub(super) space: &'a EmbeddingSpace,
}

impl<'a> From<&'a EmbeddingJobClaim> for ClaimFence<'a> {
    fn from(claim: &'a EmbeddingJobClaim) -> Self {
        Self {
            job_id: claim.job_id,
            claim_token: claim.claim_token,
            owner: &claim.owner,
            entity_id: claim.entity_id.into_inner(),
            space: &claim.space,
        }
    }
}

/// Bind `fence` to the placeholders of [`claim_fence`].
pub(super) fn bind_claim_fence<'q>(query: PgQuery<'q>, fence: ClaimFence<'q>) -> PgQuery<'q> {
    query
        .bind(fence.job_id)
        .bind(fence.claim_token)
        .bind(fence.owner.stored_owner_id())
        .bind(fence.entity_id)
        .bind(fence.space.model_id())
        .bind(crate::pgvector::Lane::of(fence.space.dim()).width)
}

/// A fenced statement that matched nothing: the claim was lost.
fn claim_held(rows_affected: u64) -> Result<(), StorageError> {
    if rows_affected == 0 {
        return Err(StorageError::Conflict(
            "embedding job claim is stale or no longer processing".into(),
        ));
    }
    Ok(())
}

/// How a claimed job leaves `processing` without a vector.
#[derive(Debug, Clone, Copy)]
enum ClaimExit {
    /// Attempted and failed for a retryable cause.
    Failed,
    /// The provider rejects the input for a permanent cause.
    FailedPermanent,
    /// Claimed but not attempted: claimable again at once.
    Released,
}

impl ClaimExit {
    const fn status(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::FailedPermanent => "failed_permanent",
            Self::Released => "pending",
        }
    }
}

async fn exit_claim<'e, E: PgExecutor<'e>>(
    pool: E,
    claim: &EmbeddingJobClaim,
    exit: ClaimExit,
    error: &str,
) -> Result<(), StorageError> {
    let result = bind_claim_fence(
        sqlx::query(concat!(
            "UPDATE proxima_core.embedding_jobs
                SET status = $7::proxima_core.embedding_job_status,
                    claimed_at = NULL,
                    claim_token = NULL,
                    last_error = $8
              WHERE ",
            claim_fence!()
        )),
        claim.into(),
    )
    .bind(exit.status())
    .bind(error)
    .execute(pool)
    .await
    .map_err(map_err)?;
    claim_held(result.rows_affected())
}

/// Delete a completed embedding job, fenced by its claim.
///
/// # Errors
///
/// Returns `Conflict` for a stale/non-processing claim; maps SQL failures
/// through the shared mapper.
pub async fn complete_embedding_job<'e, E: PgExecutor<'e>>(
    pool: E,
    claim: &EmbeddingJobClaim,
) -> Result<(), StorageError> {
    let result = bind_claim_fence(
        sqlx::query(concat!(
            "DELETE FROM proxima_core.embedding_jobs WHERE ",
            claim_fence!()
        )),
        claim.into(),
    )
    .execute(pool)
    .await
    .map_err(map_err)?;
    claim_held(result.rows_affected())
}

/// Refresh the lease timestamp for token- and owner-matching processing claims.
///
/// Missing claims are skipped rather than treated as a conflict: a batch
/// heartbeat includes claims that earlier steps may already have completed.
/// Claim-token fencing still prevents an old drainer from renewing or
/// mutating a successor claim.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub async fn renew_embedding_jobs<'e, E: PgExecutor<'e>>(
    pool: E,
    claims: &[EmbeddingJobClaim],
) -> Result<u64, StorageError> {
    if claims.is_empty() {
        return Ok(0);
    }
    let job_ids: Vec<uuid::Uuid> = claims.iter().map(|claim| claim.job_id).collect();
    let claim_tokens: Vec<uuid::Uuid> = claims.iter().map(|claim| claim.claim_token).collect();
    let owner_ids: Vec<uuid::Uuid> = claims
        .iter()
        .map(|claim| claim.owner.stored_owner_id())
        .collect();
    let result = sqlx::query(
        "UPDATE proxima_core.embedding_jobs j
            SET claimed_at = now()
           FROM unnest($1::uuid[], $2::uuid[], $3::uuid[])
                AS claim(job_id, claim_token, owner_id)
          WHERE j.job_id = claim.job_id
            AND j.claim_token = claim.claim_token
            AND j.owner_id = claim.owner_id
            AND j.status = 'processing'",
    )
    .bind(&job_ids)
    .bind(&claim_tokens)
    .bind(&owner_ids)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
}

/// Fail an attempted job for a retryable cause: `failed`, with `error`
/// kept on the row.
///
/// There is no attempt counter or `next_attempt_at`, so `failed` is the retry
/// dead-end that `reconcile_embeddings` lifts a memory out of — requeueing
/// here instead would spin a broken provider at the drain loop's interval with
/// nothing recording why. Permanent rejection uses
/// [`fail_embedding_job_permanently`]; a claimed-but-unattempted job uses
/// [`release_embedding_jobs`].
///
/// # Errors
///
/// Returns `Conflict` for a stale/non-processing claim; maps SQL failures
/// through the shared mapper.
pub async fn fail_embedding_job<'e, E: PgExecutor<'e>>(
    pool: E,
    claim: &EmbeddingJobClaim,
    error: &str,
) -> Result<(), StorageError> {
    exit_claim(pool, claim, ClaimExit::Failed, error).await
}

/// Terminally fail a job whose input the provider rejects for a permanent
/// cause (e.g. over the embedding model's token limit): `failed_permanent`,
/// with `error` kept on the row.
///
/// The separate status — not a marker string inside `last_error` — is what
/// keeps `reconcile_embeddings` from cycling the job reject-retry forever:
/// its requeue arm names `failed` only.
///
/// # Errors
///
/// Returns `Conflict` for a stale/non-processing claim; maps SQL failures
/// through the shared mapper.
pub async fn fail_embedding_job_permanently<'e, E: PgExecutor<'e>>(
    pool: E,
    claim: &EmbeddingJobClaim,
    error: &str,
) -> Result<(), StorageError> {
    exit_claim(pool, claim, ClaimExit::FailedPermanent, error).await
}

/// Return claimed-but-unattempted jobs to `pending`.
///
/// Batch-drain uses this when one provider call covering many jobs
/// fails for a transient cause. Nothing was tried, so the rows are
/// immediately claimable again; `error` records why they were let go.
///
/// # Errors
///
/// Returns `Conflict` when any claim is stale/non-processing; maps SQL
/// failures through the shared mapper. The batch is atomic.
pub async fn release_embedding_jobs(
    pool: &PgPool,
    claims: &[EmbeddingJobClaim],
    error: &str,
) -> Result<(), StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    release_embedding_jobs_on_connection(&mut tx, claims, error).await?;
    tx.commit().await.map_err(map_err)
}

/// # Errors
/// Returns storage errors from query execution or invalid stored data.
pub async fn release_embedding_jobs_on_connection(
    pool: &mut PgConnection,
    claims: &[EmbeddingJobClaim],
    error: &str,
) -> Result<(), StorageError> {
    for claim in claims {
        exit_claim(&mut *pool, claim, ClaimExit::Released, error).await?;
    }
    Ok(())
}

/// Return `processing` jobs claimed more than `older_than_seconds` ago to
/// `pending`.
///
/// The one recovery path for a drainer that died holding a claim. Rows with
/// no `claimed_at` at all are stale by definition — nothing can date them,
/// so nothing else can ever free them.
///
/// # Errors
///
/// Returns `ConstraintViolation` for a non-positive window, otherwise maps
/// SQL failures through the shared mapper.
pub async fn reclaim_stale_embedding_jobs<'e, E: PgExecutor<'e>>(
    pool: E,
    older_than_seconds: i64,
) -> Result<u64, StorageError> {
    if older_than_seconds < 1 {
        return Err(StorageError::ConstraintViolation(
            "stale processing reclaim window must be positive".into(),
        ));
    }
    let result = sqlx::query(
        "UPDATE proxima_core.embedding_jobs
            SET status = 'pending',
                claimed_at = NULL,
                claim_token = NULL
          WHERE status = 'processing'
            AND (
                claimed_at IS NULL
                OR claimed_at < now()
                    - make_interval(secs => ($1::bigint)::double precision)
            )",
    )
    .bind(older_than_seconds)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
}

/// Enqueue pending jobs for owner-scoped Facts missing a current
/// embedding.
///
/// # Errors
///
/// Returns `ConstraintViolation` for negative limits, otherwise maps SQL
/// failures through the shared mapper.
pub async fn enqueue_missing_embedding_jobs(
    pool: &PgPool,
    permit: &OwnerWritePermit,
    space: &EmbeddingSpace,
    limit: i64,
    non_embeddable_schemas: &[String],
) -> Result<u64, StorageError> {
    let mut tx = crate::begin_compatible_owner_transaction(pool, permit.owner_scope()).await?;
    let result = enqueue_missing_embedding_jobs_on_connection(
        tx.as_mut(),
        permit,
        space,
        limit,
        non_embeddable_schemas,
    )
    .await;
    crate::owner_scope::finish_transaction(tx, result).await
}

async fn enqueue_missing_embedding_jobs_on_connection(
    pool: &mut PgConnection,
    permit: &OwnerWritePermit,
    space: &EmbeddingSpace,
    limit: i64,
    non_embeddable_schemas: &[String],
) -> Result<u64, StorageError> {
    let limit = ensure_nonnegative_limit(limit)?;
    if limit == 0 {
        return Ok(0);
    }
    let owner_id = permit.owner().stored_owner_id();
    // Existing jobs are already accounted for, regardless of their status.
    // Exclude them before limiting so they cannot hide later missing work.
    let ids = missing_embedding_ids(
        &mut *pool,
        owner_id,
        space,
        limit,
        non_embeddable_schemas,
        true,
    )
    .await?;
    if ids.is_empty() {
        return Ok(0);
    }
    let entity_ids: Vec<uuid::Uuid> = ids.into_iter().map(MemoryId::into_inner).collect();
    let result = sqlx::query(
        "INSERT INTO proxima_core.embedding_jobs (entity_id, model_id, dim, owner_id)
         SELECT t, $2, $4, $1
           FROM unnest($3::uuid[]) AS t
         ON CONFLICT (owner_id, entity_id, model_id, dim)
         DO NOTHING",
    )
    .bind(owner_id)
    .bind(space.model_id())
    .bind(&entity_ids)
    .bind(crate::pgvector::Lane::of(space.dim()).width)
    .execute(&mut *pool)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
}

/// Owner-scoped pending+failed embedding job counts in one round trip.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub async fn count_embedding_job_status<'e>(
    pool: impl PgExecutor<'e>,
    owner: &Owner,
) -> Result<EmbeddingJobStatusCounts, StorageError> {
    let owner_id = owner.stored_owner_id();
    let row: (i64, i64) = sqlx::query_as(
        "SELECT
             count(*) FILTER (WHERE status IN ('pending', 'processing')),
             count(*) FILTER (WHERE status IN ('failed', 'failed_permanent'))
           FROM proxima_core.embedding_jobs
          WHERE owner_id = $1",
    )
    .bind(owner_id)
    .fetch_one(pool)
    .await
    .map_err(map_err)?;
    Ok(EmbeddingJobStatusCounts {
        pending: nonnegative_count(row.0, "pending embedding job")?,
        failed: nonnegative_count(row.1, "failed embedding job")?,
    })
}

/// Enqueue `entity_id`'s durable embedding jobs, one per space, in the
/// caller's transaction, so the job rows and the memory row land together or
/// not at all.
///
/// Idempotent on the table's natural key `(owner_id, entity_id, model_id,
/// dim)`, which is why a replayed write and a re-enqueued deferral are both
/// free. The row records no memory kind, so every kind queues the same way.
///
/// # Errors
///
/// Maps SQL failures through the shared mapper.
pub(crate) async fn enqueue_embedding_jobs_in_tx<'s>(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner_id: uuid::Uuid,
    entity_id: uuid::Uuid,
    spaces: impl IntoIterator<Item = &'s EmbeddingSpace>,
) -> Result<(), StorageError> {
    let (model_ids, dims): (Vec<&str>, Vec<i16>) = spaces
        .into_iter()
        .map(|space| {
            (
                space.model_id(),
                crate::pgvector::Lane::of(space.dim()).width,
            )
        })
        .unzip();
    if model_ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO proxima_core.embedding_jobs (entity_id, model_id, dim, owner_id)
         SELECT $1, space.model_id, space.dim, $4
           FROM unnest($2::text[], $3::smallint[]) AS space(model_id, dim)
         ON CONFLICT (owner_id, entity_id, model_id, dim)
         DO NOTHING",
    )
    .bind(entity_id)
    .bind(&model_ids)
    .bind(&dims)
    .bind(owner_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The claim is one statement: it exists to hand a drainer work in a
    /// single round trip.
    #[test]
    fn claiming_is_a_single_statement() {
        assert!(!CLAIM_EMBEDDING_JOBS_SQL.contains(';'));
    }

    /// Golden text: the claim is pinned per arm, so an edit to either
    /// ordered scan is a deliberate change to this test as well.
    #[test]
    fn the_claim_sql_is_pinned() {
        assert_eq!(CLAIM_EMBEDDING_JOBS_SQL, CLAIM_GOLDEN);
    }

    #[test]
    fn missing_embedding_scan_does_not_probe_flavor_tables() {
        let src = include_str!("jobs.rs");
        let needle = format!("{}{}", "to_reg", "class(");
        assert!(
            !src.contains(&needle),
            "chunks are memory rows; do not dual-scan proxima_code"
        );
    }

    /// `reclaim_stale_embedding_jobs` can only date a claim the claim
    /// itself stamped.
    #[test]
    fn the_claim_stamps_claimed_at() {
        assert!(CLAIM_EMBEDDING_JOBS_SQL.contains("claimed_at = now()"));
    }

    #[test]
    fn the_claim_names_pending_only() {
        assert_eq!(
            CLAIM_EMBEDDING_JOBS_SQL
                .matches("status = 'pending'")
                .count(),
            1
        );
    }

    #[test]
    fn claim_needs_no_growing_entity_exclusion_parameter() {
        let migration = include_str!("../../../migrations/0015_v016_embedding_spaces.sql");
        assert!(
            migration.contains("UNIQUE (owner_id, entity_id, model_id, dim)"),
            "the DB must admit at most one job per entity and space"
        );
        assert!(!CLAIM_EMBEDDING_JOBS_SQL.contains("ANY("));
        assert!(!CLAIM_EMBEDDING_JOBS_SQL.contains("$3"));
    }

    #[test]
    fn job_claim_shape_tracks_durable_claim_state() {
        let claim = EmbeddingJobClaim {
            job_id: uuid::Uuid::from_u128(2),
            owner: Owner::Personal(proxima_core::UserId::new(uuid::Uuid::from_u128(1))),
            entity_kind: proxima_core::EntityKind::Fact,
            entity_id: MemoryId::new(uuid::Uuid::from_u128(3)),
            space: EmbeddingSpace::new("model", proxima_core::EmbeddingDim::D768),
            claim_token: uuid::Uuid::from_u128(4),
        };
        assert_eq!(claim.space.model_id(), "model");

        let migration = include_str!("../../../migrations/0001_v008.sql");
        let job_table = migration
            .split_once("CREATE TABLE proxima_core.embedding_jobs (")
            .expect("the migration defines embedding_jobs")
            .1
            .split_once("\n);")
            .expect("embedding_jobs definition is closed")
            .0;
        assert!(!job_table.contains("embedding_version"));
        assert!(!job_table.contains("attempts"));
    }

    const CLAIM_GOLDEN: &str = r"WITH claimed AS (
             SELECT job_id
               FROM proxima_core.embedding_jobs
              WHERE status = 'pending'
                AND owner_id <> ALL($2::uuid[])
              ORDER BY job_id ASC
              FOR UPDATE SKIP LOCKED
              LIMIT $1
         )
         UPDATE proxima_core.embedding_jobs j
            SET status = 'processing',
                claimed_at = now(),
                claim_token = uuidv7()
           FROM claimed, proxima_core.memory m, proxima_core.owners o
          WHERE j.job_id = claimed.job_id
            AND m.t = j.entity_id
            AND o.owner_id = j.owner_id
        RETURNING o.kind AS owner_kind,
                  j.job_id,
                  j.owner_id,
                  m.kind AS entity_kind,
                  j.entity_id,
                  j.model_id,
                  j.dim,
                  j.claim_token";
}
