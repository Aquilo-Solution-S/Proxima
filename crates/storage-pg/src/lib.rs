//! Postgres storage port impls.
//!
//! The verb logic lives under [`verbs`]; this module wires the
//! `PgStorage` struct, connection lifecycle, and migration runner,
//! then delegates each narrow storage port method to its per-verb
//! implementation.
#[cfg(any(test, feature = "test-fixtures"))]
extern crate self as proxima_storage_pg;

#[doc(hidden)]
pub use proxima_core as core;

use std::sync::Arc;
use std::time::Duration;

use proxima_core::StorageError;
use proxima_core::storage_ports::StoragePorts;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
pub use verbs::fact_embeddings::{
    EmbeddingInlineDrainOutcome, EmbeddingReconcileOptions, EmbeddingReconcileOutcome,
    EmbeddingReconcileScope,
};
pub use verbs::retention_maintenance::{
    ChangeEventPruneOptions, ChangeEventPruneOutcome, PruneOwnerOutcome, RetentionEnforceOptions,
    RetentionEnforceOutcome, RetentionOwnerOutcome,
};

use crate::error::internal;
use crate::pgvector::SET_HNSW_ITERATIVE_SCAN_SQL;

#[doc(hidden)]
pub mod access;
mod authorship;
mod change_event;
mod error;
#[doc(hidden)]
pub use error::map_err;
mod pg_ident;
mod pgvector;
mod ports;
pub mod sidecars;
pub mod query {
    pub use crate::verbs::query::{
        MAX_SNAPSHOT_EDGES, authorized_code_chunk_head_candidates, fact_entity_id_for,
    };
}
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_fixtures;
pub mod verbs;
/// Stable, discoverable re-export of the exported `OwnerAccessPort` adapter
/// (see [`access::PgOwnerAccessResolver`]) for embedding hosts.
pub use access::PgOwnerAccessResolver;
pub use sidecars::{
    PgSidecarKey, PgSidecarRegistry, PgSidecarRegistryFrozen, core_pg_sidecars,
    register_core_pg_sidecars,
};

/// Default DB URL when `DATABASE_URL` is unset. Matches the
/// dev DB created locally via `createdb proxima_dev`.
pub const DEFAULT_DATABASE_URL: &str = "postgres://postgres@localhost/proxima_dev";

/// Migration versions deleted from the v0.0.4 destructive baseline.
///
/// `SQLx` stores core and flavor migrations in one `public._sqlx_migrations`
/// table. Keep this explicit list in sync between stale-DB preflight and the
/// guarded local reset path; do not delete broad version ranges.
pub const RETIRED_PRE_V004_MIGRATION_VERSIONS: &[i64] = &[2, 3, 4, 5, 6, 7, 20_260_622_000_000];

/// Embedded core migration set under `crates/storage-pg/migrations/`.
///
/// `ignore_missing = true` is load-bearing when the same database also
/// records flavor migrations in `SQLx`'s default `_sqlx_migrations` table.
#[must_use]
pub fn core_migrator() -> sqlx::migrate::Migrator {
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator
}

