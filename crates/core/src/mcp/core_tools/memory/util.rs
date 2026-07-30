use crate::McpToolError;
use crate::verbs::goal_write::IdempotencyKey;

/// Normalize an explicitly-provided idempotency key to the shared
/// write-surface contract — trimmed, 1..=180 chars — by parsing it
/// through the same [`IdempotencyKey`] type the goal tools use, so the
/// memory and goal families cannot drift on cap, whitespace handling,
/// or error text. The trimmed key is what feeds dedup (`uuid::new_v5`)
/// and the stored payload. An omitted key (`None`) is always allowed —
/// the caller derives one instead.
///
/// # Errors
///
/// Returns [`McpToolError::InvalidInput`] when `key` is `Some` and blank
/// after trimming or over the character cap.
pub fn normalize_idempotency_key(key: Option<String>) -> Result<Option<String>, McpToolError> {
    key.map(|raw| {
        IdempotencyKey::new(raw)
            .map(IdempotencyKey::into_string)
            .map_err(McpToolError::InvalidInput)
    })
    .transpose()
}

/// Normalize an optional `source_batch_key` under the same trimmed
/// 1..=180-char contract as idempotency keys, with its own error text.
///
/// # Errors
///
/// Returns [`McpToolError::InvalidInput`] when the key is blank after
/// trimming or over the character cap.
pub fn normalize_batch_key(key: Option<String>) -> Result<Option<String>, McpToolError> {
    key.map(|raw| {
        IdempotencyKey::new(raw)
            .map(IdempotencyKey::into_string)
            .map_err(|_| {
                McpToolError::InvalidInput(
                    "source_batch_key must be 1..=180 chars after trimming".into(),
                )
            })
    })
    .transpose()
}

/// Clock-skew tolerance for caller-supplied `observed_at` timestamps.
const OBSERVED_AT_FUTURE_SKEW: time::Duration = time::Duration::minutes(5);

/// Parse an optional caller-supplied `observed_at` backdate (RFC3339).
/// Historical import writes the original observation time into the Fact's
/// receipt provenance (`fact_receipts.observed_at`/`occurred_at`); it does
/// not alter `memories.created_at`, which orders supersession heads and
/// recency and deliberately has no write path.
///
/// # Errors
///
/// Returns [`McpToolError::InvalidInput`] when the value is not RFC3339 or
/// lies in the future beyond a small clock-skew tolerance (an observation
/// cannot postdate its own recording).
pub fn parse_observed_at(raw: Option<&str>) -> Result<Option<time::OffsetDateTime>, McpToolError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let parsed = time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
        .map_err(|err| {
        McpToolError::InvalidInput(format!("observed_at must be an RFC3339 timestamp: {err}"))
    })?;
    if parsed > time::OffsetDateTime::now_utc() + OBSERVED_AT_FUTURE_SKEW {
        return Err(McpToolError::InvalidInput(
            "observed_at must not be in the future".into(),
        ));
    }
    Ok(Some(parsed))
}

/// Upper bound on distinct normalized tags per memory.
const MAX_TAGS: usize = 16;

/// Fold one tag to the form it is stored and compared in: trimmed and
/// ASCII-lowercased.
///
/// The write side and the search filter must fold identically or a tag
/// written as `Rust` cannot be found by searching `Rust` — a silent miss,
/// since a filter that matches nothing is indistinguishable from a memory
/// that does not exist. Both call this rather than repeating the two steps.
#[must_use]
pub fn fold_tag(tag: &str) -> String {
    tag.trim().to_ascii_lowercase()
}

