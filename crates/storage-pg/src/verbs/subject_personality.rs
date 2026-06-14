//! Idempotent lookup-or-mint of a per-subject personality.
//!
//! # Concurrency
//!
//! The slow path serializes concurrent callers using a session-level PG
//! advisory lock in a namespace distinct from
//! `master_token_personality`. Without it, two callers could both
//! observe an empty mapping, mint personalities, and race the mapping
//! insert; the loser would return an orphan personality id.

use proxima_core::{
    InstantiatePersonalityRequest, MasterTokenPersonality, MemoryId, Owner, OwnerPrincipalKind,
    PersonalityInstanceId, Principal, StorageError,
};
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

use super::consolidate;
use crate::error::map_err;

const SUBJECT_DISPLAY_NAME: &str = "subject";
const SUBJECT_PURPOSE: &str = "Per-subject principal identity";
const LOCK_KEY_DOMAIN: &[u8] = b"subject_personality_lock_v1";

/// Idempotent lookup-or-mint of the per-subject personality.
///
/// # Errors
///
/// Returns [`StorageError::Internal`] when the database round-trip fails
/// or when the upstream `instantiate_personality` slow path itself errors.
pub async fn ensure_subject_personality(
    pool: &PgPool,
    owner: &Owner,
    subject: &Principal,
) -> Result<MasterTokenPersonality, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let (subject_kind, subject_principal_id) = principal_columns(subject);

    // Fast path: lock-free read. Hits on every call after the first.
    if let Some(found) = lookup_pool(
        pool,
        owner_kind,
        owner_principal_id,
        owner_org_id,
        subject_kind,
        subject_principal_id,
    )
    .await?
    {
        return Ok(found);
    }

    // Slow path: take a session-level advisory lock on a pinned
    // connection so concurrent first-connects can't both mint.
    let mut conn = pool.acquire().await.map_err(map_err)?;
    let key = lock_key(
        owner.org_id.into_inner(),
        subject_kind,
        subject_principal_id,
    );
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(key)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_err)?;

    let result = mint_under_lock(
        &mut conn,
        pool,
        owner,
        owner_kind,
        owner_principal_id,
        owner_org_id,
        subject_kind,
        subject_principal_id,
    )
    .await;

    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .fetch_one(&mut *conn)
        .await;

    result
}

#[allow(clippy::too_many_arguments)]
async fn mint_under_lock(
    conn: &mut PgConnection,
    pool: &PgPool,
    owner: &Owner,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: Uuid,
    owner_org_id: Uuid,
    subject_kind: OwnerPrincipalKind,
    subject_principal_id: Uuid,
) -> Result<MasterTokenPersonality, StorageError> {
    // Re-check inside the lock: a peer may have minted while we waited.
    if let Some(found) = lookup_conn(
        conn,
        owner_kind,
        owner_principal_id,
        owner_org_id,
        subject_kind,
        subject_principal_id,
    )
    .await?
    {
        return Ok(found);
    }

    let req = InstantiatePersonalityRequest {
        principal: owner.principal.clone(),
        org_id: Some(owner.org_id),
        display_name: SUBJECT_DISPLAY_NAME.into(),
        purpose: SUBJECT_PURPOSE.into(),
    };
    let resp = consolidate::instantiate_personality(pool, &req).await?;
    let instance_id = resp.instance_id;

    sqlx::query(
        r"INSERT INTO proxima_core.subject_personality (
             owner_principal_kind, owner_principal_id, owner_org_id,
             subject_principal_kind, subject_principal_id,
             personality_instance_id
         ) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(subject_kind)
    .bind(subject_principal_id)
    .bind(instance_id.into_inner())
    .execute(&mut *conn)
    .await
    .map_err(map_err)?;

    let root_id: Uuid = sqlx::query(
        r"SELECT current_root_perspective_memory_id
             FROM proxima_core.personality
             WHERE personality_instance_id = $1",
    )
    .bind(instance_id.into_inner())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_err)?
    .get("current_root_perspective_memory_id");

    Ok(MasterTokenPersonality {
        instance_id,
        self_perspective_memory_id: MemoryId::new(root_id),
    })
}

async fn lookup_pool(
    pool: &PgPool,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: Uuid,
    owner_org_id: Uuid,
    subject_kind: OwnerPrincipalKind,
    subject_principal_id: Uuid,
) -> Result<Option<MasterTokenPersonality>, StorageError> {
    let row = sqlx::query(
        r"SELECT sp.personality_instance_id,
                  p.current_root_perspective_memory_id
             FROM proxima_core.subject_personality sp
             JOIN proxima_core.personality p
               ON p.personality_instance_id = sp.personality_instance_id
             WHERE sp.owner_principal_kind = $1
               AND sp.owner_principal_id = $2
               AND sp.owner_org_id = $3
               AND sp.subject_principal_kind = $4
               AND sp.subject_principal_id = $5
             LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(subject_kind)
    .bind(subject_principal_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.as_ref().map(into_personality))
}

async fn lookup_conn(
    conn: &mut PgConnection,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: Uuid,
    owner_org_id: Uuid,
    subject_kind: OwnerPrincipalKind,
    subject_principal_id: Uuid,
) -> Result<Option<MasterTokenPersonality>, StorageError> {
    let row = sqlx::query(
        r"SELECT sp.personality_instance_id,
                  p.current_root_perspective_memory_id
             FROM proxima_core.subject_personality sp
             JOIN proxima_core.personality p
               ON p.personality_instance_id = sp.personality_instance_id
             WHERE sp.owner_principal_kind = $1
               AND sp.owner_principal_id = $2
               AND sp.owner_org_id = $3
               AND sp.subject_principal_kind = $4
               AND sp.subject_principal_id = $5
             LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(subject_kind)
    .bind(subject_principal_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_err)?;
    Ok(row.as_ref().map(into_personality))
}

fn into_personality(row: &sqlx::postgres::PgRow) -> MasterTokenPersonality {
    MasterTokenPersonality {
        instance_id: PersonalityInstanceId::new(row.get("personality_instance_id")),
        self_perspective_memory_id: MemoryId::new(row.get("current_root_perspective_memory_id")),
    }
}

fn lock_key(org_id: Uuid, subject_kind: OwnerPrincipalKind, subject_id: Uuid) -> i64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(LOCK_KEY_DOMAIN);
    hasher.update(org_id.as_bytes());
    hasher.update(subject_kind.as_str().as_bytes());
    hasher.update(subject_id.as_bytes());
    let hash = hasher.finalize();
    let bytes: [u8; 8] = hash.as_bytes()[..8]
        .try_into()
        .expect("blake3 hash is 32 bytes");
    i64::from_le_bytes(bytes)
}

fn owner_columns(owner: &Owner) -> (OwnerPrincipalKind, Uuid, Uuid) {
    let (kind, principal_id) = principal_columns(&owner.principal);
    (kind, principal_id, owner.org_id.into_inner())
}

fn principal_columns(principal: &Principal) -> (OwnerPrincipalKind, Uuid) {
    match principal {
        Principal::User(u) => (OwnerPrincipalKind::User, u.into_inner()),
        Principal::Group(g) => (OwnerPrincipalKind::Group, g.into_inner()),
    }
}
