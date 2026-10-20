//! Unknown ledger versions are refused across the entire composition before DDL.

#[path = "fixtures/setup_error.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod setup_error;
use setup_error::expect_described;
use std::borrow::Cow;

#[path = "../../storage-pg/tests/support/migration_reset.rs"]
mod migration_reset_support;
use migration_reset_support::{apply_pre_identity_schema, pre_reset_pairs};
use proxima::{
    CoreAdoption, MigrationError, NamedMigrator, PendingMigration, pending_migrations,
    preflight_without_migrations, run_core_and_flavor_migrations,
};
use proxima_pg_testkit::SplitRoleDb;
use proxima_storage_pg::{
    PgMigrationStorage, PgPoolConfig, PgTuning, adopt::PRE_RESET, core_migrator,
};
use sqlx::migrate::{MigrateError, Migration, MigrationType, Migrator};
use sqlx::{PgPool, SqlSafeStr};

const FIRST: i64 = 20_991_001_000_010;
const UNKNOWN: i64 = FIRST + 1;

async fn core_history_db(count: usize) -> SplitRoleDb {
    let db = expect_described(
        "owned PG required",
        SplitRoleDb::create("proxima_policy_adoption", &[]).await,
    )
    .await;
    let platform =
        expect_described("platform pool", PgPool::connect(db.platform_url()).await).await;
    expect_described(
        "schema before store identities",
        apply_pre_identity_schema(&platform).await,
    )
    .await;
    platform.close().await;
    let admin = expect_described("admin pool", PgPool::connect(&db.admin_url()).await).await;
    sqlx::query("DELETE FROM public._sqlx_migrations")
        .execute(&admin)
        .await
        .expect("clear current core ledger");
    for (version, checksum) in pre_reset_pairs(&PRE_RESET).into_iter().take(count) {
        sqlx::query(
            "INSERT INTO public._sqlx_migrations
                 (version, description, success, checksum, execution_time)
             VALUES ($1, 'pre-reset', true, $2, 0)",
        )
        .bind(version)
        .bind(checksum)
        .execute(&admin)
        .await
        .expect("pre-reset ledger row");
    }
    admin.close().await;
    db
}

#[tokio::test]
async fn the_facade_adopts_the_complete_pre_reset_main_ledger() {
    let db = core_history_db(PRE_RESET.len()).await;
    let pg = storage(&db).await;
    let before = pending_migrations(&pg.clone_pool_for_backend(), &[])
        .await
        .expect("complete pre-reset history is recognized by the planner");
    assert!(
        before
            .iter()
            .any(|pending| matches!(pending, PendingMigration::CoreAdoption { .. }))
    );

    run_core_and_flavor_migrations(
        &pg,
        Vec::new(),
        &proxima::PgSidecarRegistryFrozen::default(),
    )
    .await
    .expect("complete pre-reset history is adopted");
    preflight_without_migrations(
        &pg,
        Vec::new(),
        &proxima::PgSidecarRegistryFrozen::default(),
    )
    .await
    .expect("adopted history passes facade preflight");

    let pool = pg.clone_pool_for_backend();
    let expected: Vec<i64> = PRE_RESET
        .iter()
        .map(|(version, _)| *version)
        .chain(core_migrator().iter().map(|migration| migration.version))
        .collect();
    let rows: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM public._sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .expect("core ledger");
    assert_eq!(
        rows, expected,
        "adoption retains history and applies current migrations"
    );
}

#[tokio::test]
async fn the_facade_classifies_the_v0035_shaped_ledger_as_adoption() {
    let db = core_history_db(30).await;
    let pg = storage(&db).await;
    let pool = pg.clone_pool_for_backend();
    let ledger_before: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM public._sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .expect("v0.0.35 ledger");
    let plan = pending_migrations(&pg.clone_pool_for_backend(), &[])
        .await
        .expect("v0.0.35 history is recognized by the planner");
    assert!(plan.iter().any(|pending| matches!(
        pending,
        PendingMigration::CoreAdoption {
            adoption: CoreAdoption::Bridge,
            ..
        }
    )));
    let after: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM public._sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .expect("v0.0.35 ledger after plan");
    assert_eq!(after, ledger_before, "planning must not run the bridge");
}

