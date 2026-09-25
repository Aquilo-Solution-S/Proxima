use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use proxima::flavor::FlavorBundle;
use proxima::{
    AppInfo, EmbedConfig, EmbedError, FlavorApp, NamedMigrator, PayloadKind, Proxima,
    ProximaBuilder, company_owner, run_core_and_flavor_migrations,
};
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::{
    AgentNoteV1, AuthError, AuthPath, Authenticator, AuthzContext, Credentials, FactPayload,
    FlavorRegistry, FlavorRegistryError, MemoryId, Owner, Role, SchemaId, SchemaVersion, ToolScope,
    UserId,
};
use proxima_pg_testkit::{admin_url, create_db, db_url, drop_db, split_role_urls, unique_db_name};
use proxima_storage_pg::{PgSidecarKey, PgStorage};
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::{Connection, SqlSafeStr};
use tokio::time::{Duration, Instant};
use uuid::Uuid;

struct GoalTestApp;

fn host_context(owner: Owner, path: AuthPath) -> AuthzContext {
    proxima_core::test_fixtures::authenticated_context(AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(owner, Role::admin())],
        path,
    ))
    .narrowed_to_owner(owner)
    .expect("trusted host resolved this exact owner")
}

#[derive(Debug)]
struct TestAuthenticator {
    subject: UserId,
    owner: Owner,
}

#[async_trait]
impl Authenticator for TestAuthenticator {
    async fn authenticate(&self, _credentials: &Credentials) -> Result<AuthzContext, AuthError> {
        Ok(AuthzContext::for_subject_with_role(
            self.subject,
            [(self.owner, Role::admin())],
            AuthPath::HostBearer,
        ))
    }
}

fn drain_note(title: &str) -> AgentNoteV1 {
    AgentNoteV1 {
        note_id: Uuid::now_v7(),
        title: title.into(),
        body: title.into(),
        tags: Vec::new(),
        idempotency_key: Some(title.into()),
    }
}

impl FlavorBundle for GoalTestApp {
    fn register(_registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

async fn count_fact_embeddings(
    pool: &sqlx::PgPool,
    memory_id: MemoryId,
    model_id: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.embeddings
          WHERE entity_id = $1
            AND model_id = $2",
    )
    .bind(memory_id.into_inner())
    .bind(model_id)
    .fetch_one(pool)
    .await
}

async fn count_embedding_jobs(
    pool: &sqlx::PgPool,
    memory_id: MemoryId,
    model_id: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.embedding_jobs
          WHERE entity_id = $1
            AND model_id = $2",
    )
    .bind(memory_id.into_inner())
    .bind(model_id)
    .fetch_one(pool)
    .await
}

async fn wait_for_embedding_drain(
    pool: &sqlx::PgPool,
    memory_id: MemoryId,
    model_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let jobs = count_embedding_jobs(pool, memory_id, model_id).await?;
        let embeddings = count_fact_embeddings(pool, memory_id, model_id).await?;
        if jobs == 0 && embeddings == 1 {
            return Ok(());
        }
        assert!(
            Instant::now() < deadline,
            "embedding worker did not drain job before deadline: jobs={jobs} embeddings={embeddings}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

impl FlavorApp for GoalTestApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "goal-test",
            title: "Goal Test",
            version: "1",
        }
    }
}

fn quoted_ident(input: &str) -> String {
    format!("\"{}\"", input.replace('"', "\"\""))
}

#[derive(Debug)]
struct FixedDimEmbedding {
    model_id: String,
    dim: usize,
}

impl FixedDimEmbedding {
    fn new(model_id: impl Into<String>, dim: usize) -> Self {
        Self {
            model_id: model_id.into(),
            dim,
        }
    }
}

#[async_trait]
impl EmbeddingClient for FixedDimEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![0.0; self.dim])
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.dim
    }
}

#[tokio::test]
async fn pre_v004_database_fails_closed_in_migration_facade() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&db_url).await?;
        sqlx::query("CREATE SCHEMA proxima_core")
            .execute(pg.pool_for_tests())
            .await?;
        sqlx::query(
            "CREATE TABLE public._sqlx_migrations (
                 version bigint PRIMARY KEY,
                 description text NOT NULL,
                 installed_on timestamptz NOT NULL DEFAULT now(),
                 success boolean NOT NULL,
                 checksum bytea NOT NULL,
                 execution_time bigint NOT NULL
             )",
        )
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO public._sqlx_migrations
                 (version, description, success, checksum, execution_time)
             VALUES (1, 'init', true, decode('00', 'hex'), 0)",
        )
        .execute(pg.pool_for_tests())
        .await?;

        let err = run_core_and_flavor_migrations(&pg, Vec::<NamedMigrator>::new())
            .await
            .expect_err("stale ledger must fail closed through facade");
        let msg = err.to_string();
        assert!(
            msg.contains("reset"),
            "error must explain reset, got: {msg}",
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("migration facade fail-closed test failed");
}

