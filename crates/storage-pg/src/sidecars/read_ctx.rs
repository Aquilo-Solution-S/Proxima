use proxima_core::owner_inverse::OwnerSurfaces;
use proxima_core::storage_ports::{
    OwnerWritePermit, SIDECAR_SESSION_READ_MAX_ROWS, SidecarSessionRead,
};
use sqlx::QueryBuilder;

use crate::pg_ident::PgIdent;
use crate::verbs::query::push_atom;

use super::{FromRow, MemoryId, PgPool, PgRow, PgSidecarRegistryFrozen, Postgres, StorageError};

/// Run one [`SidecarSessionRead`] against an open session transaction, scoped
/// to `permit`'s owner.
///
/// The whole statement is emitted here: `SELECT to_jsonb(s) FROM <table> s
/// WHERE s.<owner column> = $1 AND <col> = $n … LIMIT n`. The caller supplies
/// a registered table, column names, and bound values — never SQL — so the
/// only statement this path can produce is a single-table `SELECT` over a
/// surface the frozen registry vouches for. That is the read-only
/// enforcement: it is the shape of the builder, not a keyword scan over a
/// caller's text.
///
/// The owner predicate is the FIRST one and comes off the permit, not off
/// `read`: a caller's predicates are `AND`-ed after it, so they can only
/// narrow the owner's rows. The owner column is read from the table's
/// declared `Surface` (`owner_columns`) — the same declaration the erase and
/// transfer legs key on, so "which column carries the owner" has one answer
/// per table. A surface that declares none is refused rather than read
/// unscoped: an empty `owner_columns` claims the row is reached through its
/// key's owner, and reaching it that way needs the declared memory-key
/// column that `owned_head_*` does not yet carry.
///
/// # Errors
///
/// `ConstraintViolation` when the table is not a registered memory sidecar,
/// when it declares no `Surface` or no owner column, when the predicate list
/// is empty, or when a table/column name is not a legal identifier.
/// `Internal` on query failure.
pub(crate) async fn read_own_sidecar_in_tx(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    permit: &OwnerWritePermit,
    read: &SidecarSessionRead<'_>,
) -> Result<Vec<serde_json::Value>, StorageError> {
    if !sidecars.is_memory_sidecar_table(read.table) {
        return Err(StorageError::ConstraintViolation(format!(
            "session sidecar read names {}, which is not a registered memory sidecar table; \
             register the payload with `pg_sidecar!` so the surface is declared",
            read.table
        )));
    }
    if read.predicates.is_empty() {
        return Err(StorageError::ConstraintViolation(
            "session sidecar read needs at least one column predicate; \
             an unfiltered scan of a sidecar table is a query, not a precondition check"
                .to_string(),
        ));
    }
    let owner_column = session_read_owner_column(surfaces, read.table)?;
    let table = PgIdent::table(read.table)?;
    let owner_ident = PgIdent::column(owner_column)?;
    let predicates = read
        .predicates
        .iter()
        .map(|(column, value)| PgIdent::column(column).map(|ident| (ident, value)))
        .collect::<Result<Vec<_>, _>>()?;
    let limit = i64::from(
        read.limit
            .unwrap_or(SIDECAR_SESSION_READ_MAX_ROWS)
            .min(SIDECAR_SESSION_READ_MAX_ROWS),
    );

    // SQL-POLICY: PgIdent
    // SQL-POLICY: QueryBuilder-bound-values
    let mut builder = QueryBuilder::<Postgres>::new("SELECT to_jsonb(s) FROM ");
    builder.push(table.as_str());
    // SQL-POLICY: PgIdent — the owner scope, stamped before anything the
    // caller asked for.
    builder.push(" s WHERE s.");
    builder.push(owner_ident.as_str());
    // SQL-POLICY: fixed-fragment
    builder.push(" = ");
    builder.push_bind(permit.owner().stored_owner_id());
    for (ident, value) in &predicates {
        // SQL-POLICY: PgIdent
        builder.push(" AND s.");
        builder.push(ident.as_str());
        // SQL-POLICY: fixed-fragment
        builder.push(" = ");
        push_atom(&mut builder, value);
    }
    // SQL-POLICY: fixed-fragment
    builder.push(" LIMIT ");
    builder.push_bind(limit);

    builder
        .build_query_scalar::<serde_json::Value>()
        .fetch_all(&mut **tx)
        .await
        .map_err(crate::error::map_err)
}

/// The column a session read scopes `table` by, off its declared `Surface`.
///
/// First declared entry when a surface names several: they are the columns
/// a transfer re-homes together, so they carry the same owner, and reading
/// through any one of them selects the same rows.
fn session_read_owner_column<'a>(
    surfaces: &'a OwnerSurfaces,
    table: &str,
) -> Result<&'a str, StorageError> {
    let Some(surface) = surfaces
        .surfaces()
        .iter()
        .find(|surface| surface.table == table)
    else {
        return Err(StorageError::ConstraintViolation(format!(
            "session sidecar read names {table}, which declares no Surface; \
             declare one on the flavor contract so the read can be owner-scoped"
        )));
    };
    surface.owner_columns.first().copied().ok_or_else(|| {
        StorageError::ConstraintViolation(format!(
            "session sidecar read of {table} cannot be owner-scoped: its Surface declares no \
             owner column. Declare an owner column on the surface, or resolve the row with \
             owned_series_head_memory_id and read it by its memory t"
        ))
    })
}