fn source(id: &'static str, version: i64, shared: bool) -> NamedMigrator {
    let migrator = Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            version,
            Cow::Borrowed("policy fixture"),
            MigrationType::Simple,
            sqlx::AssertSqlSafe(
                "CREATE TABLE IF NOT EXISTS public.policy_fixture (id int)".to_owned(),
            )
            .into_sql_str(),
            false,
        )]),
        ..Migrator::DEFAULT
    };
    if shared {
        NamedMigrator::new(id, migrator)
    } else {
        NamedMigrator::flavor(id, migrator)
    }
}

async fn storage(db: &SplitRoleDb) -> PgMigrationStorage {
    expect_described(
        "migration storage",
        PgMigrationStorage::connect_with_config(
            db.platform_url(),
            PgPoolConfig::default(),
            PgTuning::default(),
        )
        .await,
    )
    .await
}

#[tokio::test]
async fn an_unknown_version_on_a_later_ledger_prevents_every_schema_change() {
    let db = expect_described(
        "owned PG required",
        SplitRoleDb::create("proxima_policy_before_ddl", &[]).await,
    )
    .await;
    let pg = storage(&db).await;
    let admin = expect_described("admin pool", PgPool::connect(&db.admin_url()).await).await;
    sqlx::raw_sql("CREATE TABLE public._sqlx_migrations_later (version bigint PRIMARY KEY, description text NOT NULL, installed_on timestamptz NOT NULL DEFAULT now(), success boolean NOT NULL, checksum bytea NOT NULL, execution_time bigint NOT NULL)")
        .execute(&admin).await.expect("later ledger");
    sqlx::query("INSERT INTO public._sqlx_migrations_later (version, description, success, checksum, execution_time) VALUES ($1, 'unknown', true, ''::bytea, 0)")
        .bind(UNKNOWN).execute(&admin).await.expect("unknown row");
    sqlx::query("ALTER TABLE public._sqlx_migrations_later OWNER TO proxima_test_platform")
        .execute(&admin)
        .await
        .expect("platform owns its migration ledger");
    let before: Vec<String> = sqlx::query_scalar("SELECT schemaname || '.' || tablename FROM pg_tables WHERE schemaname NOT IN ('pg_catalog', 'information_schema') ORDER BY 1")
        .fetch_all(&admin).await.expect("table census");
    let sources = || {
        vec![
            source("earlier", FIRST, false),
            source("later", FIRST + 2, false),
        ]
    };
    let planned = pending_migrations(&admin, &sources())
        .await
        .expect_err("unknown ledger version must refuse a plan");
    let ran = run_core_and_flavor_migrations(
        &pg,
        sources(),
        &proxima::PgSidecarRegistryFrozen::default(),
    )
    .await
    .expect_err("unknown ledger version must refuse a run");
    assert!(
        matches!(
            planned,
            MigrationError::Flavor {
                source: "later",
                err: MigrateError::VersionMissing(UNKNOWN)
            }
        ),
        "{planned:?}"
    );
    assert_eq!(format!("{planned:?}"), format!("{ran:?}"));
    let after: Vec<String> = sqlx::query_scalar("SELECT schemaname || '.' || tablename FROM pg_tables WHERE schemaname NOT IN ('pg_catalog', 'information_schema') ORDER BY 1")
        .fetch_all(&admin).await.expect("table census");
    assert_eq!(after, before, "refused run changed the schema");
    let core_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM public._sqlx_migrations")
        .fetch_one(&admin)
        .await
        .expect("core ledger count");
    assert_eq!(core_rows, 0, "refused run applied core migrations");
    admin.close().await;
}

