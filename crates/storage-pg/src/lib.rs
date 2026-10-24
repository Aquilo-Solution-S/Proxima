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

/// Expansion paths for the exported sidecar macros (`$crate::__private::sqlx`,
/// `uuid`, and the column-type crates), so a flavor invoking `pg_sidecar!`
/// through the `proxima` facade needs none of them as a direct dependency.
#[doc(hidden)]
pub mod __private {
    pub use ::rust_decimal;
    pub use ::serde_json;
    pub use ::sqlx;
    pub use ::time;
    pub use ::uuid;
}

use proxima_core::StorageError;
use proxima_core::env_value;
use proxima_core::storage_ports::{StoragePorts, StoragePortsBuilder};
use sqlx::{PgConnection, PgPool};
use std::sync::Arc;
pub use verbs::fact_embeddings::{
    EmbeddingReconcileOptions, EmbeddingReconcileOutcome, EmbeddingReconcileScope,
};
pub use verbs::maintenance::{
    ColdPurgeRetryOptions, ColdPurgeRetryOutcome, MAX_FOREIGN_PURGE_STORES, MAX_STUCK_PURGE_DEBTS,
    ObjectPurgePassOutcome, StuckPurgeDebt,
};

use crate::error::internal;
use crate::pgvector::set_hnsw_search_sql;

#[doc(hidden)]
pub mod access;
mod baseline;
#[doc(hidden)]
pub mod definitions;
mod delegated_authority;
mod error;
mod lifecycle_scope;
mod maintenance_lease;
mod owner_scope;
#[cfg(test)]
mod pause_point;
#[doc(hidden)]
pub use error::map_err;
pub use error::{MAX_TRANSACTION_ATTEMPTS, is_transient_conflict};
pub mod integrity;
mod pg_enums;
pub use pg_enums::{PublicationDeliveryState, PublicationRecordState, PublicationStateParseError};
mod pg_ident;
mod pgvector;
mod platform_scope;
mod pool_config;
mod ports;
pub mod projection;
mod rls_guard;
pub mod search_candidates;
pub mod sidecars;
mod store_wide;
pub mod vector_index;
pub use store_wide::STORE_WIDE_TABLES;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_fixtures;
mod tuning;
pub mod verbs;
mod write_wake;
/// Stable, discoverable re-export of the exported `OwnerAccessPort` adapter
/// (see [`access::PgOwnerAccessResolver`]) for embedding hosts.
pub use access::PgOwnerAccessResolver;
/// Owner-registry and membership writes on a caller's platform transaction.
/// No authorization runs, the authorization hooks included: the host vouches
/// for these writes as for its own rows. No transaction control, scope setting or retry runs; the caller
/// retries its whole transaction. All effects follow from committed rows.
pub use access::owner_columns::{
    MembershipAddOutcome, add_group_member_on_connection as add_group_member,
    bootstrap_group_admin_on_connection as bootstrap_group_admin, register_group,
    remove_group_member_on_connection as remove_group_member,
    remove_group_member_relation_on_connection as remove_group_member_relation,
    replace_group_member_relation_on_connection as replace_group_member_relation,
};
/// Authority facts protected through a caller-owned platform transaction.
pub use access::{
    AuthorityTargets, DirectGroupMembership, DirectUserMembership, GroupContainment,
    PgAuthorityGroup, PgAuthorityPerson, PgAuthoritySession, RegisteredOwner,
};
/// Role probes on a caller's connection (see [`access::roles_for_subject`]):
/// what the resolver answers, for a host that already holds a transaction.
pub use access::{direct_group_relations, group_role, roles_for_subject};
pub use delegated_authority::PgDelegationStore;
pub use owner_scope::begin_owner_transaction;
pub use platform_scope::{PgPlatformScope, begin_migration_transaction};
pub use pool_config::PgPoolConfig;
pub use ports::{
    AgreedCommand, CommandRegistrationError, HostStateHandler, PgCommandDispatcher,
    PgHostStateLifecyclePort, PgHostStateParticipant, PgHostStateSession, SubjectMemberships,
};
pub use rls_guard::assert_runtime_rls;
pub use sidecars::{
    PgSidecarKey, PgSidecarRegistry, PgSidecarRegistryFrozen, core_pg_sidecars,
    register_core_pg_sidecars,
};
pub use tuning::{HnswIterativeScan, PgTuning};
pub use verbs::ingest_receipt::ingest_receipt;
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
pub use verbs::owner_export::export_statement_sql_for_tests;

/// Ledger namespace boundary between sequential core and dated flavor versions.
pub const CORE_MIGRATION_VERSION_CEILING: i64 = 9999;

