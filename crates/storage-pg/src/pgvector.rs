use std::fmt::Write as _;

use crate::tuning::{DEFAULT_HNSW_MAX_SCAN_TUPLES, HnswIterativeScan, PgTuning};

pub(crate) const REQUIRED_PGVECTOR_MAJOR: u32 = 0;
pub(crate) const REQUIRED_PGVECTOR_MINOR: u32 = 8;
pub(crate) const REQUIRED_PGVECTOR_PATCH: u32 = 0;

/// The HNSW search settings for one semantic search, in one statement.
///
/// `SET LOCAL` takes a single parameter, so these are inherently several
/// statements — and sqlx's `query` uses the extended protocol, which sends
/// one statement per round trip. Every semantic search therefore paid two
/// round trips before its query even started. `raw_sql` uses the simple
/// protocol, which accepts them in one message; there is nothing to bind
/// here, so the usual reason to prefer the extended protocol does not apply.
///
/// `hnsw.max_scan_tuples` is only reachable under an iterative scan, so the
/// `Off` arm genuinely has nothing to set. The second condition is not of
/// that kind: at [`DEFAULT_HNSW_MAX_SCAN_TUPLES`] the clause is dropped
/// because it matches *this crate's* constant, not because it matches the
/// server's effective value — so at the default the session INHERITS
/// whatever the server carries rather than pinning it. Measured on
/// Postgres 18.4 / pgvector 0.8.5: with
/// `ALTER DATABASE … SET hnsw.max_scan_tuples = 500000`, a session running
/// the statement below verbatim reports an effective `hnsw.max_scan_tuples`
/// of 500000 while [`PgTuning`] reports 20000. This is therefore the one
/// tuning knob whose shipped default is not asserted; a server- or
/// database-level override wins until an operator sets
/// `PROXIMA_PG_HNSW_MAX_SCAN_TUPLES` explicitly. It matters more than it
/// would for the other knobs because `hnsw.max_scan_tuples`, not the SQL
/// `LIMIT`, is what actually bounds the shipped (pushdown) arm's index
/// scan.
pub(crate) fn set_hnsw_search_sql(tuning: &PgTuning) -> String {
    let mut sql = format!(
        "SET LOCAL hnsw.ef_search = {}; SET LOCAL hnsw.iterative_scan = {}",
        tuning.hnsw_ef_search,
        tuning.hnsw_iterative_scan.as_setting()
    );
    if tuning.hnsw_iterative_scan != HnswIterativeScan::Off
        && tuning.hnsw_max_scan_tuples != DEFAULT_HNSW_MAX_SCAN_TUPLES
    {
        write!(
            &mut sql,
            "; SET LOCAL hnsw.max_scan_tuples = {}",
            tuning.hnsw_max_scan_tuples
        )
        .expect("write to String is infallible");
    }
    sql
}

#[must_use]
pub(crate) fn literal(vec: &[f32]) -> String {
    let mut out = String::with_capacity(vec.len().saturating_mul(8).saturating_add(2));
    out.push('[');
    for (idx, value) in vec.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        write!(&mut out, "{value}").expect("write to String is infallible");
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden text: at default tuning the session settings must be the
    /// statement that shipped before they were built from anything.
    #[test]
    fn default_tuning_sets_the_settings_it_always_set() {
        assert_eq!(
            set_hnsw_search_sql(&PgTuning::default()),
            "SET LOCAL hnsw.ef_search = 100; SET LOCAL hnsw.iterative_scan = relaxed_order"
        );
    }

    #[test]
    fn a_raised_scan_ceiling_is_appended_under_an_iterative_scan() {
        let tuning = PgTuning {
            hnsw_ef_search: 200,
            hnsw_max_scan_tuples: 60_000,
            ..PgTuning::default()
        };

        assert_eq!(
            set_hnsw_search_sql(&tuning),
            "SET LOCAL hnsw.ef_search = 200; SET LOCAL hnsw.iterative_scan = relaxed_order; \
             SET LOCAL hnsw.max_scan_tuples = 60000"
        );
    }

    /// Without an iterative scan the ceiling has nothing to bound, so it is
    /// not sent even when it is set.
    #[test]
    fn a_scan_ceiling_is_dropped_when_iterative_scan_is_off() {
        let tuning = PgTuning {
            hnsw_iterative_scan: HnswIterativeScan::Off,
            hnsw_max_scan_tuples: 60_000,
            ..PgTuning::default()
        };

        assert_eq!(
            set_hnsw_search_sql(&tuning),
            "SET LOCAL hnsw.ef_search = 100; SET LOCAL hnsw.iterative_scan = off"
        );
    }
}
