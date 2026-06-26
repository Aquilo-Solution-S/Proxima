//! Persisted access-grant repository (the `access_grants` table from migration
//! 0005). One Zanzibar-shaped relation `(resource, relation, subject)` resolved
//! for the engine's two authorization entry points, plus the grant-management
//! writes (share/unshare/visibility) and the multiple-owner ops.
//!
//! Queries are runtime `sqlx::query`/`query_as` (not the checked macros) so the
//! crate builds offline; correctness is pinned by the PG integration tests.

use proxima_core::access::{
    AccessGrantRow, EntryAccessFacts, GrantResource, GrantSelector, NewAccessGrant, Relation,
    RemoveOwnerOutcome, Visibility,
};
use proxima_core::{
    MemoryId, Owner, OwnerPrincipalKind, PersonalityInstanceId, Principal, StorageError,
};
use sqlx::PgPool;

use crate::error::map_err;

/// Storage encoding of `proxima_core.grant_resource_kind`. Local to the
/// repository — the engine speaks [`GrantResource`].
#[derive(Debug, Clone, Copy, sqlx::Type)]
#[sqlx(
    type_name = "proxima_core.grant_resource_kind",
    rename_all = "lowercase"
)]
enum ResourceKind {
    Space,
    Memory,
}

/// Storage encoding of `proxima_core.grant_subject_kind`.
#[derive(Debug, Clone, Copy, sqlx::Type)]
#[sqlx(
    type_name = "proxima_core.grant_subject_kind",
    rename_all = "lowercase"
)]
enum SubjectKind {
    Principal,
    Group,
}

fn split_resource(resource: GrantResource) -> (ResourceKind, Option<uuid::Uuid>) {
    match resource {
        GrantResource::Space => (ResourceKind::Space, None),
        GrantResource::Memory(id) => (ResourceKind::Memory, Some(id.into_inner())),
    }
}

fn subject_columns(
    subject: &Principal,
    is_group: bool,
) -> (SubjectKind, OwnerPrincipalKind, uuid::Uuid) {
    let (kind, id) = subject.columns();
    let subject_kind = if is_group {
        SubjectKind::Group
    } else {
        SubjectKind::Principal
    };
    (subject_kind, kind, id)
}

/// Decode a `(relation, is_group, subject_kind, subject_id)` row into the
/// engine's [`AccessGrantRow`].
fn to_grant_row(
    (relation, is_group, subject_kind, subject_id): (
        Relation,
        bool,
        OwnerPrincipalKind,
        uuid::Uuid,
    ),
) -> AccessGrantRow {
    AccessGrantRow {
        relation,
        subject: subject_kind.with_uuid(subject_id),
        subject_is_group: is_group,
    }
}

// The subject-match predicate shared by space + entry resolution: the grant's
// subject is the caller directly, OR a group the caller is a `member` of (one
// level, no nesting). `$P_KIND`/`$P_ID` are the caller's principal columns.
const SUBJECT_MATCH: &str = "
    (
      (g.subject_kind = 'principal'
         AND g.subject_principal_kind = $P_KIND AND g.subject_principal_id = $P_ID)
      OR (g.subject_kind = 'group' AND EXISTS (
          SELECT 1 FROM proxima_core.access_grants m
           WHERE m.resource_kind = 'space' AND m.relation = 'member'
             AND m.grant_state = 'active'
             AND m.owner_principal_kind = g.subject_principal_kind
             AND m.owner_principal_id = g.subject_principal_id
             AND m.subject_principal_kind = $P_KIND AND m.subject_principal_id = $P_ID))
    )";

pub(crate) async fn resolve_space_relations(
    pool: &PgPool,
    space_owner: &Owner,
    principal: &Principal,
) -> Result<Vec<AccessGrantRow>, StorageError> {
    let (owner_kind, owner_id) = space_owner.columns();
    let (p_kind, p_id) = principal.columns();
    let sql = format!(
        "SELECT g.relation,
                (g.subject_kind = 'group') AS is_group,
                g.subject_principal_kind,
                g.subject_principal_id
           FROM proxima_core.access_grants g
          WHERE g.resource_kind = 'space'
            AND g.owner_principal_kind = $1 AND g.owner_principal_id = $2
            AND g.grant_state = 'active'
            AND {match}",
        match = SUBJECT_MATCH.replace("$P_KIND", "$3").replace("$P_ID", "$4"),
    );
    let rows: Vec<(Relation, bool, OwnerPrincipalKind, uuid::Uuid)> = sqlx::query_as(&sql)
        .bind(owner_kind)
        .bind(owner_id)
        .bind(p_kind)
        .bind(p_id)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;
    Ok(rows.into_iter().map(to_grant_row).collect())
}

