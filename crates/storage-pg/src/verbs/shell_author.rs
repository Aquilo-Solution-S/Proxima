//! Lazy backfill / idempotent lookup of the `proxima/shell-author`
//! personality. Used as provenance for master-token MCP-CRUD writes.

use proxima_core::{
    InstantiatePersonalityRequest, Owner, PersonalityInstanceId, Principal, StorageError,
};
use sqlx::PgPool;

use super::consolidate;

const SHELL_AUTHOR_DISPLAY_NAME: &str = "shell-author";
const SHELL_AUTHOR_PURPOSE: &str =
    "Substrate authorship for master-token MCP CRUD writes";

pub async fn ensure_shell_author(
    pool: &PgPool,
    owner: &Owner,
) -> Result<PersonalityInstanceId, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);

    // Fast path: lookup by owner + display_name (joined via memories.text).
    let row: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT p.personality_instance_id
         FROM proxima_core.personality p
         JOIN proxima_core.memories m
           ON m.memory_id = p.current_root_perspective_memory_id
         WHERE p.owner_principal_kind = $1
           AND p.owner_principal_id = $2
           AND p.owner_org_id = $3
           AND m.text = $4
           AND p.status <> 'tombstoned'
         LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(SHELL_AUTHOR_DISPLAY_NAME)
    .fetch_optional(pool)
    .await
    .map_err(|err| StorageError::Internal(err.to_string()))?;

    if let Some(id) = row {
        return Ok(PersonalityInstanceId::new(id));
    }

    // Slow path: instantiate via the existing transactional verb.
    let req = InstantiatePersonalityRequest {
        owner: owner.clone(),
        display_name: SHELL_AUTHOR_DISPLAY_NAME.into(),
        purpose: SHELL_AUTHOR_PURPOSE.into(),
    };
    let resp = consolidate::instantiate_personality(pool, &req).await?;
    Ok(resp.instance_id)
}

fn owner_columns(owner: &Owner) -> (&'static str, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}