/// Whether a host permits ledger versions absent from its migration sources.
///
/// Forgiveness is an explicit deployment choice, for example while two
/// releases share a database. It never relaxes checksums or schema checks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MissingMigrationPolicy {
    /// Refuse any recorded version that no composed source ships.
    #[default]
    Reject,
    /// Permit recorded versions absent from the composed sources.
    Allow,
}

/// Where a store keeps its vectors; it decides which part of the core schema
/// the store installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorStorage {
    /// In the store's own database, with the `vector` extension: the
    /// migration whose description starts with `pgvector` and the vector
    /// candidate functions apply.
    Local,
    /// In a host vector index ([`proxima_core::storage_ports::VectorIndex`]): the store
    /// writes no vector row, and its database needs no `vector` extension.
    HostIndex,
}

impl VectorStorage {
    /// Whether a store of this kind applies `migration`. A migration whose
    /// file name starts `pgvector` belongs to [`Self::Local`] alone.
    #[must_use]
    pub fn applies(self, migration: &sqlx::migrate::Migration) -> bool {
        match self {
            Self::Local => true,
            Self::HostIndex => !migration.description.starts_with("pgvector"),
        }
    }
}

/// One operation reported by the core schema planner.
#[derive(Debug, Clone)]
pub enum CoreSchemaAction {
    /// Apply an embedded `SQLx` migration.
    Migration(sqlx::migrate::Migration),
    /// Replace the mutable definitions bundle.
    Definitions { checksum: Vec<u8>, release: String },
}

/// Embedded core migration set under `crates/storage-pg/migrations/`: every
/// migration of every store kind, which is what the ledger is validated
/// against. [`core_migrator_for`] is the set a store applies.
///
/// `SQLx` ignores versions outside this source because composed host sources
/// can share its ledger. The migration facade validates their union; direct
/// backend migration runs validate the core ledger before applying anything.
#[must_use]
pub fn core_migrator() -> sqlx::migrate::Migrator {
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator
}

/// The core migrations a store of this kind applies, in order.
#[must_use]
pub fn core_migrator_for(vectors: VectorStorage) -> sqlx::migrate::Migrator {
    let mut migrator = core_migrator();
    let applied: Vec<sqlx::migrate::Migration> = migrator
        .iter()
        .filter(|migration| vectors.applies(migration))
        .cloned()
        .collect();
    migrator.migrations = std::borrow::Cow::Owned(applied);
    migrator
}

struct CoreSchemaState {
    migrator: sqlx::migrate::Migrator,
    recorded: std::collections::BTreeSet<i64>,
}

impl CoreSchemaState {
    async fn read(
        connection: &mut PgConnection,
        policy: MissingMigrationPolicy,
    ) -> Result<Self, StorageError> {
        let ledger_exists: bool =
            sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
                .fetch_one(&mut *connection)
                .await
                .map_err(internal)?;
        let rows: Vec<(i64, Vec<u8>, bool)> = if ledger_exists {
            sqlx::query_as(
                "SELECT version, checksum, success FROM public._sqlx_migrations
                  WHERE version <= $1 ORDER BY version",
            )
            .bind(CORE_MIGRATION_VERSION_CEILING)
            .fetch_all(&mut *connection)
            .await
            .map_err(internal)?
        } else {
            Vec::new()
        };
        let migrator = core_migrator();
        let first_version = migrator
            .iter()
            .next()
            .expect("embedded core baseline")
            .version;
        let mut recorded = std::collections::BTreeSet::new();
        for (version, checksum, success) in rows {
            if !success {
                return Err(internal(sqlx::migrate::MigrateError::Dirty(version)));
            }
            if version >= first_version {
                if let Some(migration) = migrator
                    .iter()
                    .find(|migration| migration.version == version)
                {
                    if checksum.as_slice() != migration.checksum.as_ref() {
                        return Err(StorageError::Internal(format!(
                            "core migration {version} checksum differs from the embedded file; restore the matching migration file or reset a development database"
                        )));
                    }
                } else if policy == MissingMigrationPolicy::Reject {
                    return Err(StorageError::Internal(format!(
                        "database was migrated by a newer release: core version {version} is not embedded"
                    )));
                }
            }
            recorded.insert(version);
        }
        let has_schema: bool = if recorded.contains(&first_version) {
            true
        } else {
            sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname LIKE 'proxima\\_%')",
            )
            .fetch_one(&mut *connection)
            .await
            .map_err(internal)?
        };
        baseline::check(&recorded, has_schema, first_version)?;
        Ok(Self { migrator, recorded })
    }
}

