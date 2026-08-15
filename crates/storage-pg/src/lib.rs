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
use proxima_core::env_value;
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
use crate::pgvector::set_hnsw_search_sql;

#[doc(hidden)]
pub mod access;
mod authorship;
mod change_event;
mod delegated_authority;
mod error;
#[doc(hidden)]
pub use error::map_err;
mod pg_ident;
mod pgvector;
mod ports;
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
pub use ports::neighbor_memory_edges_sql_for_tests;
pub mod sidecars;
pub mod query {
    pub use crate::verbs::query::{
        CodeChunkVectorCandidate, CodeChunkVectorFilters, MAX_SNAPSHOT_EDGES,
        authorized_code_chunk_head_candidates, fact_entity_id_for, nearest_code_chunk_candidates,
    };
}
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_fixtures;
mod tuning;
pub mod verbs;
/// Stable, discoverable re-export of the exported `OwnerAccessPort` adapter
/// (see [`access::PgOwnerAccessResolver`]) for embedding hosts.
pub use access::PgOwnerAccessResolver;
pub use delegated_authority::PgDelegationStore;
pub use sidecars::{
    PgSidecarKey, PgSidecarRegistry, PgSidecarRegistryFrozen, core_pg_sidecars,
    register_core_pg_sidecars,
};
pub use tuning::{HnswIterativeScan, PgTuning, SemanticIndexFirst};

/// Default DB URL when `DATABASE_URL` is unset. Matches the
/// dev DB created locally via `createdb proxima_dev`.
pub const DEFAULT_DATABASE_URL: &str = "postgres://postgres@localhost/proxima_dev";

/// Namespace boundary between core and flavor migration versions.
///
/// Core migrations use small sequential integer versions (`0001_init.sql`,
/// `0011_v007.sql`, …); flavor migrations use date-shaped versions
/// (`20260801000020_…`). Every ledger row at or below this ceiling belongs to
/// the core lane and must be accounted for by the embedded core migrator —
/// that invariant is what lets the preflight below detect draft and retired
/// versions *generically*, with no enumerated version lists (see
/// docs/how-to/migrations.md).
pub const CORE_MIGRATION_VERSION_CEILING: i64 = 9999;

/// Embedded core migration set under `crates/storage-pg/migrations/`.
///
/// `ignore_missing = true` is load-bearing twice over: a database migrated
/// before the v0.0.7 per-flavor ledger split still carries flavor rows in
/// the shared `public._sqlx_migrations` table, and the squash workflow
/// (docs/how-to/migrations.md) leaves orphaned draft rows behind that are
/// forgiven rather than enumerated.
#[must_use]
pub fn core_migrator() -> sqlx::migrate::Migrator {
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator
}

