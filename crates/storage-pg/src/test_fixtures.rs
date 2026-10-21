use proxima_core::StorageError;
use std::sync::Arc;

use proxima_storage_pg::{
    PgPlatformScope, PgPoolConfig, PgStorage, PgTuning, VectorStorage, core_migrator_for,
};

use proxima_pg_testkit::{
    DbGuard, FNV_OFFSET_BASIS, TemplateFamily, create_db_from_template,
    create_named_db_from_template, db_url, drop_db, ensure_template, ensure_template_in,
    fnv1a64_extend, split_role_urls, split_role_urls_without_pgvector,
};
use sqlx::Connection;

/// The test roles for `database`; a host-index database gets no `vector`
/// extension.
async fn roles_for(
    database: &str,
    vectors: VectorStorage,
) -> Result<(String, String), sqlx::Error> {
    match vectors {
        VectorStorage::Local => split_role_urls(database).await,
        VectorStorage::HostIndex => split_role_urls_without_pgvector(database).await,
    }
}

/// The host-index kind's templates are a family of their own: a template
/// build drops the idle templates of its family, and the two kinds must not
/// evict each other's.
fn host_index_family() -> TemplateFamily {
    TemplateFamily::new("proxima_tmpl_corehi_").expect("a valid template family")
}

/// What the template of this store kind is a function of: its migrations, its
/// definitions and the release.
fn core_template_hash(vectors: VectorStorage) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for migration in core_migrator_for(vectors).iter() {
        hash = fnv1a64_extend(hash, &migration.version.to_be_bytes());
        hash = fnv1a64_extend(hash, migration.checksum.as_ref());
    }
    hash = fnv1a64_extend(
        hash,
        proxima_storage_pg::definitions::checksum_hex(vectors).as_bytes(),
    );
    fnv1a64_extend(hash, proxima_core::RELEASE_VERSION.as_bytes())
}

#[must_use]
pub fn core_template_name(vectors: VectorStorage) -> String {
    let hash = core_template_hash(vectors);
    match vectors {
        VectorStorage::Local => format!("proxima_tmpl_core_{hash:016x}"),
        VectorStorage::HostIndex => host_index_family().template_name(hash),
    }
}

/// Build the full core schema template once per migration set.
///
/// # Errors
///
/// Returns admin connection, template build, or migration errors.
pub async fn ensure_core_template(vectors: VectorStorage) -> Result<String, sqlx::Error> {
    let template = core_template_name(vectors);
    let build = |pool: sqlx::PgPool| async move {
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await?;
        let (_runtime_url, platform_url) = roles_for(&database, vectors).await?;
        pool.close().await;
        let mut connection = sqlx::PgConnection::connect(&platform_url).await?;
        let mut transaction = proxima_storage_pg::begin_migration_transaction(&mut connection)
            .await
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        proxima_storage_pg::run_core_schema(
            &mut transaction,
            proxima_storage_pg::MissingMigrationPolicy::Reject,
            vectors,
        )
        .await
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        transaction.commit().await
    };
    match vectors {
        VectorStorage::Local => ensure_template(&template, build).await?,
        VectorStorage::HostIndex => {
            ensure_template_in(&host_index_family(), core_template_hash(vectors), build)
                .await?
                .release()
                .await?;
        }
    }
    Ok(template)
}

/// Create `name` already migrated through the full core template of a store
/// with pgvector vectors, cloned from that template. Drop it with `drop_db`
/// like any other test database.
///
/// # Errors
///
/// Returns admin connection, template build, or clone errors.
pub async fn create_core_db(name: &str) -> Result<(), sqlx::Error> {
    let template = ensure_core_template(VectorStorage::Local).await?;
    create_named_db_from_template(name, &template).await
}

/// Clone a fresh test database from the core migrated template.
///
/// The returned [`DbGuard`] drops the clone when the test passes and keeps
/// it (printing a `psql` URL) when the test panics.
///
/// # Panics
///
/// Panics when the local test Postgres admin connection, template
/// creation, clone creation, or cloned database connection fails.
pub async fn fresh_pg(prefix: &str) -> (PgStorage, DbGuard) {
    fresh_pg_with(prefix, None).await
}

/// [`fresh_pg`] for a store whose vectors live in `index`: its database has
/// no `vector` extension.
///
/// # Panics
///
/// As [`fresh_pg`].
pub async fn fresh_pg_host_index(
    prefix: &str,
    index: Arc<dyn proxima_core::storage_ports::VectorIndex>,
) -> (PgStorage, DbGuard) {
    fresh_pg_with(prefix, Some(index)).await
}