#[tokio::test]
async fn pre_v004_database_surfaces_typed_reset_error_through_boot() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&db_url).await?;
        sqlx::query("CREATE SCHEMA proxima_core")
            .execute(pg.pool_for_tests())
            .await?;
        sqlx::query(
            "CREATE TABLE public._sqlx_migrations (
                 version bigint PRIMARY KEY,
                 description text NOT NULL,
                 installed_on timestamptz NOT NULL DEFAULT now(),
                 success boolean NOT NULL,
                 checksum bytea NOT NULL,
                 execution_time bigint NOT NULL
             )",
        )
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO public._sqlx_migrations
                 (version, description, success, checksum, execution_time)
             VALUES (1, 'init', true, decode('00', 'hex'), 0)",
        )
        .execute(pg.pool_for_tests())
        .await?;
        pg.pool_for_tests().close().await;
        drop(pg);

        let config = EmbedConfig {
            database_url: db_url.clone(),
            platform_database_url: None,
            s3: None,
        };
        let owner = company_owner(Uuid::now_v7());

        let err = ProximaBuilder::new(config, owner)
            .boot()
            .await
            .expect_err("stale ledger must fail closed through boot()");

        match err {
            EmbedError::SchemaResetRequired { details } => {
                assert!(
                    details.contains("0001_v008") || details.contains("checksum"),
                    "reset details should name the schema mismatch, got: {details}"
                );
            }
            other => {
                panic!(
                    "expected EmbedError::SchemaResetRequired, boot() collapsed it to: {other:?}"
                )
            }
        }
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("typed reset error propagation test failed");
}

#[tokio::test]
async fn facade_run_with_custom_auth_needs_no_separate_owner_access() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let (runtime_url, platform_url) = split_role_urls(&db_name).await.expect("split roles");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let subject = UserId::new(Uuid::now_v7());
        let running = Proxima::<GoalTestApp>::app()
            .database_url(runtime_url)
            .platform_database_url(platform_url)
            .owner(owner)
            .authenticator(Arc::new(TestAuthenticator { subject, owner }))
            .tool_scope(ToolScope::All)
            .with_mcp()
            .mcp_bind("127.0.0.1:0".parse()?)
            .run()
            .await?;

        let addr = running.mcp_addr.expect("mcp bound");
        assert!(addr.ip().is_loopback());
        let expected_url = format!("http://{addr}/mcp");
        assert_eq!(
            running.engine.mcp_url().as_deref(),
            Some(expected_url.as_str())
        );
        running.shutdown().await;
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("facade run integration test failed");
}

#[tokio::test]
async fn facade_boot_exposes_pg_sidecars_and_worker_drains_embedding_jobs() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let (runtime_url, platform_url) = split_role_urls(&db_name).await.expect("split roles");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let model_id = "facade-drain-embed";
        let inspection = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let built = Proxima::<GoalTestApp>::app()
            .database_url(runtime_url)
            .platform_database_url(platform_url)
            .owner(owner)
            .tool_scope(ToolScope::All)
            .embed_client(Arc::new(ConstantEmbedding::prefixed(
                model_id,
                &[0.25, 0.5, 0.75],
            )))
            .build()
            .await?;
        let note_key = PgSidecarKey::new(
            PayloadKind::Fact,
            SchemaId::new(proxima_core::AgentNoteV1::SCHEMA_ID.into()),
            SchemaVersion::new(proxima_core::AgentNoteV1::SCHEMA_VERSION),
        );
        assert!(
            built.pg_sidecars.contains(&note_key),
            "boot result exposes the frozen core PG sidecar registry"
        );

        let payload = drain_note("facade worker drain fact");
        let authz = host_context(owner, AuthPath::HostBearer);
        let outcome = built
            .engine
            .ingest_fact(
                &authz,
                proxima::FactWrite::new(owner, "test/facade-worker", &payload),
            )
            .await?;
        assert_eq!(
            count_fact_embeddings(&inspection, outcome.memory_id, model_id).await?,
            0
        );
        assert_eq!(
            count_embedding_jobs(&inspection, outcome.memory_id, model_id).await?,
            1
        );

        let cancel = tokio_util::sync::CancellationToken::new();
        let worker = built.spawn_embedding_worker(cancel.clone());
        wait_for_embedding_drain(&inspection, outcome.memory_id, model_id).await?;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), worker).await??;

        built.shutdown();
        inspection.close().await;
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("facade boot sidecar/worker integration test failed");
}

