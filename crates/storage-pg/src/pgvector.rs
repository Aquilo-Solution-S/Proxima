use std::fmt::Write as _;

use proxima_core::{EmbeddingDim, EmbeddingSpace, StorageError};

use crate::tuning::{HnswIterativeScan, PgTuning};

pub(crate) const REQUIRED_PGVECTOR_MAJOR: u32 = 0;
pub(crate) const REQUIRED_PGVECTOR_MINOR: u32 = 8;
pub(crate) const REQUIRED_PGVECTOR_PATCH: u32 = 0;

/// The HNSW search settings for one semantic search, in one statement.
///
/// `SET LOCAL` takes a single parameter, so these are inherently several
/// statements — and sqlx's `query` uses the extended protocol, which sends
/// one statement per round trip, making every semantic search pay two
/// round trips before its query starts. `raw_sql` uses the simple
/// protocol, which accepts them in one message; there is nothing to bind
/// here, so the usual reason to prefer the extended protocol does not apply.
///
/// `hnsw.max_scan_tuples` is only reachable under an iterative scan, so the
/// `Off` arm sends nothing. When iterative scan is on, the session always
/// pins the crate/env value so a server GUC cannot raise the ceiling.
pub(crate) fn set_hnsw_search_sql(tuning: &PgTuning) -> String {
    let mut sql = format!(
        "SET LOCAL hnsw.ef_search = {}; SET LOCAL hnsw.iterative_scan = {}",
        tuning.hnsw_ef_search,
        tuning.hnsw_iterative_scan.as_setting()
    );
    if tuning.hnsw_iterative_scan != HnswIterativeScan::Off {
        write!(
            &mut sql,
            "; SET LOCAL hnsw.max_scan_tuples = {}",
            tuning.hnsw_max_scan_tuples
        )
        .expect("write to String is infallible");
    }
    sql
}

/// One width lane's fixed SQL, for a statement that aliases
/// `proxima_core.embeddings` as `emb`.
///
/// Migration 0015 indexes each supported width with a partial expression
/// index, `(vec::vector(N)) WHERE dim = N` (`halfvec` above 2000 dims). The
/// planner serves a query from that index only when the statement text names
/// the same expression and a predicate that implies `dim = N`, including
/// under a generic plan, so the width is spelled as a literal here and never
/// bound. Every fragment is a compile-time constant chosen by a closed enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Lane {
    /// The stored `dim` value.
    pub(crate) width: i16,
    /// `emb.dim = N`: the lane index's predicate.
    pub(crate) predicate: &'static str,
    /// `emb.vec::vector(N)`: the lane index's expression.
    pub(crate) vec: &'static str,
    /// `::vector(N)`: the cast a query-vector bind takes to meet `vec`.
    pub(crate) cast: &'static str,
}

macro_rules! lane {
    ($width:literal, $kind:literal) => {
        Lane {
            width: $width,
            predicate: concat!("emb.dim = ", $width),
            vec: concat!("emb.vec::", $kind, "(", $width, ")"),
            cast: concat!("::", $kind, "(", $width, ")"),
        }
    };
}

impl Lane {
    #[must_use]
    pub(crate) const fn of(dim: EmbeddingDim) -> Self {
        match dim {
            EmbeddingDim::D384 => lane!(384, "vector"),
            EmbeddingDim::D768 => lane!(768, "vector"),
            EmbeddingDim::D1024 => lane!(1024, "vector"),
            EmbeddingDim::D1536 => lane!(1536, "vector"),
            EmbeddingDim::D2048 => lane!(2048, "halfvec"),
            EmbeddingDim::D3072 => lane!(3072, "halfvec"),
        }
    }

    /// The lane for a stored `dim`.
    ///
    /// # Errors
    ///
    /// See [`stored_dim`].
    pub(crate) fn from_stored(dim: i16) -> Result<Self, StorageError> {
        stored_dim(dim).map(Self::of)
    }
}

/// The width a stored `dim` column holds.
///
/// # Errors
///
/// `StorageError::Internal` for a width no lane indexes. The column's CHECK
/// makes that unreachable for a row this binary's migrations wrote.
pub(crate) fn stored_dim(dim: i16) -> Result<EmbeddingDim, StorageError> {
    usize::try_from(dim)
        .ok()
        .and_then(EmbeddingDim::from_width)
        .ok_or_else(|| StorageError::Internal(format!("stored embedding width {dim} has no lane")))
}

/// The space a stored `(model_id, dim)` pair names.
///
/// # Errors
///
/// See [`stored_dim`].
pub(crate) fn stored_space(model_id: String, dim: i16) -> Result<EmbeddingSpace, StorageError> {
    Ok(EmbeddingSpace::new(model_id, stored_dim(dim)?))
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

    /// The fragments must spell exactly the expression and predicate
    /// migration 0015 indexes, or the lane falls back to a sequential scan.
    #[test]
    fn every_lane_spells_its_migration_index() {
        let migration = include_str!("../migrations/0015_v016_embedding_spaces.sql");
        for dim in EmbeddingDim::ALL {
            let lane = Lane::of(dim);
            assert_eq!(usize::try_from(lane.width).ok(), Some(dim.width()));
            assert_eq!(Lane::from_stored(lane.width).ok(), Some(lane));
            // pgvector builds no HNSW index over `vector` wider than 2000.
            let kind = if dim.width() > 2000 {
                "halfvec"
            } else {
                "vector"
            };
            let index = format!(
                "CREATE INDEX embeddings_hnsw_d{w} ON proxima_core.embeddings\n    \
                 USING hnsw ((vec::{kind}({w})) {kind}_cosine_ops) WHERE dim = {w};",
                w = dim.width()
            );
            assert!(migration.contains(&index), "missing lane index: {index}");
            assert_eq!(lane.predicate, format!("emb.dim = {}", dim.width()));
            assert_eq!(lane.vec, format!("emb.vec::{kind}({})", dim.width()));
            assert_eq!(lane.cast, format!("::{kind}({})", dim.width()));
        }
        assert!(Lane::from_stored(512).is_err());
    }

    /// Golden text: at default tuning the session settings are exactly this
    /// statement, so a change to the defaults or to the builder has to be
    /// typed here.
    #[test]
    fn default_tuning_sets_the_golden_session_statement() {
        assert_eq!(
            set_hnsw_search_sql(&PgTuning::default()),
            "SET LOCAL hnsw.ef_search = 100; SET LOCAL hnsw.iterative_scan = relaxed_order; \
             SET LOCAL hnsw.max_scan_tuples = 20000"
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
