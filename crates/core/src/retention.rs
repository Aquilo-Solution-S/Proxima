//! Retention: how long one owner keeps one schema's memory
//! (docs/data-and-erasure.md).
//!
//! One [`Retention`] row per owner and schema. Unset keeps for good, and
//! nothing inherits: a group's row is its own. The row is set by whoever may
//! erase that owner's memory by hand (`Engine::set_retention`), and the host's
//! clock ages each series by its newest version's `occurred_at`, else its
//! admission time ([`crate::verbs::own_erase::SeriesSelection::OccurredBefore`]).

use crate::{OwnerId, SchemaId};

/// Days a schema's memory is kept: `1..=36500`.
///
/// The constructor is the only way in, so a held value is always in range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeepDays(u16);

impl KeepDays {
    pub const MIN: u16 = 1;
    pub const MAX: u16 = 36_500;

    /// # Errors
    /// [`KeepDaysOutOfRange`] for a value outside `1..=36500`.
    pub fn new(days: i64) -> Result<Self, KeepDaysOutOfRange> {
        u16::try_from(days)
            .ok()
            .filter(|days| (Self::MIN..=Self::MAX).contains(days))
            .map(Self)
            .ok_or(KeepDaysOutOfRange(days))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// The instant before which a memory has aged out at `now`.
    #[must_use]
    pub fn cutoff(self, now: time::OffsetDateTime) -> time::OffsetDateTime {
        now - time::Duration::days(i64::from(self.0))
    }
}

/// A `keep_days` outside `1..=36500`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("keep_days must be between 1 and 36500, got {0}")]
pub struct KeepDaysOutOfRange(pub i64);

/// One owner's retention for one schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retention {
    pub owner: OwnerId,
    pub schema: SchemaId,
    pub keep_days: KeepDays,
}

#[cfg(test)]
mod tests {
    use super::{KeepDays, KeepDaysOutOfRange};

    #[test]
    fn keep_days_holds_one_to_thirty_six_thousand_five_hundred() {
        for bad in [0, 36_501, -1, i64::from(u16::MAX) + 1] {
            assert_eq!(KeepDays::new(bad), Err(KeepDaysOutOfRange(bad)));
        }
        assert_eq!(KeepDays::new(1).map(KeepDays::get), Ok(1));
        assert_eq!(KeepDays::new(36_500).map(KeepDays::get), Ok(36_500));
    }

    #[test]
    fn the_cutoff_lies_keep_days_before_now() {
        let now = time::OffsetDateTime::from_unix_timestamp(1_793_793_600).expect("valid");
        let cutoff = KeepDays::new(30).expect("in range").cutoff(now);
        assert_eq!(now.unix_timestamp() - cutoff.unix_timestamp(), 30 * 86_400);
    }
}
