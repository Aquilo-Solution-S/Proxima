//! Direct `OwnerRef` column helpers.

use proxima_core::{
    EntityId, GroupId, MembershipRow, OwnerRef, OwnerRefKind, Relation, StorageError, UserId,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::{internal, map_err, with_bounded_retry};

#[must_use]
pub fn owner_binds(owner: &OwnerRef) -> (OwnerRefKind, Option<uuid::Uuid>) {
    match *owner {
        OwnerRef::World => (OwnerRefKind::World, None),
        OwnerRef::Personal(user) => (OwnerRefKind::Personal, Some(user.into_inner())),
        OwnerRef::Group(group) => (OwnerRefKind::Group, Some(group.into_inner())),
    }
}

#[must_use]
pub fn owner_arrays(owners: &[OwnerRef]) -> (Vec<OwnerRefKind>, Vec<Option<uuid::Uuid>>) {
    owners.iter().map(owner_binds).unzip()
}

/// Fail closed when a NEW `memories`/`goals` entity row would be created
/// under [`OwnerRef::World`].
///
/// Kernel law: World is universally readable and never a write owner. The
/// v0.0.5 baseline correction (`0008_v005.sql`) dropped the blanket
/// `world_not_write_owner_chk` from `memories`/`goals` so the
/// publish-to-World owner TRANSFER (`transfer_to_world`, an UPDATE) can
/// persist World ownership — which also removed the DB-level backstop
/// against raw storage-verb callers (e.g. `flavors/code`, which invokes
/// these verbs directly and bypasses `Engine::authorize_write`'s World
/// short-circuit). This helper restores that backstop one layer up: every
/// row-creating verb choke point calls it before its INSERT.
///
/// Deliberately NOT wired into [`owner_binds`]: rows that legitimately
/// reference a World-owned entity post-publish, and the transfer UPDATE
/// itself, must keep encoding World.
///
/// # Errors
///
/// Returns [`StorageError::ConstraintViolation`] — the same error class
/// the dropped DDL CHECK produced — when `owner` is [`OwnerRef::World`].
pub(crate) fn reject_world_write_owner(owner: &OwnerRef) -> Result<(), StorageError> {
    if matches!(owner, OwnerRef::World) {
        return Err(StorageError::ConstraintViolation(
            "World is read-only and never a write owner; new entity rows cannot be created \
             under OwnerRef::World (the publish-to-World owner transfer is the only path \
             that sets it)"
                .into(),
        ));
    }
    Ok(())
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn resolve_membership(
    pool: &PgPool,
    owner: &OwnerRef,
) -> Result<Vec<MembershipRow>, StorageError> {
    let OwnerRef::Personal(user) = owner else {
        return Ok(Vec::new());
    };

    let rows: Vec<(uuid::Uuid, Relation)> = sqlx::query_as(
        "SELECT group_id, relation
           FROM proxima_core.group_memberships
          WHERE member_user_id = $1
          ORDER BY group_id, relation",
    )
    .bind(user.into_inner())
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|(group, relation)| MembershipRow {
            group: GroupId::new(group),
            relation,
        })
        .collect())
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn lock_group_membership_tx(
    tx: &mut Transaction<'_, Postgres>,
    group_id: GroupId,
) -> Result<(), StorageError> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('proxima-group-membership:' || $1::text, 0))",
    )
    .bind(group_id.into_inner())
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// # Errors
///
/// Returns `Conflict` when any Admin row already exists for the group, and
/// `Internal` on sqlx failure.
pub(crate) async fn bootstrap_group_admin(
    pool: &PgPool,
    group_id: GroupId,
    first_admin_user_id: UserId,
    _granted_by: uuid::Uuid,
) -> Result<(), StorageError> {
    // Retry the whole transaction on transient deadlock/serialization,
    // like the other atomic writers.
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        lock_group_membership_tx(&mut tx, group_id).await?;
        let inserted = sqlx::query(
            "INSERT INTO proxima_core.group_memberships
                (group_id, member_user_id, relation)
             SELECT $1, $2, $3
              WHERE NOT EXISTS (
                  SELECT 1
                    FROM proxima_core.group_memberships
                   WHERE group_id = $1
                     AND relation = $3
              )",
        )
        .bind(group_id.into_inner())
        .bind(first_admin_user_id.into_inner())
        .bind(Relation::Admin)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        if inserted.rows_affected() == 0 {
            tx.rollback().await.map_err(map_err)?;
            return Err(StorageError::Conflict(
                "group already has an Admin membership".into(),
            ));
        }

        tx.commit().await.map_err(map_err)?;
        Ok(())
    })
    .await
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn add_group_member(
    pool: &PgPool,
    group_id: GroupId,
    member_user_id: UserId,
    relation: Relation,
    _granted_by: uuid::Uuid,
) -> Result<(), StorageError> {
    // Retry the whole transaction on a transient deadlock/serialization
    // failure, matching every other atomic writer.
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        lock_group_membership_tx(&mut tx, group_id).await?;
        sqlx::query(
            "INSERT INTO proxima_core.group_memberships
                (group_id, member_user_id, relation)
             VALUES ($1, $2, $3)
             ON CONFLICT (group_id, member_user_id, relation) DO NOTHING",
        )
        .bind(group_id.into_inner())
        .bind(member_user_id.into_inner())
        .bind(relation)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
        tx.commit().await.map_err(map_err)?;
        Ok(())
    })
    .await
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn remove_group_member(
    pool: &PgPool,
    group_id: GroupId,
    member_user_id: UserId,
) -> Result<(), StorageError> {
    // Retry the whole transaction on a transient deadlock/serialization
    // failure, matching every other atomic writer.
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        lock_group_membership_tx(&mut tx, group_id).await?;
        sqlx::query(
            "DELETE FROM proxima_core.group_memberships
              WHERE group_id = $1
                AND member_user_id = $2",
        )
        .bind(group_id.into_inner())
        .bind(member_user_id.into_inner())
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
        tx.commit().await.map_err(map_err)?;
        Ok(())
    })
    .await
}

