//! An Engine over the example flavor on a split-role database, for the
//! substrate suites that used to run on the code flavor.
//!
//! The database is a clone of one [`HostTemplate`] holding core and the
//! example flavor's lane. The Engine is composed by hand rather than booted,
//! so a suite can register schemas of its own next to the flavor's. `pool` is the
//! admin connection: it reads every row the census needs and registers
//! notebooks past owner RLS.

use std::sync::{Arc, LazyLock};

use proxima::flavor::{FlavorBundle, PgSidecarRegistry, register_core_pg_sidecars};
use proxima::testkit::{HostTemplate, SplitRoleDb, TemplateFamily, describe_connection_error};
use proxima_core::{Engine, FlavorRegistry, OwnerId};
use proxima_example::ExampleFlavor;
use proxima_storage_pg::{PgMigrationStorage, PgPlatformScope, PgPoolConfig, PgStorage, PgTuning};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

/// Connections a fixture Engine's runtime and platform pools may open.
///
/// A test drives one Engine at a time, so the shipped default of ten only
/// gave a test room to hold idle connections the server's
/// `max_connections` is shared out of: nextest runs eight test processes at
/// once, each with several Engines and probe pools. Four leaves a nested
/// acquire and a concurrent statement room without queueing.
pub const ENGINE_POOL_CONNECTIONS: u32 = 4;

/// The pool policy of a fixture Engine: the shipped defaults, sized to
/// [`ENGINE_POOL_CONNECTIONS`].
#[must_use]
pub fn engine_pool_config() -> PgPoolConfig {
    PgPoolConfig {
        max_connections: ENGINE_POOL_CONNECTIONS,
        ..PgPoolConfig::default()
    }
}

/// A test's own pool (admin probes, a barrier, a hand-held writer) capped at
/// `max_connections`. Connected eagerly, so an unreachable server fails here.
///
/// # Errors
///
/// The connection error.
pub async fn fixture_pool(url: &str, max_connections: u32) -> Result<sqlx::PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(url)
        .await
}

/// Core and the example flavor, migrated once per server and cloned per test.
static TEMPLATE: LazyLock<HostTemplate> = LazyLock::new(|| {
    HostTemplate::new(
        TemplateFamily::new("proxima_example_engine_").expect("a valid template family"),
        "example engine",
        ExampleFlavor::migrators(),
        frozen_sidecars(),
    )
    .expect("core plus the example flavor is a lineup the runner accepts")
});

/// The same frozen sidecar composition boot uses before migrations.
pub fn frozen_sidecars() -> proxima::flavor::PgSidecarRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    ExampleFlavor::register(&mut registry).expect("example registration");
    let registry = registry.try_freeze().expect("example registry");
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    ExampleFlavor::register_pg_sidecars(&mut sidecars);
    sidecars
        .freeze_against(&registry)
        .expect("example sidecars")
}

/// A split-role clone of the template: core and the example flavor
/// migrated, nothing else composed. For suites that talk to Postgres
/// directly.
///
/// # Panics
///
/// When Postgres is unavailable.
pub async fn migrated_db(prefix: &str) -> SplitRoleDb {
    expect_described(
        "PG required for tests",
        TEMPLATE
            .clone_split_role(prefix, &["proxima_example"])
            .await,
    )
    .await
}

/// `result`'s value, or its error as `context: error`. When the error reads
/// as the server or a pool out of connections, the text also carries the
/// test server's connection use ([`describe_connection_error`]), so a setup
/// failure names its resource instead of only a pool timeout.
///
/// # Errors
///
/// The described error.
pub async fn described<T, E: std::fmt::Display>(
    context: &str,
    result: Result<T, E>,
) -> Result<T, String> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => Err(describe_connection_error(context, &error).await),
    }
}

/// [`described`] for setup that cannot go on without its value.
///
/// # Panics
///
/// With the described error.
pub async fn expect_described<T, E: std::fmt::Display>(context: &str, result: Result<T, E>) -> T {
    described(context, result)
        .await
        .unwrap_or_else(|why| panic!("{why}"))
}

