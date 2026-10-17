//! Runtime coverage for the migration reset boundary.

use std::collections::{BTreeMap, BTreeSet};

#[path = "support/migration_reset.rs"]
mod migration_reset_support;

use migration_reset_support::{
    apply_pre_identity_schema, pre_reset_pairs as shared_pre_reset_pairs,
};
use proxima_core::StorageError;
use proxima_pg_testkit::{DbGuard, admin_url_or_skip, create_db, split_role_urls};
use proxima_storage_pg::{
    PgMigrationStorage, PgPoolConfig, PgTuning, adopt::PRE_RESET, definitions,
    ensure_core_schema_current,
};
use sqlx::AssertSqlSafe;

const BASELINE: &str = include_str!("../migrations/0033_baseline.sql");

fn pre_reset_pairs() -> Vec<(i64, Vec<u8>)> {
    shared_pre_reset_pairs(&PRE_RESET)
}

fn definitions_record_sql(checksum: &str, release: &str) -> String {
    format!(
        "CREATE OR REPLACE FUNCTION proxima_core.definitions_applied()
         RETURNS TABLE(checksum text, release text) LANGUAGE sql IMMUTABLE
         AS $$ SELECT '{checksum}', '{release}' $$"
    )
}

fn declared_names(source: &str, keyword: &str) -> BTreeMap<String, usize> {
    let mut names = BTreeMap::new();
    for line in source.lines().map(str::trim) {
        let Some(index) = line.find(keyword) else {
            continue;
        };
        if !line.starts_with("CREATE") {
            continue;
        }
        let rest = line[index + keyword.len()..].trim_start();
        let name = rest
            .split(['(', ' ', '\t'])
            .next()
            .unwrap_or_default()
            .trim_start_matches("proxima_core.");
        if !name.is_empty() {
            *names.entry(name.to_owned()).or_insert(0) += 1;
        }
    }
    names
}

async fn catalog_snapshot(pool: &sqlx::PgPool) -> Vec<String> {
    let functions: Vec<String> = sqlx::query_scalar(
        "SELECT format('%s|%s|%s|%s', p.proname,
                       pg_get_function_identity_arguments(p.oid), p.prokind,
                       COALESCE(p.proacl::text, '') ||
                       CASE WHEN p.prokind <> 'a' THEN pg_get_functiondef(p.oid) ELSE '' END)
           FROM pg_proc p
           JOIN pg_namespace n ON n.oid = p.pronamespace
          WHERE n.nspname = 'proxima_core'
          ORDER BY p.proname, pg_get_function_identity_arguments(p.oid), p.prokind",
    )
    .fetch_all(pool)
    .await
    .expect("functions");
    let aggregates: Vec<String> = sqlx::query_scalar(
        "SELECT format('%s|%s|%s|%s', p.proname,
                       pg_get_function_identity_arguments(p.oid),
                       COALESCE(a.aggtransfn::regprocedure::text, ''),
                       COALESCE(p.proacl::text, ''))
           FROM pg_proc p
           JOIN pg_namespace n ON n.oid = p.pronamespace
           JOIN pg_aggregate a ON a.aggfnoid = p.oid
          WHERE n.nspname = 'proxima_core'
          ORDER BY p.proname, pg_get_function_identity_arguments(p.oid)",
    )
    .fetch_all(pool)
    .await
    .expect("aggregates");
    let triggers: Vec<String> = sqlx::query_scalar(
        "SELECT format('%s|%s|%s', c.relname, t.tgname, pg_get_triggerdef(t.oid))
           FROM pg_trigger t
           JOIN pg_class c ON c.oid = t.tgrelid
           JOIN pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = 'proxima_core' AND NOT t.tgisinternal
          ORDER BY c.relname, t.tgname",
    )
    .fetch_all(pool)
    .await
    .expect("triggers");
    let policies: Vec<String> = sqlx::query_scalar(
        "SELECT format('%s|%s|%s|%s', c.relname, p.polname,
                       pg_get_expr(p.polqual, p.polrelid),
                       pg_get_expr(p.polwithcheck, p.polrelid))
           FROM pg_policy p
           JOIN pg_class c ON c.oid = p.polrelid
           JOIN pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = 'proxima_core'
          ORDER BY c.relname, p.polname",
    )
    .fetch_all(pool)
    .await
    .expect("policies");
    [functions, aggregates, triggers, policies].concat()
}

