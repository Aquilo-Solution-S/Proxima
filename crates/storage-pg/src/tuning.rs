//! Storage-tier tuning knobs, read once per [`crate::PgStorage`].
//!
//! The defaults are the shipped search path: the semantic branch drives off
//! the nearest-neighbour index with the owner scope pushed onto the scan
//! ([`SemanticIndexFirst::Pushdown`]) and collapses candidates with the
//! window-function spelling (`candidate_window_dedup`). Setting
//! `PROXIMA_PG_SEMANTIC_INDEX_FIRST=off` and
//! `PROXIMA_PG_CANDIDATE_WINDOW_DEDUP=off` restores the legacy path
//! byte-for-byte. Every other field defaults to the behaviour this crate had
//! before the knob existed. Values are resolved at construction and carried
//! on the handle; no query path re-reads the environment.

use std::ops::RangeInclusive;

use proxima_core::{StorageError, env_value};

pub(crate) const DEFAULT_HNSW_EF_SEARCH: u32 = 100;
pub(crate) const DEFAULT_HNSW_MAX_SCAN_TUPLES: u32 = 20_000;
pub(crate) const DEFAULT_SEMANTIC_OVERFETCH_PER_RESULT: u64 = 64;
pub(crate) const DEFAULT_SEMANTIC_OVERFETCH_MIN: u64 = 512;

/// Accepted range for the two window knobs.
///
/// The window they compute is formatted into `LIMIT`, so an absurd value is
/// not a slow query but a statement the server rejects at run time, far from
/// the variable that caused it. Bounded here, the widest window the branch
/// can emit is `u32::MAX * 4096`, which is a `bigint` Postgres accepts.
const SEMANTIC_OVERFETCH_PER_RESULT_RANGE: RangeInclusive<u64> = 1..=4_096;
const SEMANTIC_OVERFETCH_MIN_RANGE: RangeInclusive<u64> = 1..=100_000;

/// Accepted range for the two HNSW session knobs, for the same reason: they
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

/// Where the semantic branch's nearest-neighbour scan sits relative to the
/// eligibility joins.
///
/// `Off` — the legacy path — keeps every join UNDER the scan's
/// `ORDER BY … LIMIT`, so the window is a budget of rows that already passed
/// them and the branch is exact.
///
/// Both other arms cut first and filter after, which trades recall for the
/// index scan: result membership becomes an ANN-window approximation.
/// `Overfetch` keeps the joins above a materialized ANN CTE, so recall
/// depends on the overfetch window exceeding the inverse of the filter's
/// selectivity — pgvector's iterative scan cannot help there, because the
/// CTE's own LIMIT is satisfied by unfiltered rows. `Pushdown` — the
/// default — puts the owner and model predicates on the embeddings scan
/// itself, which is what lets iterative scan satisfy THOSE: an eligible row
/// can no longer be displaced from the window by another owner's. Every
/// other query predicate — `schema_id`, `kind`, `tags`, `since`, `until` —
/// still sits above the window on both arms and carries overfetch's recall
/// bound (`semantic_search_filters_query_predicates_before_candidate_limit`
/// runs the three arms against exactly that).
///
/// The `#[default]` attribute below is the single declaration of which arm
/// ships: [`PgTuning::default`] reads it rather than naming `Pushdown` a
/// second time, the way the `hnsw_iterative_scan` field beside it already
/// does. Two spellings of the same fact can only ever diverge, and
/// `defaults_are_the_shipped_search_path` pins the one that is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SemanticIndexFirst {
    Off,
    Overfetch,
    #[default]
    Pushdown,
}

