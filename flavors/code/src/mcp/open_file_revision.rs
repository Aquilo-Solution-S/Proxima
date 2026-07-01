use proxima_core::{CORE_DERIVED_FROM_RELATION, Tool, ToolCtx, ToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::payloads::{CodeChunkV1, FileRevisionV1, FileState};

use super::CodeToolCtxExt;
use super::code_store;
use super::sql::{map_storage, owner_columns, resolve_repo_identifier};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeOpenFileRevisionArgs {
    #[schemars(description = "Repo handle from code search output, typically `R...`.")]
    pub repo_handle: String,
    #[schemars(
        description = "Repo-relative file path to open at the current indexed head revision."
    )]
    pub file_path: String,
    #[serde(default)]
    #[schemars(
        description = "Whether to include chunk text in the response. Defaults to false, but line/text limits imply true."
    )]
    pub include_text: bool,
    #[schemars(
        description = "Optional 1-based starting line for a text window. Omit or null to return chunk snippets only."
    )]
    pub line_start: Option<i64>,
    #[schemars(
        description = "Optional maximum number of lines from `line_start`. Omit or null when no line window is needed."
    )]
    pub line_limit: Option<i64>,
    #[schemars(
        description = "Optional cap on returned text bytes per chunk. Omit or null to use the default projection."
    )]
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
    pub state: FileState,
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

impl Tool for CodeOpenFileRevisionTool {
    const NAME: &'static str = "proxima-code_open_file_revision";
    const DESCRIPTION: &'static str =
        "Return the current head revision and head chunks for one repo_handle/file_path pair.";

    type Args = CodeOpenFileRevisionArgs;
    type Output = CodeOpenFileRevisionOutput;

