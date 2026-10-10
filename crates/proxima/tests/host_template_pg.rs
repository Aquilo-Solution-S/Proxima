//! `HostTemplate`: a migrated, split-role database per test, cloned from a
//! template that is built once, named by what its migrators contribute and
//! collected with its own family only.

use std::borrow::Cow;
use std::sync::{Arc, LazyLock};

use proxima::flavor::{FlavorBundle, FlavorRegistry, FlavorRegistryError, NamedMigrator};
use proxima::host::MigrationError;
use proxima::testkit::{HostTemplate, SplitRoleDb, TemplateFamily, admin_url, drop_db};
use proxima::{AppInfo, BuiltProxima, FlavorApp, Proxima, ToolScope};
use proxima_core::{Owner, UserId};
use proxima_storage_pg::PgPoolConfig;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::{Connection, PgConnection, PgPool, SqlSafeStr};
use uuid::Uuid;

// The statements below are compile-time `concat!` constants over a literal
// table name, written as macros so each `sqlx::query` call shows its literal.

/// The rows of one migration ledger, as the database spells them.
macro_rules! ledger_rows {
    ($executor:expr, $table:literal) => {
        sqlx::query_as::<_, (i64, Vec<u8>, String)>(concat!(
            "SELECT version, checksum, installed_on::text FROM ",
            $table,
            " ORDER BY version"
        ))
        .fetch_all($executor)
    };
}

/// A runtime role reads a ledger and every write form on it is refused with
/// `insufficient_privilege`.
macro_rules! assert_ledger_is_read_only_to {
    ($runtime:expr, $table:literal) => {{
        let rows = ledger_rows!(&mut $runtime, $table)
            .await
            .expect("the runtime role reads a ledger");
        assert!(!rows.is_empty(), "{} has rows", $table);
        let refusals = [
            sqlx::query(concat!(
                "INSERT INTO ",
                $table,
                " SELECT * FROM ",
                $table,
                " WHERE false"
            ))
            .execute(&mut $runtime)
            .await,
            sqlx::query(concat!(
                "UPDATE ",
                $table,
                " SET description = 'runtime mutation'"
            ))
            .execute(&mut $runtime)
            .await,
            sqlx::query(concat!("DELETE FROM ", $table))
                .execute(&mut $runtime)
                .await,
            sqlx::query(concat!("TRUNCATE ", $table))
                .execute(&mut $runtime)
                .await,
        ];
        for refusal in refusals {
            let error = refusal.expect_err("a runtime write to a ledger must be refused");
            assert_eq!(
                code_of(&error).as_deref(),
                Some("42501"),
                "{}: {error}",
                $table
            );
        }
    }};
}

/// The tables a host lineup records on: core's, the Code flavor's and the
/// host's own.
const LEDGER_TABLES: [&str; 3] = [
    "public._sqlx_migrations",
    "public._sqlx_migrations_proxima_code",
    "public._sqlx_migrations_hosttpl",
];

const NOTES: i64 = 20_990_101_000_010;
const SEED: &str = "INSERT INTO public.hosttpl_notes VALUES (1, 'seeded by the host migration')";

fn migration(version: i64, sql: &str) -> Migration {
    Migration::new(
        version,
        Cow::Owned(format!("host {version}")),
        MigrationType::Simple,
        sqlx::AssertSqlSafe(sql.to_owned()).into_sql_str(),
        false,
    )
}

fn migrator(migrations: Vec<Migration>) -> Migrator {
    Migrator {
        migrations: Cow::Owned(migrations),
        ..Migrator::DEFAULT
    }
}

/// The host's own migrator, on its own ledger, after the Code flavor's.
fn host_migrator() -> NamedMigrator {
    NamedMigrator::flavor(
        "hosttpl",
        migrator(vec![
            migration(
                NOTES,
                "CREATE TABLE public.hosttpl_notes (id integer PRIMARY KEY, body text NOT NULL)",
            ),
            migration(NOTES + 1, SEED),
        ]),
    )
}

