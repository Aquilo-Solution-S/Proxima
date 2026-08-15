use proxima_core::{Owner, OwnerRefKind, StorageError};

mod jobs;
mod ops;
mod reconcile;
#[cfg(test)]
mod tests;
mod text;
mod write;

#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use jobs::claim_embedding_jobs_sql_for_tests;
pub(crate) use jobs::enqueue_embedding_job_in_tx;
pub use jobs::{
    claim_pending_embedding_jobs, complete_embedding_job, count_embedding_job_status,
    count_failed_embedding_jobs, count_pending_embedding_jobs, enqueue_missing_embedding_jobs,
    fail_embedding_job, fail_embedding_job_permanently, list_facts_missing_embedding,
    release_embedding_jobs,
};
pub(crate) use ops::{embedding_ann_observability, sweep_orphan_embedding_rows};
pub use reconcile::{
    EmbeddingInlineDrainOutcome, EmbeddingReconcileOptions, EmbeddingReconcileOutcome,
    EmbeddingReconcileScope, drain_embedding_jobs_inline, reconcile_embeddings,
};
pub use text::{load_embedding_text, load_fact_text, load_fact_text_in_tx};
pub(crate) use write::{insert_embedding, insert_embedding_chunks, insert_memory_embedding};

fn owner_parts(owner: &Owner) -> (OwnerRefKind, Option<uuid::Uuid>) {
    owner.columns()
}

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
