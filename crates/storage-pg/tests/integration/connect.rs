//! Connectivity smoke test. Requires a reachable PG — the dev-compose
//! setup at `proxima:proxima@localhost/proxima` is the default path.

use proxima_storage_pg::PgStorage;

const DEV_URL: &str = "postgres://proxima:proxima@localhost/proxima";

#[tokio::test]
async fn connect_to_default_dev_db() {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEV_URL.to_string());

    match PgStorage::connect(&url).await {
        Ok(_) => {
            // Pool acquired + SELECT 1 succeeded.
        }
        Err(e) => {
            panic!("PG required for tests but unavailable: {e}");
        }
    }
}
