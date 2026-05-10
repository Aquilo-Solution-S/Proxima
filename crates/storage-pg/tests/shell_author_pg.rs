//! PG coverage for the substrate-managed shell-author personality.

mod common;

use common::{drop_db, fresh_pg};
use proxima_core::storage::Storage;
use proxima_core::{OrgId, Owner, Principal, UserId};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn ensure_shell_author_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;

    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    };

    let first = pg.ensure_shell_author_personality(&owner).await?;
    let second = pg.ensure_shell_author_personality(&owner).await?;
    assert_eq!(first, second, "second call returns the same instance");

    drop_db(&db).await?;
    Ok(())
}
