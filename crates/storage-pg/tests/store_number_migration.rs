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

/// Owner policies through the store-identity section (migration 0034), without
/// the sections later migrations add, for fixtures that stop at 0034.
fn store_identity_policies() -> &'static str {
    let (through, _) = definitions::POLICIES
        .split_once("\n-- publication_counter\n")
        .expect("the publication-counter section follows store identity");
    assert!(
        through.contains("\n-- store_identity\n"),
        "the publication-counter section follows store identity"
    );
    through
}

#[tokio::test]
async fn retained_writer_rows_refuse_attribution_migration_without_changes() -> TestResult {
    let (_guard, platform, admin) = empty_store().await?;
    apply_historical_schema(&platform, &[33, 34], store_identity_policies()).await?;
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

async fn memory_pin_checks_catalog(
    connection: &mut sqlx::PgConnection,
) -> Result<serde_json::Value, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT jsonb_build_object(
             'oid', p.oid, 'owner', p.proowner,
             'definition', pg_get_functiondef(p.oid),
             'definer', p.prosecdef, 'settings', p.proconfig, 'acl', p.proacl,
             'public_execute', EXISTS (
                 SELECT 1 FROM aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) acl
                  WHERE acl.grantee = 0 AND acl.privilege_type = 'EXECUTE'))
           FROM pg_proc p WHERE p.oid = 'proxima_core.memory_pin_checks()'::regprocedure",
    )
    .fetch_one(connection)
    .await
}

#[tokio::test]
async fn attribution_migration_installs_the_restoration_seal_before_definitions() -> TestResult {
    let (_guard, platform, _admin) = empty_store().await?;
    apply_historical_schema(&platform, &[33, 34], store_identity_policies()).await?;
    let mut connection = platform.acquire().await?;
    let mut transaction = begin_migration_transaction(&mut connection).await?;
    // The historical fixture installs functions without their later-column triggers.
    sqlx::query("DROP FUNCTION proxima_core.memory_pin_checks()")
        .execute(transaction.as_mut())
        .await?;
    core_migrator().run(transaction.as_mut()).await?;
    let installed = memory_pin_checks_catalog(transaction.as_mut()).await?;
    assert_eq!(installed["definer"], true);
    assert_eq!(
        installed["settings"],
        serde_json::json!(["search_path=pg_catalog, proxima_core, pg_temp"])
    );
    assert_eq!(installed["public_execute"], false);
    let ledger: Vec<(i64, bool)> =
        sqlx::query_as("SELECT version, success FROM public._sqlx_migrations ORDER BY version")
            .fetch_all(transaction.as_mut())
            .await?;
    assert_eq!(
        ledger,
        [
            (33, true),
            (34, true),
            (35, true),
            (36, true),
            (37, true),
            (38, true),
            (39, true),
            (40, true),
            (41, true),
            (42, true),
            (43, true),
            (44, true),
        ]
    );
    assert!(!definitions::current_matches(transaction.as_mut()).await?);

    run_core_schema(&mut transaction, MissingMigrationPolicy::Reject).await?;
    assert!(definitions::current_matches(transaction.as_mut()).await?);
    assert_eq!(
        memory_pin_checks_catalog(transaction.as_mut()).await?,
        installed,
        "the migration must install the same sealed function as the definitions refresh"
    );
    transaction.commit().await?;
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
            ("memory".into(), "uuid".into()),
            // #480, migration 0037.
            ("edge_kind".into(), "text".into())
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

/// One parent of an external row at the 0036 shape: `(t, memory)` only.
async fn seed_external_parent(
    admin: &PgPool,
    owner: Uuid,
    kind: &str,
    schema_id: &str,
    origins: &[Uuid],
    refs: &[Uuid],
    cooled: bool,
) -> TestResult<Uuid> {
    let handle = Uuid::now_v7();
    let t = Uuid::now_v7();
    let content: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, $2, $3) RETURNING content_id",
    )
    .bind(owner)
    .bind(schema_id)
    .bind(t.as_bytes().repeat(2))
    .fetch_one(admin)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, $2::proxima_core.memory_kind, $3, $4, $5)",
    )
    .bind(handle)
    .bind(kind)
    .bind(schema_id)
    .bind(owner)
    .bind(t)
    .execute(admin)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory
             (handle, t, kind, owner_id, schema_id, content_id, origins, refs, \"by\",
              occurred_at)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, $5, $6, $7, $8, $4,
                 CASE WHEN $3 = 'fact' THEN uuid_extract_timestamp($2) END)",
    )
    .bind(handle)
    .bind(t)
    .bind(kind)
    .bind(owner)
    .bind(schema_id)
    .bind(content)
    .bind(origins)
    .bind(refs)
    .execute(admin)
    .await?;
    if cooled {
        sqlx::query(
            "INSERT INTO proxima_core.cooled
                 (t, handle, owner_id, kind, object_key, content_id, origins, refs, goal_refs, \"by\")
             SELECT t, handle, owner_id, kind, 'cold/' || t, content_id, origins, refs, goal_refs, \"by\"
               FROM proxima_core.memory WHERE t = $1",
        )
        .bind(t)
        .execute(admin)
        .await?;
        sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
            .bind(t)
            .execute(admin)
            .await?;
        sqlx::query("DELETE FROM proxima_core.memory_head WHERE handle = $1")
            .bind(handle)
            .execute(admin)
            .await?;
    }
    sqlx::query("INSERT INTO proxima_core.memory_external_origin (t, memory) VALUES ($1, $2)")
        .bind(t)
        .bind(Uuid::now_v7())
        .execute(admin)
        .await?;
    Ok(t)
}

