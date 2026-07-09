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

use crate::error::internal;
use crate::pgvector::SET_HNSW_ITERATIVE_SCAN_SQL;

#[doc(hidden)]
pub mod access;
mod authorship;
mod change_event;
mod error;
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
        // P1.4 (analysis 2026-07-05): a conservative per-statement timeout bounds
        // a runaway query (e.g. a pathological search) so it cannot pin a pool
        // connection indefinitely and starve the gateway. Generous by default
        // (5 min — only a truly stuck statement hits it, so bulk compliance
        // erase/migrations are unaffected); tune or disable (0) per deployment.
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
        core_migrator().run(&self.pool).await.map_err(internal)?;
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
        // (P1.4 pool tuning). Use keys nothing else in the process sets.
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
