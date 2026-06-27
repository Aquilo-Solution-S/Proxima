//! Persisted access-grant repository (the `access_grants` table from migration
//! 0005). One Zanzibar-shaped relation `(resource, relation, subject)` resolved
//! for the engine's two authorization entry points, plus the grant-management
//! writes (share/unshare/visibility) and the multiple-owner ops.
//!
//! Queries are runtime `sqlx::query`/`query_as` (not the checked macros) so the
//! crate builds offline; correctness is pinned by the PG integration tests.

use proxima_core::access::{
    AccessGrantRow, EntryAccessFacts, GrantResource, GrantSelector, GrantSubject, NewAccessGrant,
    Relation, RelationSelector, RemoveOwnerOutcome, Visibility,
};
use proxima_core::personality::{MemorySnapshot, WakeChainDepth};
use proxima_core::{
    EntityKind, MemoryId, Owner, OwnerPrincipalKind, PersonalityInstanceId, Principal, SchemaId,
    SchemaVersion, StorageError,
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

type PublicMemoryRow = (
    uuid::Uuid,
    Option<EntityKind>,
    String,
    i32,
    Option<String>,
    i16,
    Option<uuid::Uuid>,
);

fn split_resource(resource: GrantResource) -> (ResourceKind, Option<uuid::Uuid>) {
    match resource {
        GrantResource::Space => (ResourceKind::Space, None),
        GrantResource::Memory(id) => (ResourceKind::Memory, Some(id.into_inner())),
    }
}

fn subject_columns(subject: &GrantSubject) -> (SubjectKind, OwnerPrincipalKind, uuid::Uuid) {
    match subject {
        GrantSubject::Principal(principal) => {
            let (kind, id) = principal.columns();
            (SubjectKind::Principal, kind, id)
        }
        GrantSubject::Group(group) => (
            SubjectKind::Group,
            OwnerPrincipalKind::Group,
            group.into_inner(),
        ),
    }
}

/// Decode a `(relation, subject_kind, subject_principal_kind, subject_id)` row
/// into the engine's [`AccessGrantRow`].
fn to_grant_row(
    (relation, subject_kind, subject_principal_kind, subject_principal_id): (
        Relation,
        SubjectKind,
        OwnerPrincipalKind,
        uuid::Uuid,
    ),
) -> Result<AccessGrantRow, StorageError> {
    let subject = match subject_kind {
        SubjectKind::Principal => {
            GrantSubject::Principal(subject_principal_kind.with_uuid(subject_principal_id))
        }
        SubjectKind::Group => {
            if subject_principal_kind != OwnerPrincipalKind::Group {
                return Err(StorageError::Internal(
                    "access grant group subject has non-group principal kind".into(),
                ));
            }
            GrantSubject::Group(proxima_core::GroupId::new(subject_principal_id))
        }
    };
    Ok(AccessGrantRow { relation, subject })
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
                g.subject_kind,
                g.subject_principal_kind,
                g.subject_principal_id
           FROM proxima_core.access_grants g
          WHERE g.resource_kind = 'space'
            AND g.owner_principal_kind = $1 AND g.owner_principal_id = $2
            AND g.grant_state = 'active'
            AND {match}",
        match = SUBJECT_MATCH.replace("$P_KIND", "$3").replace("$P_ID", "$4"),
    );
    let rows: Vec<(Relation, SubjectKind, OwnerPrincipalKind, uuid::Uuid)> = sqlx::query_as(&sql)
        .bind(owner_kind)
        .bind(owner_id)
        .bind(p_kind)
        .bind(p_id)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;
    rows.into_iter().map(to_grant_row).collect()
}

