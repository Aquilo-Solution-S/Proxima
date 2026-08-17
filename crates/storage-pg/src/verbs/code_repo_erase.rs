//! Erase one registered repo: flavor sidecars + their memory series + repo row.

use proxima_core::{Owner, StorageError};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;

type Tx<'a> = Transaction<'a, Postgres>;

#[derive(Debug, Clone)]
pub struct CodeRepoEraseOutcome {
    pub repo_id: Uuid,
    pub completed_at: time::OffsetDateTime,
    pub facts_deleted: u64,
    pub abstractions_deleted: u64,
    pub edges_deleted: u64,
    pub embeddings_deleted: u64,
    pub receipts_deleted: u64,
    pub source_batches_deleted: u64,
    pub repo_record_deleted: bool,
}

/// Erase one code-flavor repo and the memory series its sidecars name.
///
/// Returns `Ok(None)` when the repo record does not exist for `owner`.
///
/// # Errors
///
/// Returns storage errors from the delete transaction.
pub async fn erase_code_repo(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<Option<CodeRepoEraseOutcome>, StorageError> {
    erase_code_repo_inner(pool, owner, repo_id)
        .await
        .map_err(map_err)
}

async fn erase_code_repo_inner(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<Option<CodeRepoEraseOutcome>, sqlx::Error> {
    let owner_id = owner.stored_owner_id();
    let mut tx = pool.begin().await?;
    let exists: Option<(Uuid,)> = sqlx::query_as(
        "SELECT repo_id FROM proxima_code.repos
          WHERE owner_id = $1 AND repo_id = $2",
    )
    .bind(owner_id)
    .bind(repo_id)
    .fetch_optional(&mut *tx)
    .await?;
    if exists.is_none() {
        return Ok(None);
    }

    let ts: Vec<Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_code.file_revision_v1 WHERE repo_id = $1
         UNION
         SELECT t FROM proxima_code.code_chunk_v1 WHERE repo_id = $1
         UNION
         SELECT t FROM proxima_code.commit_v1 WHERE repo_id = $1
         UNION
         SELECT t FROM proxima_code.commit_summary_v1 WHERE repo_id = $1
         UNION
         SELECT t FROM proxima_code.test_requested_v1 WHERE repo_id = $1",
    )
    .bind(repo_id)
    .fetch_all(&mut *tx)
    .await?;

    sqlx::query(
        "DELETE FROM proxima_code.code_chunk_call_v1
                  WHERE caller_memory_id = ANY($1::uuid[])
                     OR callee_memory_id = ANY($1::uuid[])",
    )
    .bind(&ts)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM proxima_code.code_chunk_v1 WHERE repo_id = $1")
        .bind(repo_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM proxima_code.file_revision_v1 WHERE repo_id = $1")
        .bind(repo_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM proxima_code.commit_summary_v1 WHERE repo_id = $1")
        .bind(repo_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM proxima_code.commit_v1 WHERE repo_id = $1")
        .bind(repo_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "DELETE FROM proxima_code.test_requested_criterion_v1
                  WHERE test_requested_memory_id = ANY($1::uuid[])",
    )
    .bind(&ts)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM proxima_code.test_requested_v1 WHERE repo_id = $1")
        .bind(repo_id)
        .execute(&mut *tx)
        .await?;

    let deleted = delete_memory_series(&mut tx, owner_id, &ts).await?;
    sqlx::query("DELETE FROM proxima_code.repo_ingestion_runs WHERE repo_id = $1")
        .bind(repo_id)
        .execute(&mut *tx)
        .await?;
    let repo_record_deleted =
        sqlx::query("DELETE FROM proxima_code.repos WHERE owner_id = $1 AND repo_id = $2")
            .bind(owner_id)
            .bind(repo_id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0;
    tx.commit().await?;

    Ok(Some(CodeRepoEraseOutcome {
        repo_id,
        completed_at: time::OffsetDateTime::now_utc(),
        facts_deleted: deleted,
        abstractions_deleted: 0,
        edges_deleted: 0,
        embeddings_deleted: 0,
        receipts_deleted: 0,
        source_batches_deleted: 0,
        repo_record_deleted,
    }))
}

async fn delete_memory_series(
    tx: &mut Tx<'_>,
    owner_id: Uuid,
    ts: &[Uuid],
) -> Result<u64, sqlx::Error> {
    if ts.is_empty() {
        return Ok(0);
    }
    sqlx::query(
        "DELETE FROM proxima_core.announce
          WHERE owner_id = $1 AND t = ANY($2::uuid[])",
    )
    .bind(owner_id)
    .bind(ts)
    .execute(&mut **tx)
    .await?;
    let handles: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT handle FROM proxima_core.memory WHERE t = ANY($1::uuid[])",
    )
    .bind(ts)
    .fetch_all(&mut **tx)
    .await?;
    let all_t: Vec<Uuid> =
        sqlx::query_scalar("SELECT t FROM proxima_core.memory WHERE handle = ANY($1::uuid[])")
            .bind(&handles)
            .fetch_all(&mut **tx)
            .await?;
    sqlx::query("DELETE FROM proxima_core.embedding_jobs WHERE entity_id = ANY($1::uuid[])")
        .bind(&all_t)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM proxima_core.embedding_heads WHERE entity_id = ANY($1::uuid[])")
        .bind(&all_t)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM proxima_core.embeddings WHERE entity_id = ANY($1::uuid[])")
        .bind(&all_t)
        .execute(&mut **tx)
        .await?;
    let result = sqlx::query("DELETE FROM proxima_core.memory WHERE handle = ANY($1::uuid[])")
        .bind(&handles)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM proxima_core.memory_head WHERE handle = ANY($1::uuid[])")
        .bind(&handles)
        .execute(&mut **tx)
        .await?;
    Ok(result.rows_affected())
}
