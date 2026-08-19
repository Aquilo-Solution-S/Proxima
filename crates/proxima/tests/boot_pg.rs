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
use proxima_pg_testkit::{admin_url, create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::{PgPoolConfig, PgSidecarKey, PgStorage};
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::{Connection, SqlSafeStr};
use tokio::time::{Duration, Instant};
use uuid::Uuid;

struct GoalTestApp;

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

fn probe_migrator() -> Migrator {
    Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            20_991_231_000_000,
            Cow::Borrowed("skip-migrations DDL probe"),
            MigrationType::Simple,
            "CREATE TABLE IF NOT EXISTS public.skip_probe (id integer)".into_sql_str(),
            false,
        )]),
        ..Migrator::DEFAULT
    }
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

fn current_user_schema_migrator() -> Migrator {
    Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            20_990_101_000_000,
            Cow::Borrowed("current user schema collision"),
            MigrationType::Simple,
            "CREATE SCHEMA AUTHORIZATION CURRENT_USER;".into_sql_str(),
            false,
        )]),
        ..Migrator::DEFAULT
    }
}

async fn force_role_first_search_path(db_name: &str) -> Result<(), sqlx::Error> {
    let mut conn = sqlx::PgConnection::connect(&admin_url()).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "ALTER ROLE CURRENT_USER IN DATABASE {} SET search_path = \"$user\", public",
        quoted_ident(db_name)
    )))
    .execute(&mut conn)
    .await?;
    conn.close().await
}

#[tokio::test]
async fn boots_engine_with_core_goal_tools_on_fresh_db() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let config = EmbedConfig {
            database_url: db_url,
            s3: None,
        };
        let owner = company_owner(Uuid::now_v7());

        let booted = ProximaBuilder::new(config, owner).boot().await?;

        assert!(booted.blobs.is_none(), "no S3 config -> no blob store");
        assert!(
            booted
                .engine
                .registry()
                .mcp_tool_ids()
                .contains("core_goal"),
            "core goal tool registered"
        );
        booted.engine.stop(booted.handle);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("embedded boot failed");
}

#[tokio::test]
async fn programmatic_pool_config_reaches_runtime_pool_construction() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool_config = PgPoolConfig {
            max_connections: 3,
            statement_timeout: Duration::from_secs(41),
            acquire_timeout: Duration::from_secs(2),
            idle_timeout: Duration::from_secs(19),
            max_lifetime: Duration::from_secs(23),
        };
        let built = Proxima::<GoalTestApp>::app()
            .database_url(db_url)
            .owner(company_owner(Uuid::now_v7()))
            .tool_scope(ToolScope::All)
            .allow_insecure_single_owner()
            .pg_pool_config(pool_config)
            .build()
            .await?;

        let options = built.pool_for_tests().options();
        assert_eq!(options.get_max_connections(), 3);
        assert_eq!(options.get_acquire_timeout(), Duration::from_secs(2));
        assert_eq!(options.get_idle_timeout(), Some(Duration::from_secs(19)));
        assert_eq!(options.get_max_lifetime(), Some(Duration::from_secs(23)));
        let statement_timeout: String = sqlx::query_scalar("SHOW statement_timeout")
            .fetch_one(built.pool_for_tests())
            .await?;
        assert_eq!(statement_timeout, "41s");

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("programmatic pool config boot failed");
}

#[tokio::test]
async fn migration_facade_runs_core_goal_schema_idempotently() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&db_url).await?;
        for _ in 0..2 {
            let report = run_core_and_flavor_migrations(&pg, Vec::<NamedMigrator>::new()).await?;
            assert!(report.sources.contains(&"proxima-core"));
        }

        let sidecar: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('proxima_core.task_goal_v1')::text")
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(sidecar.as_deref(), Some("proxima_core.task_goal_v1"));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("migration facade integration test failed");
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
async fn migration_facade_keeps_tracking_public_when_flavor_creates_current_user_schema() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    force_role_first_search_path(&db_name)
        .await
        .expect("test role search_path should be configurable");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&db_url).await?;
        let report = run_core_and_flavor_migrations(
            &pg,
            [NamedMigrator::new(
                "role-schema-flavor",
                current_user_schema_migrator(),
            )],
        )
        .await?;
        assert_eq!(report.sources, ["proxima-core", "role-schema-flavor"]);
        pg.pool_for_tests().close().await;
        drop(pg);

        let pg = PgStorage::connect(&db_url).await?;
        let report = run_core_and_flavor_migrations(
            &pg,
            [NamedMigrator::new(
                "role-schema-flavor",
                current_user_schema_migrator(),
            )],
        )
        .await?;
        assert_eq!(report.sources, ["proxima-core", "role-schema-flavor"]);

        let tracking_schemas: Vec<String> = sqlx::query_scalar(
            "SELECT table_schema::text
               FROM information_schema.tables
              WHERE table_name = '_sqlx_migrations'
              ORDER BY table_schema",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert_eq!(tracking_schemas, ["public"]);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("migration facade should pin sqlx tracking to public");
}

