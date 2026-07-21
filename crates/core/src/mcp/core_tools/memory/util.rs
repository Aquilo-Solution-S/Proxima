use crate::McpToolError;

/// Upper bound on an explicitly-provided idempotency key, in characters.
const MAX_IDEMPOTENCY_KEY_CHARS: usize = 200;

/// Reject an explicitly-provided idempotency key that is empty or longer
/// than [`MAX_IDEMPOTENCY_KEY_CHARS`]. An omitted key (`None`) is always
/// allowed — the caller derives one instead. Shared by the three append
/// tools (`core_remember`, `core_record_utterance`, `core_derive`) so the
/// dedup-key contract stays identical across every write surface, rather
/// than one tool rejecting a blank key while its siblings feed it straight
/// into `uuid::new_v5` as if it were real.
///
/// # Errors
///
/// Returns [`McpToolError::InvalidInput`] when `key` is `Some` and either
/// blank or over the character cap.
pub fn validate_idempotency_key(key: Option<&str>) -> Result<(), McpToolError> {
    if let Some(key) = key
        && (key.is_empty() || key.chars().count() > MAX_IDEMPOTENCY_KEY_CHARS)
    {
        return Err(McpToolError::InvalidInput(
            "idempotency_key must be 1..=200 chars when provided".into(),
        ));
    }
    Ok(())
}

pub fn normalize_tags(tags: Vec<String>) -> Result<Vec<String>, McpToolError> {
    if tags.len() > 16 {
        return Err(McpToolError::InvalidInput("at most 16 tags".into()));
    }
    let mut out = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = tag.trim().to_ascii_lowercase();
        if tag.is_empty() || tag.chars().count() > 48 {
            return Err(McpToolError::InvalidInput(
                "tag must be 1..=48 chars".into(),
            ));
        }
        out.push(tag);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{MAX_IDEMPOTENCY_KEY_CHARS, validate_idempotency_key};

    #[test]
    fn omitted_idempotency_key_is_allowed() {
        assert!(validate_idempotency_key(None).is_ok());
    }

    #[test]
    fn blank_idempotency_key_is_rejected() {
        // An empty string must not slip through as a real dedup key.
        assert!(validate_idempotency_key(Some("")).is_err());
    }

    #[test]
    fn idempotency_key_at_the_cap_is_allowed() {
        let key = "k".repeat(MAX_IDEMPOTENCY_KEY_CHARS);
        assert!(validate_idempotency_key(Some(&key)).is_ok());
    }

    #[test]
    fn idempotency_key_over_the_cap_is_rejected() {
        let key = "k".repeat(MAX_IDEMPOTENCY_KEY_CHARS + 1);
        assert!(validate_idempotency_key(Some(&key)).is_err());
    }

    #[test]
    fn idempotency_key_cap_counts_characters_not_bytes() {
        // Multi-byte chars: MAX of them is fine, MAX+1 is not, even though
        // the byte length is well over the character cap in both cases.
        let at_cap = "é".repeat(MAX_IDEMPOTENCY_KEY_CHARS);
        let over_cap = "é".repeat(MAX_IDEMPOTENCY_KEY_CHARS + 1);
        assert!(validate_idempotency_key(Some(&at_cap)).is_ok());
        assert!(validate_idempotency_key(Some(&over_cap)).is_err());
    }
}
