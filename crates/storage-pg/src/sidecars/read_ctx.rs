use super::{EdgeId, FromRow, MemoryId, PgPool, PgRow, Postgres, StorageError};

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

    /// Fetch sidecar rows using a backend-owned, `ANY($1)` edge-id query.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` when SQL validation or query execution fails.
    pub async fn fetch_all_by_edge_ids<T>(
        self,
        sql: &str,
        edge_ids: &[EdgeId],
    ) -> Result<Vec<T>, StorageError>
    where
        for<'r> T: FromRow<'r, PgRow> + Send + Unpin,
    {
        validate_sidecar_read_sql(sql, self.allow_core_schema)?;
        let raw_edge_ids = edge_ids
            .iter()
            .map(|edge_id| (*edge_id).into_inner())
            .collect::<Vec<_>>();
        sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(&raw_edge_ids)
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
