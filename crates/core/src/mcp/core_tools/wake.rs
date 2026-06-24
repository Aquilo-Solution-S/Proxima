use crate::mcp::{CoreActionMeta, McpActionArgSpec, McpTool, McpToolCtx, McpToolError};
use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::add_wake_entry::{AddWakeEntryArgs, AddWakeEntryOutput, add_wake_entry};
use super::list_wake_entries::{ListWakeEntriesArgs, ListWakeEntriesOutput, list_wake_entries};
use super::remove_wake_entry::{RemoveWakeEntryArgs, RemoveWakeEntryOutput, remove_wake_entry};
use super::set_wake_entries::{SetWakeEntriesArgs, SetWakeEntriesOutput, set_wake_entries};
use super::update_wake_entry::{UpdateWakeEntryArgs, UpdateWakeEntryOutput, update_wake_entry};
use super::{
    DESTRUCTIVE_IDEMPOTENT, DESTRUCTIVE_NON_IDEMPOTENT, READ_ONLY, WRITE_IDEMPOTENT,
    WRITE_NON_IDEMPOTENT,
};

const CORE_WAKE_ADD_SCOPE_KEY: &str = "core_wake:add";
const CORE_WAKE_UPDATE_SCOPE_KEY: &str = "core_wake:update";
const CORE_WAKE_REMOVE_SCOPE_KEY: &str = "core_wake:remove";
const CORE_WAKE_SET_SCOPE_KEY: &str = "core_wake:set";
const CORE_WAKE_LIST_SCOPE_KEY: &str = "core_wake:list";
const PERSONALITY_CONFIG_CHANGED_SCHEMA_IDS: &[&str] =
    &[<super::PersonalityConfigChangedV1 as crate::FactPayload>::SCHEMA_ID];

pub const CORE_WAKE_ACTIONS: &[CoreActionMeta] = &[
    CoreActionMeta {
        tool: CoreWakeTool::NAME,
        action: "add",
        scope_key: CORE_WAKE_ADD_SCOPE_KEY,
        description: "Append one wake entry to a personality.",
        produces_schema_ids: PERSONALITY_CONFIG_CHANGED_SCHEMA_IDS,
        annotations: WRITE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreWakeTool::NAME,
        action: "update",
        scope_key: CORE_WAKE_UPDATE_SCOPE_KEY,
        description: "Update one wake entry's mutable fields.",
        produces_schema_ids: PERSONALITY_CONFIG_CHANGED_SCHEMA_IDS,
        annotations: WRITE_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreWakeTool::NAME,
        action: "remove",
        scope_key: CORE_WAKE_REMOVE_SCOPE_KEY,
        description: "Remove one wake entry.",
        produces_schema_ids: PERSONALITY_CONFIG_CHANGED_SCHEMA_IDS,
        annotations: DESTRUCTIVE_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreWakeTool::NAME,
        action: "set",
        scope_key: CORE_WAKE_SET_SCOPE_KEY,
        description: "Replace all wake entries for a personality.",
        produces_schema_ids: PERSONALITY_CONFIG_CHANGED_SCHEMA_IDS,
        annotations: DESTRUCTIVE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreWakeTool::NAME,
        action: "list",
        scope_key: CORE_WAKE_LIST_SCOPE_KEY,
        description: "List wake entries on one personality.",
        produces_schema_ids: &[],
        annotations: READ_ONLY,
    },
];

#[derive(Debug, Default)]
pub struct CoreWakeTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CoreWakeArgs {
    Add(AddWakeEntryArgs),
    Update(UpdateWakeEntryArgs),
    Remove(RemoveWakeEntryArgs),
    Set(SetWakeEntriesArgs),
    List(ListWakeEntriesArgs),
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CoreWakeOutput {
    Add(AddWakeEntryOutput),
    Update(UpdateWakeEntryOutput),
    Remove(RemoveWakeEntryOutput),
    Set(SetWakeEntriesOutput),
    List(ListWakeEntriesOutput),
}

impl McpTool for CoreWakeTool {
    const NAME: &'static str = "core_wake";
    const DESCRIPTION: &'static str = "Wake-entry dispatcher — add/update/remove/set/list.";
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = PERSONALITY_CONFIG_CHANGED_SCHEMA_IDS;
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[
        McpActionArgSpec {
            action: "add",
            allowed_fields: &["personality", "entry"],
            required_fields: &["personality", "entry"],
        },
        McpActionArgSpec {
            action: "update",
            allowed_fields: &["wake_entry", "patch"],
            required_fields: &["wake_entry", "patch"],
        },
        McpActionArgSpec {
            action: "remove",
            allowed_fields: &["wake_entry"],
            required_fields: &["wake_entry"],
        },
        McpActionArgSpec {
            action: "set",
            allowed_fields: &["personality", "entries"],
            required_fields: &["personality", "entries"],
        },
        McpActionArgSpec {
            action: "list",
            allowed_fields: &["personality"],
            required_fields: &["personality"],
        },
    ];
    type Args = CoreWakeArgs;
    type Output = CoreWakeOutput;

    fn call(
        ctx: McpToolCtx,
        args: CoreWakeArgs,
    ) -> BoxFuture<'static, Result<CoreWakeOutput, McpToolError>> {
        Box::pin(async move {
            match args {
                CoreWakeArgs::Add(args) => add_wake_entry(ctx, args).await.map(CoreWakeOutput::Add),
                CoreWakeArgs::Update(args) => update_wake_entry(ctx, args)
                    .await
                    .map(CoreWakeOutput::Update),
                CoreWakeArgs::Remove(args) => remove_wake_entry(ctx, args)
                    .await
                    .map(CoreWakeOutput::Remove),
                CoreWakeArgs::Set(args) => {
                    set_wake_entries(ctx, args).await.map(CoreWakeOutput::Set)
                }
                CoreWakeArgs::List(args) => {
                    list_wake_entries(ctx, args).await.map(CoreWakeOutput::List)
                }
            }
        })
    }
}