impl SemanticIndexFirst {
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
            "overfetch" => Ok(Self::Overfetch),
            "pushdown" => Ok(Self::Pushdown),
            _ => Err(StorageError::Unavailable(format!(
                "invalid {key}={value}; expected off, overfetch, or pushdown"
            ))),
        }
    }
}

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
    pub semantic_index_first: SemanticIndexFirst,
    /// Collapse candidate rows with a ranking window function instead of
    /// `DISTINCT ON`, and spell the supersedes head filter as a unique-join
    /// anti-join instead of a per-row `NOT EXISTS` probe. Result membership
    /// is identical either way; `off` restores the legacy statement text.
    pub candidate_window_dedup: bool,
    pub hnsw_ef_search: u32,
    pub hnsw_iterative_scan: HnswIterativeScan,
    /// Ceiling on tuples an iterative HNSW scan may visit. Only emitted
    /// into the session settings when it differs from this crate's
    /// `DEFAULT_HNSW_MAX_SCAN_TUPLES`, so the shipped statement text stays
    /// what it was — which means that at the default this field reports a
    /// ceiling the session inherits rather than one it pins, and a server-
    /// or database-level `hnsw.max_scan_tuples` wins over it. See
    /// `crate::pgvector::set_hnsw_search_sql`.
    pub hnsw_max_scan_tuples: u32,
    /// Nearest-neighbour candidates fetched per requested result, and the
    /// floor that window never drops below.
    pub semantic_overfetch_per_result: u64,
    pub semantic_overfetch_min: u64,
}

