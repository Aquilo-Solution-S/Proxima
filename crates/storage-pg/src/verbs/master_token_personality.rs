//! Idempotent lookup-or-mint of the per-master-token shell-author
//! personality. Used as provenance for every master-token MCP call.
//!
//! # Concurrency
//!
//! The slow path (cold start for a `(token, owner)` pair) serializes
//! concurrent callers using a transaction-scoped PG advisory lock keyed
//! on `master_token_id`. Without it, two callers could both
//! observe an empty mapping, both mint a personality, and only one of
//! the two `INSERT ... ON CONFLICT DO NOTHING` rows would land — the
//! losing caller's personality (plus its memories) becomes an orphan,
//! and that caller returns a different `instance_id` than the canonical
//! mapping points to.

use proxima_core::{
    InstantiatePersonalityRequest, MasterTokenPersonality, MemoryId, Owner, OwnerPrincipalKind,
    PersonalityInstanceId, StorageError,
};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::consolidate;
use crate::error::map_err;

const SHELL_AUTHOR_DISPLAY_NAME: &str = "shell-author";
const LOCK_KEY_DOMAIN: &[u8] = b"master_token_personality_lock_v1";

/// Idempotent lookup-or-mint of the per-master-token shell-author
/// personality. Used as provenance for every master-token MCP call.
///
/// # Errors
///
/// Returns [`StorageError::Internal`] when the database round-trip fails
/// or when the upstream `instantiate_personality` slow path itself errors.
pub async fn ensure_master_token_personality(
    pool: &PgPool,
    owner: &Owner,
    master_token_id: Uuid,
) -> Result<MasterTokenPersonality, StorageError> {
    let (kind, principal_id) = owner.columns();

    // Fast path: lock-free read. Hits on every call after the first.
    if let Some(found) = lookup_pool(pool, master_token_id, kind, principal_id).await? {
        return Ok(found);
    }

    // Slow path: take a transaction-scoped advisory lock so concurrent
    // first-connects can't both mint.
    let mut tx = pool.begin().await.map_err(map_err)?;
    let key = lock_key(master_token_id);
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(key)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_err)?;

    let result = mint_under_lock(&mut tx, owner, master_token_id, kind, principal_id).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(result)
}

async fn mint_under_lock(
    conn: &mut PgConnection,
    owner: &Owner,
    master_token_id: Uuid,
    kind: OwnerPrincipalKind,
    principal_id: Uuid,
) -> Result<MasterTokenPersonality, StorageError> {
    // Re-check inside the lock: a peer may have minted while we waited.
    if let Some(found) = lookup_conn(conn, master_token_id, kind, principal_id).await? {
        return Ok(found);
    }

    let req = InstantiatePersonalityRequest {
        principal: owner.clone(),
        display_name: SHELL_AUTHOR_DISPLAY_NAME.into(),
    };
    let resp = consolidate::instantiate_personality_on_conn(&mut *conn, &req).await?;
    let instance_id = resp.instance_id;

    sqlx::query!(
        r#"INSERT INTO proxima_core.master_token_personality (
             master_token_id, owner_principal_kind, owner_principal_id,
             personality_instance_id
         ) VALUES ($1, $2, $3, $4)"#,
        master_token_id,
        kind as OwnerPrincipalKind,
        principal_id,
        instance_id.into_inner(),
    )
    .execute(&mut *conn)
    .await
    .map_err(map_err)?;

    let root_id: Uuid = sqlx::query_scalar!(
        r#"SELECT current_root_perspective_memory_id
             FROM proxima_core.personality
             WHERE personality_instance_id = $1"#,
        instance_id.into_inner(),
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(map_err)?;

    Ok(MasterTokenPersonality {
        instance_id,
        self_perspective_memory_id: MemoryId::new(root_id),
    })
}

async fn lookup_pool(
    pool: &PgPool,
    master_token_id: Uuid,
    kind: OwnerPrincipalKind,
    principal_id: Uuid,
) -> Result<Option<MasterTokenPersonality>, StorageError> {
    let row = sqlx::query!(
        r#"SELECT mtp.personality_instance_id,
                  p.current_root_perspective_memory_id
             FROM proxima_core.master_token_personality mtp
             JOIN proxima_core.personality p
               ON p.personality_instance_id = mtp.personality_instance_id
             WHERE mtp.master_token_id = $1
               AND mtp.owner_principal_kind = $2
               AND mtp.owner_principal_id = $3
             LIMIT 1"#,
        master_token_id,
        kind as OwnerPrincipalKind,
        principal_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;
    Ok(row.map(|r| {
        into_personality((
            r.personality_instance_id,
            r.current_root_perspective_memory_id,
        ))
    }))
}

async fn lookup_conn(
    conn: &mut PgConnection,
    master_token_id: Uuid,
    kind: OwnerPrincipalKind,
    principal_id: Uuid,
) -> Result<Option<MasterTokenPersonality>, StorageError> {
    let row = sqlx::query!(
        r#"SELECT mtp.personality_instance_id,
                  p.current_root_perspective_memory_id
             FROM proxima_core.master_token_personality mtp
             JOIN proxima_core.personality p
               ON p.personality_instance_id = mtp.personality_instance_id
             WHERE mtp.master_token_id = $1
               AND mtp.owner_principal_kind = $2
               AND mtp.owner_principal_id = $3
             LIMIT 1"#,
        master_token_id,
        kind as OwnerPrincipalKind,
        principal_id,
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_err)?;
    Ok(row.map(|r| {
        into_personality((
            r.personality_instance_id,
            r.current_root_perspective_memory_id,
        ))
    }))
}

fn into_personality((instance_id, root_id): (Uuid, Uuid)) -> MasterTokenPersonality {
    MasterTokenPersonality {
        instance_id: PersonalityInstanceId::new(instance_id),
        self_perspective_memory_id: MemoryId::new(root_id),
    }
}

fn lock_key(master_token_id: Uuid) -> i64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(LOCK_KEY_DOMAIN);
    hasher.update(master_token_id.as_bytes());
    let hash = hasher.finalize();
    let bytes: [u8; 8] = hash.as_bytes()[..8]
        .try_into()
        .expect("blake3 hash is 32 bytes");
    i64::from_le_bytes(bytes)
}
