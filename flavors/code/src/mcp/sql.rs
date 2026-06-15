//! Owner-scoped head-by-natural-key CTEs and lookup helpers for code MCP tools.

use proxima_core::{McpToolCtx, McpToolError, Owner, OwnerPrincipalKind};

pub const CHUNK_HEADS_CTE: &str = r"
chunk_heads AS (
    SELECT memory_id, repo_id, file_path, chunk_index,
           text, language, chunk_type,
           byte_range_start, byte_range_end,
           line_range_start, line_range_end
    FROM (
        SELECT DISTINCT ON (c.repo_id, c.file_path, c.chunk_index)
            c.memory_id, c.repo_id, c.file_path, c.chunk_index,
            c.text, c.language, c.chunk_type,
            c.byte_range_start, c.byte_range_end,
            c.line_range_start, c.line_range_end,
            c.state
        FROM proxima_code.code_chunk_v1 c
        JOIN proxima_core.memories m USING (memory_id)
        WHERE m.owner_principal_kind = $1
          AND m.owner_principal_id   = $2
        ORDER BY c.repo_id, c.file_path, c.chunk_index, m.created_at DESC
    ) latest
    WHERE state = 'Present'
)
";

pub const FILE_REVISION_HEADS_CTE: &str = r"
file_revision_heads AS (
    SELECT DISTINCT ON (f.repo_id, f.file_path)
        f.memory_id, f.repo_id, f.file_path, f.language,
        f.content_sha256, f.size_bytes, f.indexed_commit_sha,
        f.state, m.created_at
    FROM proxima_code.file_revision_v1 f
    JOIN proxima_core.memories m USING (memory_id)
    WHERE m.owner_principal_kind = $1
      AND m.owner_principal_id   = $2
    ORDER BY f.repo_id, f.file_path, m.created_at DESC
)
";

pub fn owner_principal(owner: &Owner) -> (OwnerPrincipalKind, uuid::Uuid) {
    owner.principal.columns()
}

pub async fn resolve_repo_identifier(
    ctx: &McpToolCtx,
    identifier: &str,
) -> Result<uuid::Uuid, McpToolError> {
    let trimmed = identifier.trim();
    if trimmed.is_empty() {
        return Err(McpToolError::InvalidInput("repo_handle required".into()));
    }
    if let Ok(repo_id) = ctx.resolve_flavor_object(trimmed, super::REPO_HANDLE_KIND) {
        return Ok(repo_id);
    }
    if let Ok(repo_id) = uuid::Uuid::parse_str(trimmed) {
        return Ok(repo_id);
    }

    let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
    let owner_org_id = ctx.owner.org_id.into_inner();
    let rows: Vec<RepoLookupRow> = sqlx::query_as(
        "SELECT repo_id
         FROM proxima_code.repos
         WHERE owner_principal_kind = $1
           AND owner_principal_id = $2
           AND owner_org_id = $3
           AND (
               lower(display_name) = lower($4)
               OR lower(canonical_path) = lower($4)
               OR lower(regexp_replace(canonical_path, '^.*/', '')) = lower($4)
           )
         ORDER BY created_at DESC
         LIMIT 2",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(trimmed)
    .fetch_all(&ctx.pool)
    .await
    .map_err(map_storage)?;

    match rows.as_slice() {
        [row] => Ok(row.repo_id),
        [] => Err(McpToolError::InvalidInput(format!(
            "repo_handle not found for owner: {identifier}"
        ))),
        _ => Err(McpToolError::InvalidInput(
            "repo_handle matched multiple repos; use a returned repo_handle".into(),
        )),
    }
}

#[derive(sqlx::FromRow)]
struct RepoLookupRow {
    repo_id: uuid::Uuid,
}

#[allow(clippy::needless_pass_by_value)]
pub fn map_storage(error: sqlx::Error) -> McpToolError {
    McpToolError::Storage(proxima_core::StorageError::Internal(error.to_string()))
}
