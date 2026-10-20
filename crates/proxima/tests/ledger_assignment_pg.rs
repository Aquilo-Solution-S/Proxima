//! Missing-version forgiveness cannot replay a baseline on a different ledger.

use std::borrow::Cow;

use proxima::{
    LedgerConflict, MigrationError, MissingMigrationPolicy, NamedMigrator,
    pending_migrations_with_policy, preflight_without_migrations_with_policy,
    run_core_and_flavor_migrations, run_core_and_flavor_migrations_with_policy,
};
use proxima_pg_testkit::SplitRoleDb;
use proxima_storage_pg::{PgMigrationStorage, PgPoolConfig, PgTuning};
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::{PgPool, SqlSafeStr};

const VERSION: i64 = 20_991_101_000_010;
const SHARED_LEDGER: &str = "public._sqlx_migrations";
const PRIVATE_LEDGER: &str = "public._sqlx_migrations_assignment";

fn source(shared: bool) -> NamedMigrator {
    let migrator = Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            VERSION,
            Cow::Borrowed("assignment baseline"),
            MigrationType::Simple,
            sqlx::AssertSqlSafe(
                "DROP TABLE IF EXISTS public.assignment_fixture; \
                 CREATE TABLE public.assignment_fixture (id int PRIMARY KEY)"
                    .to_owned(),
            )
            .into_sql_str(),
            false,
        )]),
        ..Migrator::DEFAULT
    };
    if shared {
        NamedMigrator::new("assignment", migrator)
    } else {
        NamedMigrator::flavor("assignment", migrator)
    }
}

fn assert_assignment_refusal(error: &MigrationError) {
    assert!(
        matches!(
            error,
            MigrationError::Ledger {
                source: "assignment",
                ledger: expected,
                conflict: LedgerConflict::RecordedElsewhere {
                    version: VERSION,
                    ledger: recorded,
                },
            } if expected == PRIVATE_LEDGER && recorded == SHARED_LEDGER
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn changing_a_baselines_ledger_is_refused_before_any_replay_under_both_policies() {
    let db = SplitRoleDb::create("proxima_ledger_assignment", &[])
        .await
        .expect("owned PG required");
    let pg = PgMigrationStorage::connect_with_config(
        db.platform_url(),
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .expect("migration storage");
    let admin = PgPool::connect(&db.admin_url()).await.expect("admin pool");
    run_core_and_flavor_migrations(
        &pg,
        vec![source(true)],
        &proxima::PgSidecarRegistryFrozen::default(),
    )
    .await
    .expect("baseline records on the shared ledger");
    sqlx::query("INSERT INTO public.assignment_fixture (id) VALUES (7)")
        .execute(&admin)
        .await
        .expect("data a baseline replay would destroy");
    let before: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM public._sqlx_migrations ORDER BY version")
            .fetch_all(&admin)
            .await
            .expect("shared ledger before refusal");

    for copied in [false, true] {
        if copied {
            // Seed the legacy copy-cutover shape without running compatibility code.
            sqlx::raw_sql(
                "CREATE TABLE public._sqlx_migrations_assignment \
                 (LIKE public._sqlx_migrations INCLUDING ALL); \
                 ALTER TABLE public._sqlx_migrations_assignment OWNER TO proxima_test_platform",
            )
            .execute(&admin)
            .await
            .expect("legacy separate ledger");
            sqlx::query(
                "INSERT INTO public._sqlx_migrations_assignment \
                 SELECT * FROM public._sqlx_migrations WHERE version = $1",
            )
            .bind(VERSION)
            .execute(&admin)
            .await
            .expect("legacy copied baseline row");
        }
        for policy in [
            MissingMigrationPolicy::Reject,
            MissingMigrationPolicy::Allow,
        ] {
            let planned = pending_migrations_with_policy(&admin, &[source(false)], policy)
                .await
                .expect_err("a plan refuses the changed ledger assignment");
            let preflight = preflight_without_migrations_with_policy(
                &pg,
                vec![source(false)],
                &proxima::PgSidecarRegistryFrozen::default(),
                policy,
            )
            .await
            .expect_err("read-only boot refuses the changed ledger assignment");
            let ran = run_core_and_flavor_migrations_with_policy(
                &pg,
                vec![source(false)],
                &proxima::PgSidecarRegistryFrozen::default(),
                policy,
            )
            .await
            .expect_err("a run refuses the changed ledger assignment");
            assert_assignment_refusal(&planned);
            assert_assignment_refusal(&preflight);
            assert_assignment_refusal(&ran);
            assert_eq!(format!("{planned:?}"), format!("{preflight:?}"));
            assert_eq!(format!("{planned:?}"), format!("{ran:?}"));

            let ids: Vec<i32> =
                sqlx::query_scalar("SELECT id FROM public.assignment_fixture ORDER BY id")
                    .fetch_all(&admin)
                    .await
                    .expect("preserved data");
            assert_eq!(ids, [7], "the refused baseline destroyed data");
            let after: Vec<i64> =
                sqlx::query_scalar("SELECT version FROM public._sqlx_migrations ORDER BY version")
                    .fetch_all(&admin)
                    .await
                    .expect("shared ledger after refusal");
            assert_eq!(after, before, "the refused run changed ledger rows");
            let created: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
                .bind(PRIVATE_LEDGER)
                .fetch_one(&admin)
                .await
                .expect("private ledger probe");
            assert_eq!(created, copied, "the refused run changed ledger existence");
        }
    }
    admin.close().await;
}
