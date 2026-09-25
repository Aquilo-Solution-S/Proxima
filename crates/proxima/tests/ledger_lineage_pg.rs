//! A flavor lane that does not continue its ledger is refused, legibly and
//! before it applies anything; one that does — an older release's fleet on
//! the same database included — still boots.
//!
//! The replaced-lane case replays the forgejo pack's 0.0.7 publication: a
//! new baseline over a database that ran the lane it replaced, which
//! crash-looped on `schema "forgejo" already exists`.

use std::borrow::Cow;

use proxima::{
    LedgerConflict, MigrationError, NamedMigrator, preflight_without_migrations,
    run_core_and_flavor_migrations,
};
use proxima_pg_testkit::SplitRoleDb;
use proxima_storage_pg::PgStorage;
use sqlx::SqlSafeStr;
use sqlx::migrate::{Migration, MigrationType, Migrator};

const ROOT: i64 = 20_990_101_000_010;
const SECOND: i64 = 20_990_102_000_010;
const SQUASHED_AWAY: i64 = 20_990_103_000_010;
const NEWER: i64 = 20_990_104_000_010;
const BASELINE: i64 = 20_990_105_000_010;
const NEVER_APPLIED: i64 = 20_990_106_000_010;

/// One version's SQL, the same in every lane that ships it (the checksum
/// is the file's bytes).
fn sql(version: i64) -> &'static str {
    match version {
        ROOT | BASELINE => "CREATE SCHEMA lineage",
        SECOND => "COMMENT ON SCHEMA lineage IS 'second'",
        SQUASHED_AWAY => "COMMENT ON SCHEMA lineage IS 'squashed away'",
        NEWER => "COMMENT ON SCHEMA lineage IS 'newer'",
        NEVER_APPLIED => "COMMENT ON SCHEMA lineage IS 'never applied'",
        other => panic!("no SQL for {other}"),
    }
}

fn lane(versions: &[i64]) -> Vec<NamedMigrator> {
    let migrations = versions
        .iter()
        .map(|version| {
            Migration::new(
                *version,
                Cow::Owned(format!("lineage {version}")),
                MigrationType::Simple,
                sqlx::AssertSqlSafe(sql(*version).to_owned()).into_sql_str(),
                false,
            )
        })
        .collect();
    vec![NamedMigrator::flavor(
        "lineage",
        Migrator {
            migrations: Cow::Owned(migrations),
            ..Migrator::DEFAULT
        },
    )]
}

async fn recorded(db: &SplitRoleDb) -> Vec<i64> {
    let admin = sqlx::PgPool::connect(&db.admin_url())
        .await
        .expect("admin pool");
    let versions = sqlx::query_scalar(
        "SELECT version FROM public._sqlx_migrations_lineage WHERE success ORDER BY version",
    )
    .fetch_all(&admin)
    .await
    .expect("ledger rows");
    admin.close().await;
    versions
}

fn conflict(result: Result<proxima::MigrationRunReport, MigrationError>) -> LedgerConflict {
    match result {
        Err(MigrationError::Ledger {
            source: "lineage",
            ledger,
            conflict,
        }) => {
            assert_eq!(ledger, "public._sqlx_migrations_lineage");
            conflict
        }
        other => panic!("expected a ledger conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn a_migration_run_refuses_a_lane_that_does_not_continue_its_ledger() {
    let db = SplitRoleDb::create("proxima_ledger_lineage", &[])
        .await
        .expect("PG required");
    let pg = PgStorage::connect(db.platform_url())
        .await
        .expect("platform storage");

    run_core_and_flavor_migrations(&pg, lane(&[ROOT, SECOND]))
        .await
        .expect("the first release migrates");

    let replaced = run_core_and_flavor_migrations(&pg, lane(&[BASELINE])).await;
    let message = replaced.as_ref().map(|_| ()).unwrap_err().to_string();
    assert_eq!(
        conflict(replaced),
        LedgerConflict::Replaced {
            root: BASELINE,
            unknown: vec![ROOT, SECOND],
        }
    );
    assert!(
        message.contains(&BASELINE.to_string()) && message.contains("replaced baseline"),
        "the refusal names the lane and the cause, not `schema already exists`: {message}"
    );
    assert_eq!(
        recorded(&db).await,
        vec![ROOT, SECOND],
        "the refused lane applied nothing"
    );

    run_core_and_flavor_migrations(&pg, lane(&[ROOT, SECOND, NEWER]))
        .await
        .expect("an upgrade migrates");
    run_core_and_flavor_migrations(&pg, lane(&[ROOT, SECOND]))
        .await
        .expect("the older release's fleet still boots beside the newer one's");
    run_core_and_flavor_migrations(&pg, lane(&[SECOND, NEWER]))
        .await
        .expect("a lane that shed its first file still boots");

    assert_eq!(
        conflict(run_core_and_flavor_migrations(&pg, lane(&[ROOT, SECOND, SQUASHED_AWAY])).await),
        LedgerConflict::Diverged {
            pending: vec![SQUASHED_AWAY],
            unknown: vec![NEWER],
        },
        "a release whose file a later one squashed away is refused"
    );
    assert_eq!(recorded(&db).await, vec![ROOT, SECOND, NEWER]);
}

#[tokio::test]
async fn a_boot_that_skips_migrations_refuses_a_ledger_it_cannot_serve() {
    let db = SplitRoleDb::create("proxima_ledger_lineage_skip", &[])
        .await
        .expect("PG required");
    let pg = PgStorage::connect(db.platform_url())
        .await
        .expect("platform storage");

    run_core_and_flavor_migrations(&pg, Vec::new())
        .await
        .expect("core migrates without the flavor");
    assert_eq!(
        conflict(preflight_without_migrations(&pg, lane(&[ROOT])).await),
        LedgerConflict::Unapplied {
            pending: vec![ROOT]
        },
        "a lane never migrated is refused, not served"
    );

    run_core_and_flavor_migrations(&pg, lane(&[ROOT, SECOND]))
        .await
        .expect("the migration step");
    preflight_without_migrations(&pg, lane(&[ROOT, SECOND]))
        .await
        .expect("a migrated lane boots");
    preflight_without_migrations(&pg, lane(&[ROOT]))
        .await
        .expect("so does an older release's");
    assert_eq!(
        conflict(preflight_without_migrations(&pg, lane(&[ROOT, SECOND, NEWER])).await),
        LedgerConflict::Unapplied {
            pending: vec![NEWER]
        }
    );
    assert_eq!(
        conflict(preflight_without_migrations(&pg, lane(&[BASELINE])).await),
        LedgerConflict::Replaced {
            root: BASELINE,
            unknown: vec![ROOT, SECOND],
        }
    );
}
