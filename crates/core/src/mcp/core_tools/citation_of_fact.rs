//! `core/citation_of_fact` — owner-scoped Fact-to-citation read-back.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::FactEntityId;
use crate::engine::{EntityHeadCitationReadRequest, FactCitationReadRequest};
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CitationOfFactArgs {
    /// Fact memory reference: `F:<uuid>`.
    pub fact: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CitationOfEntityHeadArgs {
    /// Stable `fact_entity_id` UUID.
    pub fact_entity_id: String,
}

#[derive(Debug, Serialize)]
pub struct CitationOfFactOutput {
    pub fact: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citation: Option<FactCitationOutput>,
}

#[derive(Debug, Serialize)]
pub struct CitationOfEntityHeadOutput {
    pub fact_entity_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citation: Option<FactCitationOutput>,
}

#[derive(Debug, Serialize)]
pub struct FactCitationOutput {
    pub citation_mapping_id: String,
    pub mapping_schema_id: String,
    pub cited_object_id: String,
    pub cited_object_schema_id: String,
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

pub(super) async fn citation_of_entity_head(
    ctx: McpToolCtx,
    args: CitationOfEntityHeadArgs,
) -> Result<CitationOfEntityHeadOutput, McpToolError> {
    let fact_entity_uuid = args
        .fact_entity_id
        .parse::<uuid::Uuid>()
        .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}")))?;
    let fact_entity_id = FactEntityId::new(fact_entity_uuid);
    let engine = ctx.require_engine()?;
    let citation = engine
        .read_entity_head_citation(
            &ctx.authz,
            &EntityHeadCitationReadRequest { fact_entity_id },
        )
        .await?
        .map(|row| fact_citation_output(&row));
    Ok(CitationOfEntityHeadOutput {
        fact_entity_id: fact_entity_uuid.to_string(),
        citation,
    })
}

fn fact_citation_output(row: &crate::verbs::query::FactCitationReadback) -> FactCitationOutput {
    FactCitationOutput {
        citation_mapping_id: row.citation_mapping_id.to_string(),
        mapping_schema_id: row.mapping_schema_id.as_str().to_string(),
        cited_object_id: row.cited_object_id.to_string(),
        cited_object_schema_id: row.cited_object_schema_id.as_str().to_string(),
    }
}
