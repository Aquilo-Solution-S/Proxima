//! `core/citation_of_fact` — owner-scoped Fact-to-citation read-back.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::FactCitationReadRequest;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CitationOfFactArgs {
    /// Fact memory reference: `<uuid>`.
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
    pub cited_object_id: String,
    pub cited_object_schema_id: String,
}

pub(super) async fn citation_of_fact(
    ctx: McpToolCtx,
    args: CitationOfFactArgs,
) -> Result<CitationOfFactOutput, McpToolError> {
    let fact_memory_id = ctx.resolve_memory(&args.fact)?;
    let engine = ctx.require_engine()?;
    let citation = engine
        .read_fact_citation(&ctx.authz, &FactCitationReadRequest { fact_memory_id })
        .await?
        .map(|row| fact_citation_output(&row));
    Ok(CitationOfFactOutput {
        fact: fact_memory_id.into_inner().to_string(),
        citation,
    })
}

fn fact_citation_output(row: &crate::verbs::query::FactCitationReadback) -> FactCitationOutput {
    FactCitationOutput {
        cited_object_id: row.cited_object_id.to_string(),
        cited_object_schema_id: row.cited_object_schema_id.as_str().to_string(),
    }
}