/// Report SQL migrations and definitions without changing the database.
///
/// The `policy` argument is `Allow` from the facade after composed host-ledger
/// and definitions validation, and `Reject` for direct storage runs.
///
/// # Errors
///
/// Refuses unsupported ledgers, changed migration checksums and newer releases.
pub async fn pending_core_schema(
    connection: &mut PgConnection,
    policy: MissingMigrationPolicy,
    vectors: VectorStorage,
) -> Result<Vec<CoreSchemaAction>, StorageError> {
    let state = CoreSchemaState::read(connection, policy).await?;
    let mut actions = Vec::new();
    for migration in state.migrator.iter() {
        if vectors.applies(migration) && !state.recorded.contains(&migration.version) {
            actions.push(CoreSchemaAction::Migration(migration.clone()));
        }
    }
    if !definitions::current_matches_with_policy(connection, policy, vectors).await? {
        actions.push(CoreSchemaAction::Definitions {
            checksum: definitions::checksum_bytes(vectors),
            release: proxima_core::RELEASE_VERSION.to_owned(),
        });
    }
    Ok(actions)
}

/// Run the core `SQLx` migrations a store of this kind applies, then its
/// definitions. The transaction holds the migration lock until commit.
///
/// ```compile_fail,E0308
/// fn migrate(connection: &mut sqlx::PgConnection) {
///     let _ = proxima_storage_pg::run_core_schema(
///         connection,
///         proxima_storage_pg::MissingMigrationPolicy::Reject,
///         proxima_storage_pg::VectorStorage::Local,
///     );
/// }
/// ```
///
/// The `policy` argument is `Allow` from the facade after composed host-ledger
/// and definitions validation, and `Reject` for direct storage runs.
///
/// # Errors
///
/// Refuses incompatible ledgers or definitions and propagates SQL failures.
pub async fn run_core_schema(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    policy: MissingMigrationPolicy,
    vectors: VectorStorage,
) -> Result<(), StorageError> {
    let connection = transaction.as_mut();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(i64::from_be_bytes(*b"proxmigr"))
        .execute(&mut *connection)
        .await
        .map_err(internal)?;
    CoreSchemaState::read(connection, policy).await?;
    core_migrator_for(vectors)
        .run_direct(None, &mut *connection, false)
        .await
        .map_err(internal)?;
    definitions::apply(connection, policy, vectors).await
}

/// Check core ledger compatibility on the caller's connection and policy.
///
/// Issues only `SELECT`s, including inside a caller's read-only snapshot.
///
/// The `policy` argument is `Allow` from the facade after composed host-ledger
/// and definitions validation, and `Reject` for direct storage runs.
///
/// # Errors
///
/// Refuses unsupported pre-reset ledgers, untracked schemas and changed
/// migration checksums. [`MissingMigrationPolicy::Reject`] also refuses
/// unknown current versions; the baseline and known checksums are mandatory
/// under both policies.
pub async fn ensure_core_ledger_compatible_on_connection_with_policy(
    connection: &mut PgConnection,
    policy: MissingMigrationPolicy,
) -> Result<(), StorageError> {
    CoreSchemaState::read(connection, policy).await.map(|_| ())
}

/// Require every embedded core version and the current definitions checksum.
/// This read-only preflight is used by hosts that skip migrations.
///
/// The `policy` argument is `Allow` from the facade after composed host-ledger
/// and definitions validation, and `Reject` for direct storage runs.
///
/// # Errors
///
/// Refuses incompatible ledgers, missing migrations or definitions, and newer
/// releases without issuing migration DDL.
pub async fn ensure_core_schema_current(
    pool: &PgPool,
    policy: MissingMigrationPolicy,
    vectors: VectorStorage,
) -> Result<(), StorageError> {
    let mut connection = pool.acquire().await.map_err(internal)?.detach();
    let mut transaction = begin_migration_transaction(&mut connection).await?;
    let result = async {
        let state = CoreSchemaState::read(&mut transaction, policy).await?;
        let pending: Vec<i64> = state
            .migrator
            .iter()
            .filter(|migration| {
                vectors.applies(migration) && !state.recorded.contains(&migration.version)
            })
            .map(|migration| migration.version)
            .collect();
        if !pending.is_empty() {
            return Err(StorageError::Internal(format!(
                "core migrations {pending:?} are not applied; run migrations before boot"
            )));
        }
        if !definitions::current_matches_with_policy(&mut transaction, policy, vectors).await? {
            return Err(StorageError::Internal(
                "database core definitions are not current; run migrations before boot".to_owned(),
            ));
        }
        Ok(())
    }
    .await;
    if result.is_ok() {
        transaction.commit().await.map_err(internal)?;
    }
    result
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
    let mut conn = pool.acquire().await.map_err(internal)?;
    ensure_pgvector_runtime_compatible_on(&mut conn, tuning).await
}

