//! The identity-layout migration requires a fresh store and never discards history.

#[path = "support/migration_reset.rs"]
pub mod migration_reset_support;

use migration_reset_support::{apply_historical_schema, apply_pre_identity_schema};
use proxima_pg_testkit::{DbGuard, create_db, db_url, split_role_urls};
use proxima_storage_pg::{
    MissingMigrationPolicy, begin_migration_transaction, core_migrator, definitions,
    run_core_schema,
};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug)]
enum ExistingIdentity {
    Memory,
    Goal,
    Cooled,
    Erased,
    ExternalOrigin,
    DelegationGrant,
}

async fn seed_identity(pool: &PgPool, existing: ExistingIdentity) -> TestResult {
    let owner = Uuid::now_v7();
    let handle = Uuid::now_v7();
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(owner)
        .execute(pool)
        .await?;
    match existing {
        ExistingIdentity::Memory | ExistingIdentity::Cooled | ExistingIdentity::Erased => {
            sqlx::query(
                "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
                 VALUES ($1, 'fact', 'test/retained-v1', $2, $3)",
            )
            .bind(handle)
            .bind(owner)
            .bind(id)
            .execute(pool)
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.memory
                     (handle, t, kind, owner_id, schema_id, occurred_at)
                 VALUES ($1, $2, 'fact', $3, 'test/retained-v1', uuid_extract_timestamp($2))",
            )
            .bind(handle)
            .bind(id)
            .bind(owner)
            .execute(pool)
            .await?;
            if matches!(existing, ExistingIdentity::Cooled) {
                sqlx::query(
                    "INSERT INTO proxima_core.cooled
                         (t, handle, owner_id, kind, object_key, origins, refs, goal_refs)
                     SELECT t, handle, owner_id, kind, 'cold/retained', origins, refs, goal_refs
                       FROM proxima_core.memory WHERE t = $1",
                )
                .bind(id)
                .execute(pool)
                .await?;
            }
            if !matches!(existing, ExistingIdentity::Memory) {
                sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
                    .bind(id)
                    .execute(pool)
                    .await?;
                sqlx::query("DELETE FROM proxima_core.memory_head WHERE handle = $1")
                    .bind(handle)
                    .execute(pool)
                    .await?;
                if matches!(existing, ExistingIdentity::Erased) {
                    sqlx::query(
                        "INSERT INTO proxima_core.erased_pin_target (t, kind) VALUES ($1, 'fact')",
                    )
                    .bind(id)
                    .execute(pool)
                    .await?;
                }
                let hot_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory")
                    .fetch_one(pool)
                    .await?;
                assert_eq!(
                    hot_rows, 0,
                    "the fixture retains only cold or erased identity"
                );
            }
        }
        ExistingIdentity::Goal => {
            sqlx::query(
                "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
                 VALUES ($1, 'test/retained-goal-v1', $2, $3)",
            )
            .bind(handle)
            .bind(owner)
            .bind(id)
            .execute(pool)
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.goal (handle, t, owner_id, title, state, request_id, text)
                 VALUES ($1, $2, $3, 'retained goal', 'Active', 'retained-goal', 'retained text')",
            )
            .bind(handle)
            .bind(id)
            .bind(owner)
            .execute(pool)
            .await?;
        }
        ExistingIdentity::ExternalOrigin => {
            // No FK: even an orphan origin must not be silently dropped.
            sqlx::query(
                "INSERT INTO proxima_core.memory_external_origin (t, store, handle)
                 VALUES ($1, 'remote', $2)",
            )
            .bind(id)
            .bind(Uuid::now_v7().to_string())
            .execute(pool)
            .await?;
        }
        ExistingIdentity::DelegationGrant => seed_delegation_grant(pool, id, owner).await?,
    }
    Ok(())
}

async fn seed_delegation_grant(pool: &PgPool, id: Uuid, owner: Uuid) -> TestResult {
    sqlx::query(
        "INSERT INTO proxima_core.delegated_authority_grants
             (delegation_id, subject_user_id, owner_id, tool_name,
              read_ceiling, write_ceiling, expires_at, auth_epoch, issued_at)
         VALUES ($1, $2, $2, 'core_remember', 'fact', 'fact',
                 TIMESTAMPTZ '2030-01-01 00:00:00Z', 1,
                 TIMESTAMPTZ '2026-01-01 00:00:00Z')",
    )
    .bind(id)
    .bind(owner)
    .execute(pool)
    .await?;
    Ok(())
}

