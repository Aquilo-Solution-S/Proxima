use proxima_core::MemoryId;
use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::sql::{CHUNK_HEADS_CTE, FILE_REVISION_HEADS_CTE, map_storage, owner_principal};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeOpenFileRevisionArgs {
    pub repo_handle: String,
    pub file_path: String,
}

#[derive(Debug, Serialize)]
pub struct CodeOpenFileRevisionOutput {
    pub revision: Option<FileRevisionInfo>,
    pub chunks: Vec<ChunkSummary>,
}

#[derive(Debug, Serialize)]
pub struct FileRevisionInfo {
    pub handle: String,
    pub repo_handle: String,
    pub file_path: String,
    pub language: Option<String>,
    pub size_bytes: i64,
    pub indexed_commit_sha: String,
    pub state: String,
}

#[derive(Debug, Serialize)]
pub struct ChunkSummary {
    pub handle: String,
    pub chunk_index: i32,
    pub chunk_type: String,
    pub line_range: (i64, i64),
    pub snippet: String,
}

#[derive(Debug)]
pub struct CodeOpenFileRevisionTool;

impl McpTool for CodeOpenFileRevisionTool {
    const NAME: &'static str = "proxima-code/code_open_file_revision";
    const DESCRIPTION: &'static str =
        "Return the current head revision and head chunks for one repo_handle/file_path pair.";

    type Args = CodeOpenFileRevisionArgs;
    type Output = CodeOpenFileRevisionOutput;

    fn call(
        ctx: McpToolCtx,
        args: CodeOpenFileRevisionArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeOpenFileRevisionOutput, McpToolError>> {
        Box::pin(async move {
            if args.file_path.trim().is_empty() {
                return Err(McpToolError::InvalidInput("file_path required".into()));
            }
            let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
            let repo_id = ctx
                .handles
                .resolve_flavor_object(&args.repo_handle, super::REPO_HANDLE_KIND)
                .ok_or_else(|| McpToolError::UnknownHandle(args.repo_handle.clone()))?;

            let revision_sql = format!(
                "WITH {FILE_REVISION_HEADS_CTE}
                 SELECT memory_id, repo_id, file_path, language, size_bytes,
                        indexed_commit_sha, state
                 FROM file_revision_heads
                 WHERE repo_id = $3 AND file_path = $4"
            );
            let revision = sqlx::query_as::<_, RevisionRow>(&revision_sql)
                .bind(owner_kind)
                .bind(owner_principal_id)
                .bind(repo_id)
                .bind(&args.file_path)
                .fetch_optional(&ctx.pool)
                .await
                .map_err(map_storage)?
                .map(|row| {
                    let handle = ctx.handles.assign_memory(MemoryId::new(row.memory_id));
                    let repo_handle = ctx.handles.assign_flavor_object(
                        super::REPO_HANDLE_KIND,
                        row.repo_id,
                        super::REPO_HANDLE_PREFIX,
                    );
                    FileRevisionInfo {
                        handle: handle.as_str().to_string(),
                        repo_handle: repo_handle.as_str().to_string(),
                        file_path: row.file_path,
                        language: row.language,
                        size_bytes: row.size_bytes,
                        indexed_commit_sha: row.indexed_commit_sha,
                        state: row.state,
                    }
                });

            let chunk_sql = format!(
                "WITH {CHUNK_HEADS_CTE}
                 SELECT memory_id, chunk_index, chunk_type,
                        line_range_start, line_range_end,
                        left(text, 480) AS snippet
                 FROM chunk_heads
                 WHERE repo_id = $3 AND file_path = $4
                 ORDER BY chunk_index ASC"
            );
            let chunk_rows: Vec<ChunkSummaryRow> = sqlx::query_as(&chunk_sql)
                .bind(owner_kind)
                .bind(owner_principal_id)
                .bind(repo_id)
                .bind(&args.file_path)
                .fetch_all(&ctx.pool)
                .await
                .map_err(map_storage)?;

            let chunks = chunk_rows
                .into_iter()
                .map(|row| {
                    let handle = ctx.handles.assign_memory(MemoryId::new(row.memory_id));
                    ChunkSummary {
                        handle: handle.as_str().to_string(),
                        chunk_index: row.chunk_index,
                        chunk_type: row.chunk_type,
                        line_range: (row.line_range_start, row.line_range_end),
                        snippet: row.snippet,
                    }
                })
                .collect();

            Ok(CodeOpenFileRevisionOutput { revision, chunks })
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct RevisionRow {
    memory_id: uuid::Uuid,
    repo_id: uuid::Uuid,
    file_path: String,
    language: Option<String>,
    size_bytes: i64,
    indexed_commit_sha: String,
    state: String,
}

#[derive(Debug, sqlx::FromRow)]
struct ChunkSummaryRow {
    memory_id: uuid::Uuid,
    chunk_index: i32,
    chunk_type: String,
    line_range_start: i64,
    line_range_end: i64,
    snippet: String,
}
