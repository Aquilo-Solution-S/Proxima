//! Code's first F→A operator: per-commit-batch summary.
//!
//! Input: one commit Fact + N file-revision Facts + M code-chunk Facts
//! sharing one closed `source_batch_id` (per docs/01 §"The contract"
//! and the M4 per-commit refactor — one batch = one commit).
//!
//! Output: a single `CommitSummaryV1` Abstraction whose provenance set
//! is every Fact in the input batch. Empty input (e.g. a closed batch
//! that emitted no commit Fact — should not happen for `LocalGitSource`)
//! returns an empty output set, per the doc-04 rule that operators
//! never force a synthesis.

use async_trait::async_trait;
use proxima_core::operators::{F2AContext, F2AOperator, FactRow, NewAbstraction, OperatorError};
use proxima_core::{AbstractionPayload, FactPayload, SchemaId, SchemaVersion};
use serde::Deserialize;

use crate::payloads::{CodeChunkV1, CommitSummaryV1, CommitV1, FileRevisionV1, FileState};

const SYSTEM_PROMPT: &str = "You are a precise code-change summarizer. \
Given a single git commit with its changed files and a sample of \
its code chunks, produce a JSON object describing the commit. \
Output ONLY the JSON object — no markdown, no preamble, no trailing text. \
Schema: {\"summary\": string (1 to 3 sentences explaining what the commit \
does and why), \"key_files\": string[] (the most relevant changed paths, \
ordered by importance, max 5), \"change_kind\": string (lowercase, one of: \
\"feature\", \"fix\", \"refactor\", \"docs\", \"test\", \"chore\", \"other\")}.";

const PROMPT_VERSION: &str = "v1";
const OPERATOR_ID: &str = "proxima-code/commit-summary";
const MAX_CHUNK_EXCERPT_BYTES: usize = 800;
const MAX_CHUNKS_IN_PROMPT: usize = 12;

#[derive(Debug, Deserialize)]
struct LlmOutput {
    summary: String,
    #[serde(default)]
    key_files: Vec<String>,
    #[serde(default = "default_change_kind")]
    change_kind: String,
}

fn default_change_kind() -> String {
    "other".to_string()
}

#[derive(Debug, Default, Clone)]
pub struct CommitSummaryOperator;

impl CommitSummaryOperator {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl F2AOperator for CommitSummaryOperator {
    fn operator_id(&self) -> &'static str {
        OPERATOR_ID
    }

    fn output_schema_id(&self) -> &'static str {
        CommitSummaryV1::SCHEMA_ID
    }

    fn output_schema_version(&self) -> u32 {
        CommitSummaryV1::SCHEMA_VERSION
    }

    fn prompt_version(&self) -> &'static str {
        PROMPT_VERSION
    }

    fn consumes(&self, schema_id: &SchemaId) -> bool {
        matches!(
            schema_id.as_str(),
            <CommitV1 as FactPayload>::SCHEMA_ID
                | <FileRevisionV1 as FactPayload>::SCHEMA_ID
                | <CodeChunkV1 as FactPayload>::SCHEMA_ID
        )
    }

    async fn run(&self, ctx: F2AContext<'_>) -> Result<Vec<NewAbstraction>, OperatorError> {
        let Some((commit, commit_memory_id)) = find_commit(ctx.facts)? else {
            // No commit Fact in the batch — nothing to summarise.
            // Per doc 04, F→A may legitimately return empty.
            return Ok(Vec::new());
        };

        let revisions = collect_revisions(ctx.facts)?;
        let chunks = collect_chunks(ctx.facts)?;

        let user_prompt = render_user_prompt(&commit, &revisions, &chunks);
        tracing::debug!(
            operator = OPERATOR_ID,
            commit_sha = %commit.sha,
            n_revisions = revisions.len(),
            n_chunks = chunks.len(),
            prompt_len = user_prompt.len(),
            "running F→A"
        );

        let raw = ctx.llm.complete_json(SYSTEM_PROMPT, &user_prompt).await?;
        let parsed: LlmOutput = serde_json::from_value(raw.clone()).map_err(|e| {
            OperatorError::OutputValidation(format!(
                "LLM output failed schema decode: {e}; raw: {raw}"
            ))
        })?;

        let change_kind = normalize_change_kind(&parsed.change_kind);
        let key_files = sanitize_key_files(parsed.key_files, &revisions);

        let summary_for_text = parsed.summary.clone();
        let typed = CommitSummaryV1 {
            repo_id: commit.repo_id,
            commit_sha: commit.sha.clone(),
            summary: parsed.summary,
            key_files,
            change_kind,
        };
        let typed_json = serde_json::to_value(&typed)
            .map_err(|e| OperatorError::Internal(format!("serialize CommitSummaryV1: {e}")))?;

        // Embedding text = the rendered narrative. Short and dense —
        // good for similarity recall when A→P lands in M6.
        let embed_text = format!(
            "{} {}\n\n{}",
            short_sha(&commit.sha),
            commit.message.lines().next().unwrap_or(""),
            summary_for_text
        );
        let embedding = ctx.embed.embed(&embed_text).await?;
        let embedding_model_id = ctx.embed.model_id().to_string();

        // Provenance = every Fact in the operator's filtered input set.
        let mut provenance = Vec::with_capacity(ctx.facts.len());
        provenance.push(commit_memory_id);
        for (_, mid) in &revisions {
            provenance.push(*mid);
        }
        for (_, mid) in &chunks {
            provenance.push(*mid);
        }

        Ok(vec![NewAbstraction {
            schema_id: SchemaId::new(CommitSummaryV1::SCHEMA_ID.to_string()),
            schema_version: SchemaVersion::new(CommitSummaryV1::SCHEMA_VERSION),
            text: summary_for_text,
            typed_payload: typed_json,
            provenance,
            embedding,
            embedding_model_id,
        }])
    }
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn find_commit(
    facts: &[FactRow],
) -> Result<Option<(CommitV1, proxima_core::MemoryId)>, OperatorError> {
    for f in facts {
        if f.schema_id.as_str() == <CommitV1 as FactPayload>::SCHEMA_ID {
            let payload: CommitV1 = serde_json::from_value(f.payload_json.clone())
                .map_err(|e| OperatorError::Internal(format!("decode CommitV1: {e}")))?;
            return Ok(Some((payload, f.memory_id)));
        }
    }
    Ok(None)
}

