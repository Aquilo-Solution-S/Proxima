//! Visit close is `closed_handle`, not a `source_batches` row.
//! This verb is a no-op so existing flavor callers keep compiling.

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::close_batch::CloseBatchOutcome;
use proxima_core::{SourceBatchId, StorageError};
use sqlx::PgPool;

/// # Errors
///
/// Never fails: there is no batch table to miss.
pub async fn close_batch(
    _pool: &PgPool,
    _permit: &OwnerWritePermit,
    source_batch_id: SourceBatchId,
) -> Result<CloseBatchOutcome, StorageError> {
    Ok(CloseBatchOutcome {
        source_batch_id,
        closed_at: time::OffsetDateTime::now_utc(),
        already_closed: true,
    })
}
