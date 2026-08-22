//! Storage-tier tuning knobs, read once per [`crate::PgStorage`].
//!
//! The three knobs are the semantic branch's HNSW session settings
//! ([`crate::pgvector::set_hnsw_search_sql`]). Each defaults to the shipped
//! behaviour, so an unset environment is production. Values are resolved at
//! construction and carried on the handle; no query path re-reads the
//! environment.
//!
//! A `PROXIMA_PG_*` variable this crate does not read is not ignored: the
//! ones in `REMOVED_TUNING_VARS` refuse at boot, for the same reason a
//! malformed value does — a knob that does nothing would ship a
//! configuration the operator did not ask for.

use std::ops::RangeInclusive;

use proxima_core::{StorageError, env_value};

pub(crate) const DEFAULT_HNSW_EF_SEARCH: u32 = 100;
pub(crate) const DEFAULT_HNSW_MAX_SCAN_TUPLES: u32 = 20_000;

/// Tuning variables this crate does not read. Setting one refuses at
/// boot rather than silently doing nothing; the error names the release
/// that removed it.
const REMOVED_TUNING_VARS: [&str; 4] = [
    "PROXIMA_PG_SEMANTIC_INDEX_FIRST",
    "PROXIMA_PG_CANDIDATE_WINDOW_DEDUP",
    "PROXIMA_PG_SEMANTIC_OVERFETCH_PER_RESULT",
    "PROXIMA_PG_SEMANTIC_OVERFETCH_MIN",
];

/// Accepted range for the two HNSW session knobs: they
/// are interpolated into `SET LOCAL` ([`crate::pgvector::set_hnsw_search_sql`]),
/// so a value pgvector's GUC rejects is not a slow query but an error on
/// *every semantic search*, far from the variable that caused it.
///
/// The bounds are pgvector's own, read off the server rather than guessed —
/// `LOAD 'vector'; SELECT min_val, max_val FROM pg_settings WHERE name LIKE
/// 'hnsw.%'` on pgvector 0.8.5 (the version this crate pins a floor of 0.8.0
/// for, [`crate::pgvector::REQUIRED_PGVECTOR_MINOR`]) reports
/// `hnsw.ef_search 1 .. 1000` and `hnsw.max_scan_tuples 1 .. 2147483647`.
/// Both GUCs are declared `integer`, which is why the scan ceiling stops at
/// `i32::MAX` rather than at `u32::MAX`.
const HNSW_EF_SEARCH_RANGE: RangeInclusive<u32> = 1..=1_000;
const HNSW_MAX_SCAN_TUPLES_RANGE: RangeInclusive<u32> = 1..=2_147_483_647;

/// pgvector's `hnsw.iterative_scan` mode for the semantic branch's session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HnswIterativeScan {
    Off,
    StrictOrder,
    #[default]
    RelaxedOrder,
}

impl HnswIterativeScan {
    /// The value as pgvector spells it in `SET LOCAL`.
    pub(crate) const fn as_setting(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::StrictOrder => "strict_order",
            Self::RelaxedOrder => "relaxed_order",
        }
    }

    fn from_lookup(
        lookup: &impl Fn(&str) -> Option<String>,
        key: &str,
        default: Self,
    ) -> Result<Self, StorageError> {
        let Some(value) = env_value(lookup, key) else {
            return Ok(default);
        };
        match value.to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "strict_order" => Ok(Self::StrictOrder),
            "relaxed_order" => Ok(Self::RelaxedOrder),
            _ => Err(StorageError::Unavailable(format!(
                "invalid {key}={value}; expected off, strict_order, or relaxed_order"
            ))),
        }
    }
}

/// Postgres-tier tuning, resolved once and carried on the storage handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PgTuning {
    pub hnsw_ef_search: u32,
    pub hnsw_iterative_scan: HnswIterativeScan,
    /// Ceiling on tuples an iterative HNSW scan may visit. Always emitted
    /// as `SET LOCAL` when iterative scan is on. See
    /// `crate::pgvector::set_hnsw_search_sql`.
    pub hnsw_max_scan_tuples: u32,
}

impl Default for PgTuning {
    fn default() -> Self {
        Self {
            hnsw_ef_search: DEFAULT_HNSW_EF_SEARCH,
            hnsw_iterative_scan: HnswIterativeScan::default(),
            hnsw_max_scan_tuples: DEFAULT_HNSW_MAX_SCAN_TUPLES,
        }
    }
}

impl PgTuning {
    /// Read tuning from the process environment, defaulting what it leaves
    /// unset.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Unavailable` when a `PROXIMA_PG_*` tuning
    /// variable is set to a value this crate cannot parse, or when a
    /// variable this release removed (`REMOVED_TUNING_VARS`) is still set.
    pub fn from_env() -> Result<Self, StorageError> {
        Ok(Self::from_lookup(&proxima_core::process_env)?.unwrap_or_default())
    }

