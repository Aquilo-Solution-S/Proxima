use std::sync::Arc;

use proxima::{
    AppInfo, EmbedConfig, FlavorApp, FlavorBundle, NamedMigrator, PayloadKind, Proxima,
    ProximaBuilder, company_owner, run_core_and_flavor_migrations,
};
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::verbs::event_ingest::EventDraft;
use proxima_core::{
    FactPayload, FlavorRegistry, GoalActivatedV1, MemoryId, SchemaId, SchemaVersion, SourceBatchId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::{PgSidecarKey, PgStorage};
use tokio::time::{Duration, Instant};
use uuid::Uuid;

struct GoalTestApp;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct TestFact {
    label: String,
}

impl FactPayload for TestFact {
    const SCHEMA_ID: &'static str = "test/facade-boot-fact-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn event_key(&self) -> Vec<u8> {
        self.label.as_bytes().to_vec()
    }

    fn render(&self) -> String {
        self.label.clone()
    }
}

impl FlavorBundle for GoalTestApp {
    fn register(registry: &mut FlavorRegistry) {
        registry.add_fact_schema::<TestFact>();
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
          WHERE entity_kind = 'Fact'
            AND entity_id = $1
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
          WHERE entity_kind = 'Fact'
            AND entity_id = $1
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
            sqlx::query_scalar("SELECT to_regclass('proxima_core.goal_activated_v1')::text")
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(sidecar.as_deref(), Some("proxima_core.goal_activated_v1"));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("migration facade integration test failed");
}

#[tokio::test]
async fn facade_run_binds_loopback_mcp_and_sets_engine_url() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let running = Proxima::<GoalTestApp>::app()
            .database_url(db_url)
            .owner(company_owner(Uuid::now_v7()))
            .allow_insecure_single_owner()
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
            .owner(owner.clone())
            .allow_insecure_single_owner()
            .embed_client(Arc::new(ConstantEmbedding::prefixed(
                model_id,
                &[0.25, 0.5, 0.75],
            )))
            .build()
            .await?;
        let goal_fact_key = PgSidecarKey::new(
            PayloadKind::Fact,
            SchemaId::new(GoalActivatedV1::SCHEMA_ID.into()),
            SchemaVersion::new(GoalActivatedV1::SCHEMA_VERSION),
        );
        assert!(
            built.pg_sidecars.contains(&goal_fact_key),
            "boot result exposes the frozen core PG sidecar registry"
        );

        let payload = TestFact {
            label: "facade worker drain fact".to_string(),
        };
        let draft = EventDraft::from_payload(
            &owner,
            "test/facade-worker",
            SourceBatchId::new(Uuid::now_v7()),
            &payload,
            time::OffsetDateTime::now_utc(),
        );
        let authz = built.single_owner_authz().expect("single owner authz");
        let outcome = built.engine.event_ingest(&authz, draft).await?;
        assert_eq!(
            count_fact_embeddings(&built.pool, outcome.memory_id, model_id).await?,
            0
        );
        assert_eq!(
            count_embedding_jobs(&built.pool, outcome.memory_id, model_id).await?,
            1
        );

        let cancel = tokio_util::sync::CancellationToken::new();
        let worker = built.spawn_embedding_worker(cancel.clone());
        wait_for_embedding_drain(&built.pool, outcome.memory_id, model_id).await?;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), worker).await??;

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("facade boot sidecar/worker integration test failed");
}
