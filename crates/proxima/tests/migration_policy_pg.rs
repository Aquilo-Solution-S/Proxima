//! Unknown ledger versions are refused across the entire composition before DDL.

use std::borrow::Cow;

use proxima::{MigrationError, NamedMigrator, pending_migrations, run_core_and_flavor_migrations};
use proxima_pg_testkit::SplitRoleDb;
use proxima_storage_pg::{PgMigrationStorage, PgPoolConfig, PgTuning};
use sqlx::migrate::{MigrateError, Migration, MigrationType, Migrator};
use sqlx::{PgPool, SqlSafeStr};

const FIRST: i64 = 20_991_001_000_010;
const UNKNOWN: i64 = FIRST + 1;

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
    PgMigrationStorage::connect_with_config(
        db.platform_url(),
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .expect("migration storage")
}

#[tokio::test]
async fn an_unknown_version_on_a_later_ledger_prevents_every_schema_change() {
    let db = SplitRoleDb::create("proxima_policy_before_ddl", &[])
        .await
        .expect("owned PG required");
    let pg = storage(&db).await;
    let admin = PgPool::connect(&db.admin_url()).await.expect("admin pool");
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
    let ran = run_core_and_flavor_migrations(&pg, sources())
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
    let db = SplitRoleDb::create("proxima_policy_union", &[])
        .await
        .expect("owned PG required");
    let pg = storage(&db).await;
    let sources = || {
        vec![
            source("first", FIRST, true),
            source("second", FIRST + 1, true),
        ]
    };
    run_core_and_flavor_migrations(&pg, sources())
        .await
        .expect("initial composition");
    run_core_and_flavor_migrations(&pg, sources())
        .await
        .expect("all shared sources recognized");
    let pool = pg.clone_pool_for_backend();
    assert!(
        pending_migrations(&pool, &sources())
            .await
            .expect("current composition")
            .is_empty()
    );
    let omitted = run_core_and_flavor_migrations(&pg, vec![source("first", FIRST, true)])
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

    let db = SplitRoleDb::create("proxima_policy_opt_in", &[])
        .await
        .expect("owned PG required");
    let pg = storage(&db).await;
    let pool = pg.clone_pool_for_backend();
    run_core_and_flavor_migrations(&pg, vec![source("first", FIRST, true)])
        .await
        .expect("initial host migration");
    // A second release also applied a core-namespace version. Both ledger
    // ranges must use the host's explicit policy, not a hidden core exception.
    sqlx::query("INSERT INTO public._sqlx_migrations (version, description, success, checksum, execution_time) VALUES (9000, 'other release', true, ''::bytea, 0)")
        .execute(&pool).await.expect("unknown core version");
    assert!(matches!(
        run_core_and_flavor_migrations(&pg, vec![]).await,
        Err(MigrationError::Core(MigrateError::VersionMissing(9000)))
    ));
    assert!(matches!(
        preflight_without_migrations(&pg, vec![]).await,
        Err(MigrationError::Core(MigrateError::VersionMissing(9000)))
    ));
    assert!(matches!(
        pending_migrations(&pool, &[]).await,
        Err(MigrationError::Core(MigrateError::VersionMissing(9000)))
    ));
    run_core_and_flavor_migrations_with_policy(&pg, vec![], MissingMigrationPolicy::Allow)
        .await
        .expect("host explicitly tolerates other releases");
    preflight_without_migrations_with_policy(&pg, vec![], MissingMigrationPolicy::Allow)
        .await
        .expect("read-only boot uses same policy");
    assert!(
        pending_migrations_with_policy(&pool, &[], MissingMigrationPolicy::Allow)
            .await
            .expect("planner uses same policy")
            .is_empty()
    );

    sqlx::query("UPDATE public._sqlx_migrations SET checksum = ''::bytea WHERE version = 5")
        .execute(&pool)
        .await
        .expect("checksum corruption");
    assert!(
        matches!(
            run_core_and_flavor_migrations_with_policy(&pg, vec![], MissingMigrationPolicy::Allow)
                .await,
            Err(MigrationError::CorePreflight(_))
        ),
        "allowing missing versions must still refuse a changed core checksum"
    );
}

#[tokio::test]
async fn shared_sources_in_the_core_number_range_are_recognized_by_the_union() {
    let db = SplitRoleDb::create("proxima_policy_low_shared", &[])
        .await
        .expect("owned PG required");
    let pg = storage(&db).await;
    let sources = || vec![source("host", 9000, true)];
    run_core_and_flavor_migrations(&pg, sources())
        .await
        .expect("host version on core ledger");
    run_core_and_flavor_migrations(&pg, sources())
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
    use proxima::{Proxima, ToolScope};

    let db = SplitRoleDb::create("proxima_policy_runtime", &[])
        .await
        .expect("owned PG required");
    let pg = storage(&db).await;
    run_core_and_flavor_migrations(&pg, vec![])
        .await
        .expect("migrated core");
    sqlx::query("INSERT INTO public._sqlx_migrations (version, description, success, checksum, execution_time) VALUES (9000, 'other release', true, ''::bytea, 0)")
        .execute(&pg.clone_pool_for_backend()).await.expect("unknown version");
    let owner = proxima::company_owner(uuid::Uuid::now_v7());
    let host = || {
        Proxima::<PolicyHost>::app()
            .database_url(db.runtime_url())
            .platform_database_url(db.platform_url())
            .owner(owner)
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
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
        allowing
            .build()
            .await
            .expect("runtime propagates explicit missing-version policy")
            .shutdown()
            .await;
    }
}
