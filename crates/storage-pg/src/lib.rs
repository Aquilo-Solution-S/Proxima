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

use proxima_core::StorageError;
use proxima_core::env_value;
use proxima_core::storage_ports::StoragePorts;
use sqlx::PgPool;
use std::sync::Arc;
pub use verbs::fact_embeddings::{
    EmbeddingInlineDrainOutcome, EmbeddingReconcileOptions, EmbeddingReconcileOutcome,
    EmbeddingReconcileScope,
};
pub use verbs::maintenance::{
    ChangeEventPruneOptions, ChangeEventPruneOutcome, ColdPurgeRetryOptions, ColdPurgeRetryOutcome,
    PruneOwnerOutcome,
};

use crate::error::internal;
use crate::pgvector::set_hnsw_search_sql;

#[doc(hidden)]
pub mod access;
mod change_event;
mod delegated_authority;
mod error;
#[doc(hidden)]
pub use error::map_err;
pub use error::{MAX_TRANSACTION_ATTEMPTS, is_transient_conflict};
pub mod integrity;
mod pg_ident;
mod pgvector;
mod pool_config;
mod ports;
pub mod projection;
pub mod sidecars;
pub mod query {
    #[cfg(any(test, feature = "test-fixtures", debug_assertions))]
    pub use crate::verbs::query::file_revision_heads_sql_for_tests;
    pub use crate::verbs::query::{
        ActiveGoalTargetRow, ChunkSeriesHead, CodeChunkVectorCandidate, CodeChunkVectorFilters,
        FileRevisionHeadRow, MAX_SNAPSHOT_EDGES, active_goals_for_memory_targets,
        nearest_code_chunk_candidates, owned_chunk_series_heads, owned_file_revision_heads,
        owned_present_chunk_indexes, owned_present_file_revision_heads_except,
        readable_chunk_head_ts_for_file, readable_file_revision_head_ts,
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
pub use pool_config::PgPoolConfig;
pub use sidecars::{
    PgSidecarKey, PgSidecarRegistry, PgSidecarRegistryFrozen, core_pg_sidecars,
    register_core_pg_sidecars,
};
pub use tuning::{HnswIterativeScan, PgTuning};

/// Namespace boundary between core and flavor migration versions.
///
/// Core migrations use small sequential integer versions (`0001_v008.sql`);
/// flavor migrations use date-shaped versions
/// (`20260801000020_…`). Every ledger row at or below this ceiling belongs to
/// the core lane and must be accounted for by the embedded core migrator —
/// that invariant is what lets the preflight below detect draft and retired
/// versions *generically*, with no enumerated version lists (see
/// docs/how-to/migrations.md).
pub const CORE_MIGRATION_VERSION_CEILING: i64 = 9999;

/// Embedded core migration set under `crates/storage-pg/migrations/`.
///
/// `ignore_missing = true` forgives ledger rows the embedded set does not
/// account for: flavor rows in the shared `public._sqlx_migrations` table,
/// and the orphaned draft rows the squash workflow
/// (docs/how-to/migrations.md) leaves behind. Both are forgiven rather than
/// enumerated.
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
///   squashed under a fresh number. Applying
///   the squashed file over that schema would re-run its DDL, so this fails
///   first with the remedy (stamp or reset) instead of a raw SQL error.
/// - **Every recorded checksum matches the embedded file.** A mismatch means
///   the file's bytes changed after this database applied it. `SQLx` itself
///   rejects this state (`VersionMismatch`), but only after the point where
///   its error can say nothing about why or what to do.
///
/// # Errors
///
/// Returns [`StorageError::SchemaResetRequired`] when schema objects exist
/// without a matching version-1 ledger, or version 1's checksum does not
/// match `0001_v008.sql`. Remedy: reset. Returns
/// [`StorageError::Internal`], naming the stamp-or-reset remedy, for draft or
/// retired versions and post-baseline checksum drift, and for catalog query
/// failures.
pub async fn ensure_core_ledger_compatible(pool: &PgPool) -> Result<(), StorageError> {
    let migration_table_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(pool)
            .await
            .map_err(internal)?;

    // `proxima\_%` rather than a list. This probe runs before any registry
    // exists, so it cannot ask which flavors are linked, and naming one from
    // inside the kernel would make a second flavor's leftover schema
    // invisible to the reset check. The prefix is the flavor schema
    // convention (`proxima_core`, `proxima_code`, ...), so the pattern makes
    // the same claim without the kernel knowing any flavor's name.
    let proxima_schema_objects: Vec<String> = sqlx::query_scalar(
        "SELECT table_schema || '.' || table_name
           FROM information_schema.tables
          WHERE table_schema LIKE 'proxima\\_%'
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
                "pre-existing Proxima schema objects without the core baseline ledger row: {}",
                proxima_schema_objects.join(", ")
            ));
        }
        if baseline_checksum_drift {
            details.push("version 1 checksum differs from 0001_v008.sql".to_string());
        }
        if !unknown_versions.is_empty() {
            details.push(format!("old migration versions: {unknown_versions:?}"));
        }
        return Err(StorageError::SchemaResetRequired {
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
         `PROXIMA_RESET_CONFIRM=reset-my-dev-db cargo run -p proxima-dev-migrate -- --reset --database-url <URL>`, \
         then re-register and re-index. See docs/how-to/migrations.md",
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
                "database core migrations at version {}; version {min_required}+ required — run the pending core migrations under crates/storage-pg/migrations/ (they are additive over the v008 baseline, so an existing database upgrades in place; a fresh one starts at 0001_v008.sql). In a split-role deploy that is the DDL-role init step, not this process (see docs/how-to/migrations.md and docs/15-deployment.md)",
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
/// current lane is absent or has the wrong type, nullability, enum order, or
/// processing-claim invariant.
#[allow(clippy::too_many_lines)]
pub async fn ensure_core_schema_markers(pool: &PgPool) -> Result<(), StorageError> {
    ensure_lexical_language_stamps(pool).await?;
    let marker_error: Option<String> = sqlx::query_scalar(
        r"SELECT CASE
         WHEN to_regclass('proxima_core.memory') IS NULL
           THEN 'missing relation proxima_core.memory'
         WHEN to_regclass('proxima_core.memory_head') IS NULL
           THEN 'missing relation proxima_core.memory_head'
         WHEN to_regclass('proxima_core.ingest_keys') IS NULL
           THEN 'missing relation proxima_core.ingest_keys'
         WHEN to_regclass('proxima_core.announce') IS NULL
           THEN 'missing relation proxima_core.announce'
         WHEN to_regclass('proxima_core.goal') IS NULL
           THEN 'missing relation proxima_core.goal'
         WHEN to_regclass('proxima_core.wake_config') IS NULL
           THEN 'missing relation proxima_core.wake_config'
         WHEN to_regclass('proxima_core.embeddings') IS NULL
           THEN 'missing relation proxima_core.embeddings'
         WHEN to_regclass('proxima_core.embedding_jobs') IS NULL
           THEN 'missing relation proxima_core.embedding_jobs'
         WHEN to_regclass('proxima_core.agent_note_v1') IS NULL
           THEN 'missing relation proxima_core.agent_note_v1'
         WHEN to_regclass('proxima_core.group_memberships') IS NULL
           THEN 'missing relation proxima_core.group_memberships'
         WHEN to_regclass('proxima_core.cold_purge_pending') IS NULL
           THEN 'missing relation proxima_core.cold_purge_pending'
         WHEN NOT EXISTS (
                  SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'proxima_core' AND table_name = 'cold_purge_pending'
                     AND column_name = 'object_key' AND data_type = 'text'
                     AND is_nullable = 'NO'
                )
           THEN 'cold_purge_pending.object_key must be text NOT NULL'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.table_constraints tc
                    JOIN information_schema.key_column_usage kcu
                      ON kcu.constraint_catalog = tc.constraint_catalog
                     AND kcu.constraint_schema = tc.constraint_schema
                     AND kcu.constraint_name = tc.constraint_name
                   WHERE tc.table_schema = 'proxima_core'
                     AND tc.table_name = 'cold_purge_pending'
                     AND tc.constraint_type = 'PRIMARY KEY'
                     AND kcu.column_name = 'object_key'
                     AND 1 = (
                         SELECT count(*)
                           FROM information_schema.key_column_usage only_kcu
                          WHERE only_kcu.constraint_catalog = tc.constraint_catalog
                            AND only_kcu.constraint_schema = tc.constraint_schema
                            AND only_kcu.constraint_name = tc.constraint_name
                     )
                )
           THEN 'cold_purge_pending.object_key must be the primary key'
         WHEN NOT EXISTS (
                  SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'proxima_core' AND table_name = 'cold_purge_pending'
                     AND column_name = 'owner_id' AND data_type = 'uuid'
                     AND is_nullable = 'NO'
                )
           THEN 'cold_purge_pending.owner_id must be uuid NOT NULL'
         WHEN NOT EXISTS (
                  SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'proxima_core' AND table_name = 'cold_purge_pending'
                     AND column_name = 'enqueued_at'
                     AND data_type = 'timestamp with time zone' AND is_nullable = 'NO'
                )
           THEN 'cold_purge_pending.enqueued_at must be timestamptz NOT NULL'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'cooled'
                     AND column_name = 'blob_id'
                     AND data_type = 'uuid'
                     AND is_nullable = 'YES'
                )
           THEN 'cooled.blob_id must be nullable uuid'
         WHEN to_regclass('proxima_core.lexical_languages') IS NULL
           THEN 'missing relation proxima_core.lexical_languages'
         WHEN to_regclass('proxima_core.lexical_default') IS NULL
           THEN 'missing relation proxima_core.lexical_default'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'lexical_default'
                     AND column_name = 'singleton'
                     AND data_type = 'boolean'
                     AND is_nullable = 'NO'
                )
           THEN 'lexical_default.singleton must be boolean NOT NULL'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.table_constraints tc
                    JOIN information_schema.key_column_usage kcu
                      ON kcu.constraint_catalog = tc.constraint_catalog
                     AND kcu.constraint_schema = tc.constraint_schema
                     AND kcu.constraint_name = tc.constraint_name
                   WHERE tc.table_schema = 'proxima_core'
                     AND tc.table_name = 'lexical_default'
                     AND tc.constraint_type = 'PRIMARY KEY'
                     AND kcu.column_name = 'singleton'
                     AND 1 = (
                         SELECT count(*)
                           FROM information_schema.key_column_usage only_kcu
                          WHERE only_kcu.constraint_catalog = tc.constraint_catalog
                            AND only_kcu.constraint_schema = tc.constraint_schema
                            AND only_kcu.constraint_name = tc.constraint_name
                     )
                )
           THEN 'lexical_default.singleton must be the sole primary-key column'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_constraint c
                    JOIN pg_class r ON r.oid = c.conrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'lexical_default'
                     AND c.contype = 'c'
                     AND c.convalidated
                     AND pg_get_expr(c.conbin, c.conrelid, true) = 'singleton'
                )
           THEN 'lexical_default.singleton CHECK (singleton) is missing or incorrect'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'lexical_default'
                     AND column_name = 'config'
                     AND udt_schema = 'pg_catalog'
                     AND udt_name = 'regconfig'
                     AND is_nullable = 'NO'
                )
           THEN 'lexical_default.config must be regconfig NOT NULL'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.table_constraints tc
                    JOIN information_schema.key_column_usage kcu
                      ON kcu.constraint_catalog = tc.constraint_catalog
                     AND kcu.constraint_schema = tc.constraint_schema
                     AND kcu.constraint_name = tc.constraint_name
                    JOIN information_schema.constraint_column_usage ccu
                      ON ccu.constraint_catalog = tc.constraint_catalog
                     AND ccu.constraint_schema = tc.constraint_schema
                     AND ccu.constraint_name = tc.constraint_name
                   WHERE tc.table_schema = 'proxima_core'
                     AND tc.table_name = 'lexical_default'
                     AND tc.constraint_type = 'FOREIGN KEY'
                     AND kcu.column_name = 'config'
                     AND ccu.table_schema = 'proxima_core'
                     AND ccu.table_name = 'lexical_languages'
                     AND ccu.column_name = 'config'
                     AND 1 = (
                         SELECT count(*)
                           FROM information_schema.key_column_usage only_kcu
                          WHERE only_kcu.constraint_catalog = tc.constraint_catalog
                            AND only_kcu.constraint_schema = tc.constraint_schema
                            AND only_kcu.constraint_name = tc.constraint_name
                     )
                )
           THEN 'lexical_default.config must reference lexical_languages(config)'
         WHEN 1 <> (
                  SELECT count(*)
                    FROM proxima_core.lexical_default
                   WHERE singleton
                )
           THEN 'lexical_default must contain exactly one singleton=true row'
         WHEN to_regprocedure('proxima_core.lexical_tsv(text)') IS NULL
           THEN 'missing function proxima_core.lexical_tsv(text)'
         WHEN to_regprocedure('proxima_core.lexical_config()') IS NULL
           THEN 'missing function proxima_core.lexical_config()'
         WHEN to_regprocedure('proxima_core.lexical_language_forget(regconfig)') IS NULL
           THEN 'missing function proxima_core.lexical_language_forget(regconfig)'
         WHEN COALESCE((
                  SELECT array_agg(e.enumlabel::text ORDER BY e.enumsortorder)
                    FROM pg_enum e
                    JOIN pg_type t ON t.oid = e.enumtypid
                    JOIN pg_namespace n ON n.oid = t.typnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND t.typname = 'embedding_job_status'
                ), ARRAY[]::text[]) <> ARRAY['pending', 'processing', 'failed', 'failed_permanent']
           THEN 'embedding_job_status labels/order must be pending, processing, failed, failed_permanent'
         WHEN COALESCE((
                  SELECT array_agg(e.enumlabel::text ORDER BY e.enumsortorder)
                    FROM pg_enum e
                    JOIN pg_type t ON t.oid = e.enumtypid
                    JOIN pg_namespace n ON n.oid = t.typnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND t.typname = 'announce_op'
                ), ARRAY[]::text[]) <> ARRAY['append', 'forget', 'erase', 'transfer']
           THEN 'announce_op labels/order must be append, forget, erase, transfer'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'embedding_jobs'
                     AND column_name = 'status'
                     AND udt_schema = 'proxima_core'
                     AND udt_name = 'embedding_job_status'
                     AND is_nullable = 'NO'
                )
           THEN 'embedding_jobs.status must be proxima_core.embedding_job_status NOT NULL'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'embedding_jobs'
                     AND column_name = 'claimed_at'
                     AND data_type = 'timestamp with time zone'
                     AND is_nullable = 'YES'
                )
           THEN 'embedding_jobs.claimed_at must be nullable timestamptz'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'embedding_jobs'
                     AND column_name = 'last_error'
                     AND data_type = 'text'
                     AND is_nullable = 'YES'
                )
           THEN 'embedding_jobs.last_error must be nullable text'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'embedding_jobs'
                     AND column_name = 'claim_token'
                     AND data_type = 'uuid'
                     AND is_nullable = 'YES'
                )
           THEN 'embedding_jobs.claim_token must be nullable uuid'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_constraint c
                    JOIN pg_class r ON r.oid = c.conrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'embedding_jobs'
                     AND c.conname = 'embedding_job_processing_claim_chk'
                     AND c.convalidated
                     AND pg_get_constraintdef(c.oid, true) =
                         'CHECK ((status = ''processing''::proxima_core.embedding_job_status) = (claimed_at IS NOT NULL AND claim_token IS NOT NULL))'
                )
           THEN 'embedding_jobs.processing claim check is missing or incorrect'
         ELSE NULL
       END",
    )
    .fetch_one(pool)
    .await
    .map_err(internal)?;

    if let Some(marker_error) = marker_error {
        return Err(StorageError::Internal(format!(
            "database is missing or has an incorrect v0.0.8 schema marker: {marker_error}; apply migrations before boot"
        )));
    }
    Ok(())
}