pub(crate) async fn resolve_entry_relations(
    pool: &PgPool,
    memory_id: MemoryId,
    principal: &Principal,
) -> Result<Vec<AccessGrantRow>, StorageError> {
    let (p_kind, p_id) = principal.columns();
    let sql = format!(
        "SELECT g.relation,
                g.subject_kind,
                g.subject_principal_kind,
                g.subject_principal_id
           FROM proxima_core.access_grants g
          WHERE g.resource_kind = 'memory'
            AND g.resource_id = $1
            AND g.grant_state = 'active'
            AND {match}",
        match = SUBJECT_MATCH.replace("$P_KIND", "$2").replace("$P_ID", "$3"),
    );
    let rows: Vec<(Relation, SubjectKind, OwnerPrincipalKind, uuid::Uuid)> = sqlx::query_as(&sql)
        .bind(memory_id.into_inner())
        .bind(p_kind)
        .bind(p_id)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;
    rows.into_iter().map(to_grant_row).collect()
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
        subject_columns(&grant.subject);
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
        subject_columns(&selector.subject);
    let sql = match selector.relation {
        RelationSelector::AllGrantable => {
            "UPDATE proxima_core.access_grants
            SET grant_state = 'revoked', revoked_at = now()
          WHERE grant_state = 'active'
            AND owner_principal_kind = $1 AND owner_principal_id = $2
            AND resource_kind = $3
            AND resource_id IS NOT DISTINCT FROM $4
            AND subject_kind = $5
            AND subject_principal_kind = $6 AND subject_principal_id = $7
            AND relation <> 'owner'"
        }
        RelationSelector::Exact(_) => {
            "UPDATE proxima_core.access_grants
            SET grant_state = 'revoked', revoked_at = now()
          WHERE grant_state = 'active'
            AND owner_principal_kind = $1 AND owner_principal_id = $2
            AND resource_kind = $3
            AND resource_id IS NOT DISTINCT FROM $4
            AND subject_kind = $5
            AND subject_principal_kind = $6 AND subject_principal_id = $7
            AND relation = $8"
        }
    };
    let mut query = sqlx::query(sql)
        .bind(owner_kind)
        .bind(owner_id)
        .bind(resource_kind)
        .bind(resource_id)
        .bind(subject_kind)
        .bind(subject_principal_kind)
        .bind(subject_principal_id);
    if let RelationSelector::Exact(relation) = selector.relation {
        query = query.bind(relation);
    }
    let result = query.execute(pool).await.map_err(map_err)?;
    Ok(result.rows_affected())
}