async fn retained_rows(pool: &PgPool) -> Result<serde_json::Value, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT jsonb_build_object(
             'memory', (SELECT jsonb_agg(to_jsonb(m) ORDER BY m.t) FROM proxima_core.memory m),
             'memory_head', (SELECT jsonb_agg(to_jsonb(h) ORDER BY h.handle) FROM proxima_core.memory_head h),
             'goal', (SELECT jsonb_agg(to_jsonb(g) ORDER BY g.t) FROM proxima_core.goal g),
             'goal_head', (SELECT jsonb_agg(to_jsonb(h) ORDER BY h.handle) FROM proxima_core.goal_head h),
             'cooled', (SELECT jsonb_agg(to_jsonb(c) ORDER BY c.t) FROM proxima_core.cooled c),
             'erased', (SELECT jsonb_agg(to_jsonb(e) ORDER BY e.t) FROM proxima_core.erased_pin_target e),
             'origins', (SELECT jsonb_agg(to_jsonb(o) ORDER BY o.t, o.store, o.handle) FROM proxima_core.memory_external_origin o))",
    )
    .fetch_one(pool)
    .await
}

async fn empty_store() -> TestResult<(DbGuard, PgPool, PgPool)> {
    let name = format!("proxima_test_identity_guard_{}", Uuid::now_v7().simple());
    create_db(&name).await?;
    let guard = DbGuard::adopt(name.clone());
    let (_, platform_url) = split_role_urls(&name).await?;
    let platform = PgPoolOptions::new()
        .max_connections(1)
        .connect(&platform_url)
        .await?;
    let admin = PgPool::connect(&db_url(&name)).await?;
    Ok((guard, platform, admin))
}

async fn pre_identity_store() -> TestResult<(DbGuard, PgPool, PgPool)> {
    let (guard, platform, admin) = empty_store().await?;
    apply_pre_identity_schema(&platform).await?;
    Ok((guard, platform, admin))
}

async fn schema_state(pool: &PgPool) -> Result<serde_json::Value, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT jsonb_build_object(
             'decoder', pg_get_functiondef('proxima_core.memory_created_at(uuid)'::regprocedure),
             'defaults', (SELECT jsonb_agg(jsonb_build_object('table', table_name, 'default', column_default) ORDER BY table_name)
                            FROM information_schema.columns
                           WHERE table_schema = 'proxima_core'
                             AND table_name IN ('memory', 'goal') AND column_name = 't'),
             'origin_columns', (SELECT jsonb_agg(jsonb_build_object('column', column_name, 'type', data_type) ORDER BY ordinal_position)
                                  FROM information_schema.columns
                                 WHERE table_schema = 'proxima_core' AND table_name = 'memory_external_origin'),
             'writer_columns', (SELECT jsonb_agg(jsonb_build_object('table', table_name, 'column', column_name,
                                      'type', data_type, 'nullable', is_nullable, 'default', column_default)
                                      ORDER BY table_name, ordinal_position)
                                  FROM information_schema.columns
                                 WHERE table_schema = 'proxima_core'
                                   AND table_name IN ('memory', 'cooled', 'goal', 'delegated_authority_grants')
                                   AND column_name IN ('by', 'via')),
             'ledger', (SELECT jsonb_agg(to_jsonb(m) ORDER BY m.version) FROM public._sqlx_migrations m))",
    )
    .fetch_one(pool)
    .await
}

