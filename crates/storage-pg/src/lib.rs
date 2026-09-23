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
use sqlx::postgres::PgArguments;
use sqlx::query::QueryScalar;
use sqlx::{PgConnection, PgPool, Postgres};
use std::sync::Arc;
pub use verbs::fact_embeddings::{
    EmbeddingReconcileOptions, EmbeddingReconcileOutcome, EmbeddingReconcileScope,
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
mod owner_scope;
#[doc(hidden)]
pub use error::map_err;
pub use error::{MAX_TRANSACTION_ATTEMPTS, is_transient_conflict};
pub mod integrity;
mod pg_enums;
mod pg_ident;
mod pgvector;
mod platform_scope;
mod pool_config;
mod ports;
pub mod projection;
mod rls_guard;
pub mod sidecars;
pub mod query {
    #[cfg(any(test, feature = "test-fixtures", debug_assertions))]
    pub use crate::verbs::query::file_revision_heads_sql_for_tests;
    pub use crate::verbs::query::{
        ActiveGoalTargetRow, ChunkSeriesHead, CodeChunkVectorCandidate, CodeChunkVectorFilters,
        FileRevisionHeadRow, MAX_SNAPSHOT_EDGES, active_goals_for_memory_targets,
        active_goals_for_memory_targets_on_connection, nearest_code_chunk_candidates,
        nearest_code_chunk_candidates_on_connection, owned_chunk_series_heads,
        owned_file_revision_heads, owned_present_chunk_indexes,
        owned_present_file_revision_heads_except, readable_chunk_head_ts_for_file,
        readable_file_revision_head_ts,
    };
}
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_fixtures;
mod tuning;
mod tx;
pub mod verbs;
/// Stable, discoverable re-export of the exported `OwnerAccessPort` adapter
/// (see [`access::PgOwnerAccessResolver`]) for embedding hosts.
pub use access::PgOwnerAccessResolver;
pub use delegated_authority::PgDelegationStore;
pub use owner_scope::{begin_compatible_owner_transaction, begin_owner_transaction};
pub use platform_scope::{PgPlatformScope, begin_migration_transaction};
pub use pool_config::PgPoolConfig;
pub use ports::{PgHostStateLifecyclePort, PgHostStateParticipant};
pub use rls_guard::{assert_runtime_rls, owner_rls_enforced};
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

fn embedded_core_checksums() -> std::collections::BTreeMap<i64, Vec<u8>> {
    core_migrator()
        .iter()
        .map(|migration| (migration.version, migration.checksum.as_ref().to_vec()))
        .collect()
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

    let embedded = embedded_core_checksums();

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
    let mut connection = pool.acquire().await.map_err(internal)?.detach();
    let mut transaction = crate::begin_migration_transaction(&mut connection).await?;
    let result = ensure_core_schema_current_on_connection(transaction.as_mut()).await;
    if result.is_ok() {
        transaction.commit().await.map_err(internal)?;
    }
    result
}

async fn ensure_core_schema_current_on_connection(
    connection: &mut PgConnection,
) -> Result<(), StorageError> {
    let migration_table_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(&mut *connection)
            .await
            .map_err(internal)?;

    if migration_table_exists {
        let min_required = min_core_migration_version();
        let max_version: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(version) FROM public._sqlx_migrations WHERE success AND version <= $1",
        )
        .bind(CORE_MIGRATION_VERSION_CEILING)
        .fetch_one(&mut *connection)
        .await
        .map_err(internal)?;
        if max_version.unwrap_or(0) < min_required {
            return Err(StorageError::Internal(format!(
                "database core migrations at version {}; version {min_required}+ required — run the pending core migrations under crates/storage-pg/migrations/ (they are additive over the v008 baseline, so an existing database upgrades in place; a fresh one starts at 0001_v008.sql). In a split-role deploy that is the DDL-role init step, not this process (see docs/how-to/migrations.md and docs/15-deployment.md)",
                max_version.unwrap_or(0)
            )));
        }
    }

    ensure_core_schema_markers_on_connection(connection).await
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
pub async fn ensure_core_schema_markers(pool: &PgPool) -> Result<(), StorageError> {
    let mut connection = pool.acquire().await.map_err(internal)?.detach();
    let mut transaction = crate::begin_migration_transaction(&mut connection).await?;
    let result = ensure_core_schema_markers_on_connection(transaction.as_mut()).await;
    if result.is_ok() {
        transaction.commit().await.map_err(internal)?;
    }
    result
}

async fn ensure_core_schema_markers_on_connection(
    connection: &mut PgConnection,
) -> Result<(), StorageError> {
    ensure_lexical_language_stamps(connection).await?;
    // These groups were one `CASE` whose first matching `WHEN` named the
    // broken marker. Both the order of the calls below and the branch order
    // inside each group are therefore load-bearing: the first group that
    // reports a marker still names the same failure the single `CASE` did, and
    // a later group may assume every marker before it holds. Two dependencies
    // ride on that assumption:
    //
    // * Cross-group: an absent relation makes a `'<relation>'::regclass` cast
    //   raise instead of returning NULL, and Postgres folds that constant
    //   while planning, so it aborts the whole statement even when an earlier
    //   `WHEN` arm of the same group would have matched. A presence leaf must
    //   therefore live in an earlier group, not merely earlier in the same
    //   `CASE`: `CORE_RELATION_MARKERS` proves `goal`,
    //   `goal_replay_declaration` and `blob_uploads` before the groups that
    //   cast them.
    // * Intra-group: a group that proves a relation and then probes its
    //   columns must keep the relation branch ahead of the column branches, so
    //   a missing relation reports as a missing relation rather than as a pile
    //   of missing columns.
    probe_marker_group(connection, sqlx::query_scalar(CORE_RELATION_MARKERS)).await?;
    probe_marker_group(
        connection,
        sqlx::query_scalar(GOAL_REPLAY_DECLARATION_MARKERS),
    )
    .await?;
    probe_marker_group(connection, sqlx::query_scalar(BLOB_UPLOAD_HASH_MARKERS)).await?;
    probe_marker_group(connection, sqlx::query_scalar(SUPPORT_RELATION_MARKERS)).await?;
    probe_marker_group(connection, sqlx::query_scalar(COLD_PURGE_PENDING_MARKERS)).await?;
    probe_marker_group(connection, sqlx::query_scalar(COOLED_COLUMN_MARKERS)).await?;
    probe_marker_group(connection, sqlx::query_scalar(ERASED_PIN_TARGET_MARKERS)).await?;
    probe_marker_group(
        connection,
        sqlx::query_scalar(PIN_LIFECYCLE_FUNCTION_MARKERS),
    )
    .await?;
    probe_marker_group(
        connection,
        sqlx::query_scalar(PIN_LIFECYCLE_FUNCTION_BODY_MARKERS),
    )
    .await?;
    probe_marker_group(connection, sqlx::query_scalar(TRIGGER_WIRING_MARKERS)).await?;
    probe_marker_group(connection, sqlx::query_scalar(LEXICAL_CONFIG_MARKERS)).await?;
    probe_marker_group(connection, sqlx::query_scalar(ENUM_ORDER_MARKERS)).await?;
    probe_marker_group(connection, sqlx::query_scalar(EMBEDDING_JOB_MARKERS)).await?;
    probe_marker_group(connection, sqlx::query_scalar(EMBEDDING_SPACE_MARKERS)).await?;
    Ok(())
}

/// Run one themed marker group and report the first marker in it that is
/// missing or wrong as the boot error callers expect.
async fn probe_marker_group(
    connection: &mut PgConnection,
    probe: QueryScalar<'static, Postgres, Option<String>, PgArguments>,
) -> Result<(), StorageError> {
    let marker_error: Option<String> = probe.fetch_one(&mut *connection).await.map_err(internal)?;
    if let Some(marker_error) = marker_error {
        return Err(StorageError::Internal(format!(
            "database is missing or has an incorrect current schema marker: {marker_error}; apply migrations before boot"
        )));
    }
    Ok(())
}

/// Relations every core lane must have before any finer marker can be
/// probed.
const CORE_RELATION_MARKERS: &str = r"SELECT CASE
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
         WHEN to_regclass('proxima_core.goal_replay_declaration') IS NULL
           THEN 'missing relation proxima_core.goal_replay_declaration'
         WHEN to_regclass('proxima_core.blob_uploads') IS NULL
           THEN 'missing relation proxima_core.blob_uploads'
         ELSE NULL
       END";

/// Columns, keys, checks and the append-only trigger of
/// `goal_replay_declaration`.
const GOAL_REPLAY_DECLARATION_MARKERS: &str = r"SELECT CASE
         WHEN NOT EXISTS (
                  SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'goal_replay_declaration'
                     AND column_name = 'goal_t'
                     AND data_type = 'uuid'
                     AND is_nullable = 'NO'
                )
           THEN 'goal_replay_declaration.goal_t must be uuid NOT NULL'
         WHEN NOT EXISTS (
                  SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'goal_replay_declaration'
                     AND column_name = 'declaration'
                     AND data_type = 'jsonb'
                     AND is_nullable = 'NO'
                )
           THEN 'goal_replay_declaration.declaration must be jsonb NOT NULL'
         WHEN NOT EXISTS (
                  SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'goal_replay_declaration'
                     AND column_name = 'edge_count'
                     AND data_type = 'integer'
                     AND is_nullable = 'NO'
                )
           THEN 'goal_replay_declaration.edge_count must be integer NOT NULL'
         WHEN NOT EXISTS (
                  SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'goal_replay_declaration'
                     AND column_name = 'created_at'
                     AND data_type = 'timestamp with time zone'
                     AND is_nullable = 'NO'
                     AND column_default = 'now()'
                )
           THEN 'goal_replay_declaration.created_at must be timestamptz NOT NULL DEFAULT now()'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_constraint c
                   WHERE c.conrelid = 'proxima_core.goal_replay_declaration'::regclass
                     AND c.conname = 'goal_replay_declaration_pkey'
                     AND c.contype = 'p'
                     AND pg_get_constraintdef(c.oid, true) = 'PRIMARY KEY (goal_t)'
                )
           THEN 'goal_replay_declaration primary key is missing or incorrect'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_constraint c
                   WHERE c.conrelid = 'proxima_core.goal_replay_declaration'::regclass
                     AND c.conname = 'goal_replay_declaration_goal_t_fkey'
                     AND c.contype = 'f'
                     AND c.confrelid = 'proxima_core.goal'::regclass
                     AND c.confdeltype = 'c'
                     AND cardinality(c.conkey) = 1
                     AND cardinality(c.confkey) = 1
                     AND (
                           SELECT a.attname
                             FROM pg_attribute a
                            WHERE a.attrelid = c.conrelid AND a.attnum = c.conkey[1]
                         ) = 'goal_t'
                     AND (
                           SELECT a.attname
                             FROM pg_attribute a
                            WHERE a.attrelid = c.confrelid AND a.attnum = c.confkey[1]
                         ) = 't'
                )
           THEN 'goal_replay_declaration.goal_t foreign key is missing or incorrect'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_constraint c
                   WHERE c.conrelid = 'proxima_core.goal_replay_declaration'::regclass
                     AND c.conname = 'goal_replay_declaration_object_chk'
                     AND c.contype = 'c'
                     AND c.convalidated
                     AND strpos(lower(pg_get_constraintdef(c.oid, true)),
                                'jsonb_typeof(declaration) = ''object''::text') > 0
                )
           THEN 'goal_replay_declaration object check is missing or incorrect'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_constraint c
                   WHERE c.conrelid = 'proxima_core.goal_replay_declaration'::regclass
                     AND c.conname = 'goal_replay_edge_count_chk'
                     AND c.contype = 'c'
                     AND c.convalidated
                     AND strpos(lower(pg_get_constraintdef(c.oid, true)), 'edge_count >= 0') > 0
                )
           THEN 'goal_replay_declaration edge-count check is missing or incorrect'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                   WHERE tr.tgrelid = 'proxima_core.goal_replay_declaration'::regclass
                     AND tr.tgname = 'goal_replay_declaration_append_only'
                     AND tr.tgenabled = 'O'
                     AND NOT tr.tgisinternal
                     AND (tr.tgtype & 2) = 2
                     AND (tr.tgtype & 16) = 16
                     AND lower(pg_get_triggerdef(tr.oid)) LIKE
                         '%execute function proxima_core.enforce_row_append_only%'
                )
           THEN 'goal_replay_declaration append-only trigger is missing or incorrect'
         ELSE NULL
       END";