#[tokio::test]
async fn boot_rejects_embedding_client_with_unsupported_width() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let (runtime_url, platform_url) = split_role_urls(&db_name).await.expect("split roles");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let config = || EmbedConfig {
            database_url: runtime_url.clone(),
            platform_database_url: Some(platform_url.clone()),
            s3: None,
        };

        // A width no lane indexes must fail fast at boot with Config,
        // before any job is claimed and then refused at insert.
        let err = ProximaBuilder::new(config(), owner)
            .embed_client(Arc::new(FixedDimEmbedding::new("unlaned", 512)))
            .boot()
            .await
            .expect_err("an unsupported embedding width must be rejected at boot");
        match err {
            EmbedError::Config(msg) => {
                assert!(
                    msg.contains("512"),
                    "message names the offending width: {msg}"
                );
                assert!(
                    msg.contains("not supported"),
                    "message explains the width is unsupported: {msg}"
                );
            }
            other => panic!("expected EmbedError::Config, got {other:?}"),
        }

        // One client and a router are two answers to one question.
        let lane_768 = proxima_core::llm::BoundEmbeddingClient::bind(Arc::new(
            FixedDimEmbedding::new("lane-768", 768),
        ))?;
        let err = ProximaBuilder::new(config(), owner)
            .embed_client(Arc::new(FixedDimEmbedding::new("lane-768", 768)))
            .embedding_router(Arc::new(proxima_core::llm::SingleClientRouter::new(
                lane_768,
            )))
            .boot()
            .await
            .expect_err("a client and a router together must be rejected at boot");
        assert!(
            matches!(&err, EmbedError::Config(msg) if msg.contains("not both")),
            "expected the either-or config error, got {err:?}"
        );

        // A supported width other than the 1024 default boots.
        let booted = ProximaBuilder::new(config(), owner)
            .embed_client(Arc::new(FixedDimEmbedding::new("lane-768", 768)))
            .boot()
            .await?;
        let route = booted.engine.embedding_route(&owner).await?;
        assert_eq!(
            route.current_client().map(|client| client.space().dim()),
            Some(proxima_core::llm::EmbeddingDim::D768),
            "a supported-width client routes every Owner"
        );
        booted.engine.stop(booted.handle);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("embedding width guard test failed");
}

fn role_ddl_migrator(role_name: &str) -> Migrator {
    Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            20_990_101_000_001,
            Cow::Borrowed("role ddl on a shared catalog"),
            MigrationType::Simple,
            // SQL-POLICY: fixed-fragment — {role_name} is minted by the calling
            // test from Uuid::now_v7().simple() under a fixed [a-z_] prefix, so the
            // spliced identifier is [a-z0-9_] only; no caller value reaches it.
            sqlx::AssertSqlSafe(format!("ALTER ROLE {role_name} SET lock_timeout = '32s';"))
                .into_sql_str(),
            false,
        )]),
        ..Migrator::DEFAULT
    }
}