#[tokio::test]
async fn facade_run_with_custom_auth_needs_no_separate_owner_access() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let subject = UserId::new(Uuid::now_v7());
        let running = Proxima::<GoalTestApp>::app()
            .database_url(db_url.clone())
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
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let model_id = "facade-drain-embed";
        let built = Proxima::<GoalTestApp>::app()
            .database_url(db_url)
            .owner(owner)
            .allow_insecure_single_owner()
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
        let authz = built.single_owner_authz().expect("single owner authz");
        let outcome = built
            .engine
            .ingest_typed_fact(&authz, "test/facade-worker", &payload)
            .await?;
        assert_eq!(
            count_fact_embeddings(built.pool_for_tests(), outcome.memory_id, model_id).await?,
            0
        );
        assert_eq!(
            count_embedding_jobs(built.pool_for_tests(), outcome.memory_id, model_id).await?,
            1
        );

        let cancel = tokio_util::sync::CancellationToken::new();
        let worker = built.spawn_embedding_worker(cancel.clone());
        wait_for_embedding_drain(built.pool_for_tests(), outcome.memory_id, model_id).await?;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), worker).await??;

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("facade boot sidecar/worker integration test failed");
}

#[tokio::test]
async fn startup_reconcile_heals_facts_ingested_without_embed_client() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let model_id = "startup-reconcile-embed";

        // First boot has NO embedding client: fact ingest writes the memory
        // but enqueues no job — the gap the startup reconcile exists to heal.
        let degraded = Proxima::<GoalTestApp>::app()
            .database_url(db_url.clone())
            .owner(owner)
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let payload = drain_note("fact written while embeddings were down");
        let authz = degraded.single_owner_authz().expect("single owner authz");
        let outcome = degraded
            .engine
            .ingest_typed_fact(&authz, "test/startup-reconcile", &payload)
            .await?;
        assert_eq!(
            count_embedding_jobs(degraded.pool_for_tests(), outcome.memory_id, model_id).await?,
            0,
            "no embed client -> ingest must not enqueue a job"
        );
        degraded.shutdown();

        // Second boot WITH a client: the worker's boot-time reconcile must
        // enqueue the missing job and the drain loop must embed it, without
        // any operator command.
        let healed = Proxima::<GoalTestApp>::app()
            .database_url(db_url)
            .owner(owner)
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
            .embed_client(Arc::new(ConstantEmbedding::prefixed(
                model_id,
                &[0.5, 0.25, 0.125],
            )))
            .build()
            .await?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let worker = healed.spawn_embedding_worker(cancel.clone());
        wait_for_embedding_drain(healed.pool_for_tests(), outcome.memory_id, model_id).await?;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), worker).await??;
        healed.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("startup reconcile self-heal test failed");
}

#[tokio::test]
async fn skip_migrations_boots_without_applying_ddl() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        // Migrate the core schema out-of-band, standing in for the DDL-role
        // init step of a split-role GitOps deploy.
        let pg = PgStorage::connect(&db_url).await?;
        run_core_and_flavor_migrations(&pg, Vec::<NamedMigrator>::new()).await?;
        pg.pool_for_tests().close().await;
        drop(pg);

        let owner = company_owner(Uuid::now_v7());
        let config = || EmbedConfig {
            database_url: db_url.clone(),
            s3: None,
        };

        // Boot with skip_migrations plus a flavor migrator that WOULD create a
        // table. skip bypasses run_sources, so the probe table stays absent
        // while boot still succeeds against the already-migrated schema.
        let skipped = ProximaBuilder::new(config(), owner)
            .flavor_named("skip-probe", |_registry| Ok(()), Some(probe_migrator()))
            .skip_migrations()
            .boot()
            .await?;
        let probe_after_skip: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('public.skip_probe')::text")
                .fetch_one(skipped.pool_for_tests())
                .await?;
        assert_eq!(
            probe_after_skip, None,
            "skip_migrations() must not issue flavor DDL"
        );
        skipped.engine.stop(skipped.handle);

        // Control: the SAME migrator on the SAME database WITHOUT skip creates
        // the table — proving the absence above is skip's doing, not a broken
        // migrator.
        let migrated = ProximaBuilder::new(config(), owner)
            .flavor_named("skip-probe", |_registry| Ok(()), Some(probe_migrator()))
            .boot()
            .await?;
        let probe_after_migrate: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('public.skip_probe')::text")
                .fetch_one(migrated.pool_for_tests())
                .await?;
        assert_eq!(
            probe_after_migrate.as_deref(),
            Some("skip_probe"),
            "a non-skipped boot must run flavor DDL"
        );
        migrated.engine.stop(migrated.handle);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("skip_migrations DDL-bypass test failed");
}

#[tokio::test]
async fn boot_rejects_embedding_client_with_wrong_dim() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let config = || EmbedConfig {
            database_url: db_url.clone(),
            s3: None,
        };

        // Wrong dim (3072 vs the fixed vector(1024)) must fail fast at boot
        // with Config, before any job is claimed against the column.
        let err = ProximaBuilder::new(config(), owner)
            .embed_client(Arc::new(FixedDimEmbedding::new("wrong-dim", 3072)))
            .boot()
            .await
            .expect_err("wrong embedding dim must be rejected at boot");
        match err {
            EmbedError::Config(msg) => {
                assert!(
                    msg.contains("3072"),
                    "message names the offending dim: {msg}"
                );
                assert!(
                    msg.contains("dim"),
                    "message explains a dim mismatch: {msg}"
                );
            }
            other => panic!("expected EmbedError::Config, got {other:?}"),
        }

        // Right dim (ConstantEmbedding is always EMBEDDING_DIM-wide) boots.
        let booted = ProximaBuilder::new(config(), owner)
            .embed_client(Arc::new(ConstantEmbedding::zero("right-dim")))
            .boot()
            .await?;
        assert!(
            booted.engine.embed_client().is_some(),
            "matching-dim client is wired into the engine"
        );
        booted.engine.stop(booted.handle);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("embedding dim guard test failed");
}