/// `blob_uploads` content-hash column and its digest-length check.
const BLOB_UPLOAD_HASH_MARKERS: &str = r"SELECT CASE
         WHEN NOT EXISTS (
                  SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'blob_uploads'
                     AND column_name = 'content_hash'
                     AND data_type = 'bytea'
                     AND is_nullable = 'YES'
                )
           THEN 'blob_uploads.content_hash must be nullable bytea'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_constraint c
                   WHERE c.conrelid = 'proxima_core.blob_uploads'::regclass
                     AND c.conname = 'blob_uploads_content_hash_chk'
                     AND c.contype = 'c'
                     AND c.convalidated
                     AND strpos(lower(pg_get_constraintdef(c.oid, true)),
                                'octet_length(content_hash) = 32') > 0
                )
           THEN 'blob_uploads content-hash check is missing or incorrect'
         ELSE NULL
       END";

/// Relations the later per-table marker groups probe columns of.
const SUPPORT_RELATION_MARKERS: &str = r"SELECT CASE
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
         WHEN to_regclass('proxima_core.publication_outbox') IS NULL
           THEN 'missing relation proxima_core.publication_outbox'
         ELSE NULL
       END";

/// Column shape and primary key of the cold-purge queue.
const COLD_PURGE_PENDING_MARKERS: &str = r"SELECT CASE
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
                     AND is_nullable = 'YES'
                )
           THEN 'cold_purge_pending.owner_id must be a nullable uuid'
         WHEN NOT EXISTS (
                  SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'proxima_core' AND table_name = 'cold_purge_pending'
                     AND column_name = 'backend' AND data_type = 'text'
                     AND is_nullable = 'NO'
                )
           THEN 'cold_purge_pending.backend must be text NOT NULL'
         WHEN NOT EXISTS (
                  SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'proxima_core' AND table_name = 'cold_purge_pending'
                     AND column_name = 'enqueued_at'
                     AND data_type = 'timestamp with time zone' AND is_nullable = 'NO'
                )
           THEN 'cold_purge_pending.enqueued_at must be timestamptz NOT NULL'
         ELSE NULL
       END";

