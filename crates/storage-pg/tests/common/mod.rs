// Each integration-test binary independently includes this module via
// `mod common;`. Items unused by a particular binary would otherwise trip
// `dead_code` even though another binary uses them.
#![allow(dead_code)]

pub mod personality;

use proxima_core::{OrgId, Owner, Principal, UserId};
pub use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

pub fn owner_fixture() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::nil())),
        org_id: OrgId::new(Uuid::nil()),
    }
}

pub async fn fresh_pg() -> Option<(PgStorage, String)> {
    let db_name = unique_db_name("proxima_test");
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    match PgStorage::connect(&url).await {
        Ok(pg) => Some((pg, db_name)),
        Err(err) => {
            let _ = drop_db(&db_name).await;
            panic!("PG required for tests but unavailable: {err}");
        }
    }
}