/// Fail closed before `SQLx` checksum/missing-file behavior when a database
/// contains pre-v0.0.4 Proxima storage artifacts.
///
/// # Errors
///
/// Returns [`StorageError::V004ResetRequired`] for stale schema state and
/// [`StorageError::Internal`] for catalog query failures.
///
/// # Panics
///
/// Panics if the embedded core migrator does not contain baseline version 1.
pub async fn ensure_v004_baseline_compatible(pool: &PgPool) -> Result<(), StorageError> {
    let migration_table_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(pool)
            .await
            .map_err(internal)?;

    let proxima_schema_objects: Vec<String> = sqlx::query_scalar(
        "SELECT table_schema || '.' || table_name
           FROM information_schema.tables
          WHERE table_schema IN ('proxima_core', 'proxima_code')
          ORDER BY table_schema, table_name
          LIMIT 20",
    )
    .fetch_all(pool)
    .await
    .map_err(internal)?;

    let mut old_versions = Vec::new();
    let mut checksum_mismatch = false;
    let mut current_v1_seen = false;
    if migration_table_exists {
        let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT version, checksum
               FROM public._sqlx_migrations
              WHERE success
                AND version = ANY($1::bigint[])
              ORDER BY version",
        )
        .bind(
            std::iter::once(1_i64)
                .chain(RETIRED_PRE_V004_MIGRATION_VERSIONS.iter().copied())
                .collect::<Vec<_>>(),
        )
        .fetch_all(pool)
        .await
        .map_err(internal)?;

        let current_v1_checksum = core_migrator()
            .iter()
            .find(|migration| migration.version == 1)
            .expect("core baseline migration version 1 exists")
            .checksum
            .as_ref()
            .to_vec();
        for (version, checksum) in rows {
            if version == 1 {
                current_v1_seen = true;
                checksum_mismatch = checksum != current_v1_checksum;
            } else {
                old_versions.push(version);
            }
        }
    }

    let untracked_proxima_schema =
        !proxima_schema_objects.is_empty() && (!migration_table_exists || !current_v1_seen);

    if !untracked_proxima_schema && old_versions.is_empty() && !checksum_mismatch {
        return Ok(());
    }

    let mut details = Vec::new();
    if untracked_proxima_schema {
        details.push(format!(
            "pre-existing Proxima schema objects without v0.0.4 baseline marker: {}",
            proxima_schema_objects.join(", ")
        ));
    }
    if !old_versions.is_empty() {
        details.push(format!("old migration versions: {old_versions:?}"));
    }
    if checksum_mismatch {
        details.push("version 1 checksum differs from v0.0.4 baseline".to_string());
    }
    Err(StorageError::V004ResetRequired {
        details: details.join("; "),
    })
}

/// Minimum applied core migration version for the current release lane.
pub const MIN_CORE_MIGRATION_VERSION: i64 = 10;

/// Fail closed when `skip_migrations` boot runs against a database that has
/// not yet applied the v0.0.6 schema lane.
///
/// # Errors
///
/// Returns [`StorageError::Internal`] when the recorded core migration version
/// or structural v0.0.6 markers are absent.
pub async fn ensure_core_schema_current(pool: &PgPool) -> Result<(), StorageError> {
    let migration_table_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(pool)
            .await
            .map_err(internal)?;

    if migration_table_exists {
        let max_version: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(version) FROM public._sqlx_migrations WHERE success AND version <= 9999",
        )
        .fetch_one(pool)
        .await
        .map_err(internal)?;
        if max_version.unwrap_or(0) < MIN_CORE_MIGRATION_VERSION {
            return Err(StorageError::Internal(format!(
                "database core migrations at version {}; version {MIN_CORE_MIGRATION_VERSION}+ required — apply v0.0.6 core migrations before boot (see MIGRATING.md)",
                max_version.unwrap_or(0)
            )));
        }
    }

    let ready: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM information_schema.columns
              WHERE table_schema = 'proxima_core'
                AND table_name = 'embedding_jobs'
                AND column_name = 'next_attempt_at'
         )
         AND EXISTS (
             SELECT 1
               FROM pg_trigger t
               JOIN pg_class c ON c.oid = t.tgrelid
               JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE n.nspname = 'proxima_core'
                AND c.relname = 'memories'
                AND t.tgname = 'memories_enforce_immutable'
         )
         AND (
             to_regclass('proxima_code.code_chunk_v1') IS NULL
             OR EXISTS (
                 SELECT 1
                   FROM pg_trigger t
                   JOIN pg_class c ON c.oid = t.tgrelid
                   JOIN pg_namespace n ON n.oid = c.relnamespace
                  WHERE n.nspname = 'proxima_code'
                    AND c.relname = 'code_chunk_v1'
                    AND t.tgname = 'code_chunk_v1_append_only'
             )
         )",
    )
    .fetch_one(pool)
    .await
    .map_err(internal)?;

    if !ready {
        return Err(StorageError::Internal(
            "database is missing v0.0.6 schema markers (embedding_jobs.next_attempt_at or memories append-only trigger); apply migrations before boot (see MIGRATING.md)".into(),
        ));
    }
    Ok(())
}

