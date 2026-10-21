//! The core baseline gate: a store installs fresh.
//!
//! The baseline replaced every earlier core migration, and no code adopts a
//! store built from them. A ledger that records a version below the baseline,
//! or a schema with no ledger, is refused with a typed reset requirement.

use std::collections::BTreeSet;

use proxima_core::StorageError;

const RESET_DETAILS: &str = "this release installs the core schema fresh: a store built from an earlier core migration set (version 56 or below) cannot be upgraded in place; reinstall it, or reset a development database";

/// Whether the recorded core versions may continue from the baseline.
///
/// # Errors
///
/// [`StorageError::SchemaResetRequired`] for a ledger below the baseline, a
/// ledger without it, or a schema no ledger accounts for.
pub(crate) fn check(
    recorded: &BTreeSet<i64>,
    has_schema: bool,
    baseline_version: i64,
) -> Result<(), StorageError> {
    let reset = || StorageError::SchemaResetRequired {
        details: RESET_DETAILS.to_owned(),
    };
    if recorded.iter().any(|version| *version < baseline_version) {
        return Err(reset());
    }
    if recorded.contains(&baseline_version) || (recorded.is_empty() && !has_schema) {
        return Ok(());
    }
    Err(reset())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use proxima_core::StorageError;

    use super::check;

    const BASELINE: i64 = 57;

    fn ledger(versions: impl IntoIterator<Item = i64>) -> BTreeSet<i64> {
        versions.into_iter().collect()
    }

    fn is_reset(result: &Result<(), StorageError>) -> bool {
        matches!(result, Err(StorageError::SchemaResetRequired { .. }))
    }

    #[test]
    fn a_blank_database_and_a_baseline_ledger_continue() {
        assert!(check(&ledger([]), false, BASELINE).is_ok());
        assert!(check(&ledger([57]), true, BASELINE).is_ok());
        assert!(check(&ledger([57, 58, 59]), true, BASELINE).is_ok());
    }

    #[test]
    fn every_earlier_core_ledger_is_refused_even_the_last_released_one() {
        assert!(is_reset(&check(&ledger(33..=56), true, BASELINE)));
        assert!(is_reset(&check(&ledger([56]), true, BASELINE)));
        assert!(is_reset(&check(&ledger(1..=32), true, BASELINE)));
        assert!(is_reset(&check(&ledger(33..=57), true, BASELINE)));
    }

    #[test]
    fn a_schema_without_the_baseline_ledger_is_refused() {
        assert!(is_reset(&check(&ledger([]), true, BASELINE)));
        assert!(is_reset(&check(&ledger([58]), true, BASELINE)));
    }
}
