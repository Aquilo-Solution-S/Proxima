//! Group-ownership access predicates over `entity_owner` and
//! `group_membership`.

use proxima_core::personality::{MemorySnapshot, SidecarSpec};
use proxima_core::{
    EntityId, EntityOwnerRow, GroupId, MembershipRow, MemoryId, OwnerPrincipalKind, Principal,
    Relation, RemoveOwnerOutcome, StorageError, UserId,
};
use sqlx::{PgConnection, PgPool};

use crate::error::map_err;
use crate::sidecars::PgSidecarRegistryFrozen;
use crate::verbs::consolidate::load_memory_by_id;

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn insert_entity_owner_home(
    conn: &mut PgConnection,
    entity_id: uuid::Uuid,
    owner: &Principal,
    granted_by: Option<uuid::Uuid>,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.entity_owner
            (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
         VALUES ($1, $2, $3, true, $4)",
    )
    .bind(entity_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(granted_by)
    .execute(conn)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn resolve_membership(
    pool: &PgPool,
    member: &Principal,
) -> Result<Vec<MembershipRow>, StorageError> {
    let Principal::User(user) = member else {
        return Ok(Vec::new());
    };

    let rows: Vec<(uuid::Uuid, Relation)> = sqlx::query_as(
        "SELECT group_id, relation
           FROM proxima_core.group_membership
          WHERE member_user_id = $1",
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
pub(crate) async fn add_entity_owner_share(
    pool: &PgPool,
    entity_id: uuid::Uuid,
    owner: &Principal,
    granted_by: Option<uuid::Uuid>,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.entity_owner
            (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
         VALUES ($1, $2, $3, false, $4)
         ON CONFLICT (entity_id, owner_principal_kind, owner_principal_id) DO NOTHING",
    )
    .bind(entity_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(granted_by)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn remove_entity_owner_share(
    pool: &PgPool,
    entity_id: uuid::Uuid,
    owner: &Principal,
) -> Result<RemoveOwnerOutcome, StorageError> {
    let (owner_kind, owner_principal_id) = owner.columns();
    let deleted = sqlx::query(
        "DELETE FROM proxima_core.entity_owner
          WHERE entity_id = $1
            AND owner_principal_kind = $2
            AND owner_principal_id = $3
            AND NOT is_home",
    )
    .bind(entity_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .execute(pool)
    .await
    .map_err(map_err)?
    .rows_affected();
    if deleted > 0 {
        return Ok(RemoveOwnerOutcome::Removed);
    }

    let row: Option<(bool,)> = sqlx::query_as(
        "SELECT is_home
           FROM proxima_core.entity_owner
          WHERE entity_id = $1
            AND owner_principal_kind = $2
            AND owner_principal_id = $3",
    )
    .bind(entity_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    Ok(match row {
        Some((true,)) => RemoveOwnerOutcome::RefusedLastOwner,
        Some((false,)) | None => RemoveOwnerOutcome::NotFound,
    })
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn list_entity_owners(
    pool: &PgPool,
    entity_id: uuid::Uuid,
) -> Result<Vec<EntityOwnerRow>, StorageError> {
    let rows: Vec<(OwnerPrincipalKind, uuid::Uuid, bool)> = sqlx::query_as(
        "SELECT owner_principal_kind, owner_principal_id, is_home
           FROM proxima_core.entity_owner
          WHERE entity_id = $1
          ORDER BY is_home DESC, created_at, owner_principal_kind::text, owner_principal_id",
    )
    .bind(entity_id)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|(kind, id, is_home)| EntityOwnerRow {
            owner: kind.with_uuid(id),
            is_home,
        })
        .collect())
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn list_world_entities(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    limit: usize,
    sidecars: &[SidecarSpec],
) -> Result<Vec<MemorySnapshot>, StorageError> {
    let (world_kind, world_id) = proxima_core::access::world().columns();
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let memory_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT m.memory_id
           FROM proxima_core.memories m
           JOIN proxima_core.entity_owner eo
             ON eo.entity_id = m.memory_id
          WHERE eo.owner_principal_kind = $1
            AND eo.owner_principal_id = $2
            AND m.tombstoned_at IS NULL
          ORDER BY m.created_at DESC, m.memory_id DESC
          LIMIT $3",
    )
    .bind(world_kind)
    .bind(world_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut snapshots = Vec::with_capacity(memory_ids.len());
    for memory_id in memory_ids {
        if let Some(snapshot) =
            load_memory_by_id(pool, pg_sidecars, MemoryId::new(memory_id), None, sidecars).await?
        {
            snapshots.push(snapshot);
        }
    }
    Ok(snapshots)
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn add_group_member(
    pool: &PgPool,
    group_id: GroupId,
    member_user_id: UserId,
    relation: Relation,
    granted_by: uuid::Uuid,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO proxima_core.group_membership
            (group_id, member_user_id, relation, granted_by)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (group_id, member_user_id, relation) DO NOTHING",
    )
    .bind(group_id.into_inner())
    .bind(member_user_id.into_inner())
    .bind(relation)
    .bind(granted_by)
    .execute(pool)
    .await
    .map_err(map_err)?;
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
    sqlx::query(
        "DELETE FROM proxima_core.group_membership
          WHERE group_id = $1
            AND member_user_id = $2",
    )
    .bind(group_id.into_inner())
    .bind(member_user_id.into_inner())
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(())
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
           FROM proxima_core.group_membership
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
pub(crate) async fn entity_is_readable(
    pool: &PgPool,
    entity: EntityId,
    read_owners: &[Principal],
) -> Result<bool, StorageError> {
    if read_owners.is_empty() {
        return Ok(false);
    }

    let kinds: Vec<OwnerPrincipalKind> = read_owners
        .iter()
        .map(|principal| principal.columns().0)
        .collect();
    let ids: Vec<uuid::Uuid> = read_owners
        .iter()
        .map(|principal| principal.columns().1)
        .collect();
    let (ok,): (bool,) = sqlx::query_as(
        "SELECT EXISTS(
             SELECT 1
               FROM proxima_core.entity_owner eo
               JOIN unnest($2::proxima_core.owner_principal_kind[], $3::uuid[]) AS s(kind, id)
                 ON eo.owner_principal_kind = s.kind
                AND eo.owner_principal_id = s.id
              WHERE eo.entity_id = $1)",
    )
    .bind(entity.uuid())
    .bind(&kinds)
    .bind(&ids)
    .fetch_one(pool)
    .await
    .map_err(map_err)?;

    Ok(ok)
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn entity_home_owner(
    pool: &PgPool,
    entity: EntityId,
) -> Result<Option<Principal>, StorageError> {
    let row: Option<(OwnerPrincipalKind, uuid::Uuid)> = sqlx::query_as(
        "SELECT owner_principal_kind, owner_principal_id
           FROM proxima_core.entity_owner
          WHERE entity_id = $1
            AND is_home",
    )
    .bind(entity.uuid())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    Ok(row.map(|(kind, id)| kind.with_uuid(id)))
}