fn collect_revisions(
    facts: &[FactRow],
) -> Result<Vec<(FileRevisionV1, proxima_core::MemoryId)>, OperatorError> {
    let mut out = Vec::new();
    for f in facts {
        if f.schema_id.as_str() == <FileRevisionV1 as FactPayload>::SCHEMA_ID {
            let payload: FileRevisionV1 = serde_json::from_value(f.payload_json.clone())
                .map_err(|e| OperatorError::Internal(format!("decode FileRevisionV1: {e}")))?;
            out.push((payload, f.memory_id));
        }
    }
    Ok(out)
}

fn collect_chunks(
    facts: &[FactRow],
) -> Result<Vec<(CodeChunkV1, proxima_core::MemoryId)>, OperatorError> {
    let mut out = Vec::new();
    for f in facts {
        if f.schema_id.as_str() == <CodeChunkV1 as FactPayload>::SCHEMA_ID {
            let payload: CodeChunkV1 = serde_json::from_value(f.payload_json.clone())
                .map_err(|e| OperatorError::Internal(format!("decode CodeChunkV1: {e}")))?;
            out.push((payload, f.memory_id));
        }
    }
    Ok(out)
}

fn render_user_prompt(
    commit: &CommitV1,
    revisions: &[(FileRevisionV1, proxima_core::MemoryId)],
    chunks: &[(CodeChunkV1, proxima_core::MemoryId)],
) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    let date = commit
        .committer_time
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| commit.committer_time.to_string());
    let _ = writeln!(
        out,
        "Commit: {} by {} <{}>\nDate: {}\n\nMessage:\n{}\n",
        short_sha(&commit.sha),
        commit.author_name,
        commit.author_email,
        date,
        commit.message.trim_end()
    );

    let _ = writeln!(out, "Changed files ({}):", revisions.len());
    for (rev, _) in revisions {
        let state = match rev.state {
            FileState::Present => "present",
            FileState::Tombstone => "tombstone",
        };
        let lang = rev.language.as_deref().unwrap_or("?");
        let _ = writeln!(
            out,
            " - {} [{state}, {lang}, {} bytes]",
            rev.file_path, rev.size_bytes
        );
    }

    if !chunks.is_empty() {
        let _ = writeln!(out, "\nCode chunks (showing up to {MAX_CHUNKS_IN_PROMPT}):");
        for (chunk, _) in chunks.iter().take(MAX_CHUNKS_IN_PROMPT) {
            let lang = chunk.language.as_deref().unwrap_or("?");
            let _ = writeln!(
                out,
                "\n[{lang}] {}:{}-{} ({})",
                chunk.file_path, chunk.line_range_start, chunk.line_range_end, chunk.chunk_type,
            );
            if chunk.text.len() > MAX_CHUNK_EXCERPT_BYTES {
                out.push_str(&chunk.text[..MAX_CHUNK_EXCERPT_BYTES.min(chunk.text.len())]);
                out.push_str("…(truncated)");
            } else {
                out.push_str(&chunk.text);
            }
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
    }

    out
}

fn normalize_change_kind(raw: &str) -> String {
    let lower = raw.trim().to_lowercase();
    match lower.as_str() {
        "feature" | "fix" | "refactor" | "docs" | "test" | "chore" => lower,
        _ => "other".to_string(),
    }
}

fn sanitize_key_files(
    proposed: Vec<String>,
    revisions: &[(FileRevisionV1, proxima_core::MemoryId)],
) -> Vec<String> {
    let valid: std::collections::HashSet<&str> = revisions
        .iter()
        .map(|(r, _)| r.file_path.as_str())
        .collect();
    proposed
        .into_iter()
        .filter(|p| valid.contains(p.as_str()))
        .take(5)
        .collect()
}
