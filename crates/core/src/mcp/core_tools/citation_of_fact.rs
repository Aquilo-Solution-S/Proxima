//! `core/citation_of_fact` — owner-scoped Fact-to-citation read-back.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpToolCtx, McpToolError};
use crate::{FactEntityId, MemoryAction};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CitationOfFactArgs {
    /// Fact memory reference in the ctx output mode: `F:<uuid>`, raw uuid, or handle.
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
    if !ctx
        .authz
        .allows_memory_action(&ctx.owner, MemoryAction::Read)
    {
        return Err(crate::error::ProtocolError::forbidden("requires memory.read on owner").into());
    }
    let fact_memory_id = ctx.resolve_fact_memory(&args.fact)?;
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let citation = storage
        .citation_of_fact(&ctx.owner, fact_memory_id)
        .await?
        .map(|row| FactCitationOutput {
            citation_mapping_id: row.citation_mapping_id.to_string(),
            mapping_schema_id: row.mapping_schema_id.as_str().to_string(),
            cited_object_id: row.cited_object_id.to_string(),
            cited_object_schema_id: row.cited_object_schema_id.as_str().to_string(),
        });
    Ok(CitationOfFactOutput {
        fact: ctx.format_fact_memory(fact_memory_id),
        citation,
    })
}

pub(super) async fn citation_of_entity_head(
    ctx: McpToolCtx,
    args: CitationOfEntityHeadArgs,
) -> Result<CitationOfEntityHeadOutput, McpToolError> {
    if !ctx
        .authz
        .allows_memory_action(&ctx.owner, MemoryAction::Read)
    {
        return Err(crate::error::ProtocolError::forbidden("requires memory.read on owner").into());
    }
    let fact_entity_uuid = args
        .fact_entity_id
        .parse::<uuid::Uuid>()
        .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}")))?;
    let fact_entity_id = FactEntityId::new(fact_entity_uuid);
    let storage = ctx
        .storage()
        .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
    let citation = storage
        .citation_of_entity_head(&ctx.owner, fact_entity_id)
        .await?
        .map(|row| FactCitationOutput {
            citation_mapping_id: row.citation_mapping_id.to_string(),
            mapping_schema_id: row.mapping_schema_id.as_str().to_string(),
            cited_object_id: row.cited_object_id.to_string(),
            cited_object_schema_id: row.cited_object_schema_id.as_str().to_string(),
        });
    Ok(CitationOfEntityHeadOutput {
        fact_entity_id: fact_entity_uuid.to_string(),
        citation,
    })
}