    /// Read tuning from an injected environment, or `None` when that
    /// environment asks for nothing the defaults do not already give.
    ///
    /// `None` rather than the defaults, so a caller layering configuration
    /// can tell an environment that asks for something other than the
    /// defaults from one that is silent: silence must not outrank tuning the
    /// host set programmatically.
    ///
    /// "Asks for something other than the defaults" is the predicate, not
    /// "tunes something": the test below is `resolved != Self::default()`, and
    /// nothing finer is available without per-field `Option`s. So an
    /// environment that sets a knob to its *shipped* value —
    /// `PROXIMA_PG_HNSW_EF_SEARCH=100` — resolves to `PgTuning::default()`
    /// and reads here as silence, and a caller layering it
    /// (`self.pg_tuning.or(base.pg_tuning)`) keeps the programmatic tuning.
    /// The environment can therefore move a deployment OFF the defaults but
    /// not back ONTO them; only non-default values are expressible.
    ///
    /// Every value goes through [`proxima_core::env_value`], so a variable
    /// set to the empty string (or to whitespace) reads as unset rather
    /// than as a value that fails to parse. A malformed value is an error
    /// rather than a silent fallback, so a typo cannot quietly ship the
    /// default arm of a measurement.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Unavailable` when a variable is set to a
    /// value this crate cannot parse, or when a variable this release
    /// removed (`REMOVED_TUNING_VARS`) is still set.
    pub fn from_lookup(
        lookup: &impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, StorageError> {
        let tuned = Self::resolve(lookup)?;
        Ok((tuned != Self::default()).then_some(tuned))
    }

    fn resolve(lookup: &impl Fn(&str) -> Option<String>) -> Result<Self, StorageError> {
        refuse_removed_vars(lookup)?;
        let defaults = Self::default();
        Ok(Self {
            hnsw_ef_search: env_int_in_range(
                lookup,
                "PROXIMA_PG_HNSW_EF_SEARCH",
                defaults.hnsw_ef_search,
                &HNSW_EF_SEARCH_RANGE,
            )?,
            hnsw_iterative_scan: HnswIterativeScan::from_lookup(
                lookup,
                "PROXIMA_PG_HNSW_ITERATIVE_SCAN",
                defaults.hnsw_iterative_scan,
            )?,
            hnsw_max_scan_tuples: env_int_in_range(
                lookup,
                "PROXIMA_PG_HNSW_MAX_SCAN_TUPLES",
                defaults.hnsw_max_scan_tuples,
                &HNSW_MAX_SCAN_TUPLES_RANGE,
            )?,
        })
    }
}

/// Refuse any removed tuning variable that is still set (blank reads as
/// unset, like every other variable here). The error names the removal, so
/// a deployment carrying a dead knob learns at boot — where the variable is
/// still in hand — rather than by silently running the shipped path under a
/// configuration banner it no longer honours.
fn refuse_removed_vars(lookup: &impl Fn(&str) -> Option<String>) -> Result<(), StorageError> {
    for key in REMOVED_TUNING_VARS {
        if let Some(value) = env_value(lookup, key) {
            return Err(StorageError::Unavailable(format!(
                "{key}={value} is set, but this knob was removed in v0.0.8 \
                 and no longer does anything; unset it"
            )));
        }
    }
    Ok(())
}

/// Parse an integer tuning variable that must land inside `range`, falling
/// back to `default` when unset.
///
/// Out of range is an error rather than a clamp: a clamped value would run
/// an arm the operator did not ask for and report it as the one they did.
fn env_int_in_range<T>(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &str,
    default: T,
    range: &RangeInclusive<T>,
) -> Result<T, StorageError>
where
    T: std::str::FromStr + PartialOrd + std::fmt::Display,
{
    let value = crate::env_int_or(lookup, key, default)?;
    if range.contains(&value) {
        return Ok(value);
    }
    Err(StorageError::Unavailable(format!(
        "out-of-range {key}={value}; expected {}..={}",
        range.start(),
        range.end()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(candidate, _)| *candidate == key)
                .map(|(_, value)| (*value).to_string())
        }
    }

    /// The defaults are the contract: an unset environment runs the shipped
    /// search path.
    #[test]
    fn defaults_are_the_shipped_search_path() {
        let tuning = PgTuning::default();

        assert_eq!(tuning.hnsw_ef_search, 100);
        assert_eq!(tuning.hnsw_iterative_scan, HnswIterativeScan::RelaxedOrder);
        assert_eq!(tuning.hnsw_max_scan_tuples, 20_000);
    }

    /// A silent environment tunes nothing, which is not the same answer as
    /// "the defaults": a caller layering configuration must be able to leave
    /// a programmatically tuned value alone.
    #[test]
    fn an_environment_that_tunes_nothing_answers_none() {
        assert_eq!(PgTuning::from_lookup(&env(&[])).unwrap(), None);
    }

    /// Empty and whitespace-only are "unset", per `proxima_core::env_value`.
    /// That covers the removed variables too: a blank removed variable is an
    /// unset one, not a boot refusal.
    #[test]
    fn blank_values_read_as_unset() {
        let tuning = PgTuning::from_lookup(&env(&[
            ("PROXIMA_PG_SEMANTIC_INDEX_FIRST", "   "),
            ("PROXIMA_PG_HNSW_EF_SEARCH", " "),
        ]))
        .unwrap();

        assert_eq!(tuning, None);
    }

    #[test]
    fn every_variable_is_read_and_trimmed() {
        let tuning = PgTuning::from_lookup(&env(&[
            ("PROXIMA_PG_HNSW_EF_SEARCH", " 200 "),
            ("PROXIMA_PG_HNSW_ITERATIVE_SCAN", "STRICT_ORDER"),
            ("PROXIMA_PG_HNSW_MAX_SCAN_TUPLES", "40000"),
        ]))
        .unwrap();

        assert_eq!(
            tuning,
            Some(PgTuning {
                hnsw_ef_search: 200,
                hnsw_iterative_scan: HnswIterativeScan::StrictOrder,
                hnsw_max_scan_tuples: 40_000,
            })
        );
    }

    #[test]
    fn the_iterative_scan_modes_parse_by_name() {
        for (raw, expected) in [
            ("off", HnswIterativeScan::Off),
            ("strict_order", HnswIterativeScan::StrictOrder),
            ("relaxed_order", HnswIterativeScan::RelaxedOrder),
        ] {
            let tuning = PgTuning::from_lookup(&env(&[("PROXIMA_PG_HNSW_ITERATIVE_SCAN", raw)]))
                .unwrap()
                .unwrap_or_default();
            assert_eq!(tuning.hnsw_iterative_scan, expected);
        }
    }

    #[test]
    fn a_malformed_value_names_the_variable_it_came_from() {
        for (key, value, expected) in [
            (
                "PROXIMA_PG_HNSW_ITERATIVE_SCAN",
                "relaxed",
                "invalid PROXIMA_PG_HNSW_ITERATIVE_SCAN=relaxed",
            ),
            (
                "PROXIMA_PG_HNSW_EF_SEARCH",
                "wide",
                "invalid integer PROXIMA_PG_HNSW_EF_SEARCH=wide",
            ),
        ] {
            let err = PgTuning::from_lookup(&env(&[(key, value)])).unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "{key}={value} reported {err}"
            );
        }
    }

