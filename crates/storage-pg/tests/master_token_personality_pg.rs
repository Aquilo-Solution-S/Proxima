//! PG coverage for the per-master-token shell-author identity.
mod common;

use common::{drop_db, fresh_pg};
use proxima_core::storage::Storage;
use proxima_core::{OrgId, Owner, Principal, UserId};
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn ensure_master_token_personality_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db)) = fresh_pg().await else {
        return Ok(());
    };
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
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    };

    let a = pg
        .ensure_master_token_personality(&owner, Uuid::now_v7())
        .await?;
    let b = pg
        .ensure_master_token_personality(&owner, Uuid::now_v7())
        .await?;
    assert_ne!(a.instance_id, b.instance_id);
    assert_ne!(a.self_perspective_memory_id, b.self_perspective_memory_id);

    drop_db(&db).await?;
    Ok(())
}

/// Concurrent first-connects for the same `(owner, token)` must
/// converge on a single canonical personality. Without the advisory
/// lock, the slow path's `SELECT → instantiate → INSERT ON CONFLICT
/// DO NOTHING` would let losing callers walk away with their own
/// orphan personality id while the mapping points to someone else's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_callers_resolve_to_single_personality() -> Result<(), Box<dyn std::error::Error>>
{
    const N: usize = 16;

    let Some((pg, db)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    let owner = Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    };
    let token = Uuid::now_v7();
    let pg = Arc::new(pg);

    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        let pg = pg.clone();
        let owner = owner.clone();
        handles.push(tokio::spawn(async move {
            pg.ensure_master_token_personality(&owner, token).await
        }));
    }

    let mut results = Vec::with_capacity(N);
    for h in handles {
        results.push(h.await??);
    }

    let canonical = &results[0];
    for (i, r) in results.iter().enumerate() {
        assert_eq!(
            r.instance_id, canonical.instance_id,
            "task #{i} returned a distinct instance_id"
        );
        assert_eq!(
            r.self_perspective_memory_id, canonical.self_perspective_memory_id,
            "task #{i} returned a distinct self_perspective_memory_id"
        );
    }

    // Exactly one shell-author personality minted for this owner.
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.personality
         WHERE owner_principal_kind = 'User'
           AND owner_principal_id = $1
           AND owner_org_id = $2",
    )
    .bind(match &owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    })
    .bind(owner.org_id.into_inner())
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(count, 1, "expected exactly one personality, got {count}");

    drop_db(&db).await?;
    Ok(())
}