async fn migration_db(prefix: &str) -> Option<(String, PgMigrationStorage, DbGuard)> {
    admin_url_or_skip()?;
    let name = format!(
        "proxima_mig_reset_{prefix}_{}",
        uuid::Uuid::new_v4().simple()
    );
    create_db(&name).await.expect("create test database");
    let (_, platform_url) = split_role_urls(&name).await.expect("split roles");
    let migration = PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .expect("migration connection");
    Some((name.clone(), migration, DbGuard::adopt(name)))
}

#[tokio::test]
async fn fresh_database_runs_baseline_and_definitions() {
    let Some((_name, migration, _guard)) = migration_db("fresh").await else {
        return;
    };
    migration.run_migrations().await.expect("fresh migration");
    let pool = migration.clone_pool_for_backend();
    let baseline: i64 = sqlx::query_scalar(
        "SELECT max(version) FROM public._sqlx_migrations WHERE success AND version = $1",
    )
    .bind(
        proxima_storage_pg::core_migrator()
            .iter()
            .next()
            .expect("baseline")
            .version,
    )
    .fetch_one(&pool)
    .await
    .expect("ledger");
    assert_eq!(
        baseline,
        proxima_storage_pg::core_migrator()
            .iter()
            .next()
            .expect("baseline")
            .version
    );
    assert!(
        definitions::current_matches(&mut pool.acquire().await.expect("connection").detach())
            .await
            .expect("definitions")
    );
}

#[tokio::test]
async fn definitions_pass_is_idempotent() {
    let Some((_name, migration, _guard)) = migration_db("idempotent").await else {
        return;
    };
    migration.run_migrations().await.expect("first migration");
    let pool = migration.clone_pool_for_backend();
    let before = catalog_snapshot(&pool).await;
    sqlx::raw_sql(AssertSqlSafe(definitions_record_sql(
        "deadbeef",
        proxima_core::RELEASE_VERSION,
    )))
    .execute(&pool)
    .await
    .expect("force definitions pass");
    migration.run_migrations().await.expect("second migration");
    let after = catalog_snapshot(&pool).await;
    assert_eq!(before, after);
}

