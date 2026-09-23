use proxima_core::StorageError;

mod jobs;
mod ops;
mod reconcile;
pub(crate) use reconcile::{embedding_owner_page, reconcile_embeddings_with_platform};
mod spaces;
pub(crate) use spaces::{
    RouteSpaces, embedding_coverage, enqueue_series_head_in_tx, purge_embedding_spaces,
    purge_series_embedding_spaces_in_tx,
};
#[cfg(test)]
mod tests;
mod text;
mod write;

#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use jobs::claim_embedding_jobs_sql_for_tests;
pub(crate) use jobs::enqueue_embedding_jobs_in_tx;
pub use jobs::{
    claim_pending_embedding_jobs, complete_embedding_job, count_embedding_job_status,
    enqueue_missing_embedding_jobs, fail_embedding_job, fail_embedding_job_permanently,
    list_facts_missing_embedding, reclaim_stale_embedding_jobs, release_embedding_jobs,
    release_embedding_jobs_on_connection, renew_embedding_jobs,
};
pub(crate) use ops::{embedding_ann_observability, sweep_orphan_embedding_rows};
pub use reconcile::{
    EmbeddingReconcileOptions, EmbeddingReconcileOutcome, EmbeddingReconcileScope,
    reconcile_embeddings,
};
pub use text::{
    load_embedding_text, load_embedding_text_on_connection, load_embedding_texts,
    load_embedding_texts_on_connection, load_fact_text, load_fact_text_in_tx,
};
pub(crate) use write::{insert_embedding, insert_memory_embedding, lock_embedding_job_claim};

fn ensure_nonnegative_limit(limit: i64) -> Result<i64, StorageError> {
    if limit < 0 {
        return Err(StorageError::ConstraintViolation(
            "limit must be nonnegative".into(),
        ));
    }
    Ok(limit)
}

fn nonnegative_count(value: i64, name: &str) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| StorageError::Internal(format!("{name} count is negative")))
}

fn usize_count(value: usize, name: &str) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| StorageError::Internal(format!("{name} count too large")))
}

fn ratio_count(value: u64, name: &str) -> Result<u32, StorageError> {
    u32::try_from(value).map_err(|_| StorageError::Internal(format!("{name} count too large")))
}
