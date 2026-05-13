use proxima_core::MemoryId;
use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::sql::{
    CHUNK_HEADS_CTE, FILE_REVISION_HEADS_CTE, map_storage, owner_principal, resolve_repo_identifier,
};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeOpenFileRevisionArgs {
    pub repo_handle: String,
    pub file_path: String,
    #[serde(default)]
    pub include_text: bool,
    pub line_start: Option<i64>,
    pub line_limit: Option<i64>,
    pub max_text_bytes: Option<usize>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_line_range: Option<(i64, i64)>,
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
            let line_window = requested_line_window(args.line_start, args.line_limit)?;
            let include_text =
                args.include_text || line_window.is_some() || args.max_text_bytes.is_some();
            let (owner_kind, owner_principal_id) = owner_principal(&ctx.owner);
            let repo_id = resolve_repo_identifier(&ctx, &args.repo_handle).await?;

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
                .map(|row| FileRevisionInfo {
                    handle: ctx.format_memory(MemoryId::new(row.memory_id)),
                    repo_handle: ctx.format_flavor_object(
                        super::REPO_HANDLE_KIND,
                        row.repo_id,
                        super::REPO_HANDLE_PREFIX,
                    ),
                    file_path: row.file_path,
                    language: row.language,
                    size_bytes: row.size_bytes,
                    indexed_commit_sha: row.indexed_commit_sha,
                    state: row.state,
                });

            let chunk_sql = format!(
                "WITH {CHUNK_HEADS_CTE}
                 SELECT memory_id, chunk_index, chunk_type,
                        line_range_start, line_range_end,
                        left(text, 480) AS snippet,
                        CASE WHEN $5::boolean THEN text ELSE NULL END AS text
                 FROM chunk_heads
                 WHERE repo_id = $3 AND file_path = $4
                   AND (
                       $6::bigint IS NULL
                       OR (line_range_end >= $6 AND line_range_start <= $7)
                   )
                 ORDER BY chunk_index ASC"
            );
            let chunk_rows: Vec<ChunkSummaryRow> = sqlx::query_as(&chunk_sql)
                .bind(owner_kind)
                .bind(owner_principal_id)
                .bind(repo_id)
                .bind(&args.file_path)
                .bind(include_text)
                .bind(line_window.map(|window| window.0))
                .bind(line_window.map(|window| window.1))
                .fetch_all(&ctx.pool)
                .await
                .map_err(map_storage)?;

            let chunks = chunk_rows
                .into_iter()
                .map(|row| {
                    let (text, text_line_range) = project_text(
                        row.text,
                        row.line_range_start,
                        line_window,
                        args.max_text_bytes,
                    );
                    ChunkSummary {
                        handle: ctx.format_memory(MemoryId::new(row.memory_id)),
                        chunk_index: row.chunk_index,
                        chunk_type: row.chunk_type,
                        line_range: (row.line_range_start, row.line_range_end),
                        snippet: row.snippet,
                        text,
                        text_line_range,
                    }
                })
                .collect();

            Ok(CodeOpenFileRevisionOutput { revision, chunks })
        })
    }
}

fn requested_line_window(
    line_start: Option<i64>,
    line_limit: Option<i64>,
) -> Result<Option<(i64, i64)>, McpToolError> {
    if line_start.is_none() && line_limit.is_none() {
        return Ok(None);
    }
    let start = line_start.unwrap_or(1);
    let limit = line_limit.unwrap_or(120);
    if start < 1 {
        return Err(McpToolError::InvalidInput("line_start must be >= 1".into()));
    }
    if !(1..=500).contains(&limit) {
        return Err(McpToolError::InvalidInput(
            "line_limit must be 1..=500".into(),
        ));
    }
    Ok(Some((start, start.saturating_add(limit - 1))))
}

fn project_text(
    text: Option<String>,
    chunk_line_start: i64,
    line_window: Option<(i64, i64)>,
    max_text_bytes: Option<usize>,
) -> (Option<String>, Option<(i64, i64)>) {
    let Some(text) = text else {
        return (None, None);
    };
    let Some((window_start, window_end)) = line_window else {
        return (Some(truncate_utf8_bytes(text, max_text_bytes)), None);
    };

    let mut selected = Vec::new();
    let mut selected_start = None;
    let mut selected_end = None;
    for (idx, line) in text.lines().enumerate() {
        let line_no = chunk_line_start + i64::try_from(idx).unwrap_or(i64::MAX);
        if line_no < window_start || line_no > window_end {
            continue;
        }
        selected_start.get_or_insert(line_no);
        selected_end = Some(line_no);
        selected.push(line);
    }
    let Some(start) = selected_start else {
        return (None, None);
    };
    let projected = truncate_utf8_bytes(selected.join("\n"), max_text_bytes);
    (
        Some(projected),
        Some((start, selected_end.unwrap_or(start))),
    )
}

fn truncate_utf8_bytes(text: String, max_text_bytes: Option<usize>) -> String {
    let Some(max) = max_text_bytes else {
        return text;
    };
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text[..end].to_string()
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
    text: Option<String>,
}
