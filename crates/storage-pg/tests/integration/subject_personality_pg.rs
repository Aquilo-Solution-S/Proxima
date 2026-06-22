//! PG coverage for per-subject identities.

use crate::common::{drop_db, fresh_pg};
use proxima_core::personality::ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID;
use proxima_core::storage::Storage;
use proxima_core::{GroupId, Principal, UserId};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn ensure_subject_personality_is_idempotent_and_mints_roots()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = Principal::User(UserId::new(Uuid::now_v7()));
    let subject = Principal::User(UserId::new(Uuid::now_v7()));

    let first = pg.ensure_subject_personality(&owner, &subject).await?;
    let second = pg.ensure_subject_personality(&owner, &subject).await?;
    assert_eq!(first, second);

    let other_subject = Principal::Group(GroupId::new(Uuid::now_v7()));
    let other = pg
        .ensure_subject_personality(&owner, &other_subject)
        .await?;
    assert_ne!(first.instance_id, other.instance_id);
    assert_ne!(
        first.self_perspective_memory_id,
        other.self_perspective_memory_id
    );

    let root_id: Uuid = sqlx::query_scalar(
        "SELECT current_root_perspective_memory_id
         FROM proxima_core.personality
         WHERE personality_instance_id = $1",
    )
    .bind(first.instance_id.into_inner())
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(root_id, first.self_perspective_memory_id.into_inner());

    // The root self-perspective is a plain Perspective memory stamped with
    // the canonical schema id — there is no sidecar table.
    let root_count: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM proxima_core.memories
         WHERE memory_id = $1
           AND kind = 'Perspective'
           AND schema_id = $2",
    )
    .bind(first.self_perspective_memory_id.into_inner())
    .bind(ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(root_count, 1);

    drop_db(&db).await?;
    Ok(())
}