fn parse_pgvector_version(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = version
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

fn pgvector_version_is_supported(version: &str) -> bool {
    let Some(found) = parse_pgvector_version(version) else {
        return false;
    };
    found
        >= (
            pgvector::REQUIRED_PGVECTOR_MAJOR,
            pgvector::REQUIRED_PGVECTOR_MINOR,
            pgvector::REQUIRED_PGVECTOR_PATCH,
        )
}

async fn ensure_pgvector_runtime_compatible(pool: &PgPool) -> Result<(), StorageError> {
    let Some(version) = sqlx::query_scalar::<_, String>(
        "SELECT extversion FROM pg_extension WHERE extname = 'vector'",
    )
    .fetch_optional(pool)
    .await
    .map_err(internal)?
    else {
        return Err(StorageError::Unavailable(
            "pgvector extension is required".into(),
        ));
    };
    if !pgvector_version_is_supported(&version) {
        return Err(StorageError::Unavailable(format!(
            "pgvector >= {}.{}.{} is required for hnsw.iterative_scan; found {version}",
            pgvector::REQUIRED_PGVECTOR_MAJOR,
            pgvector::REQUIRED_PGVECTOR_MINOR,
            pgvector::REQUIRED_PGVECTOR_PATCH
        )));
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|err| StorageError::Unavailable(format!("begin pgvector preflight: {err}")))?;
    sqlx::query(SET_HNSW_ITERATIVE_SCAN_SQL)
        .execute(tx.as_mut())
        .await
        .map_err(|err| {
            StorageError::Unavailable(format!("pgvector hnsw.iterative_scan unavailable: {err}"))
        })?;
    tx.commit()
        .await
        .map_err(|err| StorageError::Unavailable(format!("commit pgvector preflight: {err}")))?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct PgStorage {
    pool: PgPool,
    sidecars: PgSidecarRegistryFrozen,
}

/// Advisory-lock key serializing embedding maintenance passes across
/// processes. ASCII `proxembm` as a big-endian i64 — arbitrary but stable;
/// changing it would let old and new binaries run maintenance concurrently.
const EMBEDDING_MAINTENANCE_LOCK_KEY: i64 = i64::from_be_bytes(*b"proxembm");

/// Guard for the global embedding-maintenance advisory lock. The session
/// lock lives on a connection detached from the pool; dropping the guard
/// closes that connection, and Postgres releases the lock with the session.
pub struct EmbeddingMaintenanceLock {
    _conn: sqlx::postgres::PgConnection,
}

impl std::fmt::Debug for EmbeddingMaintenanceLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddingMaintenanceLock")
            .finish_non_exhaustive()
    }
}

/// Advisory-lock key serializing retention maintenance passes across
/// processes. ASCII `proxretn` as a big-endian i64 — arbitrary but stable,
/// distinct from [`EMBEDDING_MAINTENANCE_LOCK_KEY`] so the two maintenance
/// families may run concurrently but never overlap themselves.
const RETENTION_MAINTENANCE_LOCK_KEY: i64 = i64::from_be_bytes(*b"proxretn");

/// Guard for the global retention-maintenance advisory lock. Same
/// detached-connection design as [`EmbeddingMaintenanceLock`]: dropping the
/// guard closes the connection, and Postgres releases the session lock.
pub struct RetentionMaintenanceLock {
    _conn: sqlx::postgres::PgConnection,
}

impl std::fmt::Debug for RetentionMaintenanceLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetentionMaintenanceLock")
            .finish_non_exhaustive()
    }
}

/// Parse a `u64` pool-tuning env var, falling back to `default` when unset or
/// unparseable. `0` is a legal value (disables the corresponding bound).
fn env_u64_or(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .unwrap_or(default)
}

/// Parse a `u32` pool-tuning env var with a floor of 1 (a pool of zero
/// connections is never valid), falling back to `default`.
fn env_u32_min1(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(default)
}