    #[allow(clippy::too_many_lines)]
    fn call(
        ctx: ToolCtx,
        args: CodeOpenFileRevisionArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeOpenFileRevisionOutput, ToolError>> {
        Box::pin(async move {
            if args.file_path.trim().is_empty() {
                return Err(ToolError::InvalidInput("file_path required".into()));
            }
            let line_window = requested_line_window(args.line_start, args.line_limit)?;
            let include_text =
                args.include_text || line_window.is_some() || args.max_text_bytes.is_some();
            let repo_id = resolve_repo_identifier(&ctx, &args.repo_handle).await?;
            let pool = code_store(&ctx)?;
            let engine = super::engine(&ctx)?;
            let (owner_kind, owner_id) = owner_columns(&ctx.owner());

            let revision_candidates: Vec<MemoryCandidateRow> = sqlx::query_as(
                "SELECT fr.memory_id
                   FROM proxima_code.file_revision_v1 fr
                   JOIN proxima_core.memories m USING (memory_id)
                   LEFT JOIN proxima_core.fact_receipts r USING (receipt_id)
                  WHERE fr.repo_id = $1
                    AND fr.file_path = $2
                    AND m.owner_kind = $3
                    AND m.owner_id IS NOT DISTINCT FROM $4
                  ORDER BY r.source_batch_id DESC NULLS LAST, m.created_at DESC
                  LIMIT 200",
            )
            .bind(repo_id)
            .bind(&args.file_path)
            .bind(owner_kind)
            .bind(owner_id)
            .fetch_all(pool.pool())
            .await
            .map_err(map_storage)?;
            let revision_ids = revision_candidates
                .iter()
                .map(|row| row.memory_id)
                .collect::<Vec<_>>();
            let revision_with_id = pool
                .authorized_fact_payloads_include_tombstones::<FileRevisionV1>(
                    &engine,
                    ctx.authz(),
                    ctx.owner(),
                    &revision_ids,
                    1,
                )
                .await?
                .into_iter()
                .find(|(_, row)| row.repo_id == repo_id && row.file_path == args.file_path)
                .map(|(memory_id, row)| {
                    let size_bytes = i64::try_from(row.size_bytes)
                        .map_err(|_| ToolError::Other("file revision size exceeds i64".into()))?;
                    Ok::<_, ToolError>((
                        memory_id,
                        FileRevisionInfo {
                            handle: ctx.format_fact_memory(memory_id),
                            repo_handle: ctx.format_flavor_object(
                                super::REPO_HANDLE_KIND,
                                row.repo_id,
                                super::REPO_HANDLE_PREFIX,
                            ),
                            file_path: row.file_path,
                            language: row.language,
                            size_bytes,
                            indexed_commit_sha: row.indexed_commit_sha,
                            state: row.state,
                        },
                    ))
                })
                .transpose()?;
            let (revision_memory_id, revision) = match revision_with_id {
                Some((memory_id, revision)) => (Some(memory_id), Some(revision)),
                None => (None, None),
            };

            if !matches!(
                revision.as_ref().map(|row| row.state),
                Some(FileState::Present)
            ) {
                return Ok(CodeOpenFileRevisionOutput {
                    revision,
                    chunks: Vec::new(),
                });
            }

            let revision_memory_id = revision_memory_id
                .ok_or_else(|| ToolError::Other("authorized revision disappeared".into()))?;
            let chunk_rows: Vec<ChunkCandidateRow> = sqlx::query_as(
                "SELECT c.memory_id
                   FROM proxima_code.code_chunk_v1 c
                   JOIN proxima_core.memories m USING (memory_id)
                   JOIN proxima_core.edges e
                     ON e.source_memory_id = c.memory_id
                    AND e.target_memory_id = $5
                    AND e.relation = $6
                   JOIN (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
                     ON eo.entity_id = m.memory_id
                  WHERE eo.owner_kind = $1
                    AND eo.owner_id IS NOT DISTINCT FROM $2
                    AND c.repo_id = $3
                    AND c.file_path = $4
                    AND c.state = 'Present'
                    AND NOT EXISTS (
                        SELECT 1
                          FROM proxima_core.memories m2
                          JOIN proxima_code.code_chunk_v1 c2 USING (memory_id)
                         WHERE m2.schema_id = m.schema_id
                           AND m2.owner_kind = m.owner_kind
                           AND m2.owner_id IS NOT DISTINCT FROM m.owner_id
                           AND c2.repo_id = c.repo_id
                           AND c2.file_path = c.file_path
                           AND c2.chunk_index = c.chunk_index
                           AND m2.source_batch_id > m.source_batch_id
                    )
                    AND (
                        $7::bigint IS NULL
                        OR (c.line_range_end >= $7 AND c.line_range_start <= $8)
                    )
                  ORDER BY c.chunk_index ASC
                  LIMIT 2000",
            )
            .bind(owner_kind)
            .bind(owner_id)
            .bind(repo_id)
            .bind(&args.file_path)
            .bind(revision_memory_id.into_inner())
            .bind(CORE_DERIVED_FROM_RELATION)
            .bind(line_window.map(|window| window.0))
            .bind(line_window.map(|window| window.1))
            .fetch_all(pool.pool())
            .await
            .map_err(map_storage)?;
            let chunk_ids = chunk_rows
                .iter()
                .map(|row| row.memory_id)
                .collect::<Vec<_>>();
            let mut chunks = pool
                .authorized_abstraction_payloads::<CodeChunkV1>(
                    &engine,
                    ctx.authz(),
                    ctx.owner(),
                    &chunk_ids,
                    2_000,
                )
                .await?
                .into_iter()
                .filter(|(_, row)| {
                    row.repo_id == repo_id
                        && row.file_path == args.file_path
                        && row.state == FileState::Present
                })
                .map(|(memory_id, row)| {
                    let (text, text_line_range) = project_text(
                        include_text.then_some(row.text.clone()),
                        i64::from(row.line_range_start),
                        line_window,
                        args.max_text_bytes,
                    );
                    Ok::<_, ToolError>(ChunkSummary {
                        handle: ctx.format_abstraction_memory(memory_id),
                        chunk_index: i32::try_from(row.chunk_index)
                            .map_err(|_| ToolError::Other("chunk_index exceeds i32".into()))?,
                        chunk_type: row.chunk_type,
                        line_range: (
                            i64::from(row.line_range_start),
                            i64::from(row.line_range_end),
                        ),
                        snippet: row.text.chars().take(480).collect(),
                        text,
                        text_line_range,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            chunks.sort_by_key(|chunk| chunk.chunk_index);

            Ok(CodeOpenFileRevisionOutput { revision, chunks })
        })
    }
}

fn requested_line_window(
    line_start: Option<i64>,
    line_limit: Option<i64>,
) -> Result<Option<(i64, i64)>, ToolError> {
    if line_start.is_none() && line_limit.is_none() {
        return Ok(None);
    }
    let start = line_start.unwrap_or(1);
    let limit = line_limit.unwrap_or(120);
    if start < 1 {
        return Err(ToolError::InvalidInput("line_start must be >= 1".into()));
    }
    if !(1..=500).contains(&limit) {
        return Err(ToolError::InvalidInput("line_limit must be 1..=500".into()));
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
struct MemoryCandidateRow {
    memory_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
struct ChunkCandidateRow {
    memory_id: uuid::Uuid,
}
