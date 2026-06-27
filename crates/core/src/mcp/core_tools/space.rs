use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::access::GrantResource;
use crate::mcp::{CoreActionMeta, McpActionArgSpec, McpTool, McpToolCtx, McpToolError};

use super::access_common::{
    GrantOutput, RelationArg, StatusOutput, format_grant, format_principal, parse_grant_subject,
};
use super::{READ_ONLY, WRITE_IDEMPOTENT};

const CORE_SPACE_SET_MEMBER_SCOPE_KEY: &str = "core_space:set_member";
const CORE_SPACE_LIST_MEMBERS_SCOPE_KEY: &str = "core_space:list_members";

pub const CORE_SPACE_ACTIONS: &[CoreActionMeta] = &[
    CoreActionMeta {
        tool: CoreSpaceTool::NAME,
        action: "set_member",
        scope_key: CORE_SPACE_SET_MEMBER_SCOPE_KEY,
        description: "Set one grant relation on a space for a principal or group subject.",
        produces_schema_ids: &[],
        annotations: WRITE_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreSpaceTool::NAME,
        action: "list_members",
        scope_key: CORE_SPACE_LIST_MEMBERS_SCOPE_KEY,
        description: "List active grants on a space.",
        produces_schema_ids: &[],
        annotations: READ_ONLY,
    },
];

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpaceSetMemberArgs {
    #[serde(default)]
    #[schemars(description = "Owner space key from core_memory_spaces. Omit for current owner.")]
    pub space: Option<String>,
    #[schemars(description = "Grant subject as user:<uuid>, group:<uuid>, or bare user uuid.")]
    pub subject: String,
    #[serde(default)]
    #[schemars(description = "Whether the subject is a group whose members inherit the grant.")]
    pub subject_is_group: bool,
    #[schemars(description = "Grant relation. owner is rejected by the engine.")]
    pub relation: RelationArg,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpaceListMembersArgs {
    #[serde(default)]
    #[schemars(description = "Owner space key from core_memory_spaces. Omit for current owner.")]
    pub space: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SpaceListMembersOutput {
    pub space: String,
    pub owner: String,
    pub grants: Vec<GrantOutput>,
}

#[derive(Debug, Default)]
pub struct CoreSpaceTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CoreSpaceArgs {
    SetMember(SpaceSetMemberArgs),
    ListMembers(SpaceListMembersArgs),
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CoreSpaceOutput {
    Status(StatusOutput),
    ListMembers(SpaceListMembersOutput),
}

impl McpTool for CoreSpaceTool {
    const NAME: &'static str = "core_space";
    const DESCRIPTION: &'static str = "Space access dispatcher — set_member/list_members.";
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[
        McpActionArgSpec {
            action: "set_member",
            allowed_fields: &["space", "subject", "subject_is_group", "relation"],
            required_fields: &["subject", "relation"],
        },
        McpActionArgSpec {
            action: "list_members",
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
                CoreSpaceArgs::SetMember(args) => {
                    set_member(ctx, args).await.map(CoreSpaceOutput::Status)
                }
                CoreSpaceArgs::ListMembers(args) => list_members(ctx, args)
                    .await
                    .map(CoreSpaceOutput::ListMembers),
            }
        })
    }
}

async fn set_member(
    ctx: McpToolCtx,
    args: SpaceSetMemberArgs,
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
            parse_grant_subject(&args.subject, args.subject_is_group)?,
            args.relation.into(),
        )
        .await?;
    Ok(StatusOutput { ok: true })
}

async fn list_members(
    ctx: McpToolCtx,
    args: SpaceListMembersArgs,
) -> Result<SpaceListMembersOutput, McpToolError> {
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
    Ok(SpaceListMembersOutput {
        space: space.key,
        owner,
        grants,
    })
}