/// Fail closed, and legibly, before `SQLx` applies anything, when the
/// database's core-lane ledger cannot be reconciled with the embedded
/// migration set.
///
/// The check is generic — there is deliberately no list of known-bad versions
/// (see docs/how-to/migrations.md: if writing a migration ever seems to
/// require adding a version list here, the migration workflow is being
/// violated). Two invariants are enforced over every successful
/// core-namespace ledger row (`version <= CORE_MIGRATION_VERSION_CEILING`):
///
/// - **Every recorded version exists in the embedded set.** A version the
///   binary does not ship is a draft or retired migration — a dev-cycle lane
///   later squashed under a fresh number, or a pre-v0.0.4 artifact. Applying
///   the squashed file over that schema would re-run its DDL, so this fails
///   first with the remedy (stamp or reset) instead of a raw SQL error.
/// - **Every recorded checksum matches the embedded file.** A mismatch means
///   the file's bytes changed after this database applied it. `SQLx` itself
///   rejects this state (`VersionMismatch`), but only after the point where
///   its error can say nothing about why or what to do.
///
/// # Errors
///
/// Returns [`StorageError::V004ResetRequired`] for pre-v0.0.4 signals —
/// Proxima schema objects with no baseline ledger marker, or a baseline
/// (version 1) checksum that predates the v0.0.4 destructive reset; the only
/// remedy there is export + reset, never a stamp. Returns
/// [`StorageError::Internal`], naming the stamp-or-reset remedy, for draft or
/// retired versions and post-baseline checksum drift, and for catalog query
/// failures.
pub async fn ensure_core_ledger_compatible(pool: &PgPool) -> Result<(), StorageError> {
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

    let mut recorded: Vec<(i64, Vec<u8>)> = Vec::new();
    if migration_table_exists {
        recorded = sqlx::query_as(
            "SELECT version, checksum
               FROM public._sqlx_migrations
              WHERE success
                AND version <= $1
              ORDER BY version",
        )
        .bind(CORE_MIGRATION_VERSION_CEILING)
        .fetch_all(pool)
        .await
        .map_err(internal)?;
    }

    let embedded: std::collections::BTreeMap<i64, Vec<u8>> = core_migrator()
        .iter()
        .map(|migration| (migration.version, migration.checksum.as_ref().to_vec()))
        .collect();

    let mut unknown_versions = Vec::new();
    let mut amended_versions = Vec::new();
    let mut baseline_seen = false;
    let mut baseline_checksum_drift = false;
    for (version, checksum) in &recorded {
        match embedded.get(version) {
            None => unknown_versions.push(*version),
            Some(expected) if *version == 1 => {
                baseline_seen = true;
                baseline_checksum_drift = checksum != expected;
            }
            Some(expected) => {
                if checksum != expected {
                    amended_versions.push(*version);
                }
            }
        }
    }

    let untracked_proxima_schema = !proxima_schema_objects.is_empty() && !baseline_seen;

    if untracked_proxima_schema || baseline_checksum_drift {
        let mut details = Vec::new();
        if untracked_proxima_schema {
            details.push(format!(
                "pre-existing Proxima schema objects without v0.0.4 baseline marker: {}",
                proxima_schema_objects.join(", ")
            ));
        }
        if baseline_checksum_drift {
            details.push("version 1 checksum differs from v0.0.4 baseline".to_string());
        }
        if !unknown_versions.is_empty() {
            details.push(format!("old migration versions: {unknown_versions:?}"));
        }
        return Err(StorageError::V004ResetRequired {
            details: details.join("; "),
        });
    }

    if unknown_versions.is_empty() && amended_versions.is_empty() {
        return Ok(());
    }

    let mut details = Vec::new();
    if !unknown_versions.is_empty() {
        details.push(format!(
            "core versions {unknown_versions:?} are recorded as applied but this binary does not \
             embed them (draft or retired migrations, e.g. a dev-cycle lane squashed under a new \
             version)"
        ));
    }
    if !amended_versions.is_empty() {
        details.push(format!(
            "core versions {amended_versions:?} were amended after this database applied them \
             (recorded checksum no longer matches the embedded file)"
        ));
    }
    Err(StorageError::Internal(format!(
        "database core-migration ledger does not reconcile with this binary: {}. \
         If the schema already matches the current lane, stamp the ledger with \
         `cargo run -p proxima-dev-migrate -- --stamp --database-url <URL>`; \
         otherwise reset (dev/staging only) with \
         `PROXIMA_V004_RESET_CONFIRM=reset-my-dev-db cargo run -p proxima-dev-migrate -- --reset --database-url <URL>`, \
         then re-register and re-index. See docs/how-to/migrations.md and MIGRATING.md",
        details.join("; ")
    )))
}

/// Minimum applied core migration version for the current release lane:
/// newest embedded core migration. Derived, not a hand-maintained constant.
///
/// # Panics
///
/// Panics if the embedded core migration set is empty, which cannot happen in
/// a correctly built binary.
#[must_use]
pub fn min_core_migration_version() -> i64 {
    core_migrator()
        .iter()
        .map(|migration| migration.version)
        .filter(|version| *version <= CORE_MIGRATION_VERSION_CEILING)
        .max()
        .expect("embedded core migration set is non-empty")
}

