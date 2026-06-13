// Each integration-test binary independently includes this module via
// `mod common;`. Items unused by a particular binary would otherwise trip
// `dead_code` even though another binary uses them.
#![allow(dead_code)]

pub mod personality;

use proxima_core::{OrgId, Owner, Principal, UserId};
#[allow(unused_imports)]
pub use proxima_pg_testkit::{
    create_db, create_db_from_template, db_url, drop_db, ensure_template, unique_db_name,
};
use proxima_storage_pg::{PgStorage, core_migrator};
use uuid::Uuid;

pub fn owner_fixture() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::nil())),
        org_id: OrgId::new(Uuid::nil()),
    }
}

pub async fn fresh_pg() -> Option<(PgStorage, String)> {
    let template = core_template_name();
    if let Err(e) = ensure_template(&template, |pool| async move {
        core_migrator().run(&pool).await.map_err(sqlx::Error::from)
    })
    .await
    {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let db_name = match create_db_from_template("proxima_test", &template).await {
        Ok(name) => name,
        Err(e) => {
            panic!("PG required for tests but admin connect failed: {e}");
        }
    };
    let url = db_url(&db_name);
    match PgStorage::connect(&url).await {
        Ok(pg) => Some((pg, db_name)),
        Err(err) => {
            let _ = drop_db(&db_name).await;
            panic!("PG required for tests but unavailable: {err}");
        }
    }
}

fn core_template_name() -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for migration in core_migrator().iter() {
        hash = hash_bytes(hash, &migration.version.to_be_bytes());
        hash = hash_bytes(hash, migration.checksum.as_ref());
    }
    format!("proxima_tmpl_core_{hash:016x}")
}

fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