/// Trim, lowercase, sort, and dedup `tags`, then cap the *distinct* result
/// at [`MAX_TAGS`]. The cap deliberately applies after normalization: a
/// caller sending `["Rust", "rust", " RUST "]` holds one tag, not three,
/// and must not be rejected for a duplicate-heavy spelling of an
/// in-contract set.
///
/// # Errors
///
/// Returns [`McpToolError::InvalidInput`] when a tag is blank, a tag
/// exceeds 48 characters, or more than [`MAX_TAGS`] distinct tags remain
/// after normalization.
pub fn normalize_tags(tags: Vec<String>) -> Result<Vec<String>, McpToolError> {
    let mut out = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = fold_tag(&tag);
        if tag.is_empty() || tag.chars().count() > 48 {
            return Err(McpToolError::InvalidInput(
                "tag must be 1..=48 chars".into(),
            ));
        }
        out.push(tag);
    }
    out.sort();
    out.dedup();
    if out.len() > MAX_TAGS {
        return Err(McpToolError::InvalidInput(
            "at most 16 distinct tags".into(),
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{MAX_TAGS, normalize_idempotency_key, normalize_tags, parse_observed_at};
    use crate::verbs::goal_write::IdempotencyKey;

    #[test]
    fn omitted_observed_at_is_allowed() {
        assert_eq!(parse_observed_at(None).expect("no backdate"), None);
    }

    #[test]
    fn historical_observed_at_parses() {
        let parsed = parse_observed_at(Some("2023-03-22T17:47:00Z"))
            .expect("valid RFC3339")
            .expect("some timestamp");
        assert_eq!(parsed.year(), 2023);
        assert_eq!(parsed.offset(), time::UtcOffset::UTC);
    }

    #[test]
    fn non_rfc3339_observed_at_is_rejected() {
        assert!(parse_observed_at(Some("22.03.2023")).is_err());
        assert!(parse_observed_at(Some("2023-03-22")).is_err());
        assert!(parse_observed_at(Some("")).is_err());
    }

    #[test]
    fn future_observed_at_is_rejected_beyond_clock_skew() {
        let far_future = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
        let raw = far_future
            .format(&time::format_description::well_known::Rfc3339)
            .expect("format");
        assert!(parse_observed_at(Some(&raw)).is_err());
        // Small skew (under the 5-minute tolerance) must pass: two hosts'
        // clocks disagreeing by seconds is not a caller error.
        let near_now = time::OffsetDateTime::now_utc() + time::Duration::seconds(30);
        let raw = near_now
            .format(&time::format_description::well_known::Rfc3339)
            .expect("format");
        assert!(parse_observed_at(Some(&raw)).is_ok());
    }

    const MAX_IDEMPOTENCY_KEY_CHARS: usize = IdempotencyKey::MAX_CHARS;

    #[test]
    fn tags_are_trimmed_lowercased_sorted_and_deduped() {
        let tags = vec![" Rust ".into(), "mcp".into(), "RUST".into()];
        assert_eq!(
            normalize_tags(tags).expect("valid tags"),
            vec!["mcp".to_string(), "rust".to_string()],
        );
    }

    #[test]
    fn blank_and_oversized_tags_are_rejected() {
        assert!(normalize_tags(vec!["  ".into()]).is_err());
        assert!(normalize_tags(vec!["x".repeat(49)]).is_err());
        assert!(normalize_tags(vec!["x".repeat(48)]).is_ok());
    }

    #[test]
    fn tag_cap_counts_distinct_tags_not_raw_input() {
        // MAX_TAGS + 1 raw spellings collapsing to one tag are in
        // contract; the cap must not fire on the pre-dedup length.
        let duplicates: Vec<String> = (0..=MAX_TAGS)
            .map(|i| {
                if i % 2 == 0 {
                    "rust".to_string()
                } else {
                    " RUST ".to_string()
                }
            })
            .collect();
        assert_eq!(
            normalize_tags(duplicates).expect("one distinct tag"),
            vec!["rust".to_string()],
        );
    }

    #[test]
    fn too_many_distinct_tags_are_rejected() {
        let at_cap: Vec<String> = (0..MAX_TAGS).map(|i| format!("tag-{i:02}")).collect();
        assert_eq!(normalize_tags(at_cap).expect("at the cap").len(), MAX_TAGS);
        let over_cap: Vec<String> = (0..=MAX_TAGS).map(|i| format!("tag-{i:02}")).collect();
        assert!(normalize_tags(over_cap).is_err());
    }

    #[test]
    fn omitted_idempotency_key_is_allowed() {
        assert_eq!(normalize_idempotency_key(None).expect("no key"), None);
    }

    #[test]
    fn blank_idempotency_key_is_rejected() {
        // An empty or whitespace-only string must not slip through as a
        // real dedup key.
        assert!(normalize_idempotency_key(Some(String::new())).is_err());
        assert!(normalize_idempotency_key(Some("   ".into())).is_err());
    }

    #[test]
    fn idempotency_key_is_trimmed_like_the_goal_family() {
        // `" k "` and `"k"` must be the same dedup key on every write
        // surface; the goal family trims, so the memory family must too.
        assert_eq!(
            normalize_idempotency_key(Some(" k ".into())).expect("valid key"),
            Some("k".to_string()),
        );
    }

    #[test]
    fn idempotency_key_at_the_cap_is_allowed() {
        let key = "k".repeat(MAX_IDEMPOTENCY_KEY_CHARS);
        assert_eq!(
            normalize_idempotency_key(Some(key.clone())).expect("at the cap"),
            Some(key),
        );
    }

    #[test]
    fn idempotency_key_over_the_cap_is_rejected() {
        let key = "k".repeat(MAX_IDEMPOTENCY_KEY_CHARS + 1);
        assert!(normalize_idempotency_key(Some(key)).is_err());
    }

    #[test]
    fn idempotency_key_cap_counts_characters_not_bytes() {
        // Multi-byte chars: MAX of them is fine, MAX+1 is not, even though
        // the byte length is well over the character cap in both cases.
        let at_cap = "é".repeat(MAX_IDEMPOTENCY_KEY_CHARS);
        let over_cap = "é".repeat(MAX_IDEMPOTENCY_KEY_CHARS + 1);
        assert!(normalize_idempotency_key(Some(at_cap)).is_ok());
        assert!(normalize_idempotency_key(Some(over_cap)).is_err());
    }
}
