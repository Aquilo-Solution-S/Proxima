//! Counts and lengths as the `u64` that reports, frames and hashes carry.

/// `count` as a `u64`, saturating at `u64::MAX`.
///
/// A length cannot exceed `u64::MAX` on any supported target, so the saturation is never
/// reached; naming it once keeps every report, frame and hash answering that case the same way.
#[must_use]
pub fn count_u64(count: usize) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::count_u64;

    #[test]
    fn a_count_keeps_its_value() {
        assert_eq!(count_u64(0), 0);
        assert_eq!(count_u64(42), 42);
        assert_eq!(
            count_u64(usize::MAX),
            u64::try_from(usize::MAX).unwrap_or(u64::MAX)
        );
    }
}