async fn refuses_without_changes(existing: ExistingIdentity) -> TestResult {
    let (_guard, platform, admin) = pre_identity_store().await?;
    seed_identity(&admin, existing).await?;
    let before = retained_rows(&admin).await?;
    assert!(
        before
            .as_object()
            .expect("row snapshot")
            .values()
            .any(|rows| rows.as_array().is_some_and(|rows| !rows.is_empty())),
        "{existing:?} must leave a physical identity behind"
    );
    let schema_before = schema_state(&admin).await?;

    let mut connection = platform.acquire().await?;
    let mut transaction = begin_migration_transaction(&mut connection).await?;
    // A caller's empty RLS scope cannot conceal the existing identities.
    sqlx::query("SELECT set_config('app.proxima_scope', '', true)")
        .execute(transaction.as_mut())
        .await?;
    let visible: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM proxima_core.memory)
              + (SELECT count(*) FROM proxima_core.memory_head)
              + (SELECT count(*) FROM proxima_core.goal)
              + (SELECT count(*) FROM proxima_core.goal_head)
              + (SELECT count(*) FROM proxima_core.cooled)
              + (SELECT count(*) FROM proxima_core.erased_pin_target)
              + (SELECT count(*) FROM proxima_core.memory_external_origin)",
    )
    .fetch_one(transaction.as_mut())
    .await?;
    assert_eq!(visible, 0, "empty scope must hide the populated fixture");
    let error = core_migrator()
        .run(transaction.as_mut())
        .await
        .expect_err("used stores must refuse the identity-layout migration");
    let sqlx::migrate::MigrateError::ExecuteMigration(error, 34) = error else {
        panic!("expected the identity migration to refuse: {error}");
    };
    let database_error = error.as_database_error().expect("typed database refusal");
    assert_eq!(database_error.code().as_deref(), Some("55000"));
    assert!(database_error.message().contains("fresh store required"));
    transaction.rollback().await?;
    assert_eq!(retained_rows(&admin).await?, before, "{existing:?}");
    assert_eq!(schema_state(&admin).await?, schema_before, "{existing:?}");
    let untouched: (bool, bool, i64) = sqlx::query_as(
        "SELECT to_regclass('proxima_core.store_identity') IS NULL,
                (SELECT bool_and(column_default IS NOT NULL)
                   FROM information_schema.columns
                  WHERE table_schema = 'proxima_core'
                    AND table_name IN ('memory', 'goal') AND column_name = 't'),
                (SELECT max(version) FROM public._sqlx_migrations)",
    )
    .fetch_one(&admin)
    .await?;
    assert_eq!(untouched, (true, true, 33));
    Ok(())
}

#[tokio::test]
async fn populated_memory_refuses_identity_migration_without_changes() -> TestResult {
    refuses_without_changes(ExistingIdentity::Memory).await
}

#[tokio::test]
async fn populated_goal_refuses_identity_migration_without_changes() -> TestResult {
    refuses_without_changes(ExistingIdentity::Goal).await
}

#[tokio::test]
async fn cooled_memory_refuses_identity_migration_without_changes() -> TestResult {
    refuses_without_changes(ExistingIdentity::Cooled).await
}

#[tokio::test]
async fn erased_identity_refuses_identity_migration_without_changes() -> TestResult {
    refuses_without_changes(ExistingIdentity::Erased).await
}

#[tokio::test]
async fn orphan_origin_refuses_identity_migration_without_changes() -> TestResult {
    refuses_without_changes(ExistingIdentity::ExternalOrigin).await
}

async fn writer_rows(pool: &PgPool) -> Result<serde_json::Value, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT jsonb_build_object(
             'memory', (SELECT jsonb_agg(to_jsonb(m) ORDER BY m.t) FROM proxima_core.memory m),
             'cooled', (SELECT jsonb_agg(to_jsonb(c) ORDER BY c.t) FROM proxima_core.cooled c),
             'goal', (SELECT jsonb_agg(to_jsonb(g) ORDER BY g.t) FROM proxima_core.goal g),
             'grants', (SELECT jsonb_agg(to_jsonb(d) ORDER BY d.delegation_id)
                          FROM proxima_core.delegated_authority_grants d))",
    )
    .fetch_one(pool)
    .await
}

async fn refuses_writer_migration(
    platform: &PgPool,
    admin: &PgPool,
    existing: ExistingIdentity,
) -> TestResult {
    seed_identity(admin, existing).await?;
    let before = writer_rows(admin).await?;
    assert_eq!(
        before
            .as_object()
            .expect("writer-bearing rows")
            .values()
            .filter(|rows| rows.as_array().is_some_and(|rows| !rows.is_empty()))
            .count(),
        1,
        "{existing:?} must populate exactly one writer-bearing table"
    );
    if matches!(existing, ExistingIdentity::DelegationGrant) {
        assert_eq!(before["grants"].as_array().map(Vec::len), Some(1));
    }
    let schema_before = schema_state(admin).await?;
    assert!(schema_before["writer_columns"].is_null());
    let versions: Vec<_> = schema_before["ledger"]
        .as_array()
        .expect("migration ledger")
        .iter()
        .map(|row| row["version"].as_i64().expect("migration version"))
        .collect();
    assert_eq!(versions, [33, 34]);
    let mut connection = platform.acquire().await?;
    let mut transaction = begin_migration_transaction(&mut connection).await?;
    sqlx::query("SELECT set_config('app.proxima_scope', '', true)")
        .execute(transaction.as_mut())
        .await?;
    let visible: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM proxima_core.memory)
              + (SELECT count(*) FROM proxima_core.cooled)
              + (SELECT count(*) FROM proxima_core.goal)
              + (SELECT count(*) FROM proxima_core.delegated_authority_grants)",
    )
    .fetch_one(transaction.as_mut())
    .await?;
    assert_eq!(visible, 0, "empty scope must hide the populated fixture");
    let error = core_migrator()
        .run(transaction.as_mut())
        .await
        .expect_err("used stores must refuse writer attribution migration");
    let sqlx::migrate::MigrateError::ExecuteMigration(error, 35) = error else {
        panic!("expected the attribution migration to refuse: {error}");
    };
    let database_error = error.as_database_error().expect("typed database refusal");
    assert_eq!(database_error.code().as_deref(), Some("55000"));
    assert!(database_error.message().contains("fresh store required"));
    assert_eq!(
        database_error
            .downcast_ref::<sqlx::postgres::PgDatabaseError>()
            .hint(),
        Some("Create a fresh store before starting this release.")
    );
    transaction.rollback().await?;
    assert_eq!(writer_rows(admin).await?, before, "{existing:?}");
    assert_eq!(schema_state(admin).await?, schema_before, "{existing:?}");
    Ok(())
}