/// What a host's `FlavorBundle::migrators` returns: the flavor's and its own.
fn migrators() -> Vec<NamedMigrator> {
    let mut all = proxima_code::CodeFlavor::migrators();
    all.push(host_migrator());
    all
}

struct HostApp;

impl FlavorBundle for HostApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        proxima_code::CodeFlavor::register(registry)
    }

    fn register_pg_sidecars(registry: &mut proxima::flavor::PgSidecarRegistry) {
        proxima_code::CodeFlavor::register_pg_sidecars(registry);
    }

    fn migrators() -> Vec<NamedMigrator> {
        migrators()
    }
}

impl FlavorApp for HostApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "host-template-test",
            title: "Host Template Test",
            version: "1",
        }
    }
}

fn family(prefix: &str) -> TemplateFamily {
    TemplateFamily::new(prefix).expect("test family")
}

/// The template most tests clone, built once per server and fingerprint.
static SUITE: LazyLock<HostTemplate> = LazyLock::new(|| {
    HostTemplate::new(family("hosttpl_suite_"), "suite-v1", migrators()).expect("suite lineup")
});

async fn clone() -> SplitRoleDb {
    SUITE
        .clone_split_role("proxima_host_template", &["proxima_code"])
        .await
        .expect("PG required")
}

async fn exists(name: &str) -> bool {
    let mut conn = PgConnection::connect(&admin_url()).await.expect("admin");
    let found = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
    )
    .bind(name)
    .fetch_one(&mut conn)
    .await
    .expect("exists");
    conn.close().await.expect("close");
    found
}

/// Every ledger row, as the database spells it, for comparing before and
/// after a boot. `installed_on` changes when a row is rewritten.
async fn ledger_rows(pool: &PgPool) -> Vec<(&'static str, i64, Vec<u8>, String)> {
    let mut snapshot = Vec::new();
    macro_rules! add {
        ($table:literal) => {
            for (version, checksum, installed) in
                ledger_rows!(pool, $table).await.expect("ledger rows")
            {
                snapshot.push(($table, version, checksum, installed));
            }
        };
    }
    add!("public._sqlx_migrations");
    add!("public._sqlx_migrations_proxima_code");
    add!("public._sqlx_migrations_hosttpl");
    snapshot
}

fn code_of(error: &sqlx::Error) -> Option<String> {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(Cow::into_owned)
}

async fn boot(db: &SplitRoleDb, owner: Owner, skip_migrations: bool) -> BuiltProxima {
    let builder = Proxima::<HostApp>::app()
        .database_url(db.runtime_url())
        .platform_database_url(db.platform_url())
        .owner(owner)
        .tool_scope(ToolScope::All)
        .pg_pool_config(PgPoolConfig::default());
    let builder = if skip_migrations {
        builder.skip_migrations()
    } else {
        builder
    };
    builder.build().await.expect("split-role boot")
}

#[tokio::test]
async fn a_clone_holds_every_migration_and_a_boot_applies_none() {
    let db = clone().await;
    let admin = PgPool::connect(&db.admin_url()).await.expect("admin");
    let before = ledger_rows(&admin).await;
    for table in LEDGER_TABLES {
        assert!(
            before.iter().any(|(held, ..)| *held == table),
            "the template holds {table}: {before:?}"
        );
    }
    let seeded: String = sqlx::query_scalar("SELECT body FROM public.hosttpl_notes WHERE id = 1")
        .fetch_one(&admin)
        .await
        .expect("the host's migration ran in the template");
    assert_eq!(seeded, "seeded by the host migration");

    let owner = Owner::Personal(UserId::new(Uuid::now_v7()));
    let booted = boot(&db, owner, false).await;
    booted.shutdown().await;
    assert_eq!(
        ledger_rows(&admin).await,
        before,
        "a boot over a clone migrates nothing"
    );

    // `skip_migrations` issues no DDL and refuses a lane with an unapplied
    // migration, so it boots only if the clone is complete.
    let skipped = boot(&db, owner, true).await;
    skipped.shutdown().await;
    assert_eq!(ledger_rows(&admin).await, before);
    admin.close().await;
}

