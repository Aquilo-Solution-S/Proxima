use std::time::Instant;

use proxima_core::StorageError;
use sqlx::{PgConnection, PgPool};

use crate::error::map_err;
use crate::tuning::PgTuning;

use super::nonnegative_count;

/// Must stay byte-identical to the definition in `migrations/0001_init.sql`
/// — same name, same operator class, default `m`/`ef_construction`. An
/// index rebuilt at other parameters is not the index production serves.
const CREATE_HNSW_INDEX_SQL: &str = "CREATE INDEX idx_embeddings_vec_hnsw ON proxima_core.embeddings USING hnsw (vec vector_cosine_ops)";

const DROP_HNSW_INDEX_SQL: &str = "DROP INDEX IF EXISTS proxima_core.idx_embeddings_vec_hnsw";

/// `to_regclass` rather than a cast: between the drop and the rebuild the
/// index does not exist, and a size read is how the caller learns that
/// rather than an error it has to classify.
const HNSW_INDEX_BYTES_SQL: &str = "SELECT COALESCE(pg_relation_size(\
     to_regclass('proxima_core.idx_embeddings_vec_hnsw')), 0)::bigint";

/// Session-scoped build resources for one bulk HNSW build.
///
/// `None` leaves the server's own setting untouched, which is what every
/// caller but a measurement harness wants; a harness pins both so two
/// builds of the same corpus are comparable. `maintenance_work_mem` is a
/// Postgres size string (`2GB`), bound rather than interpolated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HnswBuildSettings {
    pub maintenance_work_mem: Option<String>,
    pub max_parallel_maintenance_workers: Option<u32>,
}

/// What one bulk HNSW build cost and produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HnswBuildReport {
    /// Wall clock of the `CREATE INDEX` statement alone.
    pub build_ms: u64,
    pub index_bytes: u64,
}

/// Drop `idx_embeddings_vec_hnsw`, answering the size it held (`0` when it
/// was already absent).
///
/// Semantic search is unavailable to every caller until
/// [`create_hnsw_index`] puts the index back; this is a backfill and
/// measurement verb, not an online one.
///
/// # Errors
///
/// Returns `StorageError::Unavailable` when `hnsw_bulk_build` is off,
/// otherwise maps SQL failures through the shared mapper.
pub async fn drop_hnsw_index(pool: &PgPool, tuning: &PgTuning) -> Result<u64, StorageError> {
    ensure_bulk_build_enabled(tuning)?;
    let mut tx = begin_index_maintenance_tx(pool).await?;
    let index_bytes = hnsw_index_bytes(tx.as_mut()).await?;
    sqlx::query(DROP_HNSW_INDEX_SQL)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;
    Ok(index_bytes)
}

/// Rebuild `idx_embeddings_vec_hnsw` over the rows already loaded, and
/// report what the build cost.
///
/// The settings are `SET LOCAL`, so they bound this build and nothing else:
/// a harness raising `maintenance_work_mem` for one 100k build does not
/// leave the server holding that ceiling for every later session.
///
/// # Errors
///
/// Returns `StorageError::Unavailable` when `hnsw_bulk_build` is off,
/// otherwise maps SQL failures through the shared mapper — including the
/// duplicate-object failure from building an index that is already there.
pub async fn create_hnsw_index(
    pool: &PgPool,
    tuning: &PgTuning,
    settings: &HnswBuildSettings,
) -> Result<HnswBuildReport, StorageError> {
    ensure_bulk_build_enabled(tuning)?;
    let mut tx = begin_index_maintenance_tx(pool).await?;
    if let Some(work_mem) = &settings.maintenance_work_mem {
        // `set_config` rather than `SET LOCAL`: the value is a parameter, and
        // `SET` takes no bind placeholders.
        sqlx::query("SELECT set_config('maintenance_work_mem', $1, true)")
            .bind(work_mem)
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
    }
    if let Some(workers) = settings.max_parallel_maintenance_workers {
        sqlx::query("SELECT set_config('max_parallel_maintenance_workers', $1, true)")
            .bind(workers.to_string())
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
    }

    let started = Instant::now();
    sqlx::query(CREATE_HNSW_INDEX_SQL)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    let build_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let index_bytes = hnsw_index_bytes(tx.as_mut()).await?;
    tx.commit().await.map_err(map_err)?;

    Ok(HnswBuildReport {
        build_ms,
        index_bytes,
    })
}

/// Refuse both verbs unless the flag that owns them is on: nothing in the
/// serving path drops an index production reads, so reaching here with the
/// flag off is a caller mistake rather than a configuration one.
fn ensure_bulk_build_enabled(tuning: &PgTuning) -> Result<(), StorageError> {
    if tuning.hnsw_bulk_build {
        return Ok(());
    }
    Err(StorageError::Unavailable(
        "bulk HNSW build is off; set PROXIMA_PG_HNSW_BULK_BUILD to drop or rebuild \
         idx_embeddings_vec_hnsw"
            .into(),
    ))
}

/// Begin an index-maintenance transaction with the pool's request-serving
/// `statement_timeout` disabled: a 100k-row HNSW build legitimately outruns
/// a request bound, and the drop's ACCESS EXCLUSIVE lock can queue behind a
/// live search. `SET LOCAL` scopes the override to this transaction.
async fn begin_index_maintenance_tx(
    pool: &PgPool,
) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    sqlx::query("SET LOCAL statement_timeout = 0")
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(tx)
}

async fn hnsw_index_bytes(conn: &mut PgConnection) -> Result<u64, StorageError> {
    let bytes: i64 = sqlx::query_scalar(HNSW_INDEX_BYTES_SQL)
        .fetch_one(conn)
        .await
        .map_err(map_err)?;
    nonnegative_count(bytes, "hnsw index bytes")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pool that never connects: the guard must answer before any verb
    /// reaches the database.
    fn unreachable_pool() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://proxima-flag-guard@127.0.0.1:1/none")
            .expect("a lazy pool parses its url without connecting")
    }

    /// Golden text: the rebuilt index must be the one `0001_init.sql`
    /// creates, character for character.
    #[test]
    fn the_rebuilt_index_matches_the_migration() {
        assert_eq!(
            CREATE_HNSW_INDEX_SQL,
            "CREATE INDEX idx_embeddings_vec_hnsw ON proxima_core.embeddings \
             USING hnsw (vec vector_cosine_ops)"
        );
        assert_eq!(
            DROP_HNSW_INDEX_SQL,
            "DROP INDEX IF EXISTS proxima_core.idx_embeddings_vec_hnsw"
        );
    }

    #[tokio::test]
    async fn dropping_the_index_is_refused_while_the_flag_is_off() {
        let err = drop_hnsw_index(&unreachable_pool(), &PgTuning::default())
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("PROXIMA_PG_HNSW_BULK_BUILD"),
            "guard reported {err}"
        );
    }

    #[tokio::test]
    async fn building_the_index_is_refused_while_the_flag_is_off() {
        let err = create_hnsw_index(
            &unreachable_pool(),
            &PgTuning::default(),
            &HnswBuildSettings::default(),
        )
        .await
        .unwrap_err();

        assert!(
            err.to_string().contains("PROXIMA_PG_HNSW_BULK_BUILD"),
            "guard reported {err}"
        );
    }

    /// The defaults must ask the server for nothing, so a build that pins
    /// no resources measures the server's own configuration.
    #[test]
    fn default_settings_pin_nothing() {
        let settings = HnswBuildSettings::default();

        assert_eq!(settings.maintenance_work_mem, None);
        assert_eq!(settings.max_parallel_maintenance_workers, None);
    }
}