/// [`ensure_pgvector_runtime_compatible`] on a connection the caller holds.
async fn ensure_pgvector_runtime_compatible_on(
    conn: &mut PgConnection,
    tuning: &PgTuning,
) -> Result<(), StorageError> {
    let Some(version) = sqlx::query_scalar::<_, String>(
        "SELECT extversion FROM pg_extension WHERE extname = 'vector'",
    )
    .fetch_optional(&mut *conn)
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

    let mut tx = sqlx::Connection::begin(&mut *conn)
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
/// so classifying them that way gets the legs wrong, silently. `memory`
/// comes back `Keyed` instead of `Bespoke`, and `blob` and `content`
/// come back `Unreachable` instead of `Deduped`; the freeze-time validation
/// that a bespoke leg has a home never runs against them.
///
/// `for_registry` over a registry holding flavor #0 alone is the same call
/// a flavor makes over its own registry, and it is what `with_flavors`
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
    store_number: u16,
    pool: PgPool,
    /// Fixture setup/inspection only; runtime operations always use `pool`.
    #[cfg(feature = "test-fixtures")]
    test_admin_pool: Option<PgPool>,
    platform_scope: PgPlatformScope,
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
    /// Where the chunk vectors live: [`Self::with_vector_index`].
    vectors: vector_index::StoreVectors,
    cold: Arc<dyn proxima_core::ColdObjectStore>,
    /// Optional host-state participant invoked on the write-session
    /// transaction. `None` keeps existing
    /// [`proxima_core::engine::UnitOfWork`] Fact behavior with no extra
    /// configuration.
    host_state: Option<RegisteredHostStateParticipant>,
}

/// Migration-only connection. It has no serving authority and therefore does
/// not carry a platform scope; the host creates serving storage only after
/// migrations have committed and a platform role has been validated.
///
/// Serving storage cannot run schema migrations:
/// ```compile_fail
/// async fn migrate(storage: &proxima_storage_pg::PgStorage) {
///     storage.run_migrations().await.unwrap();
/// }
/// ```
#[derive(Debug, Clone)]
pub struct PgMigrationStorage {
    pool: PgPool,
    tuning: PgTuning,
    vectors: VectorStorage,
}

async fn run_migrations_on_pool(
    pool: &PgPool,
    tuning: &PgTuning,
    vectors: VectorStorage,
) -> Result<(), StorageError> {
    let mut conn = pool.acquire().await.map_err(internal)?;
    sqlx::query("SET statement_timeout = 0")
        .execute(&mut *conn)
        .await
        .map_err(internal)?;
    let migrated = async {
        let mut transaction = begin_migration_transaction(&mut conn).await?;
        run_core_schema(&mut transaction, MissingMigrationPolicy::Reject, vectors).await?;
        transaction.commit().await.map_err(internal)
    }
    .await;
    conn.detach();
    migrated?;
    match vectors {
        VectorStorage::Local => ensure_pgvector_runtime_compatible(pool, tuning).await,
        VectorStorage::HostIndex => Ok(()),
    }
}

fn validate_store_number(persisted: Option<i32>, configured: u16) -> Result<(), StorageError> {
    match persisted {
        Some(number) if number == i32::from(configured) => Ok(()),
        Some(number) => Err(StorageError::Unavailable(format!(
            "configured store number {configured} differs from persisted store number {number}"
        ))),
        None => Err(StorageError::Unavailable(
            "store has no persisted number; initialize it before serving".into(),
        )),
    }
}

impl PgMigrationStorage {
    /// Connect the separately configured schema-management lane.
    ///
    /// # Errors
    /// Returns pool configuration or database connection errors. Migration
    /// authority is validated when [`Self::run_migrations`] starts its transaction.
    pub async fn connect_with_config(
        url: &str,
        pool_config: PgPoolConfig,
        tuning: PgTuning,
    ) -> Result<Self, StorageError> {
        let pool_config = pool_config.validate()?;
        let pool = pool_config
            .pool_options()
            .connect_with(pool_config.connect_options(url)?)
            .await
            .map_err(|error| StorageError::Unavailable(error.to_string()))?;
        sqlx::query("SELECT 1")
            .execute(&pool)
            .await
            .map_err(|error| StorageError::Unavailable(error.to_string()))?;
        Ok(Self {
            pool,
            tuning,
            vectors: VectorStorage::Local,
        })
    }

    /// Where this store keeps its vectors.
    #[must_use]
    pub fn vector_storage(&self) -> VectorStorage {
        self.vectors
    }

