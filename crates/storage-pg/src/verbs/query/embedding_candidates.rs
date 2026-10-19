//! Bounded expansion when store admission drops vector-index candidates.

pub(crate) use crate::vector_index::SEMANTIC_SCAN_CAP;

/// Expand a full index window until enough candidates pass the store's read.
/// Exhaustion and the scan cap terminate even when fewer results exist.
pub(super) fn next_candidate_window(
    window: u32,
    fetched: usize,
    admitted: usize,
    target: u32,
) -> Option<u32> {
    let fetched = u32::try_from(fetched).unwrap_or(u32::MAX);
    let admitted = u32::try_from(admitted).unwrap_or(u32::MAX);
    (fetched >= window && admitted < target && window < SEMANTIC_SCAN_CAP)
        .then(|| window.saturating_mul(2).min(SEMANTIC_SCAN_CAP))
}