async fn fresh_pg_with(
    prefix: &str,
    index: Option<Arc<dyn proxima_core::storage_ports::VectorIndex>>,
) -> (PgStorage, DbGuard) {
    let vectors = match index {
        Some(_) => VectorStorage::HostIndex,
        None => VectorStorage::Local,
    };
    let template = ensure_core_template(vectors)
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but admin connect failed: {e}"));

    let db_name = create_db_from_template(prefix, &template)
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but admin connect failed: {e}"));
    let (runtime_url, platform_url) = roles_for(&db_name, vectors)
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but role provisioning failed: {e}"));
    let migration = proxima_storage_pg::PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .unwrap_or_else(|e| panic!("PG required for tests but migration connect failed: {e}"))
    .with_vector_storage(vectors);
    migration
        .run_migrations()
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but migration failed: {e}"));
    migration
        .initialize_store_number(1)
        .await
        .expect("fixture store identity");
    let platform_pool = serving_platform_pool(&platform_url)
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but platform connect failed: {e}"));
    let platform_scope = PgPlatformScope::new(platform_pool, &["proxima_core"])
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but platform validation failed: {e}"));
    let runtime_url = proxima_pg_testkit::through_test_pooler(&runtime_url);
    let connected = match index {
        Some(index) => {
            PgStorage::connect_with_vector_index(
                &runtime_url,
                PgPoolConfig::default(),
                PgTuning::default(),
                platform_scope,
                1,
                index,
            )
            .await
        }
        None => {
            PgStorage::connect_with_config(
                &runtime_url,
                PgPoolConfig::default(),
                PgTuning::default(),
                platform_scope,
                1,
            )
            .await
        }
    };
    let storage = match connected {
        Ok(pg) => pg,
        Err(err) => {
            let _ = drop_db(&db_name).await;
            panic!("PG required for tests but unavailable: {err}");
        }
    };
    let admin_pool = sqlx::PgPool::connect(&db_url(&db_name))
        .await
        .unwrap_or_else(|e| panic!("PG required for tests but admin pool failed: {e}"));
    (
        storage.with_admin_pool_for_tests(admin_pool),
        DbGuard::adopt(db_name),
    )
}

/// The fixture's platform pool: direct, or through the transaction-mode
/// pooler named by `PROXIMA_TEST_PGBOUNCER_URL` with the serving pool policy
/// (a pooler refuses the startup parameters a bare `sqlx` connect sends).
async fn serving_platform_pool(platform_url: &str) -> Result<sqlx::PgPool, StorageError> {
    if proxima_pg_testkit::test_pooler_url().is_some() {
        return PgPoolConfig::default()
            .connect_lazy(&proxima_pg_testkit::through_test_pooler(platform_url));
    }
    sqlx::PgPool::connect(platform_url)
        .await
        .map_err(|error| StorageError::Unavailable(error.to_string()))
}

/// Connect a disposable database through the real split-role boot sequence.
///
/// Test callers pass the administrator URL used to create the database. The
/// database name is taken from its final path component; role provisioning,
/// migrations, platform validation, and runtime connection then happen in
/// the same order as production boot.
///
/// # Errors
///
/// Refuses an invalid database URL or failed role provisioning, migration,
/// platform validation, or database connection.
#[cfg(feature = "test-fixtures")]
pub async fn connect_for_tests(url: String) -> Result<PgStorage, StorageError> {
    let database = url
        .rsplit('/')
        .next()
        .and_then(|path| path.split('?').next())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            StorageError::Unavailable("test database URL has no database name".into())
        })?;
    let (runtime_url, platform_url) = split_role_urls(database)
        .await
        .map_err(|error| StorageError::Unavailable(error.to_string()))?;
    let migration = proxima_storage_pg::PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await?;
    migration.run_migrations().await?;
    migration.initialize_store_number(1).await?;
    let platform_pool = serving_platform_pool(&platform_url).await?;
    let platform_scope = PgPlatformScope::new(platform_pool, &["proxima_core"]).await?;
    let storage = PgStorage::connect_with_config(
        &proxima_pg_testkit::through_test_pooler(&runtime_url),
        PgPoolConfig::default(),
        PgTuning::default(),
        platform_scope,
        1,
    )
    .await?;
    let admin_pool = sqlx::PgPool::connect(&url)
        .await
        .map_err(|error| StorageError::Unavailable(error.to_string()))?;
    Ok(storage.with_admin_pool_for_tests(admin_pool))
}

impl PgStorage {
    /// Test-only serving connection using the real split-role boot sequence.
    ///
    /// # Errors
    ///
    /// Returns the provisioning, migration, validation, or connection error
    /// from [`connect_for_tests`].
    #[cfg(feature = "test-fixtures")]
    pub async fn connect_for_tests(url: String) -> Result<Self, StorageError> {
        connect_for_tests(url).await
    }
}

/// Register `group` as a group in `proxima_core.owners`, the way a host
/// creates one before writing under it. Idempotent; an id already
/// registered as a user is a `Conflict`.
///
/// # Errors
///
/// Storage errors from the registration.
pub async fn register_group_for_tests(
    pool: &sqlx::PgPool,
    group: proxima_core::GroupId,
) -> Result<(), proxima_core::StorageError> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|err| proxima_core::StorageError::Unavailable(err.to_string()))?;
    crate::access::owner_columns::register_group(&mut conn, group).await
}

/// Stable writer for synthetic storage rows with no authenticated request.
#[must_use]
pub const fn writer() -> proxima_core::WriterAttribution {
    proxima_core::WriterAttribution {
        by: uuid::Uuid::from_u128(0x509),
        via: None,
    }
}