    /// Install the schema of a store that keeps its vectors in `vectors`
    /// (default [`VectorStorage::Local`]). A host that gives its serving
    /// storage a vector index installs [`VectorStorage::HostIndex`] here, and
    /// the facade derives both from the same setting.
    #[must_use]
    pub fn with_vector_storage(mut self, vectors: VectorStorage) -> Self {
        self.vectors = vectors;
        self
    }

    /// Write this store's number on its first boot, or verify the number
    /// already persisted. Call after migrations and before serving connects.
    ///
    /// # Errors
    /// Refuses a number different from the database's immutable identity.
    pub async fn initialize_store_number(&self, store_number: u16) -> Result<(), StorageError> {
        let mut connection = self.pool.acquire().await.map_err(error::map_err)?;
        let mut tx = begin_migration_transaction(&mut connection).await?;
        sqlx::query(
            "INSERT INTO proxima_core.store_identity (singleton, store_number) VALUES (true, $1) ON CONFLICT (singleton) DO NOTHING",
        )
        .bind(i32::from(store_number))
        .execute(tx.as_mut())
        .await
        .map_err(error::map_err)?;
        let persisted = sqlx::query_scalar(
            "SELECT store_number FROM proxima_core.store_identity WHERE singleton",
        )
        .fetch_optional(tx.as_mut())
        .await
        .map_err(error::map_err)?;
        validate_store_number(persisted, store_number)?;
        tx.commit().await.map_err(error::map_err)
    }

    /// The migration runner's pool. Serving code cannot use this as a
    /// `PgStorage` without supplying a validated platform capability.
    #[cfg(any(feature = "backend-api", feature = "test-fixtures"))]
    #[doc(hidden)]
    #[must_use]
    pub fn clone_pool_for_backend(&self) -> PgPool {
        self.pool.clone()
    }

    /// # Errors
    ///
    /// Returns migration or database compatibility errors.
    pub async fn run_migrations(&self) -> Result<(), StorageError> {
        run_migrations_on_pool(&self.pool, &self.tuning, self.vectors).await
    }

    /// # Errors
    ///
    /// Returns an error when the installed pgvector cannot serve this deployment.
    pub async fn ensure_pgvector_compatible(&self) -> Result<(), StorageError> {
        match self.vectors {
            VectorStorage::Local => {
                ensure_pgvector_runtime_compatible(&self.pool, &self.tuning).await
            }
            VectorStorage::HostIndex => Ok(()),
        }
    }
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
    /// Acquire `owner`'s exclusive fence as the transaction's first erase
    /// lock. Callers that perform flavor-owned queries or DML before
    /// delegating a physical erase must call this immediately after BEGIN and
    /// before any source/handle/target locks. Only `owner`'s writes wait; the
    /// rows a flavor table keeps for `owner` are the ones its surface names in
    /// `owner_column`.
    ///
    /// # Errors
    /// Returns a storage error when `PostgreSQL` cannot acquire the fence.
    pub async fn lock_before_physical_erase(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        owner: &proxima_core::OwnerId,
    ) -> Result<(), StorageError> {
        crate::access::owner_columns::lock_owner_fence_exclusive_tx(tx, owner).await
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

/// A held embedding-maintenance lease: no other embedding maintenance pass
/// runs on this database while it is held.
///
/// The lease is a row of `proxima_core.maintenance_lease`, renewed in the
/// background while the guard lives. [`Self::release`] deletes it; dropping
/// the guard deletes it best effort, and a lease nobody renews expires after
/// a minute. Nothing about it is tied to one database session, so it holds
/// behind a transaction-mode pooler.
pub struct EmbeddingMaintenanceLock {
    lease: maintenance_lease::MaintenanceLease,
}

impl EmbeddingMaintenanceLock {
    /// Confirm the lease is still this pass's, extending it.
    ///
    /// # Errors
    ///
    /// [`StorageError::Unavailable`] when the lease expired and another
    /// pass may hold it; stop the pass.
    pub async fn renew(&self) -> Result<(), StorageError> {
        self.lease.renew().await
    }

    /// Delete the lease now, so the next pass need not wait for it to
    /// expire.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the delete; the lease then expires.
    pub async fn release(self) -> Result<(), StorageError> {
        self.lease.release().await
    }
}

impl std::fmt::Debug for EmbeddingMaintenanceLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddingMaintenanceLock")
            .finish_non_exhaustive()
    }
}

