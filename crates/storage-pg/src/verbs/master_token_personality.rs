//! Idempotent lookup-or-mint of the per-master-token shell-author
//! personality. Used as provenance for every master-token MCP call.

use proxima_core::{
    InstantiatePersonalityRequest, MasterTokenPersonality, MemoryId, Owner,
    PersonalityInstanceId, Principal, StorageError,
};
use sqlx::PgPool;
use uuid::Uuid;

use super::consolidate;

const SHELL_AUTHOR_DISPLAY_NAME: &str = "shell-author";
const SHELL_AUTHOR_PURPOSE: &str = "Per-master-token MCP client identity";

pub async fn ensure_master_token_personality(
    pool: &PgPool,
    owner: &Owner,
    master_token_id: Uuid,
) -> Result<MasterTokenPersonality, StorageError> {
    let (kind, principal_id, org_id) = owner_columns(owner);

    // Fast path: existing mapping.
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT mtp.personality_instance_id,
                p.current_root_perspective_memory_id
         FROM proxima_core.master_token_personality mtp
         JOIN proxima_core.personality p
           ON p.personality_instance_id = mtp.personality_instance_id
         WHERE mtp.master_token_id = $1
           AND mtp.owner_principal_kind = $2
           AND mtp.owner_principal_id = $3
           AND mtp.owner_org_id = $4
         LIMIT 1",
    )
    .bind(master_token_id)
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .fetch_optional(pool)
    .await
    .map_err(|err| StorageError::Internal(err.to_string()))?;

    if let Some((instance_id, root_id)) = row {
        return Ok(MasterTokenPersonality {
            instance_id: PersonalityInstanceId::new(instance_id),
            self_perspective_memory_id: MemoryId::new(root_id),
        });
    }

    // Slow path: instantiate, then map.
    let req = InstantiatePersonalityRequest {
        owner: owner.clone(),
        display_name: SHELL_AUTHOR_DISPLAY_NAME.into(),
        purpose: SHELL_AUTHOR_PURPOSE.into(),
    };
    let resp = consolidate::instantiate_personality(pool, &req).await?;
    let instance_id = resp.instance_id;

    sqlx::query(
        "INSERT INTO proxima_core.master_token_personality (
             master_token_id, owner_principal_kind, owner_principal_id,
             owner_org_id, personality_instance_id
         ) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (master_token_id, owner_principal_kind,
                      owner_principal_id, owner_org_id) DO NOTHING",
    )
    .bind(master_token_id)
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(instance_id.into_inner())
    .execute(pool)
    .await
    .map_err(|err| StorageError::Internal(err.to_string()))?;

    let root_id: Uuid = sqlx::query_scalar(
        "SELECT current_root_perspective_memory_id
         FROM proxima_core.personality
         WHERE personality_instance_id = $1",
    )
    .bind(instance_id.into_inner())
    .fetch_one(pool)
    .await
    .map_err(|err| StorageError::Internal(err.to_string()))?;

    Ok(MasterTokenPersonality {
        instance_id,
        self_perspective_memory_id: MemoryId::new(root_id),
    })
}

fn owner_columns(owner: &Owner) -> (&'static str, Uuid, Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}