#[derive(Clone, Copy, Debug)]
pub struct PgSidecarReadCtx<'a> {
    pool: &'a PgPool,
    allow_core_schema: bool,
}

impl<'a> From<&'a PgPool> for PgSidecarReadCtx<'a> {
    fn from(pool: &'a PgPool) -> Self {
        Self {
            pool,
            allow_core_schema: false,
        }
    }
}

impl PgSidecarReadCtx<'_> {
    pub(super) fn for_registered_table(mut self, table: &str) -> Self {
        self.allow_core_schema = table.starts_with("proxima_core.");
        self
    }
}

impl PgSidecarReadCtx<'_> {
    /// Fetch at most one sidecar row using a backend-owned, memory-id-bound query.
    ///
    /// The helper intentionally does not expose `PgPool` to flavor crates. It
    /// rejects core-table SQL so sidecar payload readback cannot become a raw
    /// `proxima_core.*` escape hatch.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` when SQL validation or query execution fails.
    pub async fn fetch_optional_by_memory_id<T>(
        self,
        sql: &str,
        memory_id: MemoryId,
    ) -> Result<Option<T>, StorageError>
    where
        for<'r> T: FromRow<'r, PgRow> + Send + Unpin,
    {
        validate_sidecar_read_sql(sql, self.allow_core_schema)?;
        sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(memory_id.into_inner())
            .fetch_optional(self.pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))
    }

    /// Fetch sidecar rows using a backend-owned, memory-id-bound query.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` when SQL validation or query execution fails.
    pub async fn fetch_all_by_memory_id<T>(
        self,
        sql: &str,
        memory_id: MemoryId,
    ) -> Result<Vec<T>, StorageError>
    where
        for<'r> T: FromRow<'r, PgRow> + Send + Unpin,
    {
        validate_sidecar_read_sql(sql, self.allow_core_schema)?;
        sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(memory_id.into_inner())
            .fetch_all(self.pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))
    }

    /// Fetch owner-pinned sidecar rows.
    ///
    /// Backend-owned SQL only: the statement comes from
    /// [`super::memory_select_batch_owner_pinned_sql`], which is why the
    /// `proxima_core.*` prohibition does not apply here — the join to
    /// `proxima_core.memory` IS the owner rule, and it is emitted by the
    /// backend rather than supplied by a flavor.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` when SQL validation or query execution fails.
    pub async fn fetch_all_by_memory_ids_owner_pinned<T>(
        self,
        sql: &str,
        memory_ids: &[MemoryId],
    ) -> Result<Vec<T>, StorageError>
    where
        for<'r> T: FromRow<'r, PgRow> + Send + Unpin,
    {
        validate_sidecar_read_sql(sql, true)?;
        let raw_memory_ids = memory_ids
            .iter()
            .map(|memory_id| (*memory_id).into_inner())
            .collect::<Vec<_>>();
        sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(&raw_memory_ids)
            .fetch_all(self.pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))
    }

    /// Fetch sidecar rows using a backend-owned, `ANY($1)` memory-id query.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` when SQL validation or query execution fails.
    pub async fn fetch_all_by_memory_ids<T>(
        self,
        sql: &str,
        memory_ids: &[MemoryId],
    ) -> Result<Vec<T>, StorageError>
    where
        for<'r> T: FromRow<'r, PgRow> + Send + Unpin,
    {
        validate_sidecar_read_sql(sql, self.allow_core_schema)?;
        let raw_memory_ids = memory_ids
            .iter()
            .map(|memory_id| (*memory_id).into_inner())
            .collect::<Vec<_>>();
        sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(&raw_memory_ids)
            .fetch_all(self.pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))
    }

    /// Fetch one scalar sidecar value using a backend-owned, memory-id-bound query.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` when SQL validation or query execution fails.
    pub async fn fetch_optional_scalar_by_memory_id<T>(
        self,
        sql: &str,
        memory_id: MemoryId,
    ) -> Result<Option<T>, StorageError>
    where
        for<'r> T: sqlx::Decode<'r, Postgres> + sqlx::Type<Postgres> + Send + Unpin,
    {
        validate_sidecar_read_sql(sql, self.allow_core_schema)?;
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(memory_id.into_inner())
            .fetch_optional(self.pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))
    }
}

pub(crate) fn validate_sidecar_read_sql(
    sql: &str,
    allow_core_schema: bool,
) -> Result<(), StorageError> {
    let lowered = sql.to_ascii_lowercase();
    if !allow_core_schema && lowered.contains("proxima_core.") {
        return Err(StorageError::Internal(
            "sidecar read SQL must not reference proxima_core.*".to_string(),
        ));
    }
    validate_sidecar_read_sql_shape(sql)
}

fn validate_sidecar_read_sql_shape(sql: &str) -> Result<(), StorageError> {
    if !sql.contains("$1") {
        return Err(StorageError::Internal(
            "sidecar read SQL must bind memory_id as $1".to_string(),
        ));
    }
    Ok(())
}