impl PgStorage {
    /// Connect using `url`, build a tuned pool, and verify
    /// connectivity by acquiring one connection.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Unavailable` on connection or
    /// query failure.
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        let mut opts: PgConnectOptions = url.parse().map_err(|e: sqlx::Error| {
            StorageError::Unavailable(format!("invalid DATABASE_URL: {e}"))
        })?;
        // A conservative per-statement timeout bounds
        // a runaway query (e.g. a pathological search) so it cannot pin a pool
        // connection indefinitely and starve the gateway. Generous by default
        // (5 min — only a truly stuck statement hits it); tune or disable (0)
        // per deployment. The two operations that can legitimately exceed it —
        // schema migrations and bulk compliance erase — explicitly opt out
        // (`run_migrations` runs on a detached timeout-free connection; the erase
        // transaction issues `SET LOCAL statement_timeout = 0`).
        let statement_timeout_ms = env_u64_or("PROXIMA_PG_STATEMENT_TIMEOUT_MS", 300_000);
        if statement_timeout_ms > 0 {
            opts = opts.options([("statement_timeout", statement_timeout_ms.to_string())]);
        }
        let pool = PgPoolOptions::new()
            .max_connections(env_u32_min1("PROXIMA_PG_MAX_CONNECTIONS", 10))
            .acquire_timeout(Duration::from_secs(env_u64_or(
                "PROXIMA_PG_ACQUIRE_TIMEOUT_SECS",
                5,
            )))
            .idle_timeout(Duration::from_secs(env_u64_or(
                "PROXIMA_PG_IDLE_TIMEOUT_SECS",
                600,
            )))
            .max_lifetime(Duration::from_secs(env_u64_or(
                "PROXIMA_PG_MAX_LIFETIME_SECS",
                1_800,
            )))
            .connect_with(opts)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        // Validate connectivity with a trivial query.
        sqlx::query!("SELECT 1 AS one")
            .fetch_one(&pool)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        Ok(Self {
            pool,
            sidecars: core_pg_sidecars(),
        })
    }

    /// Read `DATABASE_URL` from env, fallback to
    /// `DEFAULT_DATABASE_URL`. Convenience for the bin / dev.
    #[must_use]
    pub fn url_from_env() -> String {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string())
    }

    #[cfg(any(
        test,
        feature = "test-fixtures",
        feature = "backend-api",
        debug_assertions
    ))]
    #[doc(hidden)]
    #[must_use]
    pub fn pool_for_tests(&self) -> &PgPool {
        &self.pool
    }

    #[cfg(any(feature = "backend-api", feature = "test-fixtures"))]
    #[doc(hidden)]
    #[must_use]
    pub fn clone_pool_for_backend(&self) -> PgPool {
        self.pool.clone()
    }

    #[must_use]
    pub fn sidecars(&self) -> &PgSidecarRegistryFrozen {
        &self.sidecars
    }

    /// Replace the entire sidecar registry.
    ///
    /// The caller must include the core sidecars. The boot/facade path
    /// enforces sidecar coverage with `freeze_against`; tests may pass
    /// deliberate partial registries.
    #[must_use]
    pub fn with_sidecars(mut self, sidecars: PgSidecarRegistryFrozen) -> Self {
        self.sidecars = sidecars;
        self
    }

    #[must_use]
    pub fn storage_ports(self: Arc<Self>) -> StoragePorts {
        StoragePorts::builder()
            .fact_ingest(self.clone())
            .mcp_call_write(self.clone())
            .mcp_call_read(self.clone())
            .memory_authoring(self.clone())
            .memory_read(self.clone())
            .memory_inspect(self.clone())
            .embedding_text(self.clone())
            .embedding_write(self.clone())
            .embedding_job(self.clone())
            .embedding_maintenance(self.clone())
            .goal_write(self.clone())
            .goal_read(self.clone())
            .goal_wake_candidate(self.clone())
            .change_event(self.clone())
            .edge_read(self.clone())
            .citation(self.clone())
            .owner_access_read(self.clone())
            .owner_membership_admin(self.clone())
            .owner_transfer(self.clone())
            .source_batch(self.clone())
            .source_cursor(self.clone())
            .fact_retention(self.clone())
            .compliance_erase(self.clone())
            .registry_projection(self)
            .build()
    }

    /// Global enqueue-only embedding reconciliation.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the reconciliation query.
    pub async fn reconcile_embeddings(
        &self,
        options: EmbeddingReconcileOptions<'_>,
    ) -> Result<EmbeddingReconcileOutcome, StorageError> {
        verbs::fact_embeddings::reconcile_embeddings(&self.pool, options).await
    }

    /// Inline drain for queued embedding jobs.
    ///
    /// # Errors
    ///
    /// Returns storage errors from claiming or writing jobs/embeddings.
    pub async fn drain_embedding_jobs_inline(
        &self,
        client: &dyn proxima_core::llm::EmbeddingClient,
        limit: i64,
    ) -> Result<EmbeddingInlineDrainOutcome, StorageError> {
        verbs::fact_embeddings::drain_embedding_jobs_inline(&self.pool, client, limit).await
    }

    /// Delete embedding infrastructure rows whose source entity no longer
    /// exists (crash residue). Operator surface for the maintenance CLI,
    /// like [`Self::reconcile_embeddings`]; in-engine callers go through
    /// `Engine::sweep_orphan_embedding_rows`, which gates on operator
    /// authority — here, holding the database credentials is that authority.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the sweep transaction.
    pub async fn sweep_orphan_embedding_rows(
        &self,
    ) -> Result<proxima_core::EmbeddingOrphanSweepOutcome, StorageError> {
        verbs::fact_embeddings::sweep_orphan_embedding_rows(&self.pool).await
    }

    /// Owner-agnostic embedding ANN health signals (backlog, orphan counts,
    /// recall canary). Operator surface for the maintenance CLI; see
    /// [`Self::sweep_orphan_embedding_rows`] for the authority note.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the observability reads.
    pub async fn embedding_ann_observability(
        &self,
    ) -> Result<proxima_core::EmbeddingAnnObservability, StorageError> {
        verbs::fact_embeddings::embedding_ann_observability(&self.pool).await
    }

    /// Try to take the global embedding-maintenance advisory lock.
    ///
    /// Returns `None` when another maintenance pass already holds it, so
    /// overlapping cron fires skip instead of double-scanning. The lock is
    /// session-scoped on a connection detached from the pool; dropping the
    /// returned guard closes that connection, which releases the lock
    /// server-side — there is no unlock call to forget.
    ///
    /// # Errors
    ///
    /// Returns storage errors from acquiring the connection or the lock query.
    pub async fn try_embedding_maintenance_lock(
        &self,
    ) -> Result<Option<EmbeddingMaintenanceLock>, StorageError> {
        Ok(self
            .try_maintenance_lock_conn(EMBEDDING_MAINTENANCE_LOCK_KEY)
            .await?
            .map(|conn| EmbeddingMaintenanceLock { _conn: conn }))
    }

    /// Try to take the global retention-maintenance advisory lock.
    ///
    /// Same contract as [`Self::try_embedding_maintenance_lock`], on its own
    /// key: `None` means another retention pass already holds it and this
    /// run should skip.
    ///
    /// # Errors
    ///
    /// Returns storage errors from acquiring the connection or the lock query.
    pub async fn try_retention_maintenance_lock(
        &self,
    ) -> Result<Option<RetentionMaintenanceLock>, StorageError> {
        Ok(self
            .try_maintenance_lock_conn(RETENTION_MAINTENANCE_LOCK_KEY)
            .await?
            .map(|conn| RetentionMaintenanceLock { _conn: conn }))
    }

    /// Session-scoped `pg_try_advisory_lock` on a connection detached from
    /// the pool; the caller wraps the connection in a guard whose drop
    /// closes it, releasing the lock server-side.
    async fn try_maintenance_lock_conn(
        &self,
        key: i64,
    ) -> Result<Option<sqlx::postgres::PgConnection>, StorageError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|err| {
                StorageError::Unavailable(format!("acquire maintenance lock connection: {err}"))
            })?
            .detach();
        let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(key)
            .fetch_one(&mut conn)
            .await
            .map_err(crate::error::map_err)?;
        Ok(locked.then_some(conn))
    }

    /// Tombstone Facts past their owner's configured retention window.
    /// Operator surface for the maintenance CLI; see
    /// [`Self::sweep_orphan_embedding_rows`] for the authority note. Each
    /// owner is processed under the per-owner legal-hold advisory lock and
    /// skipped while a hold is active (docs/13 forward rule).
    ///
    /// # Errors
    ///
    /// Returns storage errors from the sweep transactions, and
    /// `ConstraintViolation` for a non-positive batch size.
    pub async fn enforce_fact_retention(
        &self,
        options: RetentionEnforceOptions,
    ) -> Result<RetentionEnforceOutcome, StorageError> {
        verbs::retention_maintenance::enforce_fact_retention(&self.pool, options).await
    }

    /// Delete `change_event` rows older than an explicit age horizon.
    /// Operator surface for the maintenance CLI; see
    /// [`Self::sweep_orphan_embedding_rows`] for the authority note. Same
    /// per-owner legal-hold gate as [`Self::enforce_fact_retention`].
    ///
    /// # Errors
    ///
    /// Returns storage errors from the prune transactions, and
    /// `ConstraintViolation` for a non-positive horizon or batch size.
    pub async fn prune_change_events(
        &self,
        options: ChangeEventPruneOptions,
    ) -> Result<ChangeEventPruneOutcome, StorageError> {
        verbs::retention_maintenance::prune_change_events(&self.pool, options).await
    }

    /// Apply all pending migrations under
    /// `crates/storage-pg/migrations/`. Idempotent — sqlx tracks
    /// applied migrations in `_sqlx_migrations`. Call once
    /// at process start before any verb dispatch.
    ///
    /// `ignore_missing = true` matches the per-flavor migrator
    /// (`flavors/*/migrations.rs`): core and every flavor share the
    /// default `_sqlx_migrations` table, so on a second run the core
    /// migrator sees flavor-authored versions it doesn't know about.
    /// Without this relaxation the second run fails with
    /// `VersionMissing(<flavor version>)`. The core version-set is
    /// still validated; we only relax the cross-author check.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Internal` on any sqlx
    /// migration failure (broken file, conflict with the
    /// recorded checksum, etc.).
    pub async fn run_migrations(&self) -> Result<(), StorageError> {
        ensure_v004_baseline_compatible(&self.pool).await?;
        // The pool's default `statement_timeout`
        // bounds request-serving queries, but a schema migration (CREATE INDEX,
        // backfill) may legitimately run longer than that — aborting one
        // mid-flight would leave the schema half-migrated. Run migrations on a
        // dedicated connection with the timeout disabled, then detach it so the
        // override is never returned to the shared pool.
        let mut conn = self.pool.acquire().await.map_err(internal)?;
        sqlx::query("SET statement_timeout = 0")
            .execute(&mut *conn)
            .await
            .map_err(internal)?;
        let migrated = core_migrator().run(&mut *conn).await.map_err(internal);
        conn.detach();
        migrated?;
        ensure_pgvector_runtime_compatible(&self.pool).await?;
        Ok(())
    }
}