/// `cooled` column types plus the digest-length and NULL-element checks
/// over its reference arrays.
const COOLED_COLUMN_MARKERS: &str = r"SELECT CASE
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
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'cooled'
                     AND column_name = 'origins'
                     AND data_type = 'ARRAY'
                     AND udt_schema = 'pg_catalog'
                     AND udt_name = '_uuid'
                     AND is_nullable = 'YES'
                )
           THEN 'cooled.origins must be nullable uuid[]'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'cooled'
                     AND column_name = 'refs'
                     AND data_type = 'ARRAY'
                     AND udt_schema = 'pg_catalog'
                     AND udt_name = '_uuid'
                     AND is_nullable = 'YES'
                )
           THEN 'cooled.refs must be nullable uuid[]'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'cooled'
                     AND column_name = 'cold_digest'
                     AND data_type = 'bytea'
                     AND is_nullable = 'YES'
                )
           THEN 'cooled.cold_digest must be nullable bytea'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_constraint c
                    JOIN pg_class r ON r.oid = c.conrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'cooled'
                     AND c.conname = 'cooled_cold_digest_len_chk'
                     AND c.contype = 'c'
                     AND c.convalidated
                     AND strpos(lower(pg_get_constraintdef(c.oid, true)),
                                'octet_length(cold_digest) = 32') > 0
                )
           THEN 'cooled.cold_digest must be NULL or exactly 32 bytes'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_constraint c
                    JOIN pg_class r ON r.oid = c.conrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'cooled'
                     AND c.conname = 'cooled_origins_no_null_chk'
                     AND c.contype = 'c'
                     AND c.convalidated
                )
           THEN 'cooled.origins must reject NULL array elements'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_constraint c
                    JOIN pg_class r ON r.oid = c.conrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'cooled'
                     AND c.conname = 'cooled_refs_no_null_chk'
                     AND c.contype = 'c'
                     AND c.convalidated
                )
           THEN 'cooled.refs must reject NULL array elements'
         -- `cooled_identity_seal` returns NEW for an array holding a NULL
         -- element and lets the column CHECK reject it. Without this marker a
         -- database missing the constraint would boot clean and silently skip
         -- the seal, so the guard the trigger delegates to is asserted here.
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_constraint c
                    JOIN pg_class r ON r.oid = c.conrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'cooled'
                     AND c.conname = 'cooled_goal_refs_no_null_chk'
                     AND c.contype = 'c'
                     AND c.convalidated
                )
           THEN 'cooled.goal_refs must reject NULL array elements'
         ELSE NULL
       END";

/// The `pin_target_kind` enum, the `erased_pin_target` ledger shape, and
/// the per-table triggers that record erased pin targets.
const ERASED_PIN_TARGET_MARKERS: &str = r"SELECT CASE
         WHEN COALESCE((
                  SELECT array_agg(e.enumlabel::text ORDER BY e.enumsortorder)
                    FROM pg_enum e
                    JOIN pg_type t ON t.oid = e.enumtypid
                    JOIN pg_namespace n ON n.oid = t.typnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND t.typname = 'pin_target_kind'
                ), ARRAY[]::text[]) <> ARRAY['fact', 'abstraction', 'perspective', 'goal']
           THEN 'pin_target_kind labels/order must be fact, abstraction, perspective, goal'
         WHEN to_regclass('proxima_core.erased_pin_target') IS NULL
           THEN 'missing relation proxima_core.erased_pin_target'
         WHEN (
                  SELECT count(*)
                    FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'erased_pin_target'
                ) <> 2
           THEN 'erased_pin_target must have exactly columns t and kind'
         WHEN EXISTS (
                  SELECT 1
                    FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'erased_pin_target'
                     AND column_name NOT IN ('t', 'kind')
                )
           THEN 'erased_pin_target must not have extra columns'
         WHEN NOT EXISTS (
                  SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'erased_pin_target'
                     AND column_name = 't'
                     AND data_type = 'uuid'
                     AND is_nullable = 'NO'
                )
           THEN 'erased_pin_target.t must be uuid NOT NULL'
         WHEN NOT EXISTS (
                  SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'erased_pin_target'
                     AND column_name = 'kind'
                     AND udt_schema = 'proxima_core'
                     AND udt_name = 'pin_target_kind'
                     AND is_nullable = 'NO'
                )
           THEN 'erased_pin_target.kind must be pin_target_kind NOT NULL'
         WHEN EXISTS (
                  SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'erased_pin_target'
                     AND column_name = 'owner_id'
                )
           THEN 'erased_pin_target must not carry owner_id'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.table_constraints tc
                    JOIN information_schema.key_column_usage kcu
                      ON kcu.constraint_catalog = tc.constraint_catalog
                     AND kcu.constraint_schema = tc.constraint_schema
                     AND kcu.constraint_name = tc.constraint_name
                   WHERE tc.table_schema = 'proxima_core'
                     AND tc.table_name = 'erased_pin_target'
                     AND tc.constraint_type = 'PRIMARY KEY'
                     AND kcu.column_name = 't'
                     AND 1 = (
                         SELECT count(*)
                           FROM information_schema.key_column_usage only_kcu
                          WHERE only_kcu.constraint_catalog = tc.constraint_catalog
                            AND only_kcu.constraint_schema = tc.constraint_schema
                            AND only_kcu.constraint_name = tc.constraint_name
                     )
                )
           THEN 'erased_pin_target.t must be the primary key'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                    JOIN pg_class r ON r.oid = tr.tgrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'erased_pin_target'
                     AND tr.tgname = 'erased_pin_target_insert_guard'
                     AND NOT tr.tgisinternal
                )
           THEN 'erased_pin_target insert guard trigger is missing'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                    JOIN pg_class r ON r.oid = tr.tgrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'erased_pin_target'
                     AND tr.tgname = 'erased_pin_target_append_only'
                     AND NOT tr.tgisinternal
                )
           THEN 'erased_pin_target append-only trigger is missing'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                    JOIN pg_class r ON r.oid = tr.tgrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'memory'
                     AND tr.tgname = 'memory_erased_pin_target'
                     AND NOT tr.tgisinternal
                )
           THEN 'memory erased-target trigger is missing'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                    JOIN pg_class r ON r.oid = tr.tgrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'cooled'
                     AND tr.tgname = 'cooled_erased_pin_target'
                     AND NOT tr.tgisinternal
                )
           THEN 'cooled erased-target trigger is missing'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                    JOIN pg_class r ON r.oid = tr.tgrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'goal'
                     AND tr.tgname = 'goal_erased_pin_target'
                     AND NOT tr.tgisinternal
                )
           THEN 'goal erased-target trigger is missing'
         ELSE NULL
       END";