pub(crate) async fn resolve_entry_relations(
    pool: &PgPool,
    memory_id: MemoryId,
    principal: &Principal,
) -> Result<Vec<AccessGrantRow>, StorageError> {
    let (p_kind, p_id) = principal.columns();
    let sql = format!(
        "SELECT g.relation,
                (g.subject_kind = 'group') AS is_group,
                g.subject_principal_kind,
                g.subject_principal_id
           FROM proxima_core.access_grants g
          WHERE g.resource_kind = 'memory'
            AND g.resource_id = $1
            AND g.grant_state = 'active'
            AND {match}",
        match = SUBJECT_MATCH.replace("$P_KIND", "$2").replace("$P_ID", "$3"),
    );
    let rows: Vec<(Relation, bool, OwnerPrincipalKind, uuid::Uuid)> = sqlx::query_as(&sql)
        .bind(memory_id.into_inner())
        .bind(p_kind)
        .bind(p_id)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;
    Ok(rows.into_iter().map(to_grant_row).collect())
}

pub(crate) async fn resolve_entry_owner(
    pool: &PgPool,
    memory_id: MemoryId,
) -> Result<Option<EntryAccessFacts>, StorageError> {
    let row: Option<(OwnerPrincipalKind, uuid::Uuid, Visibility)> = sqlx::query_as(
        "SELECT owner_principal_kind, owner_principal_id, visibility
           FROM proxima_core.memories
          WHERE memory_id = $1 AND tombstoned_at IS NULL",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.map(|(kind, id, visibility)| EntryAccessFacts {
        owner: kind.with_uuid(id),
        visibility,
    }))
}