/// Every `lexical_language` column in `proxima_core` is FK-stamped against
/// `lexical_languages(config)`, and flavor #0 declared each one.
///
/// `lexical_language_forget` is safe only because that FK exists on every
/// stamped column: it deletes from `lexical_languages` and lets referential
/// integrity refuse while any row still holds the configuration. A table
/// added to the migration with a stamped column and no FK makes the forget
/// silently incomplete rather than loud.
///
/// The expected set is flavor #0's declared `lexical_language_column`
/// surfaces, and the check runs in both directions: a stamped column nobody
/// declared fails as loudly as a declared stamp with no FK.
async fn ensure_lexical_language_stamps(pool: &PgPool) -> Result<(), StorageError> {
    let mut declared: Vec<(String, String)> = proxima_core::FLAVOR_0
        .all_surfaces()
        .filter_map(|surface| {
            let column = surface.lexical_language_column?;
            let table = surface.table.strip_prefix("proxima_core.")?;
            Some((table.to_owned(), column.to_owned()))
        })
        .collect();
    declared.sort();
    declared.dedup();

    let mut stamped: Vec<(String, String)> = sqlx::query_as(
        "SELECT tc.table_name::text, kcu.column_name::text
           FROM information_schema.table_constraints tc
           JOIN information_schema.key_column_usage kcu
             ON kcu.constraint_catalog = tc.constraint_catalog
            AND kcu.constraint_schema = tc.constraint_schema
            AND kcu.constraint_name = tc.constraint_name
           JOIN information_schema.constraint_column_usage ccu
             ON ccu.constraint_catalog = tc.constraint_catalog
            AND ccu.constraint_schema = tc.constraint_schema
            AND ccu.constraint_name = tc.constraint_name
          WHERE tc.table_schema = 'proxima_core'
            AND tc.constraint_type = 'FOREIGN KEY'
            AND ccu.table_schema = 'proxima_core'
            AND ccu.table_name = 'lexical_languages'
            AND ccu.column_name = 'config'",
    )
    .fetch_all(pool)
    .await
    .map_err(internal)?;
    // `lexical_default.config` references the same table but is the active
    // configuration, not a stamp on a row of content.
    stamped.retain(|(table, _)| table != "lexical_default");
    stamped.sort();
    stamped.dedup();

    if stamped != declared {
        let missing = declared
            .iter()
            .filter(|entry| !stamped.contains(entry))
            .map(|(table, column)| format!("proxima_core.{table}.{column}"))
            .collect::<Vec<_>>();
        let undeclared = stamped
            .iter()
            .filter(|entry| !declared.contains(entry))
            .map(|(table, column)| format!("proxima_core.{table}.{column}"))
            .collect::<Vec<_>>();
        return Err(StorageError::Internal(format!(
            "every stamped lexical_language column must reference lexical_languages(config) \
             and be declared by flavor #0: missing FK {missing:?}, undeclared stamp {undeclared:?}"
        )));
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

/// Flavor #0's surfaces resolved THROUGH A REGISTRY, which is the only way
/// they resolve correctly.
///
/// `OwnerSurfaces::from_surfaces` is a test seam, and says so on itself: it
/// classifies every surface against an EMPTY bespoke list, because a surface
/// handed over loose has no contract to have exempted it. Flavor #0's
/// surfaces are not loose — they belong to a contract with a bespoke list —
/// so classifying them that way gets the legs wrong, silently. `memory` and
/// `cooled` come back `Keyed` instead of `Bespoke`, and `blob` and `content`
/// come back `Unreachable` instead of `Deduped`; the freeze-time validation
/// that a bespoke leg has a home never runs against them.
///
/// `for_registry` over a registry holding flavor #0 alone is the same shape
/// the code flavor's `flavor_surfaces()` uses, and it is what `with_flavors`
/// does once a host widens the set.
fn flavor_0_surfaces() -> proxima_core::owner_inverse::OwnerSurfaces {
    // A fresh registry holds flavor #0 and nothing else, and flavor #0 is a
    // `const` contract this crate compiles against. There is no input here
    // for a freeze to reject.
    let registry = proxima_core::FlavorRegistry::new()
        .try_freeze()
        .expect("flavor #0 alone freezes: it is the only contract in a fresh registry");
    proxima_core::owner_inverse::OwnerSurfaces::for_registry(&registry)
}

#[derive(Clone)]
pub struct PgStorage {
    pool: PgPool,
    sidecars: PgSidecarRegistryFrozen,
    /// The declared surfaces, resolved into legs once.
    ///
    /// Defaults to flavor #0's, exactly as `sidecars` defaults to
    /// `core_pg_sidecars()`: a storage built without `with_flavors` still
    /// forgets the kernel's derived rows. `with_flavors` widens it to every
    /// registered flavor, and a flavor that declares its own
    /// `DeleteWithMemory` surface is reached only after that call — the
    /// same coverage contract `with_sidecars` states.
    ///
    /// Resolved by [`flavor_0_surfaces`], through a registry. Building it
    /// from a loose surface list is a different answer, not a cheaper one.
    surfaces: proxima_core::owner_inverse::OwnerSurfaces,
    search_projections: Vec<proxima_core::verbs::schema::MemorySearchProjection>,
    embed_units: Vec<proxima_core::verbs::schema::MemoryEmbedUnit>,
    tuning: PgTuning,
    embedding_runtime_policy: proxima_core::EmbeddingRuntimePolicy,
    cold: Arc<dyn proxima_core::ColdObjectStore>,
}

impl std::fmt::Debug for PgStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgStorage").finish_non_exhaustive()
    }
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

/// Advisory-lock key serializing storage-maintenance passes across
/// processes. ASCII `proxretn` as a big-endian i64 — arbitrary but stable,
/// distinct from [`EMBEDDING_MAINTENANCE_LOCK_KEY`] so the two maintenance
/// families may run concurrently but never overlap themselves.
///
/// The bytes are a key, not a label: they need not describe the pass, and
/// rotating them would let a process on the old value and one on the new
/// run the pass at the same time.
const STORAGE_MAINTENANCE_LOCK_KEY: i64 = i64::from_be_bytes(*b"proxretn");

/// Guard for the global storage-maintenance advisory lock. Same
/// detached-connection design as [`EmbeddingMaintenanceLock`]: dropping the
/// guard closes the connection, and Postgres releases the session lock.
pub struct StorageMaintenanceLock {
    _conn: sqlx::postgres::PgConnection,
}

impl std::fmt::Debug for StorageMaintenanceLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageMaintenanceLock")
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
/// Generic over the integer type so the `u32` and `u64` readers are one
/// function rather than two identical ones differing only in which `FromStr`
/// runs. `crate::tuning` reads its own knobs through this one.
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

impl PgStorage {
    /// Connect using `url`, build a tuned pool, and verify
    /// connectivity by acquiring one connection.
    ///
    /// Pool and query tuning are read from the environment. Use
    /// [`Self::connect_with_config`] when configuration was already resolved.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Unavailable` on connection or
    /// query failure, or on a malformed tuning variable.
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        Self::connect_with_config(url, PgPoolConfig::from_env()?, PgTuning::from_env()?).await
    }

    /// Connect using `url` with tuning supplied by the caller.
    ///
    /// A measurement harness sets the fields it is ablating directly, so an
    /// arm never depends on mutating the process environment. The pool and
    /// timeout settings are still read from the process environment. Runtime
    /// hosts with an injected lookup should use [`Self::connect_with_config`].
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Unavailable` on connection or
    /// query failure.
    pub async fn connect_with_tuning(url: &str, tuning: PgTuning) -> Result<Self, StorageError> {
        Self::connect_with_config(url, PgPoolConfig::from_env()?, tuning).await
    }

    /// Connect with fully resolved pool and query policy.
    ///
    /// This is the canonical host path: it never consults process environment.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Unavailable` on invalid policy, connection, or
    /// query failure.
    pub async fn connect_with_config(
        url: &str,
        pool_config: PgPoolConfig,
        tuning: PgTuning,
    ) -> Result<Self, StorageError> {
        let pool_config = pool_config.validate()?;
        // A conservative per-statement timeout bounds
        // a runaway query (e.g. a pathological search) so it cannot pin a pool
        // connection indefinitely and starve the gateway. Generous by default
        // (5 min — only a truly stuck statement hits it); tune or disable (0)
        // per deployment. The two operations that can legitimately exceed it —
        // schema migrations and bulk owner erase — explicitly opt out
        // (`run_migrations` runs on a detached timeout-free connection; the erase
        // transaction issues `SET LOCAL statement_timeout = 0`).
        let connect_options = pool_config.connect_options(url)?;
        let pool = pool_config
            .pool_options()
            .connect_with(connect_options)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        sqlx::query("SELECT 1")
            .execute(&pool)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        Ok(Self {
            pool,
            sidecars: core_pg_sidecars(),
            surfaces: flavor_0_surfaces(),
            search_projections: Vec::new(),
            embed_units: Vec::new(),
            tuning,
            embedding_runtime_policy: proxima_core::EmbeddingRuntimePolicy::default(),
            cold: Arc::new(verbs::forget::MemoryColdStore::default()),
        })
    }

    /// Replace the forget/hydrate object store (S3 in the host).
    #[must_use]
    pub fn with_cold(mut self, cold: Arc<dyn proxima_core::ColdObjectStore>) -> Self {
        self.cold = cold;
        self
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

    /// The declared surfaces this storage resolves its legs from.
    ///
    /// Flavor #0's until [`Self::with_flavors`] widens it; see the field.
    #[must_use]
    pub fn surfaces(&self) -> &proxima_core::owner_inverse::OwnerSurfaces {
        &self.surfaces
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

    /// Install everything the frozen flavors tell storage about payload
    /// text: the search projections the read path ranks on, and the embed
    /// units the drain reads.
    ///
    /// One setter rather than two on purpose. Installing the projections
    /// without the embed units is neither a compile error nor a test
    /// failure, because a schema with no embed unit is indistinguishable
    /// from a schema that declares no embedding: the drain drops the job and
    /// a fixture waits forever for a provider call that never comes. Taking
    /// the registry makes the half-configured state unconstructible.
    #[must_use]
    pub fn with_flavors(mut self, registry: &proxima_core::FlavorRegistryFrozen) -> Self {
        self.search_projections = registry.search_projections().to_vec();
        self.embed_units = registry.embed_units().to_vec();
        self.surfaces = proxima_core::owner_inverse::OwnerSurfaces::for_registry(registry);
        self
    }

    /// Apply the host's validated embedding runtime policy to every storage
    /// reclaim and stale-observability path.
    #[must_use]
    pub fn with_embedding_runtime_policy(
        mut self,
        policy: proxima_core::EmbeddingRuntimePolicy,
    ) -> Self {
        self.embedding_runtime_policy = policy;
        self
    }

    #[must_use]
    pub fn storage_ports(self: Arc<Self>) -> StoragePorts {
        StoragePorts::builder()
            .fact_ingest(self.clone())
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
            .citation(self.clone())
            .owner_access_read(self.clone())
            .owner_membership_admin(self.clone())
            .owner_transfer(self.clone())
            .source_batch(self.clone())
            .source_cursor(self.clone())
            .owner_erase(self.clone())
            .registry_projection(self.clone())
            .write_session(self)
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
        verbs::fact_embeddings::reconcile_embeddings(
            &self.pool,
            options,
            self.embedding_runtime_policy.stale_claim_timeout_seconds(),
        )
        .await
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
        verbs::fact_embeddings::drain_embedding_jobs_inline(
            &self.pool,
            client,
            limit,
            &self.embed_units,
            self.embedding_runtime_policy,
        )
        .await
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
        verbs::fact_embeddings::embedding_ann_observability(
            &self.pool,
            self.embedding_runtime_policy.stale_claim_timeout_seconds(),
        )
        .await
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

    /// Try to take the global storage-maintenance advisory lock.
    ///
    /// Same contract as [`Self::try_embedding_maintenance_lock`], on its own
    /// key: `None` means another maintenance pass already holds it and this
    /// run should skip.
    ///
    /// # Errors
    ///
    /// Returns storage errors from acquiring the connection or the lock query.
    pub async fn try_storage_maintenance_lock(
        &self,
    ) -> Result<Option<StorageMaintenanceLock>, StorageError> {
        Ok(self
            .try_maintenance_lock_conn(STORAGE_MAINTENANCE_LOCK_KEY)
            .await?
            .map(|conn| StorageMaintenanceLock { _conn: conn }))
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

    /// Delete change-log rows older than an explicit age horizon. Log
    /// rotation, not a retention promise: the horizon is an operator choice
    /// with no default, and the rows are `proxima_core.announce`, the
    /// substrate's own change log.
    ///
    /// Operator surface for the maintenance CLI; see
    /// [`Self::sweep_orphan_embedding_rows`] for the authority note.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the prune transactions, and
    /// `ConstraintViolation` for a non-positive horizon or batch size.
    pub async fn prune_change_log(
        &self,
        options: ChangeEventPruneOptions,
    ) -> Result<ChangeEventPruneOutcome, StorageError> {
        verbs::maintenance::prune_change_log(&self.pool, options).await
    }

    /// Retry a bounded batch of durable exact-key cold/object-store purge debts.
    /// Object deletion occurs without an open database transaction; successful
    /// keys are reconciled afterward in short idempotent transactions.
    ///
    /// # Errors
    ///
    /// Returns storage errors while reading or reconciling pending rows, and a
    /// constraint violation for a non-positive batch size.
    pub async fn retry_cold_object_purges(
        &self,
        options: ColdPurgeRetryOptions,
    ) -> Result<ColdPurgeRetryOutcome, StorageError> {
        verbs::maintenance::retry_cold_object_purges(&self.pool, self.cold.as_ref(), options).await
    }

    /// Apply all pending migrations under
    /// `crates/storage-pg/migrations/`. Idempotent — sqlx tracks
    /// applied migrations in `_sqlx_migrations`. Call once
    /// at process start before any verb dispatch.
    ///
    /// `ignore_missing = true` forgives ledger rows the embedded set does
    /// not account for: flavor rows in the shared table, and orphaned draft
    /// rows left behind when a dev-cycle lane is squashed under a fresh
    /// version number (docs/how-to/migrations.md). The core version-set is
    /// still checksum-validated — both by `SQLx` and, more legibly, by
    /// [`ensure_core_ledger_compatible`] first.
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
    fn core_migrator_is_the_v008_baseline_plus_additive_migrations() {
        let versions: Vec<i64> = super::core_migrator()
            .iter()
            .map(|migration| migration.version)
            .collect();
        assert_eq!(
            versions,
            vec![1, 2],
            "v0.0.8 is one frozen file (0001_v008.sql) and every release after it appends: \
             v0.0.9 is 0002_v009_declaration_triggers.sql"
        );
    }

    /// The v0.0.7 ALTER lane occupied versions 2..=21 and the squash to a
    /// single v008 baseline retired all of them.
    ///
    /// Version 2 is now v0.0.9's additive migration, which reuses a retired
    /// number. That is safe, and this is why: the tripwire for a pre-v0.0.8
    /// database is version 1's checksum, which is the legacy `0001_init.sql`
    /// and can never match `0001_v008.sql`. `ensure_core_ledger_compatible`
    /// compares it first and returns `SchemaResetRequired` before version 2
    /// is ever reached, so no pre-v008 database can mistake the v009
    /// migration for the legacy version it recorded.
    ///
    /// The number is asserted to be v0.0.9's, not merely absent, so the
    /// reuse stays a decision rather than an accident.
    #[test]
    fn no_legacy_alter_version_survives_the_v008_squash() {
        let migrator = super::core_migrator();
        let versions: Vec<i64> = migrator.iter().map(|migration| migration.version).collect();
        let version_2 = migrator
            .iter()
            .find(|migration| migration.version == 2)
            .expect("version 2 is v0.0.9's additive migration");
        assert!(
            version_2
                .sql
                .as_str()
                .contains("assert_memory_declares_sidecar"),
            "version 2 must be the v0.0.9 declaration-trigger migration, not a resurrected \
             legacy ALTER"
        );
        for dead in 3..=21 {
            assert!(
                !versions.contains(&dead),
                "legacy version {dead} must be gone"
            );
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

    /// Flavor #0 declares every lexical-stamped table, and the marker query,
    /// the FK-backed `lexical_language_forget()` completeness argument and
    /// this pin all read that declaration.
    ///
    /// The name below is the whole set, not a sample of it: a sixth
    /// searchable core sidecar changes the declaration, not any of the
    /// three readers.
    #[test]
    fn flavor_0_declares_exactly_one_lexical_stamped_table() {
        let declared = proxima_core::FLAVOR_0.lexical_stamped_tables();

        assert_eq!(declared, vec!["proxima_core.projection"]);
    }
}
