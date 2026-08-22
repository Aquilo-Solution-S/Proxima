//! Lookup helpers for code MCP tools.

use proxima_core::{Owner, OwnerRefKind, ToolCtx, ToolError};

use super::CodeToolCtxExt;
use super::code_store;

pub fn owner_columns(owner: &Owner) -> (OwnerRefKind, uuid::Uuid) {
    owner.columns()
}

/// Resolve a `repo_handle`, a bare repo UUID, or a display name / path to a
/// repo id that **exists for this owner**.
///
/// Handle, UUID, and name all go through one existence query (PK lookup).
/// A repo belonging to a different owner is reported exactly like one that
/// does not exist — this cannot probe other owners' repo ids.
pub async fn resolve_repo_identifier(
    ctx: &ToolCtx,
    identifier: &str,
) -> Result<uuid::Uuid, ToolError> {
    let trimmed = identifier.trim();
    if trimmed.is_empty() {
        return Err(ToolError::InvalidInput("repo_handle required".into()));
    }
    // Parsing an id shape proves only that it *is* an id shape. Whether it
    // names a repo is the query's business, below.
    let claimed_id = ctx
        .resolve_flavor_object(trimmed, super::REPO_HANDLE_KIND)
        .ok()
        .or_else(|| uuid::Uuid::parse_str(trimmed).ok());

    let (owner_kind, owner_id) = owner_columns(&ctx.owner());
    let pool = code_store(ctx)?;
    // The name arms are gated behind `claimed_id IS NULL`, so an id-shaped
    // identifier resolves only as an id and never falls through to a name
    // match.
    let rows: Vec<RepoLookupRow> = sqlx::query_as(
        "SELECT repo_id
         FROM proxima_code.repos
         WHERE owner_kind = $1
           AND owner_id = $2
           AND (
               ($4::uuid IS NOT NULL AND repo_id = $4)
               OR ($4::uuid IS NULL AND (
                      lower(display_name) = lower($3)
                      OR lower(canonical_path) = lower($3)
                      OR lower(regexp_replace(canonical_path, '^.*/', '')) = lower($3)
                  ))
           )
         ORDER BY created_at DESC
         LIMIT 2",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(trimmed)
    .bind(claimed_id)
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