/// Presence of the functions the pin-target and cooled triggers call.
const PIN_LIFECYCLE_FUNCTION_MARKERS: &str = r"SELECT CASE
         WHEN to_regprocedure('proxima_core.lock_pin_targets(uuid[])') IS NULL
           THEN 'missing function proxima_core.lock_pin_targets(uuid[])'
         WHEN to_regprocedure('proxima_core.assert_erased_pin_target_insert()') IS NULL
           THEN 'missing function proxima_core.assert_erased_pin_target_insert()'
         WHEN to_regprocedure('proxima_core.cooled_identity_seal()') IS NULL
           THEN 'missing function proxima_core.cooled_identity_seal()'
         WHEN to_regprocedure('proxima_core.cooled_append_only()') IS NULL
           THEN 'missing function proxima_core.cooled_append_only()'
         ELSE NULL
       END";

/// Statement order inside the pin-target and forget-grounding functions:
/// the lock must be taken before the rows it guards are read.
const PIN_LIFECYCLE_FUNCTION_BODY_MARKERS: &str = r"SELECT CASE
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_proc p
                   WHERE p.oid = to_regprocedure(
                         'proxima_core.record_erased_pin_target(uuid,proxima_core.pin_target_kind)'
                     )
                     AND strpos(lower(pg_get_functiondef(p.oid)), 'lock_pin_targets') > 0
                     AND strpos(lower(pg_get_functiondef(p.oid)), 'existing_kind <> target_kind') > 0
                     AND strpos(lower(pg_get_functiondef(p.oid)), 'erased_pin_target_writer') > 0
                     AND strpos(lower(pg_get_functiondef(p.oid)), 'insert into proxima_core.erased_pin_target') > 0
                     AND strpos(lower(pg_get_functiondef(p.oid)), 'lock_pin_targets')
                         < strpos(lower(pg_get_functiondef(p.oid)), 'existing_kind <> target_kind')
                     AND strpos(lower(pg_get_functiondef(p.oid)), 'existing_kind <> target_kind')
                         < strpos(lower(pg_get_functiondef(p.oid)), 'erased_pin_target_writer')
                     AND strpos(lower(pg_get_functiondef(p.oid)), 'erased_pin_target_writer')
                         < strpos(lower(pg_get_functiondef(p.oid)), 'insert into proxima_core.erased_pin_target')
                )
           THEN 'record_erased_pin_target body/order is incorrect'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_proc p
                   WHERE p.oid = to_regprocedure('proxima_core.cooled_forget_grounding()')
                     AND strpos(lower(pg_get_functiondef(p.oid)), 'lock_pin_targets') > 0
                     AND strpos(lower(pg_get_functiondef(p.oid)), 'for update') > 0
                     AND strpos(lower(pg_get_functiondef(p.oid)), 'footprint grew') > 0
                     AND strpos(lower(pg_get_functiondef(p.oid)),
                                'not (m.t = any (dependent_ids))') > 0
                     AND strpos(lower(pg_get_functiondef(p.oid)),
                                'using errcode = ''40001''') > 0
                     AND strpos(lower(pg_get_functiondef(p.oid)), 'lock_pin_targets')
                         < strpos(lower(pg_get_functiondef(p.oid)),
                                  'not (m.t = any (dependent_ids))')
                     AND strpos(lower(pg_get_functiondef(p.oid)),
                                  'not (m.t = any (dependent_ids))')
                         < strpos(lower(pg_get_functiondef(p.oid)), 'footprint grew')
                     AND strpos(lower(pg_get_functiondef(p.oid)), 'footprint grew')
                         < strpos(lower(pg_get_functiondef(p.oid)),
                                  'using errcode = ''40001''')
                     AND strpos(lower(pg_get_functiondef(p.oid)),
                                  'using errcode = ''40001''')
                         < strpos(lower(pg_get_functiondef(p.oid)), 'for update')
                )
           THEN 'cooled_forget_grounding body/order is incorrect'
         ELSE NULL
       END";

