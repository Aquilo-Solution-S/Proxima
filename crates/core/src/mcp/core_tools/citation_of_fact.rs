//! `core/citation_of_fact` — owner-scoped Fact-to-citation read-back.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::FactCitationReadRequest;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CitationOfFactArgs {
    /// Fact memory reference: `F:<uuid>`.
    pub fact: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CitationOfFactOutput {
    pub fact: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citation: Option<FactCitationOutput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FactCitationOutput {
    pub citation_mapping_id: String,
    pub mapping_schema_id: String,
    pub cited_object_id: String,
    pub cited_object_schema_id: String,
    /// The `core/uploaded-blob-page-span-v1` locator, when the mapping
    /// carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_span: Option<crate::citations::UploadedBlobPageSpanV1>,
    /// Uploaded-document metadata, when the cited object is a
    /// `core/uploaded-blob-v1`. Never carries storage coordinates —
    /// fetch bytes via `core_upload` `read_url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<UploadedBlobDocumentOutput>,
}

/// What the cited document IS (name, type, size, content hash, upload
/// time) — deliberately not where it lives.
#[derive(Debug, Serialize, JsonSchema)]
pub struct UploadedBlobDocumentOutput {
    pub filename: String,
    pub mime: String,
    pub byte_len: u64,
    pub sha256_hex: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub uploaded_at: time::OffsetDateTime,
}

pub(super) async fn citation_of_fact(
    ctx: McpToolCtx,
    args: CitationOfFactArgs,
) -> Result<CitationOfFactOutput, McpToolError> {
    let fact_memory_id = ctx.resolve_fact_memory(&args.fact)?;
    let engine = ctx.require_engine()?;
    let citation = engine
        .read_fact_citation(&ctx.authz, &FactCitationReadRequest { fact_memory_id })
        .await?
        .map(|row| fact_citation_output(&row));
    Ok(CitationOfFactOutput {
        fact: ctx.format_fact_memory(fact_memory_id),
        citation,
    })
}

fn fact_citation_output(row: &crate::verbs::query::FactCitationReadback) -> FactCitationOutput {
    FactCitationOutput {
        citation_mapping_id: row.citation_mapping_id.to_string(),
        mapping_schema_id: row.mapping_schema_id.as_str().to_string(),
        cited_object_id: row.cited_object_id.to_string(),
        cited_object_schema_id: row.cited_object_schema_id.as_str().to_string(),
        page_span: row.page_span,
        document: row
            .uploaded_blob
            .as_ref()
            .map(|blob| UploadedBlobDocumentOutput {
                filename: blob.filename.clone(),
                mime: blob.mime.clone(),
                byte_len: blob.byte_len,
                sha256_hex: blob.sha256_hex.clone(),
                uploaded_at: blob.uploaded_at,
            }),
    }
}