    /// Both integer knobs reach the server as `SET LOCAL` literals. A value
    /// the server would reject there has to refuse at boot instead, where
    /// the variable that caused it is still in hand; otherwise it surfaces
    /// as an error on every search, naming a GUC rather than the variable an
    /// operator typed.
    ///
    /// The bounds are pgvector's own (see `HNSW_EF_SEARCH_RANGE`), so `1001`
    /// and `2147483648` are the first values the GUC itself refuses.
    #[test]
    fn an_out_of_range_hnsw_knob_refuses_at_boot() {
        for (key, value) in [
            ("PROXIMA_PG_HNSW_EF_SEARCH", "0"),
            ("PROXIMA_PG_HNSW_EF_SEARCH", "1001"),
            ("PROXIMA_PG_HNSW_MAX_SCAN_TUPLES", "0"),
            ("PROXIMA_PG_HNSW_MAX_SCAN_TUPLES", "2147483648"),
        ] {
            let err = PgTuning::from_lookup(&env(&[(key, value)])).unwrap_err();
            assert!(
                err.to_string()
                    .contains(&format!("out-of-range {key}={value}")),
                "{key}={value} reported {err}"
            );
        }
    }

    /// A removed knob fails loudly rather than no-ops: a deployment still
    /// setting one learns at boot, from an error that names the removal,
    /// instead of silently running the shipped path under a knob it thinks
    /// it turned.
    #[test]
    fn a_removed_variable_still_set_refuses_at_boot() {
        for key in REMOVED_TUNING_VARS {
            let err = PgTuning::from_lookup(&env(&[(key, "off")])).unwrap_err();
            let message = err.to_string();
            assert!(
                message.contains(&format!("{key}=off")) && message.contains("removed in v0.0.8"),
                "{key} reported {err}"
            );
        }
    }
}
