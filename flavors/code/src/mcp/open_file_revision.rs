use proxima_core::verbs::query::{EdgeFilter, EdgeReadRequest};
use proxima_core::{CORE_DERIVED_FROM_RELATION, EntityRef, Tool, ToolCtx, ToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::payloads::{CodeChunkV1, FileRevisionV1, FileState};

use super::CodeToolCtxExt;
use super::code_store;
use super::sql::{map_storage, resolve_repo_identifier};

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
        description = "Optional 1-based starting line for a text window. Must be >= 1. Omit or null to return chunk snippets only."
    )]
    pub line_start: Option<i64>,
    #[schemars(
        description = "Optional maximum number of lines from `line_start`; values above 500 are clamped, 0 is rejected, default 120. Omit or null when no line window is needed."
    )]
    pub line_limit: Option<i64>,
    #[schemars(
        description = "Optional cap on returned text bytes per chunk, at least 1; a cut chunk is flagged text_truncated=true. Omit or null to use the default projection, and pass include_text=false to skip text entirely."
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
    /// `true` when `max_text_bytes` cut this chunk's text. Its two siblings
    /// have always said so — `core_search_memories` with `body_truncated`,
    /// `proxima-code_search_chunks` with `snippet_truncated` — and without
    /// it a caller cannot tell a chunk that ends there from one that was
    /// cut off mid-statement.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub text_truncated: bool,
}

#[derive(Debug)]
pub struct CodeOpenFileRevisionTool;

impl Tool for CodeOpenFileRevisionTool {
    const NAME: &'static str = "proxima-code_open_file_revision";
    const DESCRIPTION: &'static str =
        "Return the current head revision and head chunks for one repo_handle/file_path pair.";
    const ANNOTATIONS: Option<proxima_core::mcp::McpToolAnnotations> = Some(super::READ_ONLY);

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
            // A zero cap used to be accepted and answered with `text: ""` on
            // every chunk — a well-formed response indistinguishable from an
            // empty file, and it silently turned text ON (`want_text` below
            // keys off `max_text_bytes.is_some()`) only to blank it. Both
            // sibling tools already refuse zero here.
            if args.max_text_bytes == Some(0) {
                return Err(ToolError::InvalidInput(
                    "max_text_bytes must be >= 1 when provided; use include_text=false to skip text"
                        .into(),
                ));
            }
            let line_window = requested_line_window(args.line_start, args.line_limit)?;
            let include_text =
                args.include_text || line_window.is_some() || args.max_text_bytes.is_some();
            let repo_id = resolve_repo_identifier(&ctx, &args.repo_handle).await?;
            let pool = code_store(&ctx)?;
            let engine = super::engine(&ctx)?;

            // Sidecar-only candidate scan (no owner filter — `memory_id` is a
            // UUIDv7 for Fact rows, so ordering by it is a valid recency
            // proxy without touching `proxima_core.*`). The authorized fetch
            // below resolves the true owner-or-World head via
            // `FileRevisionV1::natural_key_columns` heads-only supersession.
            let revision_candidates: Vec<MemoryCandidateRow> = sqlx::query_as(
                "SELECT fr.memory_id
                   FROM proxima_code.file_revision_v1 fr
                  WHERE fr.repo_id = $1
                    AND fr.file_path = $2
                  ORDER BY fr.memory_id DESC
                  LIMIT 200",
            )
            .bind(repo_id)
            .bind(&args.file_path)
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

