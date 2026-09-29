//! Seeds of the two sidecar selections, `SidecarEquals` and
//! `DeclaredBefore`: which series of one own schema they name, oldest first,
//! before the closure expands them.
//!
//! Both read the schema's declared sidecar joined on its declared memory key
//! to `proxima_core.memory`, under `m.owner_id = $1 AND m.schema_id = $2`.
//! The owner comes off the permit and the schema off the flavor contract, so
//! a caller's column can only narrow them. Every column a selection names is
//! looked up in the catalog before a statement splices it: a column the
//! sidecar does not have, or a `DeclaredBefore` column that is not
//! `timestamptz`, is refused before any lock or delete.

use proxima_core::verbs::query::SidecarAtom;
use proxima_core::{SchemaId, StorageError};
use sqlx::{Postgres, QueryBuilder, Transaction};
use uuid::Uuid;

use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::sidecars::PgSidecarRegistryFrozen;
use crate::verbs::query::push_atom;

/// The columns of table `$1` named in `$2`, and whether each is `timestamptz`.
const SIDECAR_COLUMNS_SQL: &str = "\
SELECT a.attname::text, a.atttypid = 'pg_catalog.timestamptz'::regtype
  FROM pg_catalog.pg_attribute a
 WHERE a.attrelid = to_regclass($1)
   AND a.attnum > 0
   AND NOT a.attisdropped
   AND a.attname = ANY($2::text[])";

/// The sidecar a selection reads, and the named columns it holds.
struct Sidecar<'a> {
    table: PgIdent<'a>,
    /// The table's declared memory-key column (`pg_sidecar!(key: …)`).
    key: PgIdent<'static>,
    /// Each named column the table holds, and whether it is `timestamptz`.
    columns: Vec<(String, bool)>,
}

impl Sidecar<'_> {
    fn is_timestamptz(&self, column: &str) -> bool {
        self.columns
            .iter()
            .any(|(name, timestamptz)| name == column && *timestamptz)
    }
}

/// Resolve `table` as a registered memory sidecar holding every column in
/// `names`.
async fn declared_sidecar<'a>(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    table: Option<&'a str>,
    names: &[&str],
) -> Result<Sidecar<'a>, StorageError> {
    let Some(table) = table else {
        return Err(StorageError::ConstraintViolation(
            "a sidecar selection names no sidecar table".into(),
        ));
    };
    let Some(key) = sidecars
        .memory_key_column(table)
        .filter(|_| sidecars.is_memory_sidecar_table(table))
    else {
        return Err(StorageError::ConstraintViolation(format!(
            "{table} is not a registered memory sidecar keyed on its memory"
        )));
    };
    let columns: Vec<(String, bool)> = sqlx::query_as(SIDECAR_COLUMNS_SQL)
        .bind(table)
        .bind(names)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)?;
    let missing: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| !columns.iter().any(|(column, _)| column == name))
        .collect();
    if !missing.is_empty() {
        return Err(StorageError::ConstraintViolation(format!(
            "{table} has no column {}",
            missing.join(", ")
        )));
    }
    Ok(Sidecar {
        table: PgIdent::table(table)?,
        key: PgIdent::column(key)?,
        columns,
    })
}

/// Series of `schema` under `owner_id` with a hot version whose sidecar row
/// matches every predicate. One seed per series, its oldest matching
/// version; oldest first, at most `limit`.
pub(super) async fn sidecar_equals_seeds(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    owner_id: Uuid,
    schema: &SchemaId,
    table: Option<&str>,
    predicates: &[(String, SidecarAtom)],
    limit: i64,
) -> Result<Vec<Uuid>, StorageError> {
    if predicates.is_empty() {
        return Err(StorageError::ConstraintViolation(
            "a sidecar-equality selection needs at least one column predicate".into(),
        ));
    }
    let names: Vec<&str> = predicates
        .iter()
        .map(|(column, _)| column.as_str())
        .collect();
    let sidecar = declared_sidecar(tx, sidecars, table, &names).await?;
    let predicates = predicates
        .iter()
        .map(|(column, value)| PgIdent::column(column).map(|ident| (ident, value)))
        .collect::<Result<Vec<_>, _>>()?;

    // SQL-POLICY: QueryBuilder-bound-values
    let mut builder =
        QueryBuilder::<Postgres>::new("SELECT t FROM (SELECT DISTINCT ON (m.handle) m.t FROM ");
    // SQL-POLICY: PgIdent — the registered sidecar and its declared key.
    builder.push(sidecar.table.as_str());
    builder.push(" s JOIN proxima_core.memory m ON m.t = s.");
    builder.push(sidecar.key.as_str());
    // SQL-POLICY: fixed-fragment — the owner and schema scope, before
    // anything the caller asked for.
    builder.push(" WHERE m.owner_id = ");
    builder.push_bind(owner_id);
    builder.push(" AND m.schema_id = ");
    builder.push_bind(schema.as_str());
    for (ident, value) in &predicates {
        // SQL-POLICY: PgIdent — a catalog-resolved column of the sidecar.
        builder.push(" AND s.");
        builder.push(ident.as_str());
        builder.push(" = ");
        push_atom(&mut builder, value);
    }
    // SQL-POLICY: fixed-fragment
    builder.push(" ORDER BY m.handle, m.t) AS matched ORDER BY t LIMIT ");
    builder.push_bind(limit);

    builder
        .build_query_scalar::<Uuid>()
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)
}

/// Series of `schema` under `owner_id` whose newest version, over every
/// version of the series, is hot and holds a `column` value older than
/// `cutoff`. The seed is that newest version; oldest value first, at most
/// `limit`.
#[expect(
    clippy::too_many_arguments,
    reason = "one selection's fields plus the transaction, registry and owner it runs under"
)]
pub(super) async fn declared_before_seeds(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    owner_id: Uuid,
    schema: &SchemaId,
    table: Option<&str>,
    column: &str,
    cutoff: time::OffsetDateTime,
    limit: i64,
) -> Result<Vec<Uuid>, StorageError> {
    let sidecar = declared_sidecar(tx, sidecars, table, &[column]).await?;
    if !sidecar.is_timestamptz(column) {
        return Err(StorageError::ConstraintViolation(format!(
            "{}.{column} is not a timestamptz column; DeclaredBefore ages a series \
             only by a timestamptz its sidecar declares",
            sidecar.table.as_str()
        )));
    }
    let column = PgIdent::column(column)?;
    // SQL-POLICY: PgIdent — the registered sidecar, its declared key, and a
    // catalog-resolved `timestamptz` column; every value is bound.
    let sql = format!(
        concat!(
            newest_versions_cte!(),
            "SELECT n.t FROM newest n
  JOIN proxima_core.memory m ON m.t = n.t
  JOIN {tbl} s ON s.{key} = n.t
 WHERE m.owner_id = $1 AND m.schema_id = $2 AND s.{col} < $3
 ORDER BY s.{col}, n.t
 LIMIT $4"
        ),
        tbl = sidecar.table.as_str(),
        key = sidecar.key.as_str(),
        col = column.as_str(),
    );
    // SQL-POLICY: PgIdent (the statement above interpolates only validated identifiers)
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(owner_id)
        .bind(schema.as_str())
        .bind(cutoff)
        .bind(limit)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)
}