/// True iff `member_user_id` currently holds exactly `relation` on `group_id`.
/// A point-in-time single-role probe, distinct from
/// [`resolve_membership`]'s full row enumeration.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn has_group_relation(
    pool: &PgPool,
    group_id: GroupId,
    member_user_id: UserId,
    relation: Relation,
) -> Result<bool, StorageError> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.group_memberships
              WHERE group_id = $1
                AND member_user_id = $2
                AND relation = $3
         )",
    )
    .bind(group_id.into_inner())
    .bind(member_user_id.into_inner())
    .bind(relation)
    .fetch_one(pool)
    .await
    .map_err(map_err)
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn list_group_members(
    pool: &PgPool,
    group_id: GroupId,
) -> Result<Vec<(UserId, Relation)>, StorageError> {
    let rows: Vec<(uuid::Uuid, Relation)> = sqlx::query_as(
        "SELECT member_user_id, relation
           FROM proxima_core.group_memberships
          WHERE group_id = $1
          ORDER BY created_at, member_user_id, relation",
    )
    .bind(group_id.into_inner())
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|(member, relation)| (UserId::new(member), relation))
        .collect())
}

/// One page of group members in the keyset total order
/// `(member_user_id, relation)`, starting strictly after `after` when
/// given. Fetches at most `limit` rows; callers over-fetch by one to
/// detect further pages. Relation ties order by the
/// `proxima_core.membership_relation` enum definition.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn list_group_members_page(
    pool: &PgPool,
    group_id: GroupId,
    after: Option<(UserId, Relation)>,
    limit: i64,
) -> Result<Vec<(UserId, Relation)>, StorageError> {
    let after_member = after.map(|(member, _)| member.into_inner());
    let after_relation = after.map(|(_, relation)| relation);
    let rows: Vec<(uuid::Uuid, Relation)> = sqlx::query_as(
        "SELECT member_user_id, relation
           FROM proxima_core.group_memberships
          WHERE group_id = $1
            AND ($2::uuid IS NULL
                 OR (member_user_id, relation)
                    > ($2::uuid, $3::proxima_core.membership_relation))
          ORDER BY member_user_id, relation
          LIMIT $4",
    )
    .bind(group_id.into_inner())
    .bind(after_member)
    .bind(after_relation)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|(member, relation)| (UserId::new(member), relation))
        .collect())
}