/// Timing, enablement and function wiring of every core lifecycle trigger.
const TRIGGER_WIRING_MARKERS: &str = r"SELECT CASE
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                    JOIN pg_class r ON r.oid = tr.tgrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'memory'
                     AND tr.tgname = 'memory_pin_checks'
                     AND tr.tgenabled = 'O'
                     AND (tr.tgtype & 2) = 2
                     AND (tr.tgtype & 4) = 4
                     AND lower(pg_get_triggerdef(tr.oid)) LIKE
                         '%execute function proxima_core.memory_pin_checks%'
                )
           THEN 'memory_pin_checks trigger must be enabled BEFORE INSERT and wired to its function'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                    JOIN pg_class r ON r.oid = tr.tgrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'erased_pin_target'
                     AND tr.tgname = 'erased_pin_target_insert_guard'
                     AND tr.tgenabled = 'O'
                     AND (tr.tgtype & 2) = 2
                     AND (tr.tgtype & 4) = 4
                     AND lower(pg_get_triggerdef(tr.oid)) LIKE
                         '%execute function proxima_core.assert_erased_pin_target_insert%'
                )
           THEN 'erased_pin_target insert guard timing or wiring is incorrect'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                    JOIN pg_class r ON r.oid = tr.tgrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'erased_pin_target'
                     AND tr.tgname = 'erased_pin_target_append_only'
                     AND tr.tgenabled = 'O'
                     AND (tr.tgtype & 2) = 2
                     AND (tr.tgtype & 16) = 16
                     AND (tr.tgtype & 8) = 8
                     AND lower(pg_get_triggerdef(tr.oid)) LIKE
                         '%execute function proxima_core.erased_pin_target_append_only%'
                )
           THEN 'erased_pin_target append-only trigger timing or wiring is incorrect'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                   JOIN pg_class r ON r.oid = tr.tgrelid
                   JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'cooled'
                     AND tr.tgname = 'cooled_forget_grounding'
                     AND tr.tgenabled = 'O'
                     AND (tr.tgtype & 2) = 2
                     AND (tr.tgtype & 4) = 4
                     AND lower(pg_get_triggerdef(tr.oid)) LIKE
                         '%execute function proxima_core.cooled_forget_grounding%'
                )
           THEN 'cooled forget-grounding trigger timing or wiring is incorrect'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                   JOIN pg_class r ON r.oid = tr.tgrelid
                   JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'cooled'
                     AND tr.tgname = 'cooled_identity_seal'
                     AND tr.tgenabled = 'O'
                     AND (tr.tgtype & 2) = 2
                     AND (tr.tgtype & 4) = 4
                     AND lower(pg_get_triggerdef(tr.oid)) LIKE
                         '%execute function proxima_core.cooled_identity_seal%'
                )
           THEN 'cooled identity-seal trigger timing or wiring is incorrect'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                    JOIN pg_class r ON r.oid = tr.tgrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'cooled'
                     AND tr.tgname = 'cooled_append_only'
                     AND tr.tgenabled = 'O'
                     AND (tr.tgtype & 2) = 2
                     AND (tr.tgtype & 16) = 16
                     AND lower(pg_get_triggerdef(tr.oid)) LIKE
                         '%execute function proxima_core.cooled_append_only%'
                )
           THEN 'cooled append-only trigger timing or wiring is incorrect'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                    JOIN pg_class r ON r.oid = tr.tgrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'goal'
                     AND tr.tgname = 'goal_pin_target_checks'
                     AND tr.tgenabled = 'O'
                     AND (tr.tgtype & 2) = 2
                     AND (tr.tgtype & 4) = 4
                     AND lower(pg_get_triggerdef(tr.oid)) LIKE
                         '%execute function proxima_core.goal_pin_target_checks%'
                )
           THEN 'goal target trigger timing or wiring is incorrect'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                    JOIN pg_class r ON r.oid = tr.tgrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'wake_config'
                     AND tr.tgname = 'wake_pin_target_checks'
                     AND tr.tgenabled = 'O'
                     AND (tr.tgtype & 2) = 2
                     AND (tr.tgtype & 4) = 4
                     AND (tr.tgtype & 16) = 16
                     AND lower(pg_get_triggerdef(tr.oid)) LIKE
                         '%execute function proxima_core.wake_pin_target_checks%'
                )
           THEN 'wake target trigger timing or wiring is incorrect'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                    JOIN pg_class r ON r.oid = tr.tgrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'memory'
                     AND tr.tgname = 'memory_erased_pin_target'
                     AND tr.tgenabled = 'O'
                     AND (tr.tgtype & 2) = 2
                     AND (tr.tgtype & 8) = 8
                     AND lower(pg_get_triggerdef(tr.oid)) LIKE
                         '%execute function proxima_core.memory_erase_witness%'
                )
           THEN 'memory witness trigger timing or wiring is incorrect'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                    JOIN pg_class r ON r.oid = tr.tgrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'cooled'
                     AND tr.tgname = 'cooled_erased_pin_target'
                     AND tr.tgenabled = 'O'
                     AND (tr.tgtype & 2) = 2
                     AND (tr.tgtype & 8) = 8
                     AND lower(pg_get_triggerdef(tr.oid)) LIKE
                         '%execute function proxima_core.cooled_erase_witness%'
                )
           THEN 'cooled witness trigger timing or wiring is incorrect'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_trigger tr
                    JOIN pg_class r ON r.oid = tr.tgrelid
                    JOIN pg_namespace n ON n.oid = r.relnamespace
                   WHERE n.nspname = 'proxima_core'
                     AND r.relname = 'goal'
                     AND tr.tgname = 'goal_erased_pin_target'
                     AND tr.tgenabled = 'O'
                     AND (tr.tgtype & 2) = 2
                     AND (tr.tgtype & 8) = 8
                     AND lower(pg_get_triggerdef(tr.oid)) LIKE
                         '%execute function proxima_core.goal_erase_witness%'
                )
           THEN 'goal witness trigger timing or wiring is incorrect'
         ELSE NULL
       END";

/// The lexical language tables, the singleton default row, and the
/// text-search functions.
const LEXICAL_CONFIG_MARKERS: &str = r"SELECT CASE
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
         ELSE NULL
       END";

/// Label order of the enums whose ordinals are persisted.
const ENUM_ORDER_MARKERS: &str = r"SELECT CASE
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
         ELSE NULL
       END";

/// `embedding_jobs` column shape and the processing-claim invariant.
const EMBEDDING_JOB_MARKERS: &str = r"SELECT CASE
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
       END";

/// Width lanes (migration 0015): every embedding table keys on the space's
/// width, the vector column is untyped, and each supported width has its
/// partial HNSW index. A lane query against a database without its index
/// still answers, by sequential scan, so the index is asserted here.
const EMBEDDING_SPACE_MARKERS: &str = r"SELECT CASE
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'embeddings'
                     AND column_name = 'dim'
                     AND data_type = 'smallint'
                     AND is_nullable = 'NO'
                )
           THEN 'embeddings.dim must be smallint NOT NULL'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'embedding_heads'
                     AND column_name = 'dim'
                     AND data_type = 'smallint'
                     AND is_nullable = 'NO'
                )
           THEN 'embedding_heads.dim must be smallint NOT NULL'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM information_schema.columns
                   WHERE table_schema = 'proxima_core'
                     AND table_name = 'embedding_jobs'
                     AND column_name = 'dim'
                     AND data_type = 'smallint'
                     AND is_nullable = 'NO'
                )
           THEN 'embedding_jobs.dim must be smallint NOT NULL'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_attribute a
                   WHERE a.attrelid = to_regclass('proxima_core.embeddings')
                     AND a.attname = 'vec'
                     AND a.atttypmod = -1
                     AND NOT a.attisdropped
                )
           THEN 'embeddings.vec must be an untyped vector'
         WHEN NOT EXISTS (
                  SELECT 1
                    FROM pg_constraint c
                   WHERE c.conrelid = to_regclass('proxima_core.embeddings')
                     AND c.conname = 'embeddings_vec_width_chk'
                     AND c.convalidated
                )
           THEN 'embeddings.vec width check is missing'
         WHEN to_regclass('proxima_core.embeddings_hnsw_d384') IS NULL
           THEN 'missing width-lane index proxima_core.embeddings_hnsw_d384'
         WHEN to_regclass('proxima_core.embeddings_hnsw_d768') IS NULL
           THEN 'missing width-lane index proxima_core.embeddings_hnsw_d768'
         WHEN to_regclass('proxima_core.embeddings_hnsw_d1024') IS NULL
           THEN 'missing width-lane index proxima_core.embeddings_hnsw_d1024'
         WHEN to_regclass('proxima_core.embeddings_hnsw_d1536') IS NULL
           THEN 'missing width-lane index proxima_core.embeddings_hnsw_d1536'
         WHEN to_regclass('proxima_core.embeddings_hnsw_d2048') IS NULL
           THEN 'missing width-lane index proxima_core.embeddings_hnsw_d2048'
         WHEN to_regclass('proxima_core.embeddings_hnsw_d3072') IS NULL
           THEN 'missing width-lane index proxima_core.embeddings_hnsw_d3072'
         ELSE NULL
       END";

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
async fn ensure_lexical_language_stamps(connection: &mut PgConnection) -> Result<(), StorageError> {
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
    .fetch_all(&mut *connection)
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
    platform_scope: Option<PgPlatformScope>,
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
    /// The flavor-declared lifecycle scopes, resolved into a fence key and a
    /// liveness probe once.
    ///
    /// Empty until `with_flavors`, and empty means REFUSE: an admission of a
    /// payload declaring a scope this storage has no declaration for cannot
    /// be fenced, and admitting it unfenced is the defect. A host that links
    /// a flavor with scopes and forgets `with_flavors` finds out on the
    /// first scoped write, not after an erase left rows behind.
    scopes: crate::access::scope_surfaces::ScopeSurfaces,
    search_projections: Vec<proxima_core::verbs::schema::MemorySearchProjection>,
    embed_units: Vec<proxima_core::verbs::schema::MemoryEmbedUnit>,
    non_embeddable_schemas: Vec<String>,
    tuning: PgTuning,
    embedding_runtime_policy: proxima_core::EmbeddingRuntimePolicy,
    cold: Arc<dyn proxima_core::ColdObjectStore>,
    /// Optional host-state participant invoked on the write-session
    /// transaction. `None` keeps existing
    /// [`proxima_core::engine::UnitOfWork`] Fact behavior with no extra
    /// configuration.
    host_state: Option<RegisteredHostStateParticipant>,
}