#[tokio::test]
async fn definitions_preserve_flavor_trigger() {
    let Some((_name, migration, _guard)) = migration_db("trigger").await else {
        return;
    };
    migration.run_migrations().await.expect("first migration");
    let pool = migration.clone_pool_for_backend();
    sqlx::raw_sql(
        "CREATE FUNCTION proxima_core.test_flavor_trigger()
         RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
         CREATE TRIGGER test_flavor_trigger
         BEFORE INSERT ON proxima_core.installation
         FOR EACH ROW EXECUTE FUNCTION proxima_core.test_flavor_trigger()",
    )
    .execute(&pool)
    .await
    .expect("flavor trigger");
    sqlx::raw_sql(AssertSqlSafe(definitions_record_sql(
        "deadbeef",
        proxima_core::RELEASE_VERSION,
    )))
    .execute(&pool)
    .await
    .expect("force definitions pass");
    migration.run_migrations().await.expect("definitions rerun");
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_trigger WHERE tgname='test_flavor_trigger')",
    )
    .fetch_one(&pool)
    .await
    .expect("trigger catalog");
    assert!(exists);
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one core-only catalog compares the declared functions, triggers and policies"
)]
async fn core_catalog_has_no_undeclared_objects() {
    let Some((_name, migration, _guard)) = migration_db("census").await else {
        return;
    };
    migration.run_migrations().await.expect("migration");
    let pool = migration.clone_pool_for_backend();
    let mut declared = declared_names(BASELINE, "FUNCTION");
    for (name, count) in declared_names(definitions::FUNCTIONS, "FUNCTION") {
        *declared.entry(name).or_default() += count;
    }
    for (name, count) in declared_names(BASELINE, "AGGREGATE") {
        *declared.entry(name).or_default() += count;
    }
    for (name, count) in declared_names(definitions::FUNCTIONS, "AGGREGATE") {
        *declared.entry(name).or_default() += count;
    }
    declared.insert("definitions_applied".into(), 1);
    let live: BTreeMap<String, usize> = sqlx::query_scalar(
        "SELECT p.proname FROM pg_proc p
           JOIN pg_namespace n ON n.oid = p.pronamespace
          WHERE n.nspname = 'proxima_core'",
    )
    .fetch_all(&pool)
    .await
    .expect("functions")
    .into_iter()
    .fold(BTreeMap::new(), |mut map, name| {
        *map.entry(name).or_default() += 1;
        map
    });
    assert_eq!(live, declared);
    let mut declared_triggers = BTreeSet::new();
    let mut trigger_name = None;
    for line in definitions::TRIGGERS.lines() {
        let tokens: Vec<_> = line.split_whitespace().collect();
        if line.trim_start().starts_with("CREATE")
            && let Some(trigger) = tokens.iter().position(|token| *token == "TRIGGER")
        {
            let name = tokens.get(trigger + 1).copied().unwrap_or_default();
            trigger_name = Some(if name == "IF" {
                tokens.get(trigger + 4).copied().unwrap_or_default()
            } else {
                name
            });
        }
        if let Some(on) = line.find(" ON proxima_core.")
            && let Some(name) = trigger_name.take()
        {
            let table = line[on + " ON proxima_core.".len()..]
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_end_matches(';')
                .to_owned();
            declared_triggers.insert((table, name.to_owned()));
        }
    }
    let live_triggers: BTreeSet<(String, String)> = sqlx::query_as(
        "SELECT c.relname, t.tgname FROM pg_trigger t
           JOIN pg_class c ON c.oid = t.tgrelid
           JOIN pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = 'proxima_core' AND NOT t.tgisinternal",
    )
    .fetch_all(&pool)
    .await
    .expect("triggers")
    .into_iter()
    .collect();
    assert_eq!(live_triggers, declared_triggers);
    let tables: BTreeSet<String> = definitions::POLICIES
        .lines()
        .filter_map(|line| {
            line.split("'proxima_core', '")
                .nth(1)?
                .split('\'')
                .next()
                .map(str::to_owned)
        })
        .collect();
    let declared_policies: BTreeSet<(String, String)> = tables
        .into_iter()
        .flat_map(|table| {
            [
                "proxima_owner_read",
                "proxima_owner_write",
                "proxima_platform",
            ]
            .map(|policy| (table.clone(), policy.to_owned()))
        })
        .collect();
    let live_policies: BTreeSet<(String, String)> = sqlx::query_as(
        "SELECT c.relname, p.polname FROM pg_policy p
           JOIN pg_class c ON c.oid = p.polrelid
           JOIN pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = 'proxima_core'",
    )
    .fetch_all(&pool)
    .await
    .expect("policies")
    .into_iter()
    .collect();
    assert_eq!(live_policies, declared_policies);
}