/// Record store number 0 on a fresh clone, as a serving boot requires.
///
/// # Errors
///
/// The described migration-connection or store-number error.
pub async fn initialize_store_number(db: &SplitRoleDb) -> Result<(), String> {
    let migration = described(
        "migration connection",
        PgMigrationStorage::connect_with_config(
            db.platform_url(),
            PgPoolConfig::default(),
            PgTuning::default(),
        )
        .await,
    )
    .await?;
    described("store number", migration.initialize_store_number(0).await).await
}

/// One test's database and the Engine over it.
pub struct ExampleEngine {
    pub pool: sqlx::PgPool,
    pub engine: Engine,
    /// Dropped last: after a passing test the clone goes, after a panic it
    /// stays for inspection.
    pub db: SplitRoleDb,
}

impl ExampleEngine {
    /// Clone the template into `<prefix>_<id>` and compose an Engine over
    /// the example flavor plus whatever `extra` registers.
    ///
    /// # Panics
    ///
    /// When Postgres is unavailable or the composition is refused.
    pub async fn new(prefix: &str, extra: impl FnOnce(&mut FlavorRegistry)) -> Self {
        let db = migrated_db(prefix).await;
        initialize_store_number(&db)
            .await
            .unwrap_or_else(|why| panic!("{why}"));
        let engine = compose(db.runtime_url(), db.platform_url(), extra).await;
        let pool =
            expect_described("admin connection", fixture_pool(&db.admin_url(), 2).await).await;
        Self { pool, engine, db }
    }

    /// Register a notebook for `owner`, as every notebook-scoped note needs.
    ///
    /// # Panics
    ///
    /// When the registration is refused.
    pub async fn notebook(&self, owner: &OwnerId) -> Uuid {
        proxima_example::testkit::register_notebook(
            &self.pool,
            &proxima_core::test_fixtures::owner_scope_for_tests(&[*owner]),
            *owner,
            "fixture notebook",
        )
        .await
        .expect("the fixture notebook registers")
    }
}

/// Compose an Engine over the example flavor plus whatever `extra`
/// registers, on its own pools (sized by [`engine_pool_config`]) to an
/// already migrated database. A suite
/// that races two Engines on one database calls this once per task, so a
/// task blocked on a lock never holds a connection the test's probes need.
///
/// # Panics
///
/// When Postgres is unavailable or the composition is refused. A boot that
/// ran out of connections says how full the test server was.
pub async fn compose(
    runtime_url: &str,
    platform_url: &str,
    extra: impl FnOnce(&mut FlavorRegistry),
) -> Engine {
    let mut registry = FlavorRegistry::new();
    ExampleFlavor::register(&mut registry).expect("the example flavor registers");
    extra(&mut registry);
    let registry = registry.freeze_or_panic_for_tests();
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    ExampleFlavor::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars
        .freeze_against(&registry)
        .expect("the example sidecars match the registry");
    let platform_pool = expect_described(
        "platform connection",
        fixture_pool(platform_url, ENGINE_POOL_CONNECTIONS).await,
    )
    .await;
    let platform_scope = expect_described(
        "platform validation",
        PgPlatformScope::new(platform_pool, &["proxima_core", "proxima_example"]).await,
    )
    .await;
    let pg = expect_described(
        "runtime connection",
        PgStorage::connect_with_config(
            runtime_url,
            engine_pool_config(),
            PgTuning::default(),
            platform_scope,
            0,
        )
        .await,
    )
    .await
    .with_sidecars(sidecars)
    .with_flavors(&registry);
    Engine::new(registry).with_storage_ports(Arc::new(pg).storage_ports())
}

/// A clock barrier: returns the first instant in a later millisecond than
/// the call's, the resolution of the admission order (`t` is a v7 UUID).
pub async fn next_millisecond() -> time::OffsetDateTime {
    let start = time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
    loop {
        let now = time::OffsetDateTime::now_utc();
        if now.unix_timestamp_nanos() / 1_000_000 > start {
            return now;
        }
        tokio::task::yield_now().await;
    }
}
