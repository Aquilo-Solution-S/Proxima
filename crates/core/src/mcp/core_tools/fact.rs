use crate::mcp::{CoreActionMeta, McpActionArgSpec, McpTool, McpToolCtx, McpToolError};
use crate::protocol::{action as protocol_action, tool as protocol_tool};
use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::READ_ONLY;
use super::citation_of_fact::{
    CitationOfEntityHeadArgs, CitationOfEntityHeadOutput, CitationOfFactArgs, CitationOfFactOutput,
    citation_of_entity_head, citation_of_fact,
};
use super::facts_citing_object::{
    FactsCitingObjectArgs, FactsCitingObjectOutput, facts_citing_object,
};

pub const CORE_FACT_ACTIONS: &[CoreActionMeta] = &[
    CoreActionMeta {
        tool: CoreFactTool::NAME,
        action: "citation_of_fact",
        scope_key: protocol_action::CORE_FACT_CITATION_OF_FACT,
        description: "Return the owner-scoped citation mapping and cited object for one Fact.",
        produces_schema_ids: &[],
        annotations: READ_ONLY,
    },
    CoreActionMeta {
        tool: CoreFactTool::NAME,
        action: "citation_of_entity_head",
        scope_key: protocol_action::CORE_FACT_CITATION_OF_ENTITY_HEAD,
        description: "Return citation data for a stateful Fact entity's current head.",
        produces_schema_ids: &[],
        annotations: READ_ONLY,
    },
    CoreActionMeta {
        tool: CoreFactTool::NAME,
        action: "facts_citing_object",
        scope_key: protocol_action::CORE_FACT_FACTS_CITING_OBJECT,
        description: "Return owner-scoped Facts whose citation mapping points at a cited object.",
        produces_schema_ids: &[],
        annotations: READ_ONLY,
    },
];

#[derive(Debug, Default)]
pub struct CoreFactTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CoreFactArgs {
    CitationOfFact(CitationOfFactArgs),
    CitationOfEntityHead(CitationOfEntityHeadArgs),
    FactsCitingObject(FactsCitingObjectArgs),
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CoreFactOutput {
    CitationOfFact(CitationOfFactOutput),
    CitationOfEntityHead(CitationOfEntityHeadOutput),
    FactsCitingObject(FactsCitingObjectOutput),
}

impl McpTool for CoreFactTool {
    const NAME: &'static str = protocol_tool::CORE_FACT;
    const DESCRIPTION: &'static str =
        "Fact/citation dispatcher — citation_of_fact/citation_of_entity_head/facts_citing_object.";
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[
        McpActionArgSpec {
            action: "citation_of_fact",
            allowed_fields: &["fact"],
            required_fields: &["fact"],
        },
        McpActionArgSpec {
            action: "citation_of_entity_head",
            allowed_fields: &["fact_entity_id"],
            required_fields: &["fact_entity_id"],
        },
        McpActionArgSpec {
            action: "facts_citing_object",
            allowed_fields: &["cited_object_id"],
            required_fields: &["cited_object_id"],
        },
    ];
    type Args = CoreFactArgs;
    type Output = CoreFactOutput;

    fn call(
        ctx: McpToolCtx,
        args: CoreFactArgs,
    ) -> BoxFuture<'static, Result<CoreFactOutput, McpToolError>> {
        Box::pin(async move {
            match args {
                CoreFactArgs::CitationOfFact(args) => citation_of_fact(ctx, args)
                    .await
                    .map(CoreFactOutput::CitationOfFact),
                CoreFactArgs::CitationOfEntityHead(args) => citation_of_entity_head(ctx, args)
                    .await
                    .map(CoreFactOutput::CitationOfEntityHead),
                CoreFactArgs::FactsCitingObject(args) => facts_citing_object(ctx, args)
                    .await
                    .map(CoreFactOutput::FactsCitingObject),
            }
        })
    }
}
