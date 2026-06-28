use crate::mcp::{CoreActionMeta, McpActionArgSpec, McpTool, McpToolCtx, McpToolError};
use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::get_personality::{GetPersonalityArgs, GetPersonalityOutput, get_personality};
use super::instantiate_personality::{
    InstantiatePersonalityArgs, InstantiatePersonalityOutput, instantiate_personality,
};
use super::list_personalities::{
    ListPersonalitiesArgs, ListPersonalitiesOutput, list_personalities,
};
use super::tombstone_personality::{
    TombstonePersonalityArgs, TombstonePersonalityOutput, tombstone_personality,
};
use super::{DESTRUCTIVE_NON_IDEMPOTENT, READ_ONLY, WRITE_NON_IDEMPOTENT};

const CORE_PERSONALITY_INSTANTIATE_SCOPE_KEY: &str = "core_personality:instantiate";
const CORE_PERSONALITY_TOMBSTONE_SCOPE_KEY: &str = "core_personality:tombstone";
const CORE_PERSONALITY_LIST_SCOPE_KEY: &str = "core_personality:list";
const CORE_PERSONALITY_GET_SCOPE_KEY: &str = "core_personality:get";
const PERSONALITY_CONFIG_CHANGED_SCHEMA_IDS: &[&str] =
    &[<super::PersonalityConfigChangedV1 as crate::FactPayload>::SCHEMA_ID];

pub const CORE_PERSONALITY_ACTIONS: &[CoreActionMeta] = &[
    CoreActionMeta {
        tool: CorePersonalityTool::NAME,
        action: "instantiate",
        scope_key: CORE_PERSONALITY_INSTANTIATE_SCOPE_KEY,
        description: "Instantiate one inert personality with a Root Perspective.",
        produces_schema_ids: PERSONALITY_CONFIG_CHANGED_SCHEMA_IDS,
        annotations: WRITE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CorePersonalityTool::NAME,
        action: "tombstone",
        scope_key: CORE_PERSONALITY_TOMBSTONE_SCOPE_KEY,
        description: "Tombstone a personality.",
        produces_schema_ids: PERSONALITY_CONFIG_CHANGED_SCHEMA_IDS,
        annotations: DESTRUCTIVE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CorePersonalityTool::NAME,
        action: "list",
        scope_key: CORE_PERSONALITY_LIST_SCOPE_KEY,
        description: "List personality instances for the authenticated owner.",
        produces_schema_ids: &[],
        annotations: READ_ONLY,
    },
    CoreActionMeta {
        tool: CorePersonalityTool::NAME,
        action: "get",
        scope_key: CORE_PERSONALITY_GET_SCOPE_KEY,
        description: "Read one personality with all wake entries.",
        produces_schema_ids: &[],
        annotations: READ_ONLY,
    },
];

#[derive(Debug, Default)]
pub struct CorePersonalityTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CorePersonalityArgs {
    Instantiate(InstantiatePersonalityArgs),
    Tombstone(TombstonePersonalityArgs),
    List(ListPersonalitiesArgs),
    Get(GetPersonalityArgs),
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CorePersonalityOutput {
    Instantiate(InstantiatePersonalityOutput),
    Tombstone(TombstonePersonalityOutput),
    List(ListPersonalitiesOutput),
    Get(GetPersonalityOutput),
}

impl McpTool for CorePersonalityTool {
    const NAME: &'static str = "core_personality";
    const DESCRIPTION: &'static str = "Personality dispatcher — instantiate/tombstone/list/get.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = PERSONALITY_CONFIG_CHANGED_SCHEMA_IDS;
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[
        McpActionArgSpec {
            action: "instantiate",
            allowed_fields: &["display_name"],
            required_fields: &["display_name"],
        },
        McpActionArgSpec {
            action: "tombstone",
            allowed_fields: &["personality", "confirm", "expect_handle"],
            required_fields: &["personality", "confirm", "expect_handle"],
        },
        McpActionArgSpec {
            action: "list",
            allowed_fields: &["include_tombstoned"],
            required_fields: &[],
        },
        McpActionArgSpec {
            action: "get",
            allowed_fields: &["personality"],
            required_fields: &["personality"],
        },
    ];
    type Args = CorePersonalityArgs;
    type Output = CorePersonalityOutput;

    fn call(
        ctx: McpToolCtx,
        args: CorePersonalityArgs,
    ) -> BoxFuture<'static, Result<CorePersonalityOutput, McpToolError>> {
        Box::pin(async move {
            match args {
                CorePersonalityArgs::Instantiate(args) => instantiate_personality(ctx, args)
                    .await
                    .map(CorePersonalityOutput::Instantiate),
                CorePersonalityArgs::Tombstone(args) => tombstone_personality(ctx, args)
                    .await
                    .map(CorePersonalityOutput::Tombstone),
                CorePersonalityArgs::List(args) => list_personalities(ctx, args)
                    .await
                    .map(CorePersonalityOutput::List),
                CorePersonalityArgs::Get(args) => get_personality(ctx, args)
                    .await
                    .map(CorePersonalityOutput::Get),
            }
        })
    }
}
