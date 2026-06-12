use proxima::{
    AppInfo, EmbedConfig, FlavorApp, FlavorBundle, NamedMigrator, Proxima, ProximaBuilder,
    company_owner, run_core_and_flavor_migrations,
};
use proxima_core::FlavorRegistry;
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/proxima";

struct GoalTestApp;

impl FlavorBundle for GoalTestApp {
    fn register(registry: &mut FlavorRegistry) {
        proxima_flavor_goal::register(registry);
    }

    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new(
            "proxima-flavor-goal",
            proxima_flavor_goal::migrator(),
        )]
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
async fn boots_engine_with_goal_flavor_on_fresh_db() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let config =
            EmbedConfig::from_lookup(|key| (key == "DATABASE_URL").then(|| db_url.clone()))?;
        let owner = company_owner(Uuid::now_v7());

        let booted = ProximaBuilder::new(config, owner)
            .flavor_named(
                "proxima-flavor-goal",
                proxima_flavor_goal::register,
                Some(proxima_flavor_goal::migrator()),
            )
            .boot()
            .await?;

        assert!(booted.blobs.is_none(), "no S3 config -> no blob store");
        assert!(
            booted.engine.registry().flavor("proxima-goal").is_some(),
            "goal flavor registered"
        );
        booted.engine.stop(booted.handle).await;
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("embedded boot failed");
}

#[tokio::test]
async fn migration_facade_runs_goal_flavor_idempotently() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&db_url).await?;
        for _ in 0..2 {
            let report = run_core_and_flavor_migrations(
                &pg,
                [NamedMigrator::new(
                    "proxima-flavor-goal",
                    proxima_flavor_goal::migrator(),
                )],
            )
            .await?;
            assert!(report.sources.contains(&"proxima-core"));
            assert!(report.sources.contains(&"proxima-flavor-goal"));
        }

        let sidecar: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('proxima_goal.goal_activated_v1')::text")
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(sidecar.as_deref(), Some("proxima_goal.goal_activated_v1"));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("migration facade integration test failed");
}

#[tokio::test]
async fn facade_run_binds_loopback_mcp_and_sets_engine_url() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
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

fn db_url(name: &str) -> String {
    match ADMIN_URL.rfind('/') {
        Some(idx) => format!("{}/{name}", &ADMIN_URL[..idx]),
        None => format!("{ADMIN_URL}/{name}"),
    }
}

async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}