/// A held storage-maintenance lease: no other storage maintenance pass (the
/// object-purge drain and upload sweep, in process or from
/// `proxima-mcp maintain-storage`) runs on this database while it is held.
/// Same lease design as [`EmbeddingMaintenanceLock`], under its own name, so
/// the two families may run concurrently but never overlap themselves.
pub struct StorageMaintenanceLock {
    lease: maintenance_lease::MaintenanceLease,
}

impl StorageMaintenanceLock {
    /// Confirm the lease is still this pass's, extending it.
    ///
    /// # Errors
    ///
    /// [`StorageError::Unavailable`] when the lease expired and another
    /// pass may hold it; stop the pass.
    pub async fn renew(&self) -> Result<(), StorageError> {
        self.lease.renew().await
    }

    /// Delete the lease now, so the next pass need not wait for it to
    /// expire.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the delete; the lease then expires.
    pub async fn release(self) -> Result<(), StorageError> {
        self.lease.release().await
    }
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
    /// The immutable number validated against this database at boot.
    #[must_use]
    pub const fn store_number(&self) -> u16 {
        self.store_number
    }

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
    pub async fn connect(
        url: &str,
        platform_scope: PgPlatformScope,
        store_number: u16,
    ) -> Result<Self, StorageError> {
        Self::connect_with_config(
            url,
            PgPoolConfig::from_env()?,
            PgTuning::from_env()?,
            platform_scope,
            store_number,
        )
        .await
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
    pub async fn connect_with_tuning(
        url: &str,
        tuning: PgTuning,
        platform_scope: PgPlatformScope,
        store_number: u16,
    ) -> Result<Self, StorageError> {
        Self::connect_with_config(
            url,
            PgPoolConfig::from_env()?,
            tuning,
            platform_scope,
            store_number,
        )
        .await
    }

    /// Connect with fully resolved pool and query policy.
    ///
    /// This is the canonical host path: it never consults process environment.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Unavailable` on invalid policy, connection, or
    /// query failure, or when the installed pgvector is missing, below 0.8.0,
    /// or refuses this deployment's HNSW search settings. Refuses a runtime
    /// role that does not obey RLS on every schema `platform_scope` was
    /// validated over.
    pub async fn connect_with_config(
        url: &str,
        pool_config: PgPoolConfig,
        tuning: PgTuning,
        platform_scope: PgPlatformScope,
        store_number: u16,
    ) -> Result<Self, StorageError> {
        Self::connect_for_purpose(
            url,
            pool_config,
            tuning,
            platform_scope,
            store_number,
            vector_index::StoreVectors::Local,
        )
        .await
    }

    /// [`Self::connect_with_config`] for a store whose vectors live in `index`
    /// (#519): the store keeps each entity's head and calls the index as
    /// [`proxima_core::storage_ports::VectorIndex`] describes. Its database
    /// needs no `vector` extension, so the pgvector preflight does not run.
    ///
    /// # Errors
    ///
    /// As [`Self::connect_with_config`], except for the pgvector checks.
    pub async fn connect_with_vector_index(
        url: &str,
        pool_config: PgPoolConfig,
        tuning: PgTuning,
        platform_scope: PgPlatformScope,
        store_number: u16,
        index: Arc<dyn proxima_core::storage_ports::VectorIndex>,
    ) -> Result<Self, StorageError> {
        Self::connect_for_purpose(
            url,
            pool_config,
            tuning,
            platform_scope,
            store_number,
            vector_index::StoreVectors::Remote(index),
        )
        .await
    }

    async fn connect_for_purpose(
        url: &str,
        pool_config: PgPoolConfig,
        tuning: PgTuning,
        platform_scope: PgPlatformScope,
        store_number: u16,
        vectors: vector_index::StoreVectors,
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
        // Lazy, then one explicit acquire: boot validates on that one held
        // connection. An eager connect hands its first connection back in a
        // spawned task, and an acquire racing that return opens a second.
        // Acquiring per statement grew the pool toward its maximum during
        // validation, and on a server near `max_connections` each refused
        // connect was retried until the acquire timeout failed the boot.
        let pool = pool_config
            .pool_options()
            .connect_lazy_with(connect_options);
        let mut conn = pool
            .acquire()
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        sqlx::query("SELECT 1")
            .execute(&mut *conn)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        // Serving storage exists only over a runtime role that obeys RLS on
        // composed and stored-flavor schemas and a pgvector that accepts this
        // deployment's search settings. The platform authority reads the
        // existing surface catalog; no caller supplies a second inventory.
        let (schemas, persisted) = platform_scope.boot_catalog().await?;
        let schema_refs: Vec<_> = schemas.iter().map(String::as_str).collect();
        rls_guard::assert_runtime_rls_on(&mut conn, &schema_refs).await?;
        if matches!(vectors, vector_index::StoreVectors::Local) {
            ensure_pgvector_runtime_compatible_on(&mut conn, &tuning).await?;
        }
        drop(conn);
        validate_store_number(persisted, store_number)?;

        Ok(Self {
            store_number,
            pool,
            #[cfg(feature = "test-fixtures")]
            test_admin_pool: None,
            platform_scope,
            sidecars: core_pg_sidecars(),
            surfaces: flavor_0_surfaces(),
            scopes: crate::access::scope_surfaces::ScopeSurfaces::default(),
            search_projections: Vec::new(),
            embed_units: Vec::new(),
            non_embeddable_schemas: Vec::new(),
            tuning,
            embedding_runtime_policy: proxima_core::EmbeddingRuntimePolicy::default(),
            vectors,
            cold: Arc::new(verbs::forget::MemoryColdStore),
            host_state: None,
        })
    }

    /// Host-only platform capability for authority resolution and maintenance.
    #[must_use]
    pub fn platform_scope_for_host(&self) -> PgPlatformScope {
        self.platform_scope.clone()
    }

    pub(crate) async fn platform_transaction(
        &self,
    ) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, StorageError> {
        self.platform_scope.begin().await
    }

    /// Fixed lifecycle algorithms need global referential evidence, but their
    /// selected owner still comes from a verified, sealed write permit.
    pub(crate) async fn owner_maintenance_transaction(
        &self,
        permit: &proxima_core::storage_ports::OwnerWritePermit,
    ) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, StorageError> {
        crate::platform_scope::begin_owner_lifecycle_transaction(
            &self.pool,
            &self.platform_scope,
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

    /// Where this store keeps its vectors.
    #[must_use]
    pub fn vector_storage(&self) -> VectorStorage {
        match self.vectors {
            vector_index::StoreVectors::Local => VectorStorage::Local,
            vector_index::StoreVectors::Remote(_) => VectorStorage::HostIndex,
        }
    }

    /// Replace the object store erase purges blob objects from (S3 in the
    /// host).
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
        #[cfg(feature = "test-fixtures")]
        if let Some(pool) = &self.test_admin_pool {
            return pool;
        }
        &self.pool
    }

    /// Attach an independent fixture setup/inspection pool. Serving ports
    /// keep using the separately validated runtime connection.
    #[cfg(feature = "test-fixtures")]
    #[doc(hidden)]
    #[must_use]
    pub fn with_admin_pool_for_tests(mut self, pool: PgPool) -> Self {
        self.test_admin_pool = Some(pool);
        self
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
        self.storage_ports_builder().build()
    }

    /// [`Self::storage_ports`] before `build`, so a host can add the
    /// optional ports storage does not implement (`owner_drop_proof`).
    #[must_use]
    pub fn storage_ports_builder(self: Arc<Self>) -> StoragePortsBuilder {
        StoragePorts::builder()
            .fact_ingest(self.clone())
            .memory_authoring(self.clone())
            .memory_read(self.clone())
            .memory_inspect(self.clone())
            .embedding_text(self.clone())
            .embedding_write(self.clone())
            .embedding_job(self.clone())
            .embedding_maintenance(self.clone())
            .goal_write(self.clone())
            .goal_read(self.clone())
            .citation(self.clone())
            .owner_access_read(self.clone())
            .owner_membership_admin(self.clone())
            .owner_transfer(self.clone())
            .source_cursor(self.clone())
            .owner_erase(self.clone())
            .write_session(self)
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
            &self.platform_scope,
            options,
            self.embedding_runtime_policy.stale_claim_timeout_seconds(),
        )
        .await
    }

    /// Delete embedding infrastructure rows whose source entity no longer
    /// exists (crash residue); with a host vector index, also compare the
    /// index with the heads and repair what differs. Operator surface for the maintenance CLI,
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
        let mut outcome =
            verbs::fact_embeddings::sweep_orphan_embedding_rows(&self.pool, &self.platform_scope)
                .await?;
        match &self.vectors {
            vector_index::StoreVectors::Local => {}
            vector_index::StoreVectors::Remote(index) => {
                let swept =
                    vector_index::sweep_index(&self.pool, &self.platform_scope, index.as_ref())
                        .await?;
                outcome.embeddings_deleted += swept.entries_deleted;
                outcome.heads_deleted += swept.heads_deleted;
            }
        }
        Ok(outcome)
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
            &self.platform_scope,
            self.embedding_runtime_policy.stale_claim_timeout_seconds(),
            self.vector_storage(),
        )
        .await
    }

    /// Try to take the embedding-maintenance lease.
    ///
    /// Returns `None` when another embedding maintenance pass holds a live
    /// lease, so overlapping cron fires skip instead of double-scanning.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the lease transaction.
    pub async fn try_embedding_maintenance_lock(
        &self,
    ) -> Result<Option<EmbeddingMaintenanceLock>, StorageError> {
        Ok(maintenance_lease::MaintenanceLease::try_acquire(
            &self.platform_scope,
            maintenance_lease::LeaseName::Embedding,
        )
        .await?
        .map(|lease| EmbeddingMaintenanceLock { lease }))
    }

    /// Try to take the storage-maintenance lease.
    ///
    /// Same contract as [`Self::try_embedding_maintenance_lock`], under its
    /// own name: `None` means another storage maintenance pass holds a live
    /// lease and this run should skip.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the lease transaction.
    pub async fn try_storage_maintenance_lock(
        &self,
    ) -> Result<Option<StorageMaintenanceLock>, StorageError> {
        Ok(maintenance_lease::MaintenanceLease::try_acquire(
            &self.platform_scope,
            maintenance_lease::LeaseName::Storage,
        )
        .await?
        .map(|lease| StorageMaintenanceLock { lease }))
    }

    /// Drain one bounded batch of the wired object store's due purge debts.
    ///
    /// Only debts naming the wired store and due now are selected, oldest
    /// schedule first. Each is deleted with no database transaction open,
    /// then cleared, or its failed attempt is recorded and the debt backs
    /// off, so a failing key never holds back the keys behind it. Debts
    /// owed to any other store are left untouched and counted as foreign.
    /// Call again while the outcome's `due` is non-zero to work through a
    /// backlog.
    ///
    /// # Errors
    ///
    /// Returns storage errors while reading or reconciling pending rows, and a
    /// constraint violation for a non-positive batch size or stuck threshold.
    pub async fn retry_cold_object_purges(
        &self,
        options: ColdPurgeRetryOptions,
    ) -> Result<ColdPurgeRetryOutcome, StorageError> {
        verbs::maintenance::retry_cold_object_purges(
            &self.pool,
            &self.platform_scope,
            self.cold.as_ref(),
            options,
        )
        .await
    }

    /// One object-purge pass: take the storage maintenance lease, then drain
    /// the wired store's due debts in batches of `options`' size until none
    /// is due or `max_batches` ran.
    ///
    /// This is the loop every host would otherwise write, and the one the
    /// Proxima runtime runs on its own cadence. When another process holds
    /// the lock (another runtime, or `proxima-mcp maintain-storage`), the
    /// pass drains nothing and reports `skipped`, with current queue counts.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the lock, the drain or the census, and a
    /// constraint violation for invalid options or a dry run.
    pub async fn run_object_purge_pass(
        &self,
        options: ColdPurgeRetryOptions,
        max_batches: u32,
    ) -> Result<ObjectPurgePassOutcome, StorageError> {
        let Some(lock) = self.try_storage_maintenance_lock().await? else {
            return verbs::maintenance::undrained_purge_pass(
                &self.platform_scope,
                self.cold.as_ref(),
                options,
                true,
            )
            .await;
        };
        let pass = self
            .drain_due_object_purge_debt(&lock, options, max_batches)
            .await;
        if let Err(error) = lock.release().await {
            tracing::debug!(%error, "storage maintenance lease release failed; it expires instead");
        }
        pass
    }

    /// [`Self::run_object_purge_pass`] for a caller that already holds the
    /// storage maintenance lease, and so may combine the drain with other
    /// maintenance under one lease. The lease is renewed before each batch.
    ///
    /// # Errors
    ///
    /// As [`Self::run_object_purge_pass`], and [`StorageError::Unavailable`]
    /// when the lease expired before a batch: the pass stops there.
    pub async fn drain_due_object_purge_debt(
        &self,
        held: &StorageMaintenanceLock,
        options: ColdPurgeRetryOptions,
        max_batches: u32,
    ) -> Result<ObjectPurgePassOutcome, StorageError> {
        verbs::maintenance::drain_due_purge_debt(
            &self.pool,
            &self.platform_scope,
            self.cold.as_ref(),
            &held.lease,
            options,
            max_batches,
        )
        .await
    }

    /// The queue the wired store sees, without draining: due, deferred and
    /// stuck debts, and debt owed to other stores.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the census, and a constraint violation
    /// for a non-positive stuck threshold.
    pub async fn object_purge_census(
        &self,
        options: ColdPurgeRetryOptions,
    ) -> Result<ObjectPurgePassOutcome, StorageError> {
        verbs::maintenance::undrained_purge_pass(
            &self.platform_scope,
            self.cold.as_ref(),
            options,
            false,
        )
        .await
    }
}
