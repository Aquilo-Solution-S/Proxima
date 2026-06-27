use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::access::GrantResource;
use crate::mcp::{CoreActionMeta, McpActionArgSpec, McpTool, McpToolCtx, McpToolError};

use super::access_common::{
    GrantOutput, GrantSubjectArg, SpaceGrantRelationArg, StatusOutput, format_grant,
    format_principal, parse_grant_subject,
};
use super::{READ_ONLY, WRITE_IDEMPOTENT};

const CORE_SPACE_SET_BINDING_SCOPE_KEY: &str = "core_space:set_binding";
const CORE_SPACE_LIST_BINDINGS_SCOPE_KEY: &str = "core_space:list_bindings";

pub const CORE_SPACE_ACTIONS: &[CoreActionMeta] = &[
    CoreActionMeta {
        tool: CoreSpaceTool::NAME,
        action: "set_binding",
        scope_key: CORE_SPACE_SET_BINDING_SCOPE_KEY,
        description: "Set one grant relation on a space for a principal or group subject.",
        produces_schema_ids: &[],
        annotations: WRITE_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreSpaceTool::NAME,
        action: "list_bindings",
        scope_key: CORE_SPACE_LIST_BINDINGS_SCOPE_KEY,
        description: "List active grants on a space.",
        produces_schema_ids: &[],
        annotations: READ_ONLY,
    },
];

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpaceSetBindingArgs {
    #[serde(default)]
    #[schemars(description = "Owner space key from core_memory_spaces. Omit for current owner.")]
    pub space: Option<String>,
    #[schemars(description = "Grant subject: {subject_kind, subject_id}.")]
    pub subject: GrantSubjectArg,
    #[schemars(description = "Grant relation: admin, editor, viewer, ingest, or member.")]
    pub relation: SpaceGrantRelationArg,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpaceListBindingsArgs {
    #[serde(default)]
    #[schemars(description = "Owner space key from core_memory_spaces. Omit for current owner.")]
    pub space: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SpaceListBindingsOutput {
    pub space: String,
    pub owner: String,
    pub grants: Vec<GrantOutput>,
}

#[derive(Debug, Default)]
pub struct CoreSpaceTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CoreSpaceArgs {
    SetBinding(SpaceSetBindingArgs),
    ListBindings(SpaceListBindingsArgs),
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CoreSpaceOutput {
    Status(StatusOutput),
    ListBindings(SpaceListBindingsOutput),
}

impl McpTool for CoreSpaceTool {
    const NAME: &'static str = "core_space";
    const DESCRIPTION: &'static str = "Space access dispatcher — set_binding/list_bindings.";
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[
        McpActionArgSpec {
            action: "set_binding",
            allowed_fields: &["space", "subject", "relation"],
            required_fields: &["subject", "relation"],
        },
        McpActionArgSpec {
            action: "list_bindings",
            allowed_fields: &["space"],
            required_fields: &[],
        },
    ];
    type Args = CoreSpaceArgs;
    type Output = CoreSpaceOutput;

    fn call(
        ctx: McpToolCtx,
        args: CoreSpaceArgs,
    ) -> BoxFuture<'static, Result<CoreSpaceOutput, McpToolError>> {
        Box::pin(async move {
            match args {
                CoreSpaceArgs::SetBinding(args) => {
                    set_binding(ctx, args).await.map(CoreSpaceOutput::Status)
                }
                CoreSpaceArgs::ListBindings(args) => list_bindings(ctx, args)
                    .await
                    .map(CoreSpaceOutput::ListBindings),
            }
        })
    }
}

async fn set_binding(
    ctx: McpToolCtx,
    args: SpaceSetBindingArgs,
) -> Result<StatusOutput, McpToolError> {
    let space = super::memory_spaces::resolve_space_owner(
        &ctx,
        args.space.as_deref(),
        super::memory_spaces::SpaceDefault::Current,
    )?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    engine
        .set_space_binding(
            &ctx.authz,
            space.owner,
            parse_grant_subject(args.subject)?,
            args.relation.into(),
        )
        .await?;
    Ok(StatusOutput { ok: true })
}

async fn list_bindings(
    ctx: McpToolCtx,
    args: SpaceListBindingsArgs,
) -> Result<SpaceListBindingsOutput, McpToolError> {
    let space = super::memory_spaces::resolve_space_owner(
        &ctx,
        args.space.as_deref(),
        super::memory_spaces::SpaceDefault::Current,
    )?;
    let owner = format_principal(&space.owner);
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let grant_rows = engine
        .list_grants(&ctx.authz, space.owner, GrantResource::Space)
        .await?;
    let grants = grant_rows.iter().map(format_grant).collect();
    Ok(SpaceListBindingsOutput {
        space: space.key,
        owner,
        grants,
    })
}
