//! Direct `OwnerRef` column helpers.

use proxima_core::{
    EntityId, GroupId, MembershipRow, OwnerRef, OwnerRefKind, Relation, StorageError, UserId,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::map_err;

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
/// Returns `Internal` on sqlx failure.
pub(crate) async fn add_group_member(
    pool: &PgPool,
    group_id: GroupId,
    member_user_id: UserId,
    relation: Relation,
    _granted_by: uuid::Uuid,
) -> Result<(), StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
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
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn remove_group_member(
    pool: &PgPool,
    group_id: GroupId,
    member_user_id: UserId,
) -> Result<(), StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
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

    let (kinds, ids) = owner_arrays(read_owners);
    let (ok,): (bool,) = sqlx::query_as(
        "WITH allowed(owner_kind, owner_id) AS (
             SELECT * FROM unnest($2::proxima_core.owner_ref_kind[], $3::uuid[])
         )
         SELECT EXISTS (
             SELECT 1
               FROM proxima_core.memories m
               JOIN allowed a
                 ON a.owner_kind = m.owner_kind
                AND a.owner_id IS NOT DISTINCT FROM m.owner_id
              WHERE m.memory_id = $1
         )
         OR EXISTS (
             SELECT 1
               FROM proxima_core.goals g
               JOIN allowed a
                 ON a.owner_kind = g.owner_kind
                AND a.owner_id IS NOT DISTINCT FROM g.owner_id
              WHERE g.goal_id = $1
         )",
    )
    .bind(entity.uuid())
    .bind(&kinds)
    .bind(&ids)
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
    let (from_kind, from_id) = owner_binds(&from_owner);
    let (to_kind, to_id) = owner_binds(&OwnerRef::World);
    let rows_affected = match entity {
        EntityId::Memory(memory_id) => sqlx::query(
            "UPDATE proxima_core.memories
                    SET owner_kind = $4, owner_id = $5
                  WHERE memory_id = $1
                    AND owner_kind = $2
                    AND owner_id IS NOT DISTINCT FROM $3
                    AND tombstoned_at IS NULL",
        )
        .bind(memory_id.into_inner())
        .bind(from_kind)
        .bind(from_id)
        .bind(to_kind)
        .bind(to_id)
        .execute(pool)
        .await
        .map_err(map_err)?
        .rows_affected(),
        EntityId::Goal(goal_id) => sqlx::query(
            "UPDATE proxima_core.goals
                    SET owner_kind = $4, owner_id = $5
                  WHERE goal_id = $1
                    AND owner_kind = $2
                    AND owner_id IS NOT DISTINCT FROM $3",
        )
        .bind(goal_id.into_inner())
        .bind(from_kind)
        .bind(from_id)
        .bind(to_kind)
        .bind(to_id)
        .execute(pool)
        .await
        .map_err(map_err)?
        .rows_affected(),
    };
    Ok(rows_affected == 1)
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn home_owner(
    pool: &PgPool,
    entity: EntityId,
) -> Result<Option<OwnerRef>, StorageError> {
    let row: Option<(OwnerRefKind, Option<uuid::Uuid>)> = sqlx::query_as(
        "SELECT owner_kind, owner_id
           FROM proxima_core.memories
          WHERE memory_id = $1
         UNION ALL
         SELECT owner_kind, owner_id
           FROM proxima_core.goals
          WHERE goal_id = $1
         LIMIT 1",
    )
    .bind(entity.uuid())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    Ok(row.and_then(|(kind, id)| kind.with_uuid(id)))
}
