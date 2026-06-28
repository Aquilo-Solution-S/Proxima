//! Group-ownership access predicates over `entity_owner` and
//! `group_membership`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use proxima_core::{
    EntityId, GroupId, MembershipRow, OwnerRef, OwnerRefKind, Relation, StorageError, UserId,
};
use sqlx::{PgConnection, PgPool};

use crate::error::map_err;

const ENTITY_OWNER_TABLE: &str = "proxima_core.entity_owner";
const ENTITY_OWNER_PLACEHOLDER: &str = "__PROXIMA_ENTITY_OWNER__";

/// Render a SQL literal that uses the private owner-table placeholder.
///
/// This keeps PR1's compatibility table spelling in this adapter while PR2
/// owns the physical schema reset. Rendered strings are cached once per call
/// site and then reused as `'static` SQL text.
#[must_use]
pub fn sql(template: &'static str) -> &'static str {
    static CACHE: OnceLock<Mutex<HashMap<&'static str, &'static str>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().expect("owner SQL cache poisoned");
    if let Some(rendered) = guard.get(template) {
        return rendered;
    }
    let rendered = template.replace(ENTITY_OWNER_PLACEHOLDER, ENTITY_OWNER_TABLE);
    let rendered = Box::leak(rendered.into_boxed_str());
    guard.insert(template, rendered);
    rendered
}

/// Render dynamically formatted SQL containing the owner-table placeholder.
#[must_use]
pub fn sql_owned(template: impl AsRef<str>) -> String {
    template
        .as_ref()
        .replace(ENTITY_OWNER_PLACEHOLDER, ENTITY_OWNER_TABLE)
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn insert_home(
    conn: &mut PgConnection,
    entity_id: uuid::Uuid,
    owner: &OwnerRef,
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
    member: &OwnerRef,
) -> Result<Vec<MembershipRow>, StorageError> {
    let OwnerRef::Personal(user) = member else {
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
pub(crate) async fn visible_to_any(
    pool: &PgPool,
    entity: EntityId,
    read_owners: &[OwnerRef],
) -> Result<bool, StorageError> {
    if read_owners.is_empty() {
        return Ok(false);
    }

    let kinds: Vec<OwnerRefKind> = read_owners
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
              WHERE eo.entity_id = $1
                AND eo.is_home)",
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
pub(crate) async fn home_owner(
    pool: &PgPool,
    entity: EntityId,
) -> Result<Option<OwnerRef>, StorageError> {
    let row: Option<(OwnerRefKind, uuid::Uuid)> = sqlx::query_as(
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
