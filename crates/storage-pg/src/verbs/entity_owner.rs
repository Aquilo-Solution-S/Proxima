//! Group-ownership access predicates over `entity_owner` and
//! `group_membership`.

use proxima_core::{
    EntityId, GroupId, MembershipRow, OwnerPrincipalKind, Principal, Relation, StorageError,
};
use sqlx::PgPool;

use crate::error::map_err;

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