impl Default for PgTuning {
    fn default() -> Self {
        Self {
            semantic_index_first: SemanticIndexFirst::default(),
            candidate_window_dedup: true,
            hnsw_ef_search: DEFAULT_HNSW_EF_SEARCH,
            hnsw_iterative_scan: HnswIterativeScan::default(),
            hnsw_max_scan_tuples: DEFAULT_HNSW_MAX_SCAN_TUPLES,
            semantic_overfetch_per_result: DEFAULT_SEMANTIC_OVERFETCH_PER_RESULT,
            semantic_overfetch_min: DEFAULT_SEMANTIC_OVERFETCH_MIN,
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
    /// variable is set to a value this crate cannot parse.
    pub fn from_env() -> Result<Self, StorageError> {
        Ok(Self::from_lookup(&proxima_core::process_env)?.unwrap_or_default())
    }

    /// Read tuning from an injected environment, or `None` when that
    /// environment asks for nothing the defaults do not already give.
    ///
    /// `None` rather than the defaults, so a caller layering configuration
    /// can tell an environment that tunes something from one that is silent:
    /// silence must not outrank tuning the host set programmatically.
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
    /// value this crate cannot parse.
    pub fn from_lookup(
        lookup: &impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, StorageError> {
        let tuned = Self::resolve(lookup)?;
        Ok((tuned != Self::default()).then_some(tuned))
    }

    fn resolve(lookup: &impl Fn(&str) -> Option<String>) -> Result<Self, StorageError> {
        let defaults = Self::default();
        Ok(Self {
            semantic_index_first: SemanticIndexFirst::from_lookup(
                lookup,
                "PROXIMA_PG_SEMANTIC_INDEX_FIRST",
                defaults.semantic_index_first,
            )?,
            candidate_window_dedup: env_bool_or(
                lookup,
                "PROXIMA_PG_CANDIDATE_WINDOW_DEDUP",
                defaults.candidate_window_dedup,
            )?,
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
            semantic_overfetch_per_result: env_int_in_range(
                lookup,
                "PROXIMA_PG_SEMANTIC_OVERFETCH_PER_RESULT",
                defaults.semantic_overfetch_per_result,
                &SEMANTIC_OVERFETCH_PER_RESULT_RANGE,
            )?,
            semantic_overfetch_min: env_int_in_range(
                lookup,
                "PROXIMA_PG_SEMANTIC_OVERFETCH_MIN",
                defaults.semantic_overfetch_min,
                &SEMANTIC_OVERFETCH_MIN_RANGE,
            )?,
        })
    }
}

/// Parse a boolean tuning variable, falling back to `default` when unset.
/// The accepted spellings are the workspace's (`1/true/yes/on` and their
/// negatives), so one flag cannot answer to a word its neighbours reject.
fn env_bool_or(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &str,
    default: bool,
) -> Result<bool, StorageError> {
    let Some(value) = env_value(lookup, key) else {
        return Ok(default);
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(StorageError::Unavailable(format!(
            "invalid boolean {key}={value}"
        ))),
    }
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
    /// search path — index-first pushdown with window-function dedup — and
    /// leaves every other knob at the behaviour that predates it.
    #[test]
    fn defaults_are_the_shipped_search_path() {
        let tuning = PgTuning::default();

        assert_eq!(tuning.semantic_index_first, SemanticIndexFirst::Pushdown);
        assert!(tuning.candidate_window_dedup);
        assert_eq!(tuning.hnsw_ef_search, 100);
        assert_eq!(tuning.hnsw_iterative_scan, HnswIterativeScan::RelaxedOrder);
        assert_eq!(tuning.hnsw_max_scan_tuples, 20_000);
        assert_eq!(tuning.semantic_overfetch_per_result, 64);
        assert_eq!(tuning.semantic_overfetch_min, 512);
    }

    /// The escape hatch: `off` on both search knobs is the legacy
    /// configuration, reachable from the environment alone.
    #[test]
    fn the_env_escape_hatch_selects_the_legacy_path() {
        let tuning = PgTuning::from_lookup(&env(&[
            ("PROXIMA_PG_SEMANTIC_INDEX_FIRST", "off"),
            ("PROXIMA_PG_CANDIDATE_WINDOW_DEDUP", "off"),
        ]))
        .unwrap()
        .expect("the legacy configuration is not the default");

        assert_eq!(tuning.semantic_index_first, SemanticIndexFirst::Off);
        assert!(!tuning.candidate_window_dedup);
        assert_eq!(
            PgTuning {
                semantic_index_first: SemanticIndexFirst::Pushdown,
                candidate_window_dedup: true,
                ..tuning
            },
            PgTuning::default(),
            "the escape hatch must move nothing but the two search knobs"
        );
    }

    /// A silent environment tunes nothing, which is not the same answer as
    /// "the defaults": a caller layering configuration must be able to leave
    /// a programmatically tuned value alone.
    #[test]
    fn an_environment_that_tunes_nothing_answers_none() {
        assert_eq!(PgTuning::from_lookup(&env(&[])).unwrap(), None);
    }

    /// Empty and whitespace-only are "unset", per `proxima_core::env_value`.
    #[test]
    fn blank_values_read_as_unset() {
        let tuning = PgTuning::from_lookup(&env(&[
            ("PROXIMA_PG_SEMANTIC_INDEX_FIRST", ""),
            ("PROXIMA_PG_CANDIDATE_WINDOW_DEDUP", "   "),
            ("PROXIMA_PG_HNSW_EF_SEARCH", " "),
        ]))
        .unwrap();

        assert_eq!(tuning, None);
    }

    #[test]
    fn every_variable_is_read_and_trimmed() {
        let tuning = PgTuning::from_lookup(&env(&[
            ("PROXIMA_PG_SEMANTIC_INDEX_FIRST", "Overfetch"),
            ("PROXIMA_PG_CANDIDATE_WINDOW_DEDUP", "off"),
            ("PROXIMA_PG_HNSW_EF_SEARCH", " 200 "),
            ("PROXIMA_PG_HNSW_ITERATIVE_SCAN", "STRICT_ORDER"),
            ("PROXIMA_PG_HNSW_MAX_SCAN_TUPLES", "40000"),
            ("PROXIMA_PG_SEMANTIC_OVERFETCH_PER_RESULT", "128"),
            ("PROXIMA_PG_SEMANTIC_OVERFETCH_MIN", "1024"),
        ]))
        .unwrap();

        assert_eq!(
            tuning,
            Some(PgTuning {
                semantic_index_first: SemanticIndexFirst::Overfetch,
                candidate_window_dedup: false,
                hnsw_ef_search: 200,
                hnsw_iterative_scan: HnswIterativeScan::StrictOrder,
                hnsw_max_scan_tuples: 40_000,
                semantic_overfetch_per_result: 128,
                semantic_overfetch_min: 1024,
            })
        );
    }

    #[test]
    fn the_index_first_modes_parse_by_name() {
        for (raw, expected) in [
            ("off", SemanticIndexFirst::Off),
            ("overfetch", SemanticIndexFirst::Overfetch),
            ("pushdown", SemanticIndexFirst::Pushdown),
        ] {
            let tuning = PgTuning::from_lookup(&env(&[("PROXIMA_PG_SEMANTIC_INDEX_FIRST", raw)]))
                .unwrap()
                .unwrap_or_default();
            assert_eq!(tuning.semantic_index_first, expected);
        }
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
                "PROXIMA_PG_SEMANTIC_INDEX_FIRST",
                "index_first",
                "invalid PROXIMA_PG_SEMANTIC_INDEX_FIRST=index_first",
            ),
            (
                "PROXIMA_PG_HNSW_ITERATIVE_SCAN",
                "relaxed",
                "invalid PROXIMA_PG_HNSW_ITERATIVE_SCAN=relaxed",
            ),
            (
                "PROXIMA_PG_CANDIDATE_WINDOW_DEDUP",
                "maybe",
                "invalid boolean PROXIMA_PG_CANDIDATE_WINDOW_DEDUP=maybe",
            ),
            (
                "PROXIMA_PG_HNSW_EF_SEARCH",
                "wide",
                "invalid integer PROXIMA_PG_HNSW_EF_SEARCH=wide",
            ),
            (
                "PROXIMA_PG_SEMANTIC_OVERFETCH_MIN",
                "many",
                "invalid integer PROXIMA_PG_SEMANTIC_OVERFETCH_MIN=many",
            ),
        ] {
            let err = PgTuning::from_lookup(&env(&[(key, value)])).unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "{key}={value} reported {err}"
            );
        }
    }

