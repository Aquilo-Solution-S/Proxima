//! `core/citation_of_fact` — owner-scoped Fact-to-citation read-back.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct CitationOfFactTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CitationOfFactArgs {
    /// Fact memory reference in the ctx output mode: `F:<uuid>`, raw uuid, or handle.
    pub fact: String,
}

#[derive(Debug, Serialize)]
pub struct CitationOfFactOutput {
    pub fact: String,
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

impl McpTool for CitationOfFactTool {
    const NAME: &'static str = "core/citation_of_fact";
    const DESCRIPTION: &'static str =
        "Return the owner-scoped citation mapping and cited object for one Fact, or none.";
    type Args = CitationOfFactArgs;
    type Output = CitationOfFactOutput;

    fn call(
        ctx: McpToolCtx,
        args: CitationOfFactArgs,
    ) -> BoxFuture<'static, Result<CitationOfFactOutput, McpToolError>> {
        Box::pin(async move {
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
        })
    }
}