#[tokio::test]
async fn a_shared_ledger_recognizes_every_composed_source() {
    let db = expect_described(
        "owned PG required",
        SplitRoleDb::create("proxima_policy_union", &[]).await,
    )
    .await;
    let pg = storage(&db).await;
    let sources = || {
        vec![
            source("first", FIRST, true),
            source("second", FIRST + 1, true),
        ]
    };
    expect_described(
        "initial composition",
        run_core_and_flavor_migrations(
            &pg,
            sources(),
            &proxima::PgSidecarRegistryFrozen::default(),
        )
        .await,
    )
    .await;
    run_core_and_flavor_migrations(&pg, sources(), &proxima::PgSidecarRegistryFrozen::default())
        .await
        .expect("all shared sources recognized");
    let pool = pg.clone_pool_for_backend();
    assert!(
        pending_migrations(&pool, &sources())
            .await
            .expect("current composition")
            .is_empty()
    );
    let omitted = run_core_and_flavor_migrations(
        &pg,
        vec![source("first", FIRST, true)],
        &proxima::PgSidecarRegistryFrozen::default(),
    )
    .await
    .expect_err("omitting a shared source is not silently forgiven");
    assert!(
        matches!(
            omitted,
            MigrationError::Core(MigrateError::VersionMissing(UNKNOWN))
        ),
        "{omitted:?}"
    );
}

#[tokio::test]
async fn missing_versions_need_an_explicit_policy_for_run_plan_and_preflight() {
    use proxima::{
        MissingMigrationPolicy, pending_migrations_with_policy, preflight_without_migrations,
        preflight_without_migrations_with_policy, run_core_and_flavor_migrations_with_policy,
    };

    let db = expect_described(
        "owned PG required",
        SplitRoleDb::create("proxima_policy_opt_in", &[]).await,
    )
    .await;
    let pg = storage(&db).await;
    let pool = pg.clone_pool_for_backend();
    run_core_and_flavor_migrations(
        &pg,
        vec![source("first", FIRST, true)],
        &proxima::PgSidecarRegistryFrozen::default(),
    )
    .await
    .expect("initial host migration");
    // A second release also applied a core-namespace version. Both ledger
    // ranges must use the host's explicit policy, not a hidden core exception.
    sqlx::query("INSERT INTO public._sqlx_migrations (version, description, success, checksum, execution_time) VALUES (9000, 'other release', true, ''::bytea, 0)")
        .execute(&pool).await.expect("unknown core version");
    assert!(matches!(
        run_core_and_flavor_migrations(&pg, vec![], &proxima::PgSidecarRegistryFrozen::default())
            .await,
        Err(MigrationError::Core(MigrateError::VersionMissing(9000)))
    ));
    assert!(matches!(
        preflight_without_migrations(&pg, vec![], &proxima::PgSidecarRegistryFrozen::default())
            .await,
        Err(MigrationError::Core(MigrateError::VersionMissing(9000)))
    ));
    assert!(matches!(
        pending_migrations(&pool, &[]).await,
        Err(MigrationError::Core(MigrateError::VersionMissing(9000)))
    ));
    run_core_and_flavor_migrations_with_policy(
        &pg,
        vec![],
        &proxima::PgSidecarRegistryFrozen::default(),
        MissingMigrationPolicy::Allow,
    )
    .await
    .expect("host explicitly tolerates other releases");
    preflight_without_migrations_with_policy(
        &pg,
        vec![],
        &proxima::PgSidecarRegistryFrozen::default(),
        MissingMigrationPolicy::Allow,
    )
    .await
    .expect("read-only boot uses same policy");
    assert!(
        pending_migrations_with_policy(&pool, &[], MissingMigrationPolicy::Allow)
            .await
            .expect("planner uses same policy")
            .is_empty()
    );

    let baseline = core_migrator().iter().next().expect("baseline").version;
    let amended =
        sqlx::query("UPDATE public._sqlx_migrations SET checksum = ''::bytea WHERE version = $1")
            .bind(baseline)
            .execute(&pool)
            .await
            .expect("checksum corruption");
    assert_eq!(amended.rows_affected(), 1, "baseline row exists to corrupt");
    assert!(
        matches!(
            run_core_and_flavor_migrations_with_policy(
                &pg,
                vec![],
                &proxima::PgSidecarRegistryFrozen::default(),
                MissingMigrationPolicy::Allow
            )
            .await,
            Err(MigrationError::CorePreflight(_))
        ),
        "allowing missing versions must still refuse a changed core checksum"
    );
}

