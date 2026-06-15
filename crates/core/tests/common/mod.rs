// Each integration-test binary in `proxima-core` independently includes
// this module via `mod common;`. Items unused by a particular binary
// would otherwise trip `dead_code` even though another binary uses them.
#![allow(dead_code)]

#[path = "../../src/test_fixtures.rs"]
mod core_test_fixtures;

#[allow(unused_imports)]
pub use core_test_fixtures::{ConstantEmbedding, owner_fixture};
#[allow(unused_imports)]
pub use proxima_pg_testkit::{
    create_db, create_db_from_template, db_url, drop_db, ensure_template, unique_db_name,
};
use proxima_storage_pg::PgStorage;

#[allow(unused_imports)]
pub use proxima_storage_pg::test_fixtures::core_template_name;

pub async fn fresh_pg() -> (PgStorage, String) {
    proxima_storage_pg::test_fixtures::fresh_pg("proxima_core_test").await
}

/// Drop the test database when the fixture goes out of scope.
pub struct PgFixture {
    pub pg: PgStorage,
    pub db: String,
}

impl PgFixture {
    pub async fn cleanup(self) {
        drop(self.pg);
        let _ = drop_db(&self.db).await;
    }
}
