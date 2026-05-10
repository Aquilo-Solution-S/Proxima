//! PG coverage for the per-master-token shell-author identity.
mod common;

use common::{drop_db, fresh_pg};
use proxima_core::storage::Storage;
use proxima_core::{OrgId, Owner, Principal, UserId};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn ensure_master_token_personality_is_idempotent()
    -> Result<(), Box<dyn std::error::Error>>
{
    let Some((pg, db)) = fresh_pg().await else { return Ok(()); };
    pg.run_migrations().await?;
    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    };
    let token = Uuid::now_v7();

    let first = pg.ensure_master_token_personality(&owner, token).await?;
    let second = pg.ensure_master_token_personality(&owner, token).await?;
    assert_eq!(first, second);

    drop_db(&db).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn distinct_tokens_resolve_to_distinct_personalities()
    -> Result<(), Box<dyn std::error::Error>>
{
    let Some((pg, db)) = fresh_pg().await else { return Ok(()); };
    pg.run_migrations().await?;
    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    };

    let a = pg.ensure_master_token_personality(&owner, Uuid::now_v7()).await?;
    let b = pg.ensure_master_token_personality(&owner, Uuid::now_v7()).await?;
    assert_ne!(a.instance_id, b.instance_id);
    assert_ne!(a.self_perspective_memory_id, b.self_perspective_memory_id);

    drop_db(&db).await?;
    Ok(())
}