/// Fail closed when `skip_migrations` boot runs against a database that has
/// not yet applied the current schema lane.
///
/// The version check alone is not enough — a database can carry the ledger
/// row without the objects, and a split-role deploy applies DDL out of band —
/// so this also probes the structural artifacts each lane introduced. Every
/// marker below is something the running binary emits unconditionally, which
/// is what makes its absence a boot failure rather than a first-query one.
///
/// # Errors
///
/// Returns [`StorageError::Internal`] when the recorded core migration version
/// or the structural markers for the current lane are absent.
pub async fn ensure_core_schema_current(pool: &PgPool) -> Result<(), StorageError> {
    let migration_table_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(pool)
            .await
            .map_err(internal)?;

    if migration_table_exists {
        let min_required = min_core_migration_version();
        let max_version: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(version) FROM public._sqlx_migrations WHERE success AND version <= $1",
        )
        .bind(CORE_MIGRATION_VERSION_CEILING)
        .fetch_one(pool)
        .await
        .map_err(internal)?;
        if max_version.unwrap_or(0) < min_required {
            return Err(StorageError::Internal(format!(
                "database core migrations at version {}; version {min_required}+ required — apply the current schema lane before boot (see MIGRATING.md)",
                max_version.unwrap_or(0)
            )));
        }
    }

    ensure_core_schema_markers(pool).await
}

