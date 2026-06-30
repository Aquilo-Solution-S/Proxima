//! Lookup helpers for code MCP tools.

use proxima_core::{Owner, OwnerRefKind, ToolCtx, ToolError};

use super::CodeToolCtxExt;
use super::code_store;

pub fn owner_columns(owner: &Owner) -> (OwnerRefKind, Option<uuid::Uuid>) {
    owner.columns()
}

pub async fn resolve_repo_identifier(
    ctx: &ToolCtx,
    identifier: &str,
) -> Result<uuid::Uuid, ToolError> {
    let trimmed = identifier.trim();
    if trimmed.is_empty() {
        return Err(ToolError::InvalidInput("repo_handle required".into()));
    }
    if let Ok(repo_id) = ctx.resolve_flavor_object(trimmed, super::REPO_HANDLE_KIND) {
        return Ok(repo_id);
    }
    if let Ok(repo_id) = uuid::Uuid::parse_str(trimmed) {
        return Ok(repo_id);
    }

    let (owner_kind, owner_id) = owner_columns(&ctx.owner());
    let pool = code_store(ctx)?;
    let rows: Vec<RepoLookupRow> = sqlx::query_as(
        "SELECT repo_id
         FROM proxima_code.repos
         WHERE owner_kind = $1
           AND owner_id = $2
           AND (
               lower(display_name) = lower($3)
               OR lower(canonical_path) = lower($3)
               OR lower(regexp_replace(canonical_path, '^.*/', '')) = lower($3)
           )
         ORDER BY created_at DESC
         LIMIT 2",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(trimmed)
    .fetch_all(pool.pool())
    .await
    .map_err(map_storage)?;

    match rows.as_slice() {
        [row] => Ok(row.repo_id),
        [] => Err(ToolError::InvalidInput(format!(
            "repo_handle not found for owner: {identifier}"
        ))),
        _ => Err(ToolError::InvalidInput(
            "repo_handle matched multiple repos; use a returned repo_handle".into(),
        )),
    }
}

#[derive(sqlx::FromRow)]
struct RepoLookupRow {
    repo_id: uuid::Uuid,
}

#[allow(clippy::needless_pass_by_value)]
pub fn map_storage(error: sqlx::Error) -> ToolError {
    ToolError::Storage(proxima_core::StorageError::Internal(error.to_string()))
}
