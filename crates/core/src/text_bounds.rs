//! One rule for "trimmed, non-blank, at most N characters", and one set of
//! words for breaking it.
//!
//! Blank and oversized are two different mistakes with two different fixes,
//! and a caller can only act on the one they made. Every length check in the
//! substrate was written with a single message for both — `title must be
//! 1..=240 chars` answered a two-space title as readily as a 241-character
//! one. For the blank case that names a range the input satisfies, which
//! reads as a server fault rather than an instruction to send content, so the
//! natural next move is to retry the same request unchanged.
//!
//! [`crate::tool::validate_trimmed_len`] fixed that for the tool SDK, but the
//! SDK sits above `verbs` and cannot be called from it, so `IdempotencyKey`,
//! `GoalWakeConfigWrite`, and `GoalWriteBuildError` kept the old shape. This
//! module is deliberately the lowest thing in the crate that any of them can
//! reach: it depends on nothing, decides only *which* half of the contract
//! broke, and leaves it to each layer to wrap [`TrimmedLenViolation::reason`]
//! in whichever error type that layer speaks. That is what lets three error
//! types refuse the same input in the same words without any of them
//! depending on the others.

/// Which half of a `1..=max` character contract a value broke.
///
/// Deliberately not an error type of its own: it is a finding, and the layer
/// that made the check owns how it is reported. [`Copy`] so it can ride
/// inside `Copy` error enums such as
/// [`crate::verbs::goal_write::GoalWriteBuildError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimmedLenViolation {
    /// Empty once surrounding whitespace is removed.
    Blank,
    /// Longer than the cap. Carries both numbers so the message can name
    /// what was sent as well as what was allowed — a caller trimming a body
    /// to fit needs to know by how much.
    TooLong {
        /// The cap, in characters.
        max: usize,
        /// The trimmed length that was sent, in characters.
        got: usize,
    },
}

impl TrimmedLenViolation {
    /// The rejection reason, phrased for `field`.
    ///
    /// `field` is the wire name of the parameter, so the message points at
    /// something the caller can see in the schema.
    #[must_use]
    pub fn reason(&self, field: &str) -> String {
        match *self {
            Self::Blank => {
                format!("{field} must not be blank; it is empty after trimming whitespace")
            }
            Self::TooLong { max, got } => {
                format!("{field} must be at most {max} chars after trimming; got {got}")
            }
        }
    }
}

/// Trim `value` and check it against `1..=max` characters, returning the
/// trimmed text or naming the bound that was actually broken.
///
/// Counts characters, not bytes. A cap that bound bytes would reject a
/// shorter text written in a language that does not fit in ASCII, and every
/// message here says "chars".
///
/// # Errors
///
/// [`TrimmedLenViolation::Blank`] when `value` is empty after trimming,
/// [`TrimmedLenViolation::TooLong`] when it is longer than `max` characters.
pub fn check_trimmed_len(value: &str, max: usize) -> Result<&str, TrimmedLenViolation> {
    let value = value.trim();
    if value.is_empty() {
        return Err(TrimmedLenViolation::Blank);
    }
    let got = value.chars().count();
    if got > max {
        return Err(TrimmedLenViolation::TooLong { max, got });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{TrimmedLenViolation, check_trimmed_len};

    #[test]
    fn the_two_failures_are_told_apart() {
        assert_eq!(
            check_trimmed_len("   ", 240),
            Err(TrimmedLenViolation::Blank)
        );
        assert_eq!(
            check_trimmed_len(&"a".repeat(241), 240),
            Err(TrimmedLenViolation::TooLong { max: 240, got: 241 }),
        );
    }

    /// The whole point of splitting the two: a caller who sent whitespace
    /// must not be quoted a cap their input already satisfies.
    #[test]
    fn a_blank_value_is_not_quoted_a_bound_it_meets() {
        let blank = TrimmedLenViolation::Blank.reason("title");
        let over = TrimmedLenViolation::TooLong { max: 240, got: 241 }.reason("title");
        assert_ne!(blank, over, "one message for two mistakes tells neither");
        assert!(
            !blank.contains("240"),
            "blank reason must not name a cap: {blank}"
        );
        assert!(
            over.contains("240") && over.contains("241"),
            "oversize reason must name the cap and what was sent: {over}"
        );
    }

    #[test]
    fn the_reported_length_is_the_trimmed_length() {
        let padded = format!("  {}  ", "a".repeat(241));
        assert_eq!(
            check_trimmed_len(&padded, 240),
            Err(TrimmedLenViolation::TooLong { max: 240, got: 241 }),
            "the surrounding whitespace is removed before it is counted",
        );
    }

    #[test]
    fn the_cap_counts_characters_not_bytes() {
        let cyrillic = "я".repeat(240);
        assert_eq!(cyrillic.len(), 480, "two bytes per char");
        assert_eq!(check_trimmed_len(&cyrillic, 240), Ok(cyrillic.as_str()));
    }

    #[test]
    fn every_reason_names_the_field() {
        for violation in [
            TrimmedLenViolation::Blank,
            TrimmedLenViolation::TooLong { max: 1, got: 2 },
        ] {
            assert!(
                violation.reason("wake prompt").starts_with("wake prompt"),
                "the field name is the caller's only pointer back into the schema",
            );
        }
    }
}
