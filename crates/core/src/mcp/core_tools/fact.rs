use crate::mcp::{CoreActionMeta, McpTool, McpToolCtx, McpToolError};
use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::citation_of_fact::{
    CitationOfEntityHeadArgs, CitationOfEntityHeadOutput, CitationOfFactArgs, CitationOfFactOutput,
    citation_of_entity_head, citation_of_fact,
};
use super::cleanup_facts::{CleanupFactsArgs, CleanupFactsOutput, cleanup_facts};
use super::facts_citing_object::{
    FactsCitingObjectArgs, FactsCitingObjectOutput, facts_citing_object,
};
use super::set_fact_retention::{SetFactRetentionArgs, SetFactRetentionOutput, set_fact_retention};
use super::{DESTRUCTIVE_IDEMPOTENT, READ_ONLY, WRITE_IDEMPOTENT};

const CORE_FACT_CITATION_OF_FACT_SCOPE_KEY: &str = "core_fact:citation_of_fact";
const CORE_FACT_CITATION_OF_ENTITY_HEAD_SCOPE_KEY: &str = "core_fact:citation_of_entity_head";
const CORE_FACT_FACTS_CITING_OBJECT_SCOPE_KEY: &str = "core_fact:facts_citing_object";
const CORE_FACT_SET_FACT_RETENTION_SCOPE_KEY: &str = "core_fact:set_fact_retention";
const CORE_FACT_CLEANUP_SCOPE_KEY: &str = "core_fact:cleanup";

pub const CORE_FACT_ACTIONS: &[CoreActionMeta] = &[
    CoreActionMeta {
        tool: CoreFactTool::NAME,
        action: "citation_of_fact",
        scope_key: CORE_FACT_CITATION_OF_FACT_SCOPE_KEY,
        description: "Return the owner-scoped citation mapping and cited object for one Fact.",
        produces_schema_ids: &[],
        annotations: READ_ONLY,
    },
    CoreActionMeta {
        tool: CoreFactTool::NAME,
        action: "citation_of_entity_head",
        scope_key: CORE_FACT_CITATION_OF_ENTITY_HEAD_SCOPE_KEY,
        description: "Return citation data for a stateful Fact entity's current head.",
        produces_schema_ids: &[],
        annotations: READ_ONLY,
    },
    CoreActionMeta {
        tool: CoreFactTool::NAME,
        action: "facts_citing_object",
        scope_key: CORE_FACT_FACTS_CITING_OBJECT_SCOPE_KEY,
        description: "Return owner-scoped Facts whose citation mapping points at a cited object.",
        produces_schema_ids: &[],
        annotations: READ_ONLY,
    },
    CoreActionMeta {
        tool: CoreFactTool::NAME,
        action: "set_fact_retention",
        scope_key: CORE_FACT_SET_FACT_RETENTION_SCOPE_KEY,
        description: "Set or clear the owner Fact-retention duration.",
        produces_schema_ids: &[],
        annotations: WRITE_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreFactTool::NAME,
        action: "cleanup",
        scope_key: CORE_FACT_CLEANUP_SCOPE_KEY,
        description: "Run the owner Fact-retention cleanup sweep.",
        produces_schema_ids: &[],
        annotations: DESTRUCTIVE_IDEMPOTENT,
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
    SetFactRetention(SetFactRetentionArgs),
    Cleanup(CleanupFactsArgs),
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CoreFactOutput {
    CitationOfFact(CitationOfFactOutput),
    CitationOfEntityHead(CitationOfEntityHeadOutput),
    FactsCitingObject(FactsCitingObjectOutput),
    SetFactRetention(SetFactRetentionOutput),
    Cleanup(CleanupFactsOutput),
}

impl McpTool for CoreFactTool {
    const NAME: &'static str = "core_fact";
    const DESCRIPTION: &'static str = "Fact/citation dispatcher — citation_of_fact/citation_of_entity_head/facts_citing_object/set_fact_retention/cleanup.";
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
                CoreFactArgs::SetFactRetention(args) => set_fact_retention(ctx, args)
                    .await
                    .map(CoreFactOutput::SetFactRetention),
                CoreFactArgs::Cleanup(args) => {
                    cleanup_facts(ctx, args).await.map(CoreFactOutput::Cleanup)
                }
            }
        })
    }
}
