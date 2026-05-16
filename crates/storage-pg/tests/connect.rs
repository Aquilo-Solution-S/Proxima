//! Connectivity smoke test. Requires a reachable PG —
//! `createdb proxima_dev` locally is the dev path.

use proxima_storage_pg::{DEFAULT_DATABASE_URL, PgStorage};

#[tokio::test]
async fn connect_to_default_dev_db() {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string());

    match PgStorage::connect(&url).await {
        Ok(_) => {
            // Pool acquired + SELECT 1 succeeded.
        }
        Err(e) => {
            panic!("PG required for tests but unavailable: {e}");
        }
    }
}