    /// Every integer knob reaches the server as a literal — the window knobs
    /// as `LIMIT`, the two HNSW knobs as `SET LOCAL`. A value the server
    /// would reject there has to refuse at boot instead, where the variable
    /// that caused it is still in hand; otherwise it surfaces as an error on
    /// every search, naming a GUC rather than the variable an operator typed.
    ///
    /// The HNSW bounds are pgvector's own (see `HNSW_EF_SEARCH_RANGE`), so
    /// `1001` and `2147483648` are the first values the GUC itself refuses.
    #[test]
    fn an_out_of_range_window_refuses_at_boot() {
        for (key, value) in [
            ("PROXIMA_PG_SEMANTIC_OVERFETCH_PER_RESULT", "0"),
            ("PROXIMA_PG_SEMANTIC_OVERFETCH_PER_RESULT", "4097"),
            (
                "PROXIMA_PG_SEMANTIC_OVERFETCH_PER_RESULT",
                "18446744073709551615",
            ),
            ("PROXIMA_PG_SEMANTIC_OVERFETCH_MIN", "0"),
            ("PROXIMA_PG_SEMANTIC_OVERFETCH_MIN", "100001"),
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

    /// The bounds still admit the widest window an operator may reasonably
    /// ask for, and the emitted `LIMIT` stays inside `bigint`.
    #[test]
    fn the_widest_admitted_window_fits_a_bigint_limit() {
        let tuning = PgTuning::from_lookup(&env(&[
            ("PROXIMA_PG_SEMANTIC_OVERFETCH_PER_RESULT", "4096"),
            ("PROXIMA_PG_SEMANTIC_OVERFETCH_MIN", "100000"),
        ]))
        .unwrap()
        .unwrap_or_default();

        let widest = u64::from(u32::MAX)
            .saturating_mul(tuning.semantic_overfetch_per_result)
            .max(tuning.semantic_overfetch_min);
        assert!(i64::try_from(widest).is_ok(), "{widest} overflows bigint");
    }
}