/// #480 migration 0037: external rows get an edge kind. Interpretations
/// (hot: by schema; cooled: a Perspective with references and no origins,
/// since `cooled` has no schema column) are references; derivations and
/// orphan rows are origins. Re-running the migration changes nothing.
#[tokio::test]
async fn external_origin_edge_kind_backfill_reads_interpretations_as_references() -> TestResult {
    let (_guard, platform, admin) = empty_store().await?;
    apply_historical_schema(&platform, &[33, 34, 35, 36], definitions::POLICIES).await?;
    let owner = Uuid::now_v7();
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(owner)
        .execute(&admin)
        .await?;
    let subject = seed_external_parent(&admin, owner, "fact", "test/f-v1", &[], &[], false)
        .await
        .map_err(|e| format!("fact: {e}"))?;
    let interpretation = "core/interpretation-v1";
    let derivation = "core/agent-derivation-v1";
    let mut expected = Vec::new();
    for (kind, schema, origins, refs, cooled, edge) in [
        (
            "perspective",
            interpretation,
            vec![],
            vec![subject],
            false,
            "reference",
        ),
        (
            "abstraction",
            derivation,
            vec![subject],
            vec![],
            false,
            "origin",
        ),
        (
            "perspective",
            interpretation,
            vec![],
            vec![subject],
            true,
            "reference",
        ),
        (
            "abstraction",
            derivation,
            vec![subject],
            vec![],
            true,
            "origin",
        ),
    ] {
        let t = seed_external_parent(&admin, owner, kind, schema, &origins, &refs, cooled)
            .await
            .map_err(|e| format!("{kind} {schema} cooled={cooled}: {e}"))?;
        expected.push((t, edge.to_owned()));
    }
    let orphan = Uuid::now_v7();
    sqlx::query("INSERT INTO proxima_core.memory_external_origin (t, memory) VALUES ($1, $2)")
        .bind(orphan)
        .bind(Uuid::now_v7())
        .execute(&admin)
        .await?;
    expected.push((orphan, "origin".to_owned()));
    // The fact's own external row is a hot non-interpretation: origin.
    expected.push((subject, "origin".to_owned()));
    expected.sort();

    // Through 0037 only: 0038 refuses this populated store (#488).
    let mut through_0037 = core_migrator();
    through_0037.migrations = std::borrow::Cow::Owned(
        through_0037
            .iter()
            .filter(|migration| migration.version <= 37)
            .cloned()
            .collect(),
    );
    let mut connection = platform.acquire().await?;
    let mut transaction = begin_migration_transaction(&mut connection).await?;
    through_0037.run(transaction.as_mut()).await?;
    transaction.commit().await?;
    let read = || async {
        sqlx::query_as::<_, (Uuid, String)>(
            "SELECT t, edge_kind FROM proxima_core.memory_external_origin ORDER BY t, edge_kind",
        )
        .fetch_all(&admin)
        .await
    };
    assert_eq!(read().await?, expected);

    sqlx::raw_sql(include_str!(
        "../migrations/0037_v039_external_origin_edge.sql"
    ))
    .execute(&admin)
    .await?;
    assert_eq!(read().await?, expected, "0037 is idempotent");
    let refused = sqlx::query(
        "INSERT INTO proxima_core.memory_external_origin (t, memory, edge_kind)
         VALUES ($1, $2, 'sideways')",
    )
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .execute(&admin)
    .await;
    assert!(refused.is_err(), "edge_kind is origin or reference");
    Ok(())
}

