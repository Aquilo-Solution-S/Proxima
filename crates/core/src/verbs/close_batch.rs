//! `CloseBatch` verb — owner-scoped, idempotent transition of a
//! `source_batch.closed_at` from NULL to `now()`.
//!
//! See docs/01-event-source.md §"The contract" and
//! docs/04-consolidation.md §"Source-batch lifecycle". Sources signal
//! batch completion via `Engine::close_batch(source_batch_id)`; F→A
//! consolidation gates on `closed_at IS NOT NULL`.
//!
//! Idempotent — closing an already-closed batch is a no-op and returns
//! the existing `closed_at`. Cross-owner closes return `NotFound` to
//! avoid information leak.

use crate::SourceBatchId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseBatchOutcome {
    pub source_batch_id: SourceBatchId,
    pub closed_at: time::OffsetDateTime,
    /// True iff the batch was already closed before this call.
    pub already_closed: bool,
}
