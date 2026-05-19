use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/proxima";

async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

#[tokio::test]
async fn intent_migrations_apply_to_fresh_db() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = format!("postgres://proxima:proxima@localhost/{db_name}");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_flavor_intent::migrator().run(pg.pool()).await?;

        let table_exists: Option<i32> = sqlx::query_scalar(
            "SELECT 1
               FROM information_schema.tables
              WHERE table_schema = 'proxima_intent'
                AND table_name = 'vision_brief_v1'",
        )
        .fetch_optional(pg.pool())
        .await?;
        assert_eq!(table_exists, Some(1));

        let typtype: Option<String> = sqlx::query_scalar(
            "SELECT t.typtype::text
               FROM pg_attribute a
               JOIN pg_class c ON c.oid = a.attrelid
               JOIN pg_namespace n ON n.oid = c.relnamespace
               JOIN pg_type t ON t.oid = a.atttypid
              WHERE n.nspname = 'proxima_intent'
                AND c.relname = 'vision_brief_v1'
                AND a.attname = 'ambition_level'
                AND NOT a.attisdropped",
        )
        .fetch_optional(pg.pool())
        .await?;
        assert_eq!(typtype.as_deref(), Some("e"));

        proxima_flavor_intent::migrator().run(pg.pool()).await?;
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("intent_migrations_apply_to_fresh_db failed");
}
