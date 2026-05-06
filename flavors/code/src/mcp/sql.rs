//! Owner-scoped head-by-natural-key CTEs for code MCP tools.

use proxima_core::{McpToolError, Owner, Principal};

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

pub fn owner_principal(owner: &Owner) -> (&'static str, uuid::Uuid) {
    match &owner.principal {
        Principal::User(user) => ("User", user.into_inner()),
        Principal::Group(group) => ("Group", group.into_inner()),
    }
}

#[allow(clippy::needless_pass_by_value)]
pub fn map_storage(error: sqlx::Error) -> McpToolError {
    McpToolError::Storage(proxima_core::StorageError::Internal(error.to_string()))
}
