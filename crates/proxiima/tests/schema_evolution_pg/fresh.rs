use super::*;

use proxiima::{Engine, FactWrite};
use proxiima_core::test_fixtures::authenticated_context;
use proxiima_pg_testkit::SplitRoleDb;

#[tokio::test]
async fn fresh_database_migrates_directly_to_v2_before_serving() {
    let (db, admin) = empty_database().await;
    let pg = PgMigrationStorage::connect_with_config(
        db.platform_url(),
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .expect("migration storage");
    let mut registry = SpiiceRegistry::new();
    register_v2(&mut registry).expect("fresh registry");
    let registry = registry.try_freeze().expect("fresh registry freezes");
    let sidecars = sidecars_v2()
        .freeze_against(&registry)
        .expect("fresh sidecars");
    migrate_after_failed_first_install(&pg, &admin, &sidecars).await;
    pg.initialize_store_number(0)
        .await
        .expect("store identity initialization");

    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT count(*) FROM proxiima_core.memory),
           (SELECT count(*) FROM proxiima_core.goal),
           (SELECT count(*) FROM proxiima_core.blob)",
    )
    .fetch_one(&admin)
    .await
    .expect("empty user graph");
    assert_eq!(counts, (0, 0, 0));
    let completion_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxiima_core.schema_evolution WHERE evolution_id='fact-v1-to-v2'",
    )
    .fetch_one(&admin)
    .await
    .expect("migration completion");
    assert_eq!(completion_count, 1);

    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    sqlx::query("INSERT INTO proxiima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(owner.into_inner())
        .execute(&admin)
        .await
        .expect("test owner");
    let pg_storage = super::storage(
        db.runtime_url(),
        db.platform_url(),
        &registry,
        sidecars_v2(),
    )
    .await;
    let engine = Engine::new(registry)
        .try_with_publication_config(PublicationConfig::new(
            PublicationSource::new("urn:proxiima:evolution").expect("publication source"),
        ))
        .expect("publication configuration")
        .with_storage_ports(Arc::new(pg_storage).storage_ports());
    let authz = authenticated_context(AuthzContext::single_owner(&owner, AuthPath::HostBearer));
    let written = engine
        .ingest_fact(
            &authz,
            FactWrite::new(
                owner,
                "fresh-v2-test",
                &FactV2 {
                    task_id: Uuid::now_v7(),
                    ended_at: 14,
                    text: "ordinary target write".into(),
                },
            ),
        )
        .await
        .expect("v2 write");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proxiima_core.memory WHERE t=$1")
            .bind(written.memory_id.into_inner())
            .fetch_one(&admin)
            .await
            .expect("written target"),
        1
    );
}

async fn empty_database() -> (SplitRoleDb, sqlx::PgPool) {
    let db = SplitRoleDb::create("proxiima_evolution_fresh_v2", &[])
        .await
        .expect("PG required");
    let admin = sqlx::PgPool::connect(&db.admin_url())
        .await
        .expect("admin pool");
    sqlx::query("DROP TABLE IF EXISTS public._sqlx_migrations")
        .execute(&admin)
        .await
        .expect("remove pre-created core ledger");
    let ledger_exists: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations')::text")
            .fetch_one(&admin)
            .await
            .expect("core ledger probe");
    assert_eq!(ledger_exists, None, "fresh database has no core ledger");
    (db, admin)
}

async fn migrate_after_failed_first_install(
    pg: &PgMigrationStorage,
    admin: &sqlx::PgPool,
    sidecars: &proxiima_storage_pg::PgSidecarRegistryFrozen,
) {
    let mut failing = migrator(evolve_retain);
    failing.migrations.to_mut().push(Migration::new(
        ANCHOR + 1,
        Cow::Borrowed("fail after schema evolution"),
        MigrationType::Simple,
        sqlx::AssertSqlSafe("SELECT 1 / 0;").into_sql_str(),
        false,
    ));
    let error = proxiima::run_core_and_spiice_migrations(
        pg,
        vec![NamedMigrator::spiice(SPIICE, failing)],
        sidecars,
    )
    .await
    .expect_err("a trailing migration failure rolls back the first install");
    assert!(error.to_string().contains("division by zero"), "{error}");
    let absent: (bool, bool, bool) = sqlx::query_as(
        "SELECT to_regclass('public._sqlx_migrations') IS NULL,
                to_regclass('public._sqlx_migrations_evolution_test') IS NULL,
                to_regclass('proxiima_core.memory') IS NULL",
    )
    .fetch_one(admin)
    .await
    .unwrap();
    assert_eq!(
        absent,
        (true, true, true),
        "no partial first install remains"
    );
    proxiima::run_core_and_spiice_migrations(
        pg,
        vec![NamedMigrator::spiice(SPIICE, migrator(evolve_retain))],
        sidecars,
    )
    .await
    .expect("explicit v2 migration after a rolled-back first install");
}
