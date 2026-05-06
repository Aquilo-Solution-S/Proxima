// Each integration-test binary independently includes this module via
// `mod common;`. Items unused by a particular binary would otherwise trip
// `dead_code` even though another binary uses them.
#![allow(dead_code)]

use proxima_core::{OrgId, Owner, Principal, UserId};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

// Override via `PROXIMA_TEST_PG_URL` (e.g. the `docker-compose.dev.yml` PG:
// `postgres://proxima:proxima@localhost/proxima`). The default targets a
// peer-auth local PG with a `postgres` superuser.
const DEFAULT_ADMIN_URL: &str = "postgres://postgres@localhost/postgres";

pub fn admin_url() -> String {
    std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| DEFAULT_ADMIN_URL.into())
}

pub fn db_url(name: &str) -> String {
    let admin = admin_url();
    // Replace the path component (database name) while preserving creds/host.
    match admin.rfind('/') {
        Some(idx) => format!("{}/{}", &admin[..idx], name),
        None => format!("{admin}/{name}"),
    }
}

pub fn owner_fixture() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::nil())),
        org_id: OrgId::new(Uuid::nil()),
    }
}

pub async fn fresh_pg() -> Option<(PgStorage, String)> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return None;
    }
    let url = db_url(&db_name);
    match PgStorage::connect(&url).await {
        Ok(pg) => Some((pg, db_name)),
        Err(err) => {
            let _ = drop_db(&db_name).await;
            eprintln!("skipping (PG unavailable): {err}");
            None
        }
    }
}

pub async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(&admin_url()).await?;
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

pub async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(&admin_url()).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}