#[tokio::test]
async fn the_runtime_role_is_unprivileged_and_cannot_write_a_ledger() {
    let db = clone().await;
    let mut runtime = PgConnection::connect(db.runtime_url())
        .await
        .expect("runtime");
    let (role, superuser, bypass_rls): (String, bool, bool) = sqlx::query_as(
        "SELECT rolname::text, rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(&mut runtime)
    .await
    .expect("role attributes");
    assert_eq!(role, "proxima_test_runtime");
    assert!(
        !superuser && !bypass_rls,
        "{role} must be NOSUPERUSER NOBYPASSRLS"
    );

    assert_ledger_is_read_only_to!(runtime, "public._sqlx_migrations");
    assert_ledger_is_read_only_to!(runtime, "public._sqlx_migrations_proxima_code");
    assert_ledger_is_read_only_to!(runtime, "public._sqlx_migrations_hosttpl");
    // The host's table is the runtime role's to read and write, not to alter.
    sqlx::query("INSERT INTO public.hosttpl_notes VALUES (2, 'runtime row')")
        .execute(&mut runtime)
        .await
        .expect("DML on a host table");
    let error = sqlx::query("ALTER TABLE public.hosttpl_notes ADD COLUMN extra integer")
        .execute(&mut runtime)
        .await
        .expect_err("the runtime role owns nothing");
    assert_eq!(code_of(&error).as_deref(), Some("42501"), "{error}");
    runtime.close().await.expect("close");
}

#[tokio::test]
async fn owner_rls_refuses_the_runtime_role_in_a_clone() {
    let db = clone().await;
    let admin = PgPool::connect(&db.admin_url()).await.expect("admin");
    let owner = Uuid::now_v7();
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'group')")
        .bind(owner)
        .execute(&admin)
        .await
        .expect("the superuser seeds an owner");

    // No authenticated owner scope on this connection: the owner is invisible
    // to the runtime role and nothing it writes lands.
    let mut runtime = PgConnection::connect(db.runtime_url())
        .await
        .expect("runtime");
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.owners")
        .fetch_one(&mut runtime)
        .await
        .expect("count");
    assert_eq!(
        visible, 0,
        "RLS hides a row from an unscoped runtime connection"
    );
    let deleted = sqlx::query("DELETE FROM proxima_core.owners")
        .execute(&mut runtime)
        .await
        .expect("delete")
        .rows_affected();
    assert_eq!(deleted, 0);
    let error =
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'group')")
            .bind(Uuid::now_v7())
            .execute(&mut runtime)
            .await
            .expect_err("an unscoped insert is refused");
    assert_eq!(code_of(&error).as_deref(), Some("42501"), "{error}");
    runtime.close().await.expect("close");

    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.owners WHERE owner_id = $1")
            .bind(owner)
            .fetch_one(&admin)
            .await
            .expect("count");
    assert_eq!(remaining, 1, "the refused connection changed nothing");
    admin.close().await;
}