#[tokio::test]
async fn newer_definitions_release_refuses_run_and_preflight() {
    let Some((_name, migration, _guard)) = migration_db("newer").await else {
        return;
    };
    migration.run_migrations().await.expect("first migration");
    let pool = migration.clone_pool_for_backend();
    sqlx::query(
        "CREATE OR REPLACE FUNCTION proxima_core.definitions_applied()
         RETURNS TABLE(checksum text, release text) LANGUAGE sql IMMUTABLE
         AS $$ SELECT 'deadbeef', '99.0.0' $$",
    )
    .execute(&pool)
    .await
    .expect("newer record");
    let run_error = migration
        .run_migrations()
        .await
        .expect_err("newer release refuses run");
    assert!(
        matches!(&run_error, StorageError::Internal(message)
            if message.contains("newer release 99.0.0")),
        "got: {run_error}"
    );
    let preflight_error =
        ensure_core_schema_current(&pool, proxima_storage_pg::MissingMigrationPolicy::Reject)
            .await
            .expect_err("newer release refuses preflight");
    assert!(
        matches!(&preflight_error, StorageError::Internal(message)
            if message.contains("newer release 99.0.0")),
        "got: {preflight_error}"
    );
}

#[tokio::test]
async fn same_release_checksum_drift_reapplies_definitions() {
    let Some((_name, migration, _guard)) = migration_db("checksum").await else {
        return;
    };
    migration.run_migrations().await.expect("first migration");
    let pool = migration.clone_pool_for_backend();
    sqlx::raw_sql(AssertSqlSafe(definitions_record_sql(
        "deadbeef",
        proxima_core::RELEASE_VERSION,
    )))
    .execute(&pool)
    .await
    .expect("drift record");
    migration
        .run_migrations()
        .await
        .expect("reapply definitions");
    let matches =
        definitions::current_matches(&mut pool.acquire().await.expect("connection").detach())
            .await
            .expect("definitions");
    assert!(matches);
}

#[tokio::test]
async fn pre_reset_main_ledger_is_stamped_without_baseline_execution() {
    let Some((_name, migration, _guard)) = migration_db("stamp").await else {
        return;
    };
    let pool = migration.clone_pool_for_backend();
    apply_pre_identity_schema(&pool)
        .await
        .expect("schema before store identities");
    let pairs = pre_reset_pairs();
    assert_eq!(pairs.len(), 32);
    let migrator = proxima_storage_pg::core_migrator();
    let baseline = migrator.iter().next().expect("embedded baseline");
    sqlx::query("DELETE FROM public._sqlx_migrations WHERE version = $1")
        .bind(baseline.version)
        .execute(&pool)
        .await
        .expect("remove current stamp");
    for (version, checksum) in pairs {
        sqlx::query(
            "INSERT INTO public._sqlx_migrations
                 (version, description, success, checksum, execution_time)
             VALUES ($1, 'old', true, $2, 0)",
        )
        .bind(version)
        .bind(checksum)
        .execute(&pool)
        .await
        .expect("restore pre-reset ledger");
    }
    migration.run_migrations().await.expect("stamp adoption");
    let (description, checksum, success): (String, Vec<u8>, bool) = sqlx::query_as(
        "SELECT description, checksum, success
           FROM public._sqlx_migrations WHERE version = $1",
    )
    .bind(baseline.version)
    .fetch_one(&pool)
    .await
    .expect("stamp");
    assert_eq!(description, baseline.description.as_ref());
    assert_eq!(checksum, baseline.checksum.as_ref());
    assert!(success);
}

#[tokio::test]
async fn incomplete_ledger_requires_schema_reset() {
    let Some((_name, migration, _guard)) = migration_db("unknown").await else {
        return;
    };
    let pool = migration.clone_pool_for_backend();
    for (version, checksum) in pre_reset_pairs().into_iter().take(29) {
        sqlx::query(
            "INSERT INTO public._sqlx_migrations
                 (version, description, success, checksum, execution_time)
             VALUES ($1, 'old', true, $2, 0)",
        )
        .bind(version)
        .bind(checksum)
        .execute(&pool)
        .await
        .expect("ledger row");
    }
    let error = migration
        .run_migrations()
        .await
        .expect_err("incomplete ledger must refuse");
    assert!(matches!(error, StorageError::SchemaResetRequired { .. }));
}