#[tokio::test]
async fn retained_writer_rows_refuse_attribution_migration_without_changes() -> TestResult {
    let (_guard, platform, admin) = empty_store().await?;
    apply_historical_schema(&platform, &[33, 34], definitions::POLICIES).await?;
    for existing in [
        ExistingIdentity::Memory,
        ExistingIdentity::Cooled,
        ExistingIdentity::Goal,
        ExistingIdentity::DelegationGrant,
    ] {
        refuses_writer_migration(&platform, &admin, existing).await?;
        sqlx::raw_sql(
            "DELETE FROM proxima_core.memory;
             DELETE FROM proxima_core.cooled;
             DELETE FROM proxima_core.goal;
             DELETE FROM proxima_core.delegated_authority_grants;
             DELETE FROM proxima_core.memory_head;
             DELETE FROM proxima_core.goal_head;",
        )
        .execute(&admin)
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn empty_store_accepts_identity_schema_and_current_definitions() -> TestResult {
    let (_guard, platform, admin) = empty_store().await?;
    let mut connection = platform.acquire().await?;
    let mut transaction = begin_migration_transaction(&mut connection).await?;
    run_core_schema(&mut transaction, MissingMigrationPolicy::Reject).await?;
    assert!(definitions::current_matches(transaction.as_mut()).await?);
    transaction.commit().await?;

    let installed: (bool, bool, i64) = sqlx::query_as(
        "SELECT to_regclass('proxima_core.store_identity') IS NOT NULL,
                (SELECT bool_and(column_default IS NULL)
                   FROM information_schema.columns
                  WHERE table_schema = 'proxima_core'
                    AND table_name IN ('memory', 'goal') AND column_name = 't'),
                (SELECT max(version) FROM public._sqlx_migrations)",
    )
    .fetch_one(&admin)
    .await?;
    let current_version = core_migrator()
        .iter()
        .map(|migration| migration.version)
        .max()
        .expect("current core migration");
    assert_eq!(installed, (true, true, current_version));
    let identity_migration_applied: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM public._sqlx_migrations WHERE version = 34 AND success)",
    )
    .fetch_one(&admin)
    .await?;
    assert!(identity_migration_applied);
    let origin_columns: Vec<(String, String)> = sqlx::query_as(
        "SELECT column_name::text, data_type::text FROM information_schema.columns
          WHERE table_schema = 'proxima_core' AND table_name = 'memory_external_origin'
          ORDER BY ordinal_position",
    )
    .fetch_all(&admin)
    .await?;
    assert_eq!(
        origin_columns,
        [
            ("t".into(), "uuid".into()),
            ("memory".into(), "uuid".into())
        ]
    );
    let protected: bool = sqlx::query_scalar(
        "SELECT c.relrowsecurity AND c.relforcerowsecurity
             AND EXISTS (SELECT 1 FROM pg_trigger t WHERE t.tgrelid = c.oid
                          AND t.tgname = 'store_identity_immutable' AND NOT t.tgisinternal)
           FROM pg_class c WHERE c.oid = 'proxima_core.store_identity'::regclass",
    )
    .fetch_one(&admin)
    .await?;
    assert!(
        protected,
        "the definitions install immutable, RLS-protected identity"
    );
    Ok(())
}
