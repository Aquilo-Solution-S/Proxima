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
use proxima::testkit::{HostTemplate, SplitRoleDb, TemplateFamily};
use proxima_core::{Engine, FlavorRegistry, OwnerId};
use proxima_example::ExampleFlavor;
use proxima_storage_pg::{PgMigrationStorage, PgPlatformScope, PgPoolConfig, PgStorage, PgTuning};
use uuid::Uuid;

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
    TEMPLATE
        .clone_split_role(prefix, &["proxima_example"])
        .await
        .expect("PG required for tests")
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
        PgMigrationStorage::connect_with_config(
            db.platform_url(),
            PgPoolConfig::default(),
            PgTuning::default(),
        )
        .await
        .expect("migration connection")
        .initialize_store_number(0)
        .await
        .expect("store number");
        let engine = compose(db.runtime_url(), db.platform_url(), extra).await;
        let pool = sqlx::PgPool::connect(&db.admin_url())
            .await
            .expect("admin connection");
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
/// registers, on its own pools to an already migrated database. A suite
/// that races two Engines on one database calls this once per task, so a
/// task blocked on a lock never holds a connection the test's probes need.
///
/// # Panics
///
/// When Postgres is unavailable or the composition is refused.
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
    let platform_pool = sqlx::PgPool::connect(platform_url)
        .await
        .expect("platform connection");
    let platform_scope = PgPlatformScope::new(platform_pool, &["proxima_core", "proxima_example"])
        .await
        .expect("platform validation");
    let pg = PgStorage::connect(runtime_url, platform_scope, 0)
        .await
        .expect("runtime connection")
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