/// The structural half of [`ensure_core_schema_current`]: probe the schema
/// artifacts each release lane introduced, without consulting the migration
/// ledger at all. `tools/dev-migrate --stamp` gates on exactly this — a
/// database that ran a since-squashed draft lane has the *schema* of the
/// current lane but a ledger that cannot yet say so, which is the one state
/// where stamping is honest.
///
/// # Errors
///
/// Returns [`StorageError::Internal`] when any structural marker for the
/// current lane is absent.
#[expect(
    clippy::too_many_lines,
    reason = "one boot probe: every marker is a separate EXISTS arm of the same \
              query, and the comment above each is what makes it auditable"
)]
pub async fn ensure_core_schema_markers(pool: &PgPool) -> Result<(), StorageError> {
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
         )
         -- v0.0.7 lane (0011_v007.sql), marker by marker. Every search emits
         -- memories.search_tsv, every sidecar without a stored column calls
         -- lexical_tsv(), and every embedding write binds chunk_index — none
         -- of them have a fallback.
         AND EXISTS (
             SELECT 1
               FROM information_schema.columns
              WHERE table_schema = 'proxima_core'
                AND table_name = 'memories'
                AND column_name = 'search_tsv'
         )
         AND EXISTS (
             SELECT 1
               FROM information_schema.columns
              WHERE table_schema = 'proxima_core'
                AND table_name = 'embeddings'
                AND column_name = 'chunk_index'
         )
         AND to_regprocedure('proxima_core.lexical_tsv(text)') IS NOT NULL
         -- Every lexical search emits proxima_core.lexical_config() to build
         -- its tsquery, and every stored search_tsv was generated through it.
         -- A database without it answers no lexical query at all.
         AND to_regprocedure('proxima_core.lexical_config()') IS NOT NULL
         -- Per-row language: every memory INSERT binds
         -- memories.lexical_language, every lexical query reads
         -- lexical_languages and ranks with the row's configuration through
         -- the two-argument lexical_tsv — none of them have a fallback.
         AND EXISTS (
             SELECT 1
               FROM information_schema.columns
              WHERE table_schema = 'proxima_core'
                AND table_name = 'memories'
                AND column_name = 'lexical_language'
         )
         AND to_regprocedure('proxima_core.lexical_tsv(regconfig, text)') IS NOT NULL
         AND to_regclass('proxima_core.lexical_languages') IS NOT NULL
         -- Edge reset. Every index write binds the new edge
         -- columns, every derived write binds authoring_perspective_id, and
         -- every goal write binds the topology columns — none of them have a
         -- fallback, and a pre-lane database would fail at first write rather
         -- than at boot. The lane REPLACED the edges table, so the marker is
         -- the new column, not the table.
         AND EXISTS (
             SELECT 1
               FROM information_schema.columns
              WHERE table_schema = 'proxima_core'
                AND table_name = 'edges'
                AND column_name = 'source_id'
         )
         AND EXISTS (
             SELECT 1
               FROM information_schema.columns
              WHERE table_schema = 'proxima_core'
                AND table_name = 'memories'
                AND column_name = 'authoring_perspective_id'
         )
         AND EXISTS (
             SELECT 1
               FROM information_schema.columns
              WHERE table_schema = 'proxima_core'
                AND table_name = 'goals'
                AND column_name = 'assignment_perspective_id'
         )
         AND to_regclass('proxima_core.interpretation_v1') IS NOT NULL
         AND EXISTS (
             SELECT 1
               FROM information_schema.columns
              WHERE table_schema = 'proxima_core'
                AND table_name = 'memories'
                AND column_name = 'kind'
                AND is_nullable = 'NO'
         )
         -- v0.0.8 delegated-authority lane. The runtime always constructs the
         -- PG store when authenticated hosting is enabled, compliance audit
         -- always binds its count column, and the trigger is the storage-side
         -- defense against widening an issued grant.
         AND to_regtype('proxima_core.access_ceiling') IS NOT NULL
         AND to_regclass('proxima_core.delegated_authority_grants') IS NOT NULL
         AND EXISTS (
             SELECT 1
               FROM information_schema.columns
              WHERE table_schema = 'proxima_core'
                AND table_name = 'compliance_audit_log'
                AND column_name = 'delegated_authority_grants_count'
         )
         AND EXISTS (
             SELECT 1
               FROM pg_trigger t
               JOIN pg_class c ON c.oid = t.tgrelid
               JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE n.nspname = 'proxima_core'
                AND c.relname = 'delegated_authority_grants'
                AND t.tgname = 'delegated_authority_grants_revoke_only'
         )
         -- Flavor lane skew: when the code flavor's tables exist, its
         -- language migration must have run too — the search builder emits
         -- s.lexical_language for the chunk projection (and reads its
         -- stored search_tsv), so a core-migrated/flavor-stale database
         -- would pass core markers at boot and then fail EVERY search with
         -- an undefined column at runtime.
         AND (
             to_regclass('proxima_code.code_chunk_v1') IS NULL
             OR (
                 EXISTS (
                     SELECT 1
                       FROM information_schema.columns
                      WHERE table_schema = 'proxima_code'
                        AND table_name = 'code_chunk_v1'
                        AND column_name = 'lexical_language'
                 )
                 AND EXISTS (
                     SELECT 1
                       FROM information_schema.columns
                      WHERE table_schema = 'proxima_code'
                        AND table_name = 'code_chunk_v1'
                        AND column_name = 'search_tsv'
                 )
             )
         )",
    )
    .fetch_one(pool)
    .await
    .map_err(internal)?;

    if !ready {
        return Err(StorageError::Internal(
            "database is missing schema markers for this release lane (v0.0.6: embedding_jobs.next_attempt_at, memories append-only trigger; v0.0.7 (0011_v007.sql): memories.search_tsv, embeddings.chunk_index, proxima_core.lexical_tsv, proxima_core.lexical_config, memories.lexical_language, proxima_core.lexical_languages, edges.source_id, memories.authoring_perspective_id, goals.assignment_perspective_id, proxima_core.interpretation_v1; v0.0.8 (0016_v008.sql): proxima_core.access_ceiling, proxima_core.delegated_authority_grants, delegated_authority_grants_revoke_only, compliance_audit_log.delegated_authority_grants_count; v0.0.8 (0020_memory_kind_fact.sql): memories.kind NOT NULL Fact; code flavor, when present: code_chunk_v1.search_tsv and code_chunk_v1.lexical_language via flavor migration 20260801000020_v007_baseline.sql); apply migrations before boot (see MIGRATING.md)".into(),
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

/// Refuse at boot what would otherwise fail on every semantic search.
///
/// The probe runs `set_hnsw_search_sql(tuning)` — the production builder over
/// this deployment's own tuning — rather than a restated literal. A restated
/// one validates whichever mode was hard-coded, so a deployment running
/// `PROXIMA_PG_HNSW_ITERATIVE_SCAN=strict_order` would boot on a preflight
/// that proved `relaxed_order` and never proved the mode it actually sends.
async fn ensure_pgvector_runtime_compatible(
    pool: &PgPool,
    tuning: &PgTuning,
) -> Result<(), StorageError> {
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
    // SQL-POLICY: fixed-fragment — the settings statement interpolates
    // nothing but this deployment's own tuning integers and enum spellings,
    // exactly as the semantic branch's own call site does.
    sqlx::raw_sql(sqlx::AssertSqlSafe(set_hnsw_search_sql(tuning)))
        .execute(tx.as_mut())
        .await
        .map_err(|err| {
            StorageError::Unavailable(format!("pgvector HNSW search settings unavailable: {err}"))
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
    tuning: PgTuning,
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

/// Parse an integer configuration variable, falling back to `default` when
/// unset. `0` is a legal value (it disables the corresponding bound wherever
/// one is expressed that way).
///
/// A malformed value is an error, not a silent fallback. Every other
/// configuration reader in the workspace already answers this way —
/// `RuntimeBuilder::apply_lookup` and `proxima-blob-s3`'s `parse_u64_env`
/// both return a `Config` error — and a typo that silently reverts pool
/// tuning to the default is the kind of thing an operator discovers from a
/// latency graph weeks later rather than from the boot that caused it.
///
/// Generic over the integer type because the `u32` and `u64` readers were
/// otherwise the same function written twice, down to the error text; the
/// only difference was which `FromStr` ran. `crate::tuning` reads its own
/// knobs through this one.
///
/// # Errors
///
/// Returns `StorageError::Unavailable` when the value is set but does not
/// parse as `T`.
fn env_int_or<T: std::str::FromStr>(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &str,
    default: T,
) -> Result<T, StorageError> {
    let Some(value) = env_value(lookup, key) else {
        return Ok(default);
    };
    value
        .parse()
        .map_err(|_| StorageError::Unavailable(format!("invalid integer {key}={value}")))
}

/// Parse a `u32` pool-tuning env var that must be at least 1, falling back to
/// `default` when unset.
///
/// Unlike [`env_int_or`], `0` is rejected rather than defaulted: a pool of
/// zero connections is never what anyone meant, so it is an operator error
/// worth naming rather than a value to quietly round up.
///
/// # Errors
///
/// Returns `StorageError::Unavailable` when the value is set but not a `u32`,
/// or is `0`.
fn env_u32_min1(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &str,
    default: u32,
) -> Result<u32, StorageError> {
    let Some(value) = env_value(lookup, key) else {
        return Ok(default);
    };
    match value.parse::<u32>() {
        Ok(0) => Err(StorageError::Unavailable(format!(
            "{key}=0 is not a usable pool size; a connection pool needs at least one connection"
        ))),
        Ok(parsed) => Ok(parsed),
        Err(_) => Err(StorageError::Unavailable(format!(
            "invalid integer {key}={value}"
        ))),
    }
}

impl PgStorage {
    /// Connect using `url`, build a tuned pool, and verify
    /// connectivity by acquiring one connection.
    ///
    /// Query and write tuning is read from the environment
    /// ([`PgTuning::from_env`]); [`Self::connect_with_tuning`] takes it as
    /// an argument instead.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Unavailable` on connection or
    /// query failure, or on a malformed tuning variable.
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        Self::connect_with_tuning(url, PgTuning::from_env()?).await
    }

    /// Connect using `url` with tuning supplied by the caller.
    ///
    /// A measurement harness sets the fields it is ablating directly, so an
    /// arm never depends on mutating the process environment. The pool and
    /// timeout settings are still read from the environment.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Unavailable` on connection or
    /// query failure.
    pub async fn connect_with_tuning(url: &str, tuning: PgTuning) -> Result<Self, StorageError> {
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
        let env = proxima_core::process_env;
        let statement_timeout_ms: u64 =
            env_int_or(&env, "PROXIMA_PG_STATEMENT_TIMEOUT_MS", 300_000)?;
        if statement_timeout_ms > 0 {
            opts = opts.options([("statement_timeout", statement_timeout_ms.to_string())]);
        }
        let pool = PgPoolOptions::new()
            .max_connections(env_u32_min1(&env, "PROXIMA_PG_MAX_CONNECTIONS", 10)?)
            .acquire_timeout(Duration::from_secs(env_int_or(
                &env,
                "PROXIMA_PG_ACQUIRE_TIMEOUT_SECS",
                5,
            )?))
            .idle_timeout(Duration::from_secs(env_int_or(
                &env,
                "PROXIMA_PG_IDLE_TIMEOUT_SECS",
                600,
            )?))
            .max_lifetime(Duration::from_secs(env_int_or(
                &env,
                "PROXIMA_PG_MAX_LIFETIME_SECS",
                1_800,
            )?))
            .connect_with(opts)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        // Validate connectivity with a trivial query.
        sqlx::query("SELECT 1")
            .execute(&pool)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        Ok(Self {
            pool,
            sidecars: core_pg_sidecars(),
            tuning,
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
    /// `ignore_missing = true` forgives ledger rows the embedded set no
    /// longer accounts for: flavor rows still in the shared table on a
    /// database migrated before the v0.0.7 per-flavor ledger split, and
    /// orphaned draft rows left behind when a dev-cycle lane is squashed
    /// under a fresh version number (docs/how-to/migrations.md). The core
    /// version-set is still checksum-validated — both by `SQLx` and, more
    /// legibly, by [`ensure_core_ledger_compatible`] first.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Internal` on any sqlx
    /// migration failure (broken file, conflict with the
    /// recorded checksum, etc.).
    pub async fn run_migrations(&self) -> Result<(), StorageError> {
        ensure_core_ledger_compatible(&self.pool).await?;
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
        ensure_pgvector_runtime_compatible(&self.pool, &self.tuning).await?;
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
    fn core_migrator_contains_the_v008_baseline() {
        let versions: Vec<i64> = super::core_migrator()
            .iter()
            .map(|migration| migration.version)
            .collect();
        assert!(versions.contains(&1), "core migrator must embed 0001_v008.sql");
        assert!(versions.contains(&2), "core migrator must embed 0002_blob_closed.sql");
        assert!(versions.contains(&3), "core migrator must embed 0003_goal.sql");
    }

    #[test]
    fn core_migrator_v008_has_no_legacy_alter_versions() {
        let versions: Vec<i64> = super::core_migrator()
            .iter()
            .map(|migration| migration.version)
            .collect();
        for dead in [8, 9, 10, 11, 16, 17, 18, 19, 20] {
            assert!(!versions.contains(&dead), "legacy version {dead} must be gone");
        }
    }

    #[test]
    fn boot_floor_is_the_newest_embedded_core_version() {
        // The floor is derived, so it can never lag the migrator — this pins
        // the two remaining assumptions: the namespace ceiling actually
        // separates core files from flavor-style date versions, and the floor
        // moves when a migration is added.
        let floor = super::min_core_migration_version();
        assert!(
            (1..=super::CORE_MIGRATION_VERSION_CEILING).contains(&floor),
            "derived boot floor {floor} must be a core-namespace version"
        );
        assert!(
            super::core_migrator()
                .iter()
                .all(|m| m.version <= super::CORE_MIGRATION_VERSION_CEILING),
            "core migrations must stay below the flavor version namespace"
        );
    }

    #[test]
    fn core_migrator_contains_v006_migrations() {
        let versions: Vec<i64> = super::core_migrator()
            .iter()
            .map(|migration| migration.version)
            .collect();
        assert!(versions.contains(&1));
        assert!(versions.contains(&2));
    }

    /// An injected lookup, so every branch is reachable. These helpers used
    /// to read the process environment directly, which left the tests below
    /// able to assert only the unset case — they named keys nothing sets, and
    /// the malformed branch they were nominally covering was never executed.
    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn pool_env_helpers_default_when_unset() {
        assert_eq!(
            super::env_u32_min1(&env(&[]), "PROXIMA_PG_MAX_CONNECTIONS", 10).unwrap(),
            10
        );
        assert_eq!(
            super::env_int_or::<u64>(&env(&[]), "PROXIMA_PG_STATEMENT_TIMEOUT_MS", 300_000)
                .unwrap(),
            300_000
        );
    }

    /// Empty and whitespace-only are "unset", per `proxima_core::env_value` —
    /// so `PROXIMA_PG_MAX_CONNECTIONS=` is not a parse error naming a value
    /// the operator never typed.
    #[test]
    fn pool_env_helpers_treat_blank_as_unset() {
        assert_eq!(
            super::env_u32_min1(
                &env(&[("PROXIMA_PG_MAX_CONNECTIONS", "")]),
                "PROXIMA_PG_MAX_CONNECTIONS",
                10
            )
            .unwrap(),
            10
        );
        assert_eq!(
            super::env_int_or::<u64>(
                &env(&[("PROXIMA_PG_IDLE_TIMEOUT_SECS", "  \t ")]),
                "PROXIMA_PG_IDLE_TIMEOUT_SECS",
                600
            )
            .unwrap(),
            600
        );
    }

    #[test]
    fn pool_env_helpers_parse_a_set_value_and_trim_it() {
        assert_eq!(
            super::env_u32_min1(
                &env(&[("PROXIMA_PG_MAX_CONNECTIONS", "25")]),
                "PROXIMA_PG_MAX_CONNECTIONS",
                10
            )
            .unwrap(),
            25
        );
        // A trailing newline survives a here-doc or a mounted secret.
        assert_eq!(
            super::env_int_or::<u64>(
                &env(&[("PROXIMA_PG_IDLE_TIMEOUT_SECS", "900\n")]),
                "PROXIMA_PG_IDLE_TIMEOUT_SECS",
                600
            )
            .unwrap(),
            900
        );
    }

    /// The behaviour change: a typo stops the boot instead of silently
    /// reverting pool tuning to the default.
    #[test]
    fn pool_env_helpers_reject_a_malformed_value() {
        let err = super::env_u32_min1(
            &env(&[("PROXIMA_PG_MAX_CONNECTIONS", "twenty")]),
            "PROXIMA_PG_MAX_CONNECTIONS",
            10,
        )
        .expect_err("a malformed pool size must not silently become the default");
        assert!(
            err.to_string()
                .contains("invalid integer PROXIMA_PG_MAX_CONNECTIONS=twenty"),
            "error must name the variable and the value: {err}"
        );
        assert!(
            super::env_int_or::<u64>(
                &env(&[("PROXIMA_PG_MAX_LIFETIME_SECS", "-1")]),
                "PROXIMA_PG_MAX_LIFETIME_SECS",
                1_800
            )
            .is_err()
        );
    }

    /// `0` splits the two helpers: it disables the bound for the `u64` ones
    /// and is meaningless for a pool size.
    #[test]
    fn zero_disables_a_u64_bound_but_is_never_a_pool_size() {
        assert_eq!(
            super::env_int_or::<u64>(
                &env(&[("PROXIMA_PG_STATEMENT_TIMEOUT_MS", "0")]),
                "PROXIMA_PG_STATEMENT_TIMEOUT_MS",
                300_000
            )
            .unwrap(),
            0,
            "statement_timeout=0 is the documented way to disable the timeout"
        );
        let err = super::env_u32_min1(
            &env(&[("PROXIMA_PG_MAX_CONNECTIONS", "0")]),
            "PROXIMA_PG_MAX_CONNECTIONS",
            10,
        )
        .expect_err("a zero pool size must be named, not rounded up to the default");
        assert!(err.to_string().contains("at least one connection"), "{err}");
    }
}