            // The current head file revision's own `derived-from` in-edges
            // are exactly its current chunk set (each commit's F->A pass
            // re-derives the full chunk set for every file it touches), so
            // no separate core-table dedup is needed here — restricting to
            // this one authorized revision id is precise on its own.
            let derived_edges = engine
                .read_edges(
                    ctx.authz(),
                    &EdgeReadRequest {
                        owner: ctx.owner(),
                        edge_ids: Vec::new(),
                        filter: EdgeFilter {
                            relation: Some(CORE_DERIVED_FROM_RELATION.to_string()),
                            source: None,
                            target: Some(EntityRef::Memory(revision_memory_id)),
                        },
                        limit: 2_000,
                        cursor: None,
                        include_payloads: false,
                    },
                )
                .await?;
            let chunk_ids = derived_edges
                .edges
                .into_iter()
                .filter_map(|edge| match edge.source {
                    EntityRef::Memory(id) => Some(id.into_inner()),
                    EntityRef::Goal(_) | EntityRef::FactEntity(_) => None,
                })
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
                    let in_line_window = match line_window {
                        Some((start, end)) => {
                            i64::from(row.line_range_end) >= start
                                && i64::from(row.line_range_start) <= end
                        }
                        None => true,
                    };
                    row.repo_id == repo_id
                        && row.file_path == args.file_path
                        && row.state == FileState::Present
                        && in_line_window
                })
                .map(|(memory_id, row)| {
                    let projected = project_text(
                        include_text.then_some(row.text.clone()),
                        i64::from(row.line_range_start),
                        line_window,
                        args.max_text_bytes,
                    );
                    let ProjectedText {
                        text,
                        text_line_range,
                        text_truncated,
                    } = projected;
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
                        text_truncated,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            chunks.sort_by_key(|chunk| chunk.chunk_index);

            Ok(CodeOpenFileRevisionOutput { revision, chunks })
        })
    }
}

/// Default window height when only `line_start` is given.
const DEFAULT_LINE_LIMIT: i64 = 120;
/// Ceiling on `line_limit`. Reject at or below zero, clamp above — the
/// substrate's rule for every other bound, and the one this used to break
/// by refusing 501 outright. The response reports the span actually
/// returned, so a clamped caller is told rather than quietly shortchanged.
const MAX_LINE_LIMIT: i64 = 500;

fn requested_line_window(
    line_start: Option<i64>,
    line_limit: Option<i64>,
) -> Result<Option<(i64, i64)>, ToolError> {
    if line_start.is_none() && line_limit.is_none() {
        return Ok(None);
    }
    let start = line_start.unwrap_or(1);
    let limit = line_limit.unwrap_or(DEFAULT_LINE_LIMIT);
    if start < 1 {
        return Err(ToolError::InvalidInput("line_start must be >= 1".into()));
    }
    if limit < 1 {
        return Err(ToolError::InvalidInput(
            "line_limit must be at least 1".into(),
        ));
    }
    let limit = limit.min(MAX_LINE_LIMIT);
    Ok(Some((start, start.saturating_add(limit - 1))))
}

/// One chunk's projected text and what the projection did to it.
struct ProjectedText {
    text: Option<String>,
    text_line_range: Option<(i64, i64)>,
    text_truncated: bool,
}

fn project_text(
    text: Option<String>,
    chunk_line_start: i64,
    line_window: Option<(i64, i64)>,
    max_text_bytes: Option<usize>,
) -> ProjectedText {
    let Some(text) = text else {
        return ProjectedText {
            text: None,
            text_line_range: None,
            text_truncated: false,
        };
    };
    let Some((window_start, window_end)) = line_window else {
        let (text, truncated) = truncate_utf8_bytes(text, max_text_bytes);
        return ProjectedText {
            text: Some(text),
            text_line_range: None,
            text_truncated: truncated,
        };
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
        return ProjectedText {
            text: None,
            text_line_range: None,
            text_truncated: false,
        };
    };
    let requested_end = selected_end.unwrap_or(start);
    let (projected, truncated) = truncate_utf8_bytes(selected.join("\n"), max_text_bytes);
    // Report the span actually returned, not the span asked for.
    // `max_text_bytes` drops trailing lines, and reporting the pre-truncation
    // end makes the response claim text it did not send — a caller that trusts
    // `text_line_range` to place the snippet in the file is then wrong about
    // every line after the cut. A line cut mid-way still counts: part of it
    // was returned.
    let returned_end = if projected.is_empty() {
        start
    } else {
        let lines = i64::try_from(projected.lines().count()).unwrap_or(i64::MAX);
        start
            .saturating_add(lines.saturating_sub(1))
            .min(requested_end)
    };
    ProjectedText {
        text: Some(projected),
        text_line_range: Some((start, returned_end)),
        text_truncated: truncated,
    }
}

/// Cut `text` to at most `max_text_bytes` on a char boundary. Returns
/// whether anything was actually dropped, which is what the caller reports
/// as `text_truncated`.
fn truncate_utf8_bytes(text: String, max_text_bytes: Option<usize>) -> (String, bool) {
    let Some(max) = max_text_bytes else {
        return (text, false);
    };
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    (text[..end].to_string(), true)
}