/// Participant instance and the exact metadata snapshot captured when it
/// was registered. Dispatch never calls metadata getters again.
#[derive(Clone)]
pub(crate) struct RegisteredHostStateParticipant {
    pub(crate) participant: Arc<dyn crate::PgHostStateParticipant>,
    pub(crate) descriptor: proxima_core::storage_ports::HostStateParticipantDescriptor,
    pub(crate) lifecycle: Option<RegisteredHostStateLifecycle>,
}

#[derive(Clone)]
pub(crate) struct RegisteredHostStateLifecycle {
    pub(crate) port: Arc<dyn crate::PgHostStateLifecyclePort>,
    pub(crate) descriptor: proxima_core::storage_ports::HostStateParticipantDescriptor,
}

/// Opaque authority-free context for one production boot's physical memory
/// erasure path. It carries the entire registry-frozen surface set and the
/// lifecycle callback actually validated against that set at boot.
#[derive(Clone)]
pub struct PgHostStateEraseContext {
    pub(crate) surfaces: proxima_core::owner_inverse::OwnerSurfaces,
    pub(crate) lifecycle: Option<RegisteredHostStateLifecycle>,
}

impl std::fmt::Debug for PgHostStateEraseContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgHostStateEraseContext")
            .field(
                "host_lifecycle_tables",
                &self.surfaces.host_lifecycle_surfaces(),
            )
            .finish_non_exhaustive()
    }
}

impl PgHostStateEraseContext {
    /// Acquire the global host-state exclusive fence as the transaction's
    /// first erase lock. Callers that perform flavor-owned queries or DML
    /// before delegating a physical erase must call this immediately after
    /// BEGIN and before any owner/source/handle/target locks.
    ///
    /// # Errors
    /// Returns a storage error when `PostgreSQL` cannot acquire the fence.
    pub async fn lock_before_physical_erase(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), StorageError> {
        crate::access::owner_columns::lock_host_lifecycle_fence_exclusive_tx(tx).await
    }

    /// Explicit fixture constructor for registries without lifecycle-owned
    /// tables. A registry that declares host lifecycle state must come from a
    /// real validated `PgStorage` boot, so tests cannot silently omit its
    /// callback.
    #[cfg(any(test, feature = "test-fixtures", debug_assertions))]
    #[doc(hidden)]
    pub fn for_surfaces_for_tests(
        surfaces: proxima_core::owner_inverse::OwnerSurfaces,
    ) -> Result<Self, StorageError> {
        if !surfaces.host_lifecycle_surfaces().is_empty() {
            return Err(StorageError::ConstraintViolation(
                "host lifecycle erase fixtures require a validated PgStorage registration".into(),
            ));
        }
        Ok(Self {
            surfaces,
            lifecycle: None,
        })
    }
}

fn validate_host_lifecycle_registration(
    surfaces: &proxima_core::owner_inverse::OwnerSurfaces,
    registered: Option<&RegisteredHostStateParticipant>,
) -> Result<(), StorageError> {
    use std::collections::BTreeSet;

    let expected = surfaces
        .host_lifecycle_surfaces()
        .iter()
        .map(|policy| policy.table)
        .collect::<BTreeSet<_>>();
    let Some(registered) = registered else {
        if expected.is_empty() {
            return Ok(());
        }
        return Err(StorageError::ConstraintViolation(
            "frozen host lifecycle surfaces have no registered participant".into(),
        ));
    };
    let Some(lifecycle) = registered.lifecycle.as_ref() else {
        if expected.is_empty() {
            return Ok(());
        }
        return Err(StorageError::ConstraintViolation(
            "frozen host lifecycle surfaces have no lifecycle callback port".into(),
        ));
    };
    if expected.is_empty() {
        return Err(StorageError::ConstraintViolation(
            "lifecycle callback port is registered but the frozen contracts declare no managed surfaces".into(),
        ));
    }
    if lifecycle.descriptor.participant_id() != registered.descriptor.participant_id() {
        return Err(StorageError::ConstraintViolation(
            "lifecycle callback participant id differs from the registered host participant".into(),
        ));
    }
    let lifecycle_tables = lifecycle
        .descriptor
        .tables()
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let registered_tables = registered
        .descriptor
        .tables()
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if registered_tables.len() != registered.descriptor.tables().len()
        || lifecycle_tables.len() != lifecycle.descriptor.tables().len()
        || lifecycle_tables != expected
        || !expected.is_subset(&registered_tables)
    {
        return Err(StorageError::ConstraintViolation(
            "registered lifecycle tables must uniquely match every frozen managed surface and be a subset of the host participant descriptor".into(),
        ));
    }
    Ok(())
}

