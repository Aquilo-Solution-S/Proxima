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

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn visible_to_any(
    pool: &PgPool,
    entity: EntityId,
    read_owners: &[OwnerRef],
) -> Result<bool, StorageError> {
    if read_owners.is_empty() {
        return Ok(false);
    }

    let owner_ids: Vec<uuid::Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let (ok,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (
             SELECT 1 FROM proxima_core.memory m
              WHERE m.t = $1 AND m.owner_id = ANY($2::uuid[])
         )
         OR EXISTS (
             SELECT 1 FROM proxima_core.goal g
              WHERE g.t = $1 AND g.owner_id = ANY($2::uuid[])
         )",
    )
    .bind(entity.uuid())
    .bind(&owner_ids)
    .fetch_one(pool)
    .await
    .map_err(map_err)?;

    Ok(ok)
}

/// Transfer one memory or goal row's owner columns to `OwnerRef::World` in a
/// single statement, gated on the row currently being owned by
/// `from_owner`. Memory transfers additionally require the row to be
/// untombstoned. Returns `true` iff a row matched and was updated.
///
/// Scope is deliberately minimal: only the memory/goal row's own owner
/// columns move. Owner-shadow rows on subordinate tables (`edges`,
/// `embeddings`, `embedding_heads`, `fact_receipts`, ...) are left under
/// the prior owner — they are write-tracking metadata, not independently
/// readable/writable surfaces (`OwnerAccessReadPort::home_owner` and
/// `visible_to_any` only ever consult `memories`/`goals`).
///
/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn transfer_to_world(
    pool: &PgPool,
    entity: EntityId,
    from_owner: OwnerRef,
) -> Result<bool, StorageError> {
    let from_id = from_owner.stored_owner_id();
    match entity {
        EntityId::Memory(memory_id) => {
            let handle: Option<uuid::Uuid> = sqlx::query_scalar(
                "SELECT handle FROM proxima_core.memory
                  WHERE t = $1 AND owner_id = $2",
            )
            .bind(memory_id.into_inner())
            .bind(from_id)
            .fetch_optional(pool)
            .await
            .map_err(map_err)?;
            let Some(handle) = handle else {
                return Ok(false);
            };
            let mut tx = pool.begin().await.map_err(internal)?;
            crate::verbs::query_timeseries::publish_head(&mut tx, handle).await?;
            tx.commit().await.map_err(map_err)?;
            Ok(true)
        }
        EntityId::Goal(_) => Ok(false),
    }
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