/// Home owner when the row is in `read_owners`. Absent and foreign are
/// both `None`.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn visible_home_owner(
    pool: &PgPool,
    entity: EntityId,
    read_owners: &[OwnerRef],
) -> Result<Option<OwnerRef>, StorageError> {
    if read_owners.is_empty() {
        return Ok(None);
    }

    let owner_ids: Vec<uuid::Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let row: Option<(OwnerRefKind, uuid::Uuid)> = sqlx::query_as(
        "SELECT o.kind::text::proxima_core.owner_kind, m.owner_id
           FROM proxima_core.memory m
           JOIN proxima_core.owners o ON o.owner_id = m.owner_id
          WHERE m.t = $1 AND m.owner_id = ANY($2::uuid[])
         UNION ALL
         SELECT o.kind::text::proxima_core.owner_kind, g.owner_id
           FROM proxima_core.goal g
           JOIN proxima_core.owners o ON o.owner_id = g.owner_id
          WHERE g.t = $1 AND g.owner_id = ANY($2::uuid[])
         LIMIT 1",
    )
    .bind(entity.uuid())
    .bind(&owner_ids)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    Ok(row.map(|(kind, id)| match kind {
        OwnerRefKind::World => proxima_core::OwnerRef::World,
        OwnerRefKind::Personal => proxima_core::OwnerRef::Personal(proxima_core::UserId::new(id)),
        OwnerRefKind::Group => proxima_core::OwnerRef::Group(proxima_core::GroupId::new(id)),
    }))
}

/// Transfer one memory or goal **series** to [`OwnerRef::World`].
///
/// Same `(handle, t)`: publish is an owner UPDATE, not a copy. Head and
/// every version on the handle move together (`MemoryHeadAligned`).
/// Returns `true` iff a row under `from_owner` matched and was updated.
///
/// Sidecar rows stay keyed by `t`. Cited `blob` rows move when no other
/// live non-World series still cites them. Embeddings / jobs follow the
/// transferred `t`s so ANN (`emb.owner_id`) stays Tesla-valve. `ingest_keys`
/// for those `t`s are deleted so the prior owner can mint a new series.
///
/// # Errors
///
/// `Conflict` when a cited blob is still referenced by another live
/// series, or a terminal Goal's `close_fact_t` is owned by someone else.
/// Unique / check violations surface as `ConstraintViolation`.
pub(crate) async fn transfer_to_world(
    pool: &PgPool,
    entity: EntityId,
    from_owner: OwnerRef,
) -> Result<bool, StorageError> {
    let from_id = from_owner.stored_owner_id();
    let world = OwnerRef::World.stored_owner_id();
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        let transferred = match entity {
            EntityId::Memory(memory_id) => {
                transfer_memory_t(&mut tx, memory_id.into_inner(), from_id, world).await?
            }
            EntityId::Goal(goal_id) => {
                transfer_goal_t(&mut tx, goal_id.into_inner(), from_id, world).await?
            }
        };
        tx.commit().await.map_err(map_err)?;
        Ok(transferred)
    })
    .await
}

async fn transfer_memory_t(
    tx: &mut Transaction<'_, Postgres>,
    t: uuid::Uuid,
    from_id: uuid::Uuid,
    world: uuid::Uuid,
) -> Result<bool, StorageError> {
    let handle: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT handle FROM proxima_core.memory
          WHERE t = $1 AND owner_id = $2",
    )
    .bind(t)
    .bind(from_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    let Some(handle) = handle else {
        return Ok(false);
    };
    transfer_memory_handle(tx, handle, from_id, world).await
}

async fn transfer_memory_handle(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    world: uuid::Uuid,
) -> Result<bool, StorageError> {
    let ts: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT t FROM proxima_core.memory WHERE handle = $1 AND owner_id = $2")
            .bind(handle)
            .bind(from_id)
            .fetch_all(&mut **tx)
            .await
            .map_err(map_err)?;
    if ts.is_empty() {
        return Ok(false);
    }
    transfer_exclusive_blobs(tx, handle, from_id, world).await?;
    let head = sqlx::query(
        "UPDATE proxima_core.memory_head
            SET owner_id = $3
          WHERE handle = $1 AND owner_id = $2",
    )
    .bind(handle)
    .bind(from_id)
    .bind(world)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    if head.rows_affected() == 0 {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE proxima_core.memory
            SET owner_id = $3
          WHERE handle = $1 AND owner_id = $2",
    )
    .bind(handle)
    .bind(from_id)
    .bind(world)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    follow_embedding_owners(tx, &ts, world).await?;
    sqlx::query("DELETE FROM proxima_core.ingest_keys WHERE t = ANY($1::uuid[])")
        .bind(&ts)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(true)
}