/// One live Memory, or (cooled) one forgotten into `cooled`, in the 0037 layout.
async fn seed_v039_memory(admin: &PgPool, owner: Uuid, cooled: bool) -> TestResult {
    let handle = Uuid::now_v7();
    let t = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'fact', 'test/retained-v1', $2, $3)",
    )
    .bind(handle)
    .bind(owner)
    .bind(t)
    .execute(admin)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, \"by\", occurred_at)
         VALUES ($1, $2, 'fact', $3, 'test/retained-v1', $3, uuid_extract_timestamp($2))",
    )
    .bind(handle)
    .bind(t)
    .bind(owner)
    .execute(admin)
    .await?;
    if cooled {
        sqlx::query(
            "INSERT INTO proxima_core.cooled
                 (t, handle, owner_id, kind, object_key, origins, refs, goal_refs, \"by\")
             SELECT t, handle, owner_id, kind, 'cold/' || t, origins, refs, goal_refs, \"by\"
               FROM proxima_core.memory WHERE t = $1",
        )
        .bind(t)
        .execute(admin)
        .await?;
        sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
            .bind(t)
            .execute(admin)
            .await?;
        sqlx::query("DELETE FROM proxima_core.memory_head WHERE handle = $1")
            .bind(handle)
            .execute(admin)
            .await?;
    }
    Ok(())
}

/// #488 migration 0038: forgetting becomes a state on the Memory row, and
/// memories forgotten before it are not migrated. A store holding any
/// Memory, Goal, cooled memory or retained identity refuses, unchanged.
#[tokio::test]
async fn forget_state_migration_refuses_a_used_store_without_changes() -> TestResult {
    for cooled in [false, true] {
        let (_guard, platform, admin) = empty_store().await?;
        apply_historical_schema(&platform, &[33, 34, 35, 36, 37], definitions::POLICIES).await?;
        let owner = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner)
            .execute(&admin)
            .await?;
        seed_v039_memory(&admin, owner, cooled).await?;
        let before = writer_rows(&admin).await?;
        let schema_before = schema_state(&admin).await?;
        let mut connection = platform.acquire().await?;
        let mut transaction = begin_migration_transaction(&mut connection).await?;
        let error = core_migrator()
            .run(transaction.as_mut())
            .await
            .expect_err("a used store must refuse the forget-state migration");
        let sqlx::migrate::MigrateError::ExecuteMigration(error, 38) = error else {
            panic!("expected migration 0038 to refuse: {error}");
        };
        let database_error = error.as_database_error().expect("typed database refusal");
        assert_eq!(
            database_error.code().as_deref(),
            Some("55000"),
            "cooled={cooled}"
        );
        assert!(database_error.message().contains("fresh store required"));
        assert_eq!(
            database_error
                .downcast_ref::<sqlx::postgres::PgDatabaseError>()
                .hint(),
            Some("Create a fresh store before starting this release.")
        );
        transaction.rollback().await?;
        drop(connection);
        assert_eq!(writer_rows(&admin).await?, before, "cooled={cooled}");
        assert_eq!(
            schema_state(&admin).await?,
            schema_before,
            "cooled={cooled}"
        );
    }
    Ok(())
}

/// #488 migration 0040 drops the cooled table. The purge debts that were
/// its objects (`cold/<t>` keys, and rows from before a backend was named)
/// go with it; an upload's purge debt on a named backend stays.
#[tokio::test]
async fn drop_cooled_migration_keeps_only_upload_purge_rows() -> TestResult {
    let (_guard, platform, admin) = empty_store().await?;
    apply_historical_schema(
        &platform,
        &[33, 34, 35, 36, 37, 38, 39],
        definitions::POLICIES,
    )
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, backend)
         VALUES ('cold/0190a8f0-0000-7000-8000-000000000001', NULL, 'bucket'),
                ('objects/before-backends', NULL, ''),
                ('objects/upload-debt', NULL, 'bucket')",
    )
    .execute(&admin)
    .await?;

    // Through 0040 only: 0042 refuses a store that still holds purge debt.
    let mut through_0040 = core_migrator();
    through_0040.migrations = std::borrow::Cow::Owned(
        through_0040
            .iter()
            .filter(|migration| migration.version <= 40)
            .cloned()
            .collect(),
    );
    let mut connection = platform.acquire().await?;
    let mut transaction = begin_migration_transaction(&mut connection).await?;
    through_0040.run(transaction.as_mut()).await?;
    transaction.commit().await?;
    drop(connection);

    let left: Vec<String> = sqlx::query_scalar(
        "SELECT object_key FROM proxima_core.cold_purge_pending ORDER BY object_key",
    )
    .fetch_all(&admin)
    .await?;
    assert_eq!(left, vec!["objects/upload-debt".to_owned()]);
    let cooled: bool = sqlx::query_scalar("SELECT to_regclass('proxima_core.cooled') IS NOT NULL")
        .fetch_one(&admin)
        .await?;
    assert!(!cooled, "0040 drops proxima_core.cooled");
    Ok(())
}
