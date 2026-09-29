use std::fmt::Write as _;

use pgvector::{HalfVector, Vector};
use proxima_core::{EmbeddingDim, EmbeddingSpace, StorageError};
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::postgres::{PgArgumentBuffer, PgTypeInfo};
use sqlx::{Encode, Postgres, Type};

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
    /// [`Lane::query_vector`] binds in the type it names, so it only checks
    /// the typmod.
    pub(crate) cast: &'static str,
    /// Whether `cast` names `halfvec`.
    half: bool,
}

macro_rules! lane {
    ($width:literal, vector) => {
        lane!($width, "vector", false)
    };
    ($width:literal, halfvec) => {
        lane!($width, "halfvec", true)
    };
    ($width:literal, $kind:literal, $half:literal) => {
        Lane {
            width: $width,
            predicate: concat!("emb.dim = ", $width),
            vec: concat!("emb.vec::", $kind, "(", $width, ")"),
            cast: concat!("::", $kind, "(", $width, ")"),
            half: $half,
        }
    };
}

impl Lane {
    #[must_use]
    pub(crate) const fn of(dim: EmbeddingDim) -> Self {
        match dim {
            EmbeddingDim::D384 => lane!(384, vector),
            EmbeddingDim::D768 => lane!(768, vector),
            EmbeddingDim::D1024 => lane!(1024, vector),
            EmbeddingDim::D1536 => lane!(1536, vector),
            EmbeddingDim::D2048 => lane!(2048, halfvec),
            EmbeddingDim::D3072 => lane!(3072, halfvec),
        }
    }

    /// `values` as the query-vector bind `cast` applies to. A `halfvec`
    /// lane rounds to half precision here rather than in the cast; that is
    /// the rounding the cast applied to a text bind, and the candidates'
    /// scores still come from the full-precision [`vector`].
    #[must_use]
    pub(crate) fn query_vector(self, values: &[f32]) -> QueryVector {
        if self.half {
            QueryVector::Half(HalfVector::from_f32_slice(values))
        } else {
            QueryVector::Full(vector(values))
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

/// `values` as a binary-encoded `vector` bind.
///
/// Never bind a vector as text: a text parameter cast in SQL
/// (`$n::vector(N)`) is not a constant under a generic plan, which Postgres
/// picks for a prepared statement after five executions, so `vector_in`
/// re-parses the whole literal for every row the statement scores.
#[must_use]
pub(crate) fn vector(values: &[f32]) -> Vector {
    Vector::from(values.to_vec())
}

/// A query vector in its [`Lane`]'s type: `vector`, or `halfvec` for the
/// lanes indexed through it. Binary-encoded, like [`vector`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum QueryVector {
    Full(Vector),
    Half(HalfVector),
}

impl Type<Postgres> for QueryVector {
    fn type_info() -> PgTypeInfo {
        Vector::type_info()
    }
}

impl Encode<'_, Postgres> for QueryVector {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        match self {
            Self::Full(values) => values.encode_by_ref(buf),
            Self::Half(values) => values.encode_by_ref(buf),
        }
    }

    /// The parameter's type is the variant's, not [`Type::type_info`]'s.
    fn produces(&self) -> Option<PgTypeInfo> {
        Some(match self {
            Self::Full(_) => Vector::type_info(),
            Self::Half(_) => HalfVector::type_info(),
        })
    }
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
            let bind = lane.query_vector(&vec![0.5; dim.width()]).produces();
            assert_eq!(
                bind.as_ref().map(sqlx::TypeInfo::name),
                Some(kind),
                "the query vector binds in the type the lane casts to"
            );
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
}