#[tokio::test]
async fn shared_sources_in_the_core_number_range_are_recognized_by_the_union() {
    let db = expect_described(
        "owned PG required",
        SplitRoleDb::create("proxima_policy_low_shared", &[]).await,
    )
    .await;
    let pg = storage(&db).await;
    let sources = || vec![source("host", 9000, true)];
    run_core_and_flavor_migrations(&pg, sources(), &proxima::PgSidecarRegistryFrozen::default())
        .await
        .expect("host version on core ledger");
    run_core_and_flavor_migrations(&pg, sources(), &proxima::PgSidecarRegistryFrozen::default())
        .await
        .expect("union recognizes the host version");
    assert!(
        pending_migrations(&pg.clone_pool_for_backend(), &sources())
            .await
            .expect("union plans the host version")
            .is_empty()
    );
}

struct PolicyHost;

impl proxima::flavor::FlavorBundle for PolicyHost {
    fn register(
        _: &mut proxima::flavor::FlavorRegistry,
    ) -> Result<(), proxima::flavor::FlavorRegistryError> {
        Ok(())
    }
    fn migrators() -> Vec<NamedMigrator> {
        vec![]
    }
}

impl proxima::FlavorApp for PolicyHost {
    fn app_info() -> proxima::AppInfo {
        proxima::AppInfo {
            id: "policy-host",
            title: "Policy host",
            version: "1",
        }
    }
}

#[tokio::test]
async fn the_runtime_passes_its_explicit_policy_to_migrating_and_read_only_boot() {
    use proxima::{Proxima, ServedSurface};

    let db = expect_described(
        "owned PG required",
        SplitRoleDb::create("proxima_policy_runtime", &[]).await,
    )
    .await;
    let pg = storage(&db).await;
    run_core_and_flavor_migrations(&pg, vec![], &proxima::PgSidecarRegistryFrozen::default())
        .await
        .expect("migrated core");
    sqlx::query("INSERT INTO public._sqlx_migrations (version, description, success, checksum, execution_time) VALUES (9000, 'other release', true, ''::bytea, 0)")
        .execute(&pg.clone_pool_for_backend()).await.expect("unknown version");
    let owner = proxima::company_owner(uuid::Uuid::now_v7());
    let host = || {
        Proxima::<PolicyHost>::app()
            .database_url(db.runtime_url())
            .store_number(0)
            .platform_database_url(db.platform_url())
            .owner(owner)
            .allow_insecure_single_owner()
            .served_surface(ServedSurface::All)
    };
    for skip in [false, true] {
        let strict = if skip {
            host().skip_migrations()
        } else {
            host()
        };
        let error = strict
            .build()
            .await
            .expect_err("runtime rejects unknown versions by default");
        assert!(error.to_string().contains("9000"), "{error}");
        let allowing = host()
            .from_lookup(|key| (key == "PROXIMA_MISSING_MIGRATION_POLICY").then(|| "allow".into()))
            .expect("host environment policy");
        let allowing = if skip {
            allowing.skip_migrations()
        } else {
            allowing
        };
        expect_described(
            "runtime propagates explicit missing-version policy",
            allowing.build().await,
        )
        .await
        .shutdown()
        .await;
    }
}