/// Boot-migrate one database while another session on a *different database*
/// of the same cluster holds an uncommitted write to the same role's shared
/// catalogs, and assert the facade rides out the lost race.
///
/// With `role_settings_pre_seeded` the role already has a committed
/// `pg_db_role_setting` row, so holder and migration both UPDATE the shared
/// tuple and the loser gets `tuple concurrently updated` (XX000). Without it
/// both INSERT the missing row and the loser gets a `23505` unique violation
/// on the catalog's index. Both shapes are deterministic: the blocked
/// statement fails the moment the holder commits.
#[expect(
    clippy::too_many_lines,
    reason = "the fixture covers two catalog race shapes with explicit setup and evidence"
)]
async fn assert_role_ddl_contention_retries_to_green(
    role_settings_pre_seeded: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let role_name = format!("proxima_test_role_{}", Uuid::now_v7().simple());
    let role_password = "proxima_test_role_password";

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let mut admin = sqlx::PgConnection::connect(&admin_url()).await?;
        // SQL-POLICY: fixed-fragment — {role_name} is minted by this test from
        // Uuid::now_v7().simple() under a fixed [a-z_] prefix, so the spliced
        // identifier is [a-z0-9_] only and no caller value reaches the statement.
        // SQL-POLICY: fixed-fragment — {role_name} is the test-minted closed
        // identifier above and {role_password} is a fixed fixture secret.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE ROLE {role_name} LOGIN PASSWORD '{role_password}' NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS"
        )))
            .execute(&mut admin)
            .await?;
        // SQL-POLICY: fixed-fragment — both identifiers are quoted; db_name is
        // generated by unique_db_name and role_name is the closed fixture role.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "GRANT CREATE ON DATABASE {} TO {}",
            quoted_ident(&db_name),
            quoted_ident(&role_name),
        )))
        .execute(&mut admin)
        .await?;
        let mut target_admin = sqlx::PgConnection::connect(&db_url(&db_name)).await?;
        sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
            .execute(&mut target_admin)
            .await?;
        // SQL-POLICY: fixed-fragment — role_name is the generated, quoted
        // fixture identifier; the target schema is the literal public schema.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "GRANT CREATE ON SCHEMA public TO {}",
            quoted_ident(&role_name),
        )))
        .execute(&mut target_admin)
        .await?;
        target_admin.close().await?;
        if role_settings_pre_seeded {
            // SQL-POLICY: fixed-fragment — same test-minted {role_name} as above.
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER ROLE {role_name} SET idle_in_transaction_session_timeout = '30s'"
            )))
            .execute(&mut admin)
            .await?;
        }

        // Hold an uncommitted write to the role's shared-catalog row from the
        // admin database. The migrating boot below runs against a different
        // database, so no database-scoped lock protects it from this.
        let mut tx = admin.begin().await?;
        // SQL-POLICY: fixed-fragment — same test-minted {role_name} as above.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER ROLE {role_name} SET statement_timeout = '31s'"
        )))
        .execute(tx.as_mut())
        .await?;

        let fixture_admin_url = admin_url();
        let authority = fixture_admin_url
            .split_once("://")
            .and_then(|(_, rest)| rest.split_once('@'))
            .map(|(_, authority)| authority.split('/').next().unwrap_or(authority))
            .ok_or("test admin URL must contain an authority")?;
        let role_url = format!("postgres://{role_name}:{role_password}@{authority}/{db_name}");
        let pg = PgStorage::connect(&role_url).await?;
        let migrator = role_ddl_migrator(&role_name);
        let run = tokio::spawn(async move {
            run_core_and_flavor_migrations(&pg, [NamedMigrator::new("role-ddl-flavor", migrator)])
                .await
        });

        // Wait until the flavor's ALTER ROLE is blocked on the held row, then
        // commit: Postgres answers the blocked statement with the lost-race
        // error the moment the holder's write commits. The migration
        // connection carries a 5s lock_timeout, so the commit must come
        // promptly once blockage is visible.
        let mut poll = sqlx::PgConnection::connect(&admin_url()).await?;
        let deadline = Instant::now() + Duration::from_mins(1);
        loop {
            if run.is_finished() {
                let outcome = run.await.expect("migration task must not panic");
                panic!("migration finished before contention became visible: {outcome:?}");
            }
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                     SELECT 1 FROM pg_stat_activity
                     WHERE state = 'active'
                       AND wait_event_type = 'Lock'
                       AND query LIKE 'ALTER ROLE%32s%'
                 )",
            )
            .fetch_one(&mut poll)
            .await?;
            if blocked {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "migration never blocked on the held role row"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tx.commit().await?;

        let report = run.await.expect("migration task must not panic")?;
        assert_eq!(report.sources, ["proxima-core", "role-ddl-flavor"]);

        // The retried migration's effect reached the shared catalog.
        let setconfig: Option<Vec<String>> = sqlx::query_scalar(
            "SELECT s.setconfig
             FROM pg_db_role_setting s
             JOIN pg_roles r ON r.oid = s.setrole
             WHERE r.rolname = $1 AND s.setdatabase = 0",
        )
        .bind(&role_name)
        .fetch_optional(&mut admin)
        .await?;
        let setconfig = setconfig.unwrap_or_default();
        assert!(
            setconfig.iter().any(|entry| entry == "lock_timeout=32s"),
            "role setting from the retried migration missing: {setconfig:?}"
        );
        Ok(())
    }
    .await;

    let database_cleanup = drop_db(&db_name).await;
    let role_cleanup = async {
        let mut cleanup = sqlx::PgConnection::connect(&admin_url()).await?;
        // SQL-POLICY: fixed-fragment — role_name is minted by this test and
        // contains only the closed [a-z0-9_] identifier alphabet.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP ROLE IF EXISTS {role_name}"
        )))
        .execute(&mut cleanup)
        .await?;
        let still_exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)")
                .bind(&role_name)
                .fetch_one(&mut cleanup)
                .await?;
        cleanup.close().await?;
        if still_exists {
            return Err(sqlx::Error::Protocol(
                "contention fixture role survived cleanup".into(),
            ));
        }
        Ok::<(), sqlx::Error>(())
    }
    .await;
    match (result, database_cleanup, role_cleanup) {
        (Err(error), _, _) => Err(error),
        (Ok(()), Err(error), _) | (Ok(()), Ok(()), Err(error)) => Err(error.into()),
        (Ok(()), Ok(()), Ok(())) => Ok(()),
    }
}

#[tokio::test]
async fn shared_catalog_role_ddl_insert_contention_retries_to_green() {
    assert_role_ddl_contention_retries_to_green(false)
        .await
        .expect("insert-shape contention retry test failed");
}

#[tokio::test]
async fn shared_catalog_role_ddl_update_contention_retries_to_green() {
    assert_role_ddl_contention_retries_to_green(true)
        .await
        .expect("update-shape contention retry test failed");
}