async fn transfer_exclusive_blobs(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    world: uuid::Uuid,
) -> Result<(), StorageError> {
    let blob_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT blob_id
           FROM proxima_core.memory
          WHERE handle = $1 AND owner_id = $2 AND blob_id IS NOT NULL",
    )
    .bind(handle)
    .bind(from_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    for blob_id in blob_ids {
        let shared: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1
                   FROM proxima_core.memory
                  WHERE blob_id = $1
                    AND handle <> $2
                    AND owner_id <> $3
             )",
        )
        .bind(blob_id)
        .bind(handle)
        .bind(world)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_err)?;
        if shared {
            return Err(StorageError::Conflict(
                "cited blob is still referenced by another live non-World series".into(),
            ));
        }
        sqlx::query("UPDATE proxima_core.blob SET owner_id = $2 WHERE blob_id = $1")
            .bind(blob_id)
            .bind(world)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

async fn follow_embedding_owners(
    tx: &mut Transaction<'_, Postgres>,
    ts: &[uuid::Uuid],
    world: uuid::Uuid,
) -> Result<(), StorageError> {
    sqlx::query(
        "UPDATE proxima_core.embeddings SET owner_id = $2 WHERE entity_id = ANY($1::uuid[])",
    )
    .bind(ts)
    .bind(world)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "UPDATE proxima_core.embedding_heads SET owner_id = $2 WHERE entity_id = ANY($1::uuid[])",
    )
    .bind(ts)
    .bind(world)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "UPDATE proxima_core.embedding_jobs SET owner_id = $2 WHERE entity_id = ANY($1::uuid[])",
    )
    .bind(ts)
    .bind(world)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn transfer_goal_t(
    tx: &mut Transaction<'_, Postgres>,
    t: uuid::Uuid,
    from_id: uuid::Uuid,
    world: uuid::Uuid,
) -> Result<bool, StorageError> {
    let handle: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT handle FROM proxima_core.goal
          WHERE t = $1 AND owner_id = $2",
    )
    .bind(t)
    .bind(from_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    let Some(handle) = handle else {
        return Ok(false);
    };
    let close_facts: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT close_fact_t
           FROM proxima_core.goal
          WHERE handle = $1 AND owner_id = $2 AND close_fact_t IS NOT NULL",
    )
    .bind(handle)
    .bind(from_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    let head = sqlx::query(
        "UPDATE proxima_core.goal_head
            SET owner_id = $3
          WHERE handle = $1 AND owner_id = $2",
    )
    .bind(handle)
    .bind(from_id)
    .bind(world)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    if head.rows_affected() == 0 {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE proxima_core.goal
            SET owner_id = $3
          WHERE handle = $1 AND owner_id = $2",
    )
    .bind(handle)
    .bind(from_id)
    .bind(world)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    let goal_ts: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT t FROM proxima_core.goal WHERE handle = $1 AND owner_id = $2")
            .bind(handle)
            .bind(world)
            .fetch_all(&mut **tx)
            .await
            .map_err(map_err)?;
    follow_embedding_owners(tx, &goal_ts, world).await?;
    for close_t in close_facts {
        let close_owner: Option<uuid::Uuid> =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(close_t)
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_err)?;
        match close_owner {
            None => {}
            Some(id) if id == world => {}
            Some(id) if id == from_id => {
                if !transfer_memory_t(tx, close_t, from_id, world).await? {
                    return Err(StorageError::Conflict(
                        "terminal goal close_fact_t could not be transferred with the goal".into(),
                    ));
                }
            }
            Some(_) => {
                return Err(StorageError::Conflict(
                    "terminal goal close_fact_t is owned by another live owner".into(),
                ));
            }
        }
    }
    Ok(true)
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn home_owner(
    pool: &PgPool,
    entity: EntityId,
) -> Result<Option<OwnerRef>, StorageError> {
    let row: Option<(OwnerRefKind, uuid::Uuid)> = sqlx::query_as(
        "SELECT o.kind::text::proxima_core.owner_kind, m.owner_id
           FROM proxima_core.memory m
           JOIN proxima_core.owners o ON o.owner_id = m.owner_id
          WHERE m.t = $1
         UNION ALL
         SELECT o.kind::text::proxima_core.owner_kind, g.owner_id
           FROM proxima_core.goal g
           JOIN proxima_core.owners o ON o.owner_id = g.owner_id
          WHERE g.t = $1
         LIMIT 1",
    )
    .bind(entity.uuid())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    Ok(row.map(|(kind, id)| match kind {
        OwnerRefKind::World => proxima_core::OwnerRef::World,
        OwnerRefKind::Personal => proxima_core::OwnerRef::Personal(proxima_core::UserId::new(id)),
        OwnerRefKind::Group => proxima_core::OwnerRef::Group(proxima_core::GroupId::new(id)),
    }))
}