pub(crate) async fn insert_access_grant(
    pool: &PgPool,
    grant: &NewAccessGrant,
) -> Result<(), StorageError> {
    let (owner_kind, owner_id) = grant.space_owner.columns();
    let (resource_kind, resource_id) = split_resource(grant.resource);
    let (subject_kind, subject_principal_kind, subject_principal_id) =
        subject_columns(&grant.subject, grant.subject_is_group);
    let result = sqlx::query(
        "INSERT INTO proxima_core.access_grants
             (grant_id, owner_principal_kind, owner_principal_id, resource_kind,
              resource_id, relation, subject_kind, subject_principal_kind,
              subject_principal_id, granted_by_personality_instance_id)
         VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT DO NOTHING",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(resource_kind)
    .bind(resource_id)
    .bind(grant.relation)
    .bind(subject_kind)
    .bind(subject_principal_kind)
    .bind(subject_principal_id)
    .bind(grant.granted_by.into_inner())
    .execute(pool)
    .await
    .map_err(map_err)?;
    let _ = result; // ON CONFLICT DO NOTHING makes re-grant idempotent.
    Ok(())
}

pub(crate) async fn revoke_access_grants(
    pool: &PgPool,
    selector: &GrantSelector,
) -> Result<u64, StorageError> {
    let (owner_kind, owner_id) = selector.space_owner.columns();
    let (resource_kind, resource_id) = split_resource(selector.resource);
    let (subject_kind, subject_principal_kind, subject_principal_id) =
        subject_columns(&selector.subject, selector.subject_is_group);
    let result = sqlx::query(
        "UPDATE proxima_core.access_grants
            SET grant_state = 'revoked', revoked_at = now()
          WHERE grant_state = 'active'
            AND owner_principal_kind = $1 AND owner_principal_id = $2
            AND resource_kind = $3
            AND resource_id IS NOT DISTINCT FROM $4
            AND subject_kind = $5
            AND subject_principal_kind = $6 AND subject_principal_id = $7
            AND ($8::proxima_core.grant_relation IS NULL OR relation = $8)",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(resource_kind)
    .bind(resource_id)
    .bind(subject_kind)
    .bind(subject_principal_kind)
    .bind(subject_principal_id)
    .bind(selector.relation)
    .execute(pool)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
}

pub(crate) async fn list_access_grants(
    pool: &PgPool,
    space_owner: &Owner,
    resource: GrantResource,
) -> Result<Vec<AccessGrantRow>, StorageError> {
    let (owner_kind, owner_id) = space_owner.columns();
    let (resource_kind, resource_id) = split_resource(resource);
    let rows: Vec<(Relation, bool, OwnerPrincipalKind, uuid::Uuid)> = sqlx::query_as(
        "SELECT relation,
                (subject_kind = 'group') AS is_group,
                subject_principal_kind,
                subject_principal_id
           FROM proxima_core.access_grants
          WHERE grant_state = 'active'
            AND owner_principal_kind = $1 AND owner_principal_id = $2
            AND resource_kind = $3
            AND resource_id IS NOT DISTINCT FROM $4
          ORDER BY created_at, relation",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(resource_kind)
    .bind(resource_id)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().map(to_grant_row).collect())
}

pub(crate) async fn set_memory_visibility(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
    visibility: Visibility,
) -> Result<(), StorageError> {
    let (owner_kind, owner_id) = owner.columns();
    let result = sqlx::query(
        "UPDATE proxima_core.memories
            SET visibility = $4
          WHERE memory_id = $1
            AND owner_principal_kind = $2 AND owner_principal_id = $3
            AND tombstoned_at IS NULL",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(visibility)
    .execute(pool)
    .await
    .map_err(map_err)?;
    if result.rows_affected() == 0 {
        return Err(StorageError::NotFound);
    }
    Ok(())
}

pub(crate) async fn count_active_entry_grants(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
) -> Result<u64, StorageError> {
    let (owner_kind, owner_id) = owner.columns();
    let (count,): (i64,) = sqlx::query_as(
        "SELECT count(*)
           FROM proxima_core.access_grants
          WHERE resource_kind = 'memory' AND resource_id = $1
            AND owner_principal_kind = $2 AND owner_principal_id = $3
            AND grant_state = 'active'",
    )
    .bind(memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_one(pool)
    .await
    .map_err(map_err)?;
    Ok(count.max(0).unsigned_abs())
}

/// Shared owner-row insert for `init_space_owner` and `add_space_owner`. Both
/// write `(space:G, owner, principal:owner_principal)`; the distinction is the
/// gate at the verb (Unrestricted-provisioning vs owner-gated).
async fn insert_owner_grant(
    pool: &PgPool,
    space: &Owner,
    owner_principal: &Principal,
    granted_by: PersonalityInstanceId,
) -> Result<(), StorageError> {
    insert_access_grant(
        pool,
        &NewAccessGrant {
            space_owner: space.clone(),
            resource: GrantResource::Space,
            relation: Relation::Owner,
            subject: owner_principal.clone(),
            subject_is_group: false,
            granted_by,
        },
    )
    .await
}

pub(crate) async fn init_space_owner(
    pool: &PgPool,
    space: &Owner,
    owner_principal: &Principal,
    granted_by: PersonalityInstanceId,
) -> Result<(), StorageError> {
    insert_owner_grant(pool, space, owner_principal, granted_by).await
}

pub(crate) async fn add_space_owner(
    pool: &PgPool,
    space: &Owner,
    new_owner: &Principal,
    granted_by: PersonalityInstanceId,
) -> Result<(), StorageError> {
    insert_owner_grant(pool, space, new_owner, granted_by).await
}

pub(crate) async fn remove_space_owner(
    pool: &PgPool,
    space: &Owner,
    owner_principal: &Principal,
) -> Result<RemoveOwnerOutcome, StorageError> {
    let (owner_kind, owner_id) = space.columns();
    let (target_kind, target_id) = owner_principal.columns();
    let mut tx = pool.begin().await.map_err(map_err)?;

    // Lock the space's active owner rows so the count + revoke are atomic.
    let owners: Vec<(OwnerPrincipalKind, uuid::Uuid)> = sqlx::query_as(
        "SELECT subject_principal_kind, subject_principal_id
           FROM proxima_core.access_grants
          WHERE resource_kind = 'space' AND relation = 'owner'
            AND grant_state = 'active'
            AND owner_principal_kind = $1 AND owner_principal_id = $2
          FOR UPDATE",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_err)?;

    let target_present = owners
        .iter()
        .any(|(kind, id)| *kind == target_kind && *id == target_id);
    if !target_present {
        tx.rollback().await.map_err(map_err)?;
        return Ok(RemoveOwnerOutcome::NotFound);
    }
    if owners.len() <= 1 {
        tx.rollback().await.map_err(map_err)?;
        return Ok(RemoveOwnerOutcome::RefusedLastOwner);
    }

    sqlx::query(
        "UPDATE proxima_core.access_grants
            SET grant_state = 'revoked', revoked_at = now()
          WHERE resource_kind = 'space' AND relation = 'owner'
            AND grant_state = 'active'
            AND owner_principal_kind = $1 AND owner_principal_id = $2
            AND subject_principal_kind = $3 AND subject_principal_id = $4",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(target_kind)
    .bind(target_id)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    tx.commit().await.map_err(map_err)?;
    Ok(RemoveOwnerOutcome::Removed)
}