fn validate_registered_state_tables(
    surfaces: &proxima_core::owner_inverse::OwnerSurfaces,
    registered: Option<&RegisteredHostStateParticipant>,
) -> Result<(), StorageError> {
    let Some(registered) = registered else {
        return Ok(());
    };
    let declared = registered
        .descriptor
        .tables()
        .iter()
        .map(|table| table.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if declared.len() != registered.descriptor.tables().len() {
        return Err(StorageError::ConstraintViolation(
            "registered host participant declares a state table more than once".into(),
        ));
    }
    if let Some(table) = declared
        .iter()
        .find(|table| !surfaces.is_declared_state_surface(table))
    {
        return Err(StorageError::ConstraintViolation(format!(
            "registered host participant table {table} is not declared in linked flavor state_surfaces"
        )));
    }
    Ok(())
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

#[derive(Debug, Clone, Copy)]
enum PoolPurpose {
    Runtime,
    Migration,
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
        Self::connect_for_purpose(url, pool_config, tuning, PoolPurpose::Runtime).await
    }

    /// Connect the separately configured schema-management lane.
    ///
    /// # Errors
    /// Refuses unsafe migration authority after RLS activation and propagates
    /// pool configuration and database errors.
    pub async fn connect_for_migrations_with_config(
        url: &str,
        pool_config: PgPoolConfig,
        tuning: PgTuning,
    ) -> Result<Self, StorageError> {
        Self::connect_for_purpose(url, pool_config, tuning, PoolPurpose::Migration).await
    }

    async fn connect_for_purpose(
        url: &str,
        pool_config: PgPoolConfig,
        tuning: PgTuning,
        purpose: PoolPurpose,
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

        if owner_rls_enforced(&pool, &["proxima_core"]).await? {
            match purpose {
                PoolPurpose::Runtime => assert_runtime_rls(&pool, &["proxima_core"]).await?,
                PoolPurpose::Migration => {
                    let mut connection = pool.acquire().await.map_err(map_err)?;
                    begin_migration_transaction(&mut connection)
                        .await?
                        .rollback()
                        .await
                        .map_err(map_err)?;
                }
            }
        }

        Ok(Self {
            pool,
            platform_scope: None,
            sidecars: core_pg_sidecars(),
            surfaces: flavor_0_surfaces(),
            scopes: crate::access::scope_surfaces::ScopeSurfaces::default(),
            search_projections: Vec::new(),
            embed_units: Vec::new(),
            non_embeddable_schemas: Vec::new(),
            tuning,
            embedding_runtime_policy: proxima_core::EmbeddingRuntimePolicy::default(),
            cold: Arc::new(verbs::forget::MemoryColdStore::default()),
            host_state: None,
        })
    }

    /// Attach the separately validated platform database capability at boot.
    #[must_use]
    pub fn with_platform_scope(mut self, scope: PgPlatformScope) -> Self {
        self.platform_scope = Some(scope);
        self
    }

    /// Host-only platform capability for authority resolution and maintenance.
    #[must_use]
    pub fn platform_scope_for_host(&self) -> Option<PgPlatformScope> {
        self.platform_scope.clone()
    }

    pub(crate) async fn platform_transaction(
        &self,
    ) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, StorageError> {
        match &self.platform_scope {
            Some(scope) => scope.begin().await,
            None => begin_compatible_owner_transaction(&self.pool, None).await,
        }
    }

    /// Fixed lifecycle algorithms need global referential evidence, but their
    /// selected owner still comes from a verified, sealed write permit.
    pub(crate) async fn owner_maintenance_transaction(
        &self,
        permit: &proxima_core::storage_ports::OwnerWritePermit,
    ) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, StorageError> {
        crate::platform_scope::begin_owner_lifecycle_transaction(
            &self.pool,
            self.platform_scope.as_ref(),
            permit,
        )
        .await
    }

    /// Register the host-state participant that runs on each write session's
    /// live transaction. Hosts that register none keep the existing
    /// [`proxima_core::engine::UnitOfWork`] Fact path with no extra
    /// configuration.
    #[must_use]
    pub fn with_host_state_participant(
        mut self,
        participant: Arc<dyn crate::PgHostStateParticipant>,
    ) -> Self {
        let descriptor = proxima_core::storage_ports::HostStateParticipantDescriptor::new(
            participant.participant_id(),
            participant.declared_tables(),
        );
        self.host_state = Some(RegisteredHostStateParticipant {
            lifecycle: participant.lifecycle_port().map(|port| {
                let descriptor = proxima_core::storage_ports::HostStateParticipantDescriptor::new(
                    port.participant_id(),
                    port.declared_tables(),
                );
                RegisteredHostStateLifecycle { port, descriptor }
            }),
            participant,
            descriptor,
        });
        self
    }

    pub(crate) fn host_lifecycle_for_surfaces(
        &self,
        surfaces: &proxima_core::owner_inverse::OwnerSurfaces,
    ) -> Result<Option<RegisteredHostStateLifecycle>, StorageError> {
        validate_host_lifecycle_registration(surfaces, self.host_state.as_ref())?;
        Ok(self
            .host_state
            .as_ref()
            .and_then(|registered| registered.lifecycle.clone()))
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

    /// Capture the boot-frozen registry and its validated host lifecycle
    /// callback for a flavor that invokes the shared physical erase verb.
    ///
    /// # Errors
    /// Returns an error if the frozen host lifecycle declarations no longer
    /// match the participant and callback captured by this storage instance.
    pub fn host_state_erase_context(&self) -> Result<PgHostStateEraseContext, StorageError> {
        validate_host_lifecycle_registration(&self.surfaces, self.host_state.as_ref())?;
        Ok(PgHostStateEraseContext {
            surfaces: self.surfaces.clone(),
            lifecycle: self
                .host_state
                .as_ref()
                .and_then(|registered| registered.lifecycle.clone()),
        })
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
        self.non_embeddable_schemas = registry.non_embeddable_schema_ids().to_vec();
        self.surfaces = proxima_core::owner_inverse::OwnerSurfaces::for_registry(registry);
        self.scopes = crate::access::scope_surfaces::ScopeSurfaces::for_registry(registry);
        self
    }

    /// Fallible flavor installation used by production boot. It validates
    /// lifecycle policy coverage against the participant and callback port
    /// captured at registration, before these surfaces can reach an engine.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::ConstraintViolation`] when lifecycle policies
    /// overlap generic inverses, lack a registered callback, or disagree with
    /// the captured participant/callback descriptors.
    pub fn try_with_flavors(
        mut self,
        registry: &proxima_core::FlavorRegistryFrozen,
    ) -> Result<Self, StorageError> {
        let surfaces = proxima_core::owner_inverse::OwnerSurfaces::try_for_registry(registry)
            .map_err(|error| StorageError::ConstraintViolation(error.to_string()))?;
        validate_host_lifecycle_registration(&surfaces, self.host_state.as_ref())?;
        validate_registered_state_tables(&surfaces, self.host_state.as_ref())?;
        self.search_projections = registry.search_projections().to_vec();
        self.embed_units = registry.embed_units().to_vec();
        self.non_embeddable_schemas = registry.non_embeddable_schema_ids().to_vec();
        self.surfaces = surfaces;
        self.scopes = crate::access::scope_surfaces::ScopeSurfaces::for_registry(registry);
        Ok(self)
    }

    /// The resolved lifecycle-scope declarations this storage fences on.
    #[must_use]
    pub fn scopes(&self) -> &crate::access::scope_surfaces::ScopeSurfaces {
        &self.scopes
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
        verbs::fact_embeddings::reconcile_embeddings_with_platform(
            &self.pool,
            self.platform_scope.as_ref(),
            options,
            self.embedding_runtime_policy.stale_claim_timeout_seconds(),
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
        verbs::fact_embeddings::sweep_orphan_embedding_rows(
            &self.pool,
            self.platform_scope.as_ref(),
        )
        .await
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
            self.platform_scope.as_ref(),
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
        verbs::maintenance::prune_change_log(&self.pool, self.platform_scope.as_ref(), options)
            .await
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
        verbs::maintenance::retry_cold_object_purges(
            &self.pool,
            self.platform_scope.as_ref(),
            self.cold.as_ref(),
            options,
        )
        .await
    }

    /// Refuse a pgvector that cannot serve this deployment's semantic
    /// search: below 0.8.0, or rejecting its HNSW session settings.
    ///
    /// [`Self::run_migrations`] runs this itself; a host that migrates
    /// through another path calls it once at boot.
    ///
    /// # Errors
    ///
    /// `StorageError::Unavailable` when the extension is missing, too old,
    /// or refuses the settings.
    pub async fn ensure_pgvector_compatible(&self) -> Result<(), StorageError> {
        ensure_pgvector_runtime_compatible(&self.pool, &self.tuning).await
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
        let migrated = async {
            let mut transaction = begin_migration_transaction(&mut conn).await?;
            core_migrator()
                .run(transaction.as_mut())
                .await
                .map_err(internal)?;
            transaction.commit().await.map_err(internal)
        }
        .await;
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
    /// Boot probes one lane index per supported width, and no other: a
    /// width added to `EmbeddingDim` must be probed here as well.
    #[test]
    fn boot_probes_every_width_lane() {
        let probe = "to_regclass('proxima_core.embeddings_hnsw_d";
        for dim in proxima_core::EmbeddingDim::ALL {
            assert!(
                super::EMBEDDING_SPACE_MARKERS.contains(&format!("{probe}{}')", dim.width())),
                "boot does not probe the {} lane",
                dim.width()
            );
        }
        assert_eq!(
            super::EMBEDDING_SPACE_MARKERS.matches(probe).count(),
            proxima_core::EmbeddingDim::ALL.len()
        );
    }

    #[test]
    fn core_migrator_is_the_v008_baseline_plus_additive_migrations() {
        let versions: Vec<i64> = super::core_migrator()
            .iter()
            .map(|migration| migration.version)
            .collect();
        assert_eq!(
            versions,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17],
            "v0.0.8 is one frozen file (0001_v008.sql) and every release after it appends: \
             v0.0.9 is 0002_v009_declaration_triggers.sql, v0.0.10 is \
             0003_v010_reference_integrity.sql, 0004_v011_goal_refs.sql, \
             the hard-erase witness is 0005_erased_pin_targets.sql, \
             0006_v013_goal_replay_declaration.sql, \
             0007_upload_content_identity.sql, 0008_cold_integrity_digest.sql, \
             0009_declared_sidecar_presence.sql, 0010_purge_queue_backend.sql \
             0011_v012_fact_outbox.sql, 0012_v013_publication_origin.sql and \
             0013_v015_agent_note_natural_key_index.sql, 0014_v015_owner_rls.sql, \
             0015_v016_embedding_spaces.sql, 0016_v016_embedding_claim_order.sql and \
             0017_v016_metadata_write_scope.sql"
        );
    }

    /// The v0.0.7 ALTER lane occupied versions 2..=21 and the squash to a
    /// single v008 baseline retired all of them. Every additive migration
    /// since reuses one of those retired numbers.
    ///
    /// That is safe, and this is why: the tripwire for a pre-v0.0.8 database
    /// is version 1's checksum, which is the legacy `0001_init.sql` and can
    /// never match `0001_v008.sql`. `ensure_core_ledger_compatible` compares
    /// it first and returns `SchemaResetRequired` before any additive version
    /// is reached, so no pre-v008 database can mistake a current migration for
    /// the legacy one it recorded. `pre_v008_database_fails_closed` holds that
    /// against a real database; this test does not, and cannot.
    ///
    /// What it does hold is content: every reused number carries ITS current
    /// migration, not merely some migration, so the reuse stays a decision
    /// rather than an accident. Which versions exist at all is
    /// [`core_migrator_is_the_v008_baseline_plus_additive_migrations`], which
    /// asserts the whole list by equality — a resurrected legacy file fails
    /// there, and naming an upper bound here only added a line to delete
    /// every time the head advanced.
    #[test]
    fn reused_versions_carry_their_current_migration() {
        let migrator = super::core_migrator();
        let carries = |version: i64, needles: &[&str], must: &str| {
            let migration = migrator
                .iter()
                .find(|migration| migration.version == version)
                .unwrap_or_else(|| panic!("version {version} is a current additive migration"));
            for needle in needles {
                assert!(
                    migration.sql.as_str().contains(needle),
                    "version {version} must {must}, not a resurrected legacy ALTER (no {needle})"
                );
            }
        };
        carries(
            2,
            &["assert_memory_declares_sidecar"],
            "be the v0.0.9 declaration-trigger migration",
        );
        carries(
            3,
            &["memory_pin_checks", "cooled_origins_no_null_chk"],
            "be the reference-integrity migration",
        );
        carries(
            4,
            &["goal_refs", "memory_goal_refs_no_null_chk"],
            "be the goal-reference split",
        );
        carries(
            5,
            &[
                "historical_restore",
                "NEW.goal_refs",
                "cooled_identity_seal",
            ],
            "restore witness-aware typed pin checks",
        );
        carries(
            6,
            &[
                "goal_replay_declaration",
                "goal_replay_declaration_append_only",
            ],
            "persist immutable Goal replay declarations",
        );
        carries(
            7,
            &[
                "blob_uploads_content_hash_chk",
                "blob_uploads_terminal_content_idx",
            ],
            "persist exact pre-publication upload content identity",
        );
        carries(
            8,
            &[
                "cold_digest",
                "cooled_cold_digest_len_chk",
                "NEW.cold_digest IS DISTINCT FROM OLD.cold_digest",
            ],
            "persist the exact cold-object digest witness",
        );
        carries(
            10,
            &["cold_purge_pending", "ADD COLUMN backend"],
            "give the durable purge queue its backend identity",
        );
        carries(
            11,
            &["publication_outbox", "publication_state"],
            "capture a listenable Fact's event in the Fact's own transaction",
        );
        carries(
            12,
            &[
                "publication_origin",
                "publication_origin_identity_immutable",
                "proxima_core.installation",
                "installation_identity_immutable",
            ],
            "pin a published Fact's immutable original owner and source, and mint \
             the installation identity a retained copy is attributed to",
        );
        carries(
            13,
            &["idx_agent_note_v1_nk", "agent_note_v1"],
            "index the natural key `core/agent-note-v1` declares",
        );
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