#[cfg(test)]
mod pgvector_tests {
    use super::{parse_pgvector_version, pgvector_version_is_supported};

    #[test]
    fn pgvector_version_parser_handles_patch_and_suffixes() {
        assert_eq!(parse_pgvector_version("0.8.2"), Some((0, 8, 2)));
        assert_eq!(parse_pgvector_version("0.8"), Some((0, 8, 0)));
        assert_eq!(parse_pgvector_version("0.8.0beta1"), Some((0, 8, 0)));
        assert_eq!(parse_pgvector_version("not-a-version"), None);
    }

    #[test]
    fn pgvector_version_floor_is_0_8_0() {
        assert!(!pgvector_version_is_supported("0.7.4"));
        assert!(pgvector_version_is_supported("0.8.0"));
        assert!(pgvector_version_is_supported("0.8.2"));
        assert!(pgvector_version_is_supported("1.0.0"));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn core_migrator_contains_v006_migrations() {
        let versions: Vec<i64> = super::core_migrator()
            .iter()
            .map(|migration| migration.version)
            .collect();
        assert!(
            versions.contains(&9),
            "core migrator must embed 0009_v006.sql"
        );
        assert!(
            versions.contains(&10),
            "core migrator must embed 0010_v006.sql"
        );
    }

    #[test]
    fn pool_env_helpers_default_when_unset() {
        // Fixed-key helpers fall back to the default for an unset/garbage key
        // (pool tuning). Use keys nothing else in the process sets.
        assert_eq!(
            super::env_u32_min1("PROXIMA_PG_MAX_CONNECTIONS_TEST_UNSET", 10),
            10
        );
        assert_eq!(
            super::env_u64_or("PROXIMA_PG_STATEMENT_TIMEOUT_MS_TEST_UNSET", 300_000),
            300_000
        );
    }
}
