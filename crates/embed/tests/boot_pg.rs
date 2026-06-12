use proxima_embed::{EmbedConfig, ProximaBuilder, company_owner};
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/proxima";

#[tokio::test]
async fn boots_engine_with_goal_flavor_on_fresh_db() {
    let db_name = format!("proxima_embed_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let config =
            EmbedConfig::from_lookup(|key| (key == "DATABASE_URL").then(|| db_url.clone()))?;
        let owner = company_owner(Uuid::now_v7());

        let booted = ProximaBuilder::new(config, owner)
            .flavor(
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