pub(crate) async fn share_entry_atomic(
    pool: &PgPool,
    grant: &NewAccessGrant,
    set_shared_if_private: bool,
) -> Result<(), StorageError> {
    let (owner_kind, owner_id) = grant.space_owner.columns();
    let (resource_kind, resource_id) = split_resource(grant.resource);
    let (subject_kind, subject_principal_kind, subject_principal_id) =
        subject_columns(&grant.subject);
    let mut tx = pool.begin().await.map_err(map_err)?;

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
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;
    let _ = result; // ON CONFLICT DO NOTHING makes re-grant idempotent.

    if set_shared_if_private && let GrantResource::Memory(memory_id) = grant.resource {
        sqlx::query(
            "UPDATE proxima_core.memories
                    SET visibility = 'shared'
                  WHERE memory_id = $1
                    AND owner_principal_kind = $2 AND owner_principal_id = $3
                    AND tombstoned_at IS NULL
                    AND visibility = 'private'",
        )
        .bind(memory_id.into_inner())
        .bind(owner_kind)
        .bind(owner_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    }

    tx.commit().await.map_err(map_err)?;
    Ok(())
}

pub(crate) async fn unshare_entry_atomic(
    pool: &PgPool,
    selector: &GrantSelector,
) -> Result<u64, StorageError> {
    let (owner_kind, owner_id) = selector.space_owner.columns();
    let (resource_kind, resource_id) = split_resource(selector.resource);
    let (subject_kind, subject_principal_kind, subject_principal_id) =
        subject_columns(&selector.subject);
    let mut tx = pool.begin().await.map_err(map_err)?;

    let sql = match selector.relation {
        RelationSelector::AllGrantable => {
            "UPDATE proxima_core.access_grants
            SET grant_state = 'revoked', revoked_at = now()
          WHERE grant_state = 'active'
            AND owner_principal_kind = $1 AND owner_principal_id = $2
            AND resource_kind = $3
            AND resource_id IS NOT DISTINCT FROM $4
            AND subject_kind = $5
            AND subject_principal_kind = $6 AND subject_principal_id = $7
            AND relation <> 'owner'"
        }
        RelationSelector::Exact(_) => {
            "UPDATE proxima_core.access_grants
            SET grant_state = 'revoked', revoked_at = now()
          WHERE grant_state = 'active'
            AND owner_principal_kind = $1 AND owner_principal_id = $2
            AND resource_kind = $3
            AND resource_id IS NOT DISTINCT FROM $4
            AND subject_kind = $5
            AND subject_principal_kind = $6 AND subject_principal_id = $7
            AND relation = $8"
        }
    };
    let mut query = sqlx::query(sql)
        .bind(owner_kind)
        .bind(owner_id)
        .bind(resource_kind)
        .bind(resource_id)
        .bind(subject_kind)
        .bind(subject_principal_kind)
        .bind(subject_principal_id);
    if let RelationSelector::Exact(relation) = selector.relation {
        query = query.bind(relation);
    }
    let result = query.execute(&mut *tx).await.map_err(map_err)?;
    let revoked = result.rows_affected();

    if let GrantResource::Memory(memory_id) = selector.resource {
        sqlx::query(
            "UPDATE proxima_core.memories m
                SET visibility = 'private'
              WHERE m.memory_id = $1
                AND m.owner_principal_kind = $2 AND m.owner_principal_id = $3
                AND m.tombstoned_at IS NULL
                AND m.visibility = 'shared'
                AND NOT EXISTS (
                    SELECT 1
                      FROM proxima_core.access_grants g
                     WHERE g.resource_kind = 'memory'
                       AND g.resource_id = $1
                       AND g.owner_principal_kind = $2
                       AND g.owner_principal_id = $3
                       AND g.grant_state = 'active'
                )",
        )
        .bind(memory_id.into_inner())
        .bind(owner_kind)
        .bind(owner_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    }

    tx.commit().await.map_err(map_err)?;
    Ok(revoked)
}

pub(crate) async fn list_access_grants(
    pool: &PgPool,
    space_owner: &Owner,
    resource: GrantResource,
) -> Result<Vec<AccessGrantRow>, StorageError> {
    let (owner_kind, owner_id) = space_owner.columns();
    let (resource_kind, resource_id) = split_resource(resource);
    let rows: Vec<(Relation, SubjectKind, OwnerPrincipalKind, uuid::Uuid)> = sqlx::query_as(
        "SELECT relation,
                subject_kind,
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
    rows.into_iter().map(to_grant_row).collect()
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

pub(crate) async fn list_public_memories(
    pool: &PgPool,
    limit: i64,
) -> Result<Vec<MemorySnapshot>, StorageError> {
    let rows: Vec<PublicMemoryRow> = sqlx::query_as(
        "SELECT memory_id, kind, schema_id, schema_version, text,
                wake_chain_depth, personality_instance_id
           FROM proxima_core.memories
          WHERE visibility = 'public'
            AND tombstoned_at IS NULL
          ORDER BY created_at DESC, memory_id DESC
          LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    rows.into_iter()
        .map(
            |(
                memory_id,
                kind,
                schema_id,
                schema_version,
                text,
                wake_chain_depth,
                personality_instance_id,
            )| {
                let schema_version = u32::try_from(schema_version).map_err(|_| {
                    StorageError::Internal(format!(
                        "public memory schema_version does not fit u32: {schema_version}"
                    ))
                })?;
                let wake_chain_depth = u16::try_from(wake_chain_depth).map_err(|_| {
                    StorageError::Internal(format!(
                        "public memory wake_chain_depth does not fit u16: {wake_chain_depth}"
                    ))
                })?;
                Ok(MemorySnapshot {
                    memory_id: MemoryId::new(memory_id),
                    kind: kind.unwrap_or(EntityKind::Fact).as_str().to_string(),
                    schema_id: SchemaId::new(schema_id),
                    schema_version: SchemaVersion::new(schema_version),
                    authoring_personality_instance_id: personality_instance_id
                        .map(PersonalityInstanceId::new),
                    text,
                    wake_chain_depth: WakeChainDepth::new(wake_chain_depth),
                    payload: None,
                })
            },
        )
        .collect()
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
    u64::try_from(count)
        .map_err(|_| StorageError::Internal(format!("negative active entry grant count: {count}")))
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
            subject: GrantSubject::Principal(owner_principal.clone()),
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