#[tokio::test]
async fn a_passing_drop_removes_the_clone_and_a_panic_keeps_it() {
    let passing = clone().await;
    let passing_name = passing.name().to_owned();
    assert!(exists(&passing_name).await);
    drop(passing);
    assert!(
        !exists(&passing_name).await,
        "a passing drop removes the clone"
    );

    let kept = clone().await;
    let kept_name = kept.name().to_owned();
    let joined = std::thread::spawn(move || {
        let _kept = kept;
        panic!("intentional");
    })
    .join();
    let still_there = exists(&kept_name).await;
    drop_db(&kept_name).await.expect("clean up the kept clone");
    assert!(joined.is_err());
    assert!(still_there, "a drop while unwinding keeps the clone");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sixteen_concurrent_clones_are_distinct_and_complete() {
    let template = Arc::new(
        HostTemplate::new(
            family(&format!(
                "hosttpl_conc{}_",
                &Uuid::now_v7().simple().to_string()[20..]
            )),
            "conc-v1",
            migrators(),
        )
        .expect("lineup"),
    );
    let callers: Vec<_> = (0..16)
        .map(|_| {
            let template = Arc::clone(&template);
            tokio::spawn(async move {
                template
                    .clone_split_role("proxima_host_template_conc", &["proxima_code"])
                    .await
            })
        })
        .collect();
    let mut clones = Vec::new();
    for caller in callers {
        clones.push(caller.await.expect("task").expect("clone"));
    }

    let mut names: Vec<&str> = clones.iter().map(SplitRoleDb::name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 16, "sixteen distinct clones");
    for clone in &clones {
        let runtime = PgPool::connect(clone.runtime_url()).await.expect("runtime");
        let seeded: i64 = sqlx::query_scalar("SELECT count(*) FROM public.hosttpl_notes")
            .fetch_one(&runtime)
            .await
            .expect("every clone is complete");
        assert_eq!(seeded, 1);
        runtime.close().await;
    }
    let name = template.template_name();
    drop(clones);
    drop_db(&name).await.expect("clean up the template");
}

#[tokio::test]
async fn a_changed_migration_is_a_new_template_and_the_old_one_is_collected() {
    let family = family(&format!(
        "hosttpl_gc{}_",
        &Uuid::now_v7().simple().to_string()[20..]
    ));
    let first = HostTemplate::new(family.clone(), "v1", migrators()).expect("lineup");
    let second = HostTemplate::new(family, "v2", migrators()).expect("lineup");
    assert_ne!(first.template_name(), second.template_name());

    let one = first
        .clone_split_role("proxima_host_template_gc", &[])
        .await
        .expect("first");
    let first_after_build = exists(&first.template_name()).await;
    // The first template has no session on it (a clone is another database),
    // so the second build collects it.
    let two = second
        .clone_split_role("proxima_host_template_gc", &[])
        .await
        .expect("second");
    let first_after_second = exists(&first.template_name()).await;
    let second_after_second = exists(&second.template_name()).await;
    drop((one, two));
    drop_db(&second.template_name()).await.expect("clean up");

    assert!(first_after_build);
    assert!(
        !first_after_second,
        "the stale template is dropped once idle"
    );
    assert!(second_after_second);
}

#[tokio::test]
async fn a_migration_error_is_a_protocol_error_and_leaves_no_template() {
    let template = HostTemplate::new(
        family(&format!(
            "hosttpl_err{}_",
            &Uuid::now_v7().simple().to_string()[20..]
        )),
        "err-v1",
        single("hostone", "INSERT INTO public.hosttpl_missing VALUES (1)"),
    )
    .expect("a lineup the runner accepts");
    let error = template
        .clone_split_role("proxima_host_template_err", &[])
        .await
        .expect_err("a failing statement cannot build");
    let present = exists(&template.template_name()).await;
    assert!(
        matches!(&error, sqlx::Error::Protocol(text) if text.contains("hosttpl_missing")),
        "{error:?}"
    );
    assert!(!present, "a failed build is never a template");
}

/// The lineup check the runner itself uses runs in `HostTemplate::new`, so a
/// lineup it refuses never gets a name. The hash could not tell it apart: it
/// reads a lane through `NamedMigrator::lane`, which lists a repeated version
/// once, so a source that repeats one would hash like the one that does not
/// and be answered from that one's cached template.
#[test]
fn a_lineup_the_runner_refuses_gets_no_template() {
    let clashing = vec![
        NamedMigrator::flavor("hostone", migrator(vec![migration(NOTES, "SELECT 1")])),
        NamedMigrator::flavor("hosttwo", migrator(vec![migration(NOTES, "SELECT 2")])),
    ];
    let repeated = vec![NamedMigrator::flavor(
        "hostone",
        migrator(vec![
            migration(NOTES, "SELECT 1"),
            migration(NOTES, "SELECT 2"),
        ]),
    )];
    for (case, migrators) in [("two sources", clashing), ("one source", repeated)] {
        let refused = HostTemplate::new(family("hosttpl_dup_"), "dup-v1", migrators);
        assert!(
            matches!(refused, Err(MigrationError::DuplicateVersion { version, .. }) if version == NOTES),
            "{case}: {refused:?}"
        );
    }
}

/// A single-migration host on a ledger of its own.
fn single(source: &'static str, sql: &str) -> Vec<NamedMigrator> {
    vec![NamedMigrator::flavor(
        source,
        migrator(vec![migration(NOTES, sql)]),
    )]
}

fn named(fingerprint: &str, migrators: Vec<NamedMigrator>) -> String {
    HostTemplate::new(family("hosttpl_names_"), fingerprint, migrators)
        .expect("lineup")
        .template_name()
}

#[test]
fn the_template_name_is_the_family_prefix_and_sixteen_hex_digits() {
    let name = named("fp", single("hostone", "SELECT 1"));
    let digits = name.strip_prefix("hosttpl_names_").expect("family prefix");
    assert_eq!(digits.len(), 16, "{name}");
    assert!(
        digits
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert!(name.len() <= 63);
    // Deterministic: what the hash covers is all it depends on.
    assert_eq!(name, named("fp", single("hostone", "SELECT 1")));
    // Another family names the same content with the same digits.
    let other = HostTemplate::new(
        family("hosttpl_other_"),
        "fp",
        single("hostone", "SELECT 1"),
    )
    .expect("lineup");
    assert_eq!(
        other.template_name().strip_prefix("hosttpl_other_"),
        Some(digits)
    );
}

#[test]
fn a_changed_checksum_ledger_source_or_fingerprint_is_a_different_template() {
    let base = named("fp", single("hostone", "SELECT 1"));
    let variants = [
        ("fingerprint", named("fp2", single("hostone", "SELECT 1"))),
        ("checksum", named("fp", single("hostone", "SELECT 2"))),
        ("source", named("fp", single("hosttwo", "SELECT 1"))),
        ("no host migrator", named("fp", Vec::new())),
    ];
    for (changed, name) in &variants {
        assert_ne!(*name, base, "a changed {changed}");
    }

    // The ledger alone: same source id, same migration, another table.
    let mut moved = migrator(vec![migration(NOTES, "SELECT 1")]);
    moved.dangerous_set_table_name("public._sqlx_migrations_elsewhere");
    let on_another_ledger = named("fp", vec![NamedMigrator::new("hostone", moved)]);
    let mut home = migrator(vec![migration(NOTES, "SELECT 1")]);
    home.dangerous_set_table_name("public._sqlx_migrations_hostone");
    let on_its_ledger = named("fp", vec![NamedMigrator::new("hostone", home)]);
    assert_ne!(on_another_ledger, on_its_ledger, "a changed ledger name");

    // Another version of the same SQL, an extra migration, a different order.
    let two = |first: &'static str, second: &'static str| {
        vec![
            NamedMigrator::flavor(first, migrator(vec![migration(NOTES, "SELECT 1")])),
            NamedMigrator::flavor(second, migrator(vec![migration(NOTES + 1, "SELECT 1")])),
        ]
    };
    assert_ne!(base, named("fp", two("hostone", "hosttwo")));
    assert_ne!(
        named("fp", two("hostone", "hosttwo")),
        named("fp", two("hosttwo", "hostone"))
    );
}

#[test]
fn the_hash_frames_its_fields() {
    // Moving a byte from the fingerprint to the source id is not the same input.
    assert_ne!(
        named("a", single("bc", "SELECT 1")),
        named("ab", single("c", "SELECT 1"))
    );
}
