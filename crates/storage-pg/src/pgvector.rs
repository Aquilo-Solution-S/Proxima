use std::fmt::Write as _;

pub(crate) const REQUIRED_PGVECTOR_MAJOR: u32 = 0;
pub(crate) const REQUIRED_PGVECTOR_MINOR: u32 = 8;
pub(crate) const REQUIRED_PGVECTOR_PATCH: u32 = 0;
pub(crate) const SET_HNSW_EF_SEARCH_SQL: &str = "SET LOCAL hnsw.ef_search = 100";
pub(crate) const SET_HNSW_ITERATIVE_SCAN_SQL: &str =
    "SET LOCAL hnsw.iterative_scan = relaxed_order";

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