#[derive(Debug, sqlx::FromRow)]
struct MemoryCandidateRow {
    memory_id: uuid::Uuid,
}

#[cfg(test)]
mod tests {
    use super::{project_text, requested_line_window};

    const SRC: &str = "line ten\nline eleven\nline twelve\nline thirteen\nline fourteen";

    #[test]
    fn a_whole_window_reports_the_whole_window() {
        let projected = project_text(Some(SRC.to_string()), 10, Some((10, 14)), None);
        assert_eq!(projected.text.as_deref(), Some(SRC));
        assert_eq!(projected.text_line_range, Some((10, 14)));
        assert!(!projected.text_truncated);
    }

    /// The reported span must describe what was sent. Before this, a
    /// byte-truncated window still reported the span that was *asked* for, so
    /// a caller placing the snippet in the file was wrong about every line
    /// after the cut.
    #[test]
    fn a_truncated_window_reports_only_the_lines_it_returned() {
        // Enough bytes for the first two lines and part of the third.
        let projected = project_text(Some(SRC.to_string()), 10, Some((10, 14)), Some(24));
        let text = projected.text.expect("text");
        assert!(text.len() <= 24);
        let (start, end) = projected.text_line_range.expect("range");
        assert_eq!(start, 10);
        assert_eq!(
            end,
            10 + i64::try_from(text.lines().count()).expect("fits") - 1,
            "reported end must match the lines actually returned: {text:?}"
        );
        assert!(end < 14, "must not claim the untruncated end");
        assert!(projected.text_truncated, "a cut chunk must say it was cut");
    }

    #[test]
    fn the_reported_end_never_exceeds_the_requested_window() {
        let projected = project_text(Some(SRC.to_string()), 10, Some((10, 11)), None);
        assert_eq!(projected.text_line_range, Some((10, 11)));
    }

    #[test]
    fn a_window_matching_no_line_returns_nothing() {
        let projected = project_text(Some(SRC.to_string()), 10, Some((99, 120)), None);
        assert!(projected.text.is_none());
        assert!(projected.text_line_range.is_none());
        assert!(!projected.text_truncated);
    }

    /// A cap that fits is not a truncation. The flag has to distinguish
    /// "this is the whole chunk" from "this is where I stopped", which is
    /// the whole reason it exists.
    #[test]
    fn a_cap_larger_than_the_text_is_not_a_truncation() {
        let projected = project_text(Some(SRC.to_string()), 10, None, Some(SRC.len()));
        assert_eq!(projected.text.as_deref(), Some(SRC));
        assert!(!projected.text_truncated);

        let projected = project_text(Some(SRC.to_string()), 10, None, Some(SRC.len() - 1));
        assert!(projected.text_truncated);
    }

    /// Cutting mid-character must not split a codepoint, and must still be
    /// reported as a cut.
    #[test]
    fn a_cap_falling_inside_a_multibyte_char_backs_off_to_a_boundary() {
        let text = "äöü".to_string(); // three 2-byte chars
        let projected = project_text(Some(text), 1, None, Some(3));
        let out = projected.text.expect("text");
        assert_eq!(out, "ä", "must back off to a char boundary, got {out:?}");
        assert!(projected.text_truncated);
    }

    /// `line_limit` follows the substrate's rule for every other bound:
    /// reject at zero, clamp above the ceiling. It used to refuse 501
    /// outright, which is the one shape no other paged read uses.
    #[test]
    fn the_line_limit_rejects_zero_and_clamps_high() {
        assert!(requested_line_window(Some(1), Some(0)).is_err());
        assert!(requested_line_window(Some(1), Some(-5)).is_err());
        assert!(requested_line_window(Some(0), Some(5)).is_err());

        assert_eq!(
            requested_line_window(Some(1), Some(500)).unwrap(),
            Some((1, 500))
        );
        assert_eq!(
            requested_line_window(Some(1), Some(501)).unwrap(),
            Some((1, 500)),
            "past the ceiling is clamped, not refused"
        );
        assert_eq!(
            requested_line_window(Some(1), Some(i64::MAX)).unwrap(),
            Some((1, 500)),
        );
        // Neither bound given is not a window at all.
        assert_eq!(requested_line_window(None, None).unwrap(), None);
        // `line_start` alone takes the default height.
        assert_eq!(
            requested_line_window(Some(10), None).unwrap(),
            Some((10, 129))
        );
    }
}
