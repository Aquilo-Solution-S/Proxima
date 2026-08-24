//! Owned current-head handle lookup by sidecar column values.
//!
//! Tesla-valve admit: sidecar row → `memory.t` → `memory_head` (`h.t = m.t`).
//! Owner-only. A transferred series is a miss for the prior owner.
//!
//! The sidecar side of the first join is the column the table's registration
//! declares (`pg_sidecar!(key: …)`, held equal to the contract `Surface`'s
//! `KeyShape::MemoryT { column }` at freeze), not a literal `t`. `m.t` and
//! `h.t` are the kernel tables' own keys and are fixed.

use proxima_core::verbs::query::SidecarAtom;
use proxima_core::{Owner, SchemaId, StorageError};
use sqlx::{PgExecutor, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::error::map_err;
use crate::pg_ident::PgIdent;

/// Which column of the head row the lookup projects.
///
/// One statement, two readers: the ingest path wants the series `handle` to
/// append a later `t` onto, a precondition check wants the head `t` itself
/// so it can read that row's sidecar. Both come off the same `memory_head`
/// row, so they are one query with one join, not two spellings of it.
#[derive(Debug, Clone, Copy)]
pub(crate) enum HeadProjection {
    Handle,
    MemoryId,
}

impl HeadProjection {
    const fn column(self) -> &'static str {
        match self {
            Self::Handle => "SELECT h.handle FROM ",
            Self::MemoryId => "SELECT h.t FROM ",
        }
    }
}

/// Current owned series handle for `schema_id` whose sidecar matches `columns`.
///
/// `key_column` is `sidecar_table`'s DECLARED memory-key column, off the
/// frozen sidecar registry — see [`memory_key_column`].
///
/// # Errors
///
/// `ConstraintViolation` when a column identifier is invalid.
/// `Internal` on query failure.
///
/// [`memory_key_column`]: crate::sidecars::PgSidecarRegistryFrozen::memory_key_column
pub async fn owned_head_handle<'e, E>(
    executor: E,
    owner: Owner,
    schema_id: &SchemaId,
    sidecar_table: &str,
    key_column: &str,
    columns: &[(&str, SidecarAtom)],
) -> Result<Option<Uuid>, StorageError>
where
    E: PgExecutor<'e>,
{
    owned_head_column(
        executor,
        owner,
        schema_id,
        sidecar_table,
        key_column,
        columns,
        HeadProjection::Handle,
    )
    .await
}

/// Current owned series head `t` for `schema_id` whose sidecar matches
/// `columns` — the [`HeadProjection::MemoryId`] reading of
/// [`owned_head_handle`].
///
/// # Errors
///
/// `ConstraintViolation` when a column identifier is invalid.
/// `Internal` on query failure.
pub(crate) async fn owned_head_memory_id<'e, E>(
    executor: E,
    owner: Owner,
    schema_id: &SchemaId,
    sidecar_table: &str,
    key_column: &str,
    columns: &[(&str, SidecarAtom)],
) -> Result<Option<Uuid>, StorageError>
where
    E: PgExecutor<'e>,
{
    owned_head_column(
        executor,
        owner,
        schema_id,
        sidecar_table,
        key_column,
        columns,
        HeadProjection::MemoryId,
    )
    .await
}

async fn owned_head_column<'e, E>(
    executor: E,
    owner: Owner,
    schema_id: &SchemaId,
    sidecar_table: &str,
    key_column: &str,
    columns: &[(&str, SidecarAtom)],
    projection: HeadProjection,
) -> Result<Option<Uuid>, StorageError>
where
    E: PgExecutor<'e>,
{
    if columns.is_empty() {
        return Err(StorageError::ConstraintViolation(
            "owned series-handle lookup requires at least one sidecar column".into(),
        ));
    }
    let table = PgIdent::table(sidecar_table)?;
    let key = PgIdent::column(key_column)?;
    let col_idents = columns
        .iter()
        .map(|(column, _)| PgIdent::column(column))
        .collect::<Result<Vec<_>, _>>()?;

    // SQL-POLICY: PgIdent
    // SQL-POLICY: QueryBuilder-bound-values
    // SQL-POLICY: fixed-fragment — `HeadProjection::column` is a closed set
    // of two literals.
    let mut builder = QueryBuilder::<Postgres>::new(projection.column());
    builder.push(table.as_str());
    // SQL-POLICY: fixed-fragment
    builder.push(" s JOIN proxima_core.memory m ON m.t = s.");
    // SQL-POLICY: PgIdent
    builder.push(key.as_str());
    // SQL-POLICY: fixed-fragment
    builder.push(
        " JOIN proxima_core.memory_head h ON h.handle = m.handle AND h.t = m.t \
         WHERE m.owner_id = ",
    );
    builder.push_bind(owner.stored_owner_id());
    // SQL-POLICY: fixed-fragment
    builder.push(" AND m.schema_id = ");
    builder.push_bind(schema_id.as_str());
    for (ident, (_, value)) in col_idents.iter().zip(columns) {
        // SQL-POLICY: PgIdent
        builder.push(" AND s.");
        builder.push(ident.as_str());
        // SQL-POLICY: fixed-fragment
        builder.push(" = ");
        push_atom(&mut builder, value);
    }
    // SQL-POLICY: fixed-fragment
    builder.push(" LIMIT 1");

    builder
        .build_query_scalar()
        .fetch_optional(executor)
        .await
        .map_err(map_err)
}

/// Bind one [`SidecarAtom`] into a builder.
///
/// The single binder for the atom vocabulary: every generated statement
/// that takes a flavor-named column value goes through it, so "which Rust
/// type does a `Text` atom bind as" has one answer.
// SQL-POLICY: QueryBuilder-bound-values
pub(crate) fn push_atom(builder: &mut QueryBuilder<Postgres>, value: &SidecarAtom) {
    match value {
        SidecarAtom::Uuid(id) => {
            builder.push_bind(*id);
        }
        SidecarAtom::Text(text) => {
            builder.push_bind(text.clone());
        }
        SidecarAtom::I32(n) => {
            builder.push_bind(*n);
        }
        SidecarAtom::I64(n) => {
            builder.push_bind(*n);
        }
        SidecarAtom::Bool(flag) => {
            builder.push_bind(*flag);
        }
    }
}

// The renamed-key fixture lives beside the builder: what it proves is that
// the emitted statement is valid Postgres against a sidecar keyed on a
// column of its own naming, which no string comparison can reach.
#[cfg(test)]
#[path = "series_handle_pg_tests.rs"]
mod series_handle_pg_tests;
