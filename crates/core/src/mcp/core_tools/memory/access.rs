use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::MemoryId;
use crate::access::GrantResource;
use crate::mcp::{CoreActionMeta, McpActionArgSpec, McpTool, McpToolCtx, McpToolError};

use super::super::access_common::{
    GrantOutput, GrantSubjectArg, RelationArg, StatusOutput, VisibilityArg, format_grant,
    parse_grant_subject,
};
use super::super::{DESTRUCTIVE_IDEMPOTENT, READ_ONLY, WRITE_IDEMPOTENT};

const CORE_MEMORY_SHARE_SCOPE_KEY: &str = "core_memory:share";
const CORE_MEMORY_UNSHARE_SCOPE_KEY: &str = "core_memory:unshare";
const CORE_MEMORY_SET_VISIBILITY_SCOPE_KEY: &str = "core_memory:set_visibility";
const CORE_MEMORY_LIST_SHARES_SCOPE_KEY: &str = "core_memory:list_shares";

pub const CORE_MEMORY_ACTIONS: &[CoreActionMeta] = &[
    CoreActionMeta {
        tool: CoreMemoryTool::NAME,
        action: "share",
        scope_key: CORE_MEMORY_SHARE_SCOPE_KEY,
        description: "Share one memory with a principal or group subject.",
        produces_schema_ids: &[],
        annotations: WRITE_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreMemoryTool::NAME,
        action: "unshare",
        scope_key: CORE_MEMORY_UNSHARE_SCOPE_KEY,
        description: "Revoke all memory grants for one subject.",
        produces_schema_ids: &[],
        annotations: DESTRUCTIVE_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreMemoryTool::NAME,
        action: "set_visibility",
        scope_key: CORE_MEMORY_SET_VISIBILITY_SCOPE_KEY,
        description: "Set memory visibility; this is the only public/private transition path.",
        produces_schema_ids: &[],
        annotations: WRITE_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreMemoryTool::NAME,
        action: "list_shares",
        scope_key: CORE_MEMORY_LIST_SHARES_SCOPE_KEY,
        description: "List active grants on one memory.",
        produces_schema_ids: &[],
        annotations: READ_ONLY,
    },
];

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryShareArgs {
    #[schemars(
        description = "Memory reference: F:<uuid>, A:<uuid>, P:<uuid>, raw uuid, or handle."
    )]
    pub memory: String,
    #[schemars(description = "Grant subject: {subject_kind, subject_id}.")]
    pub subject: GrantSubjectArg,
    #[schemars(description = "Grant relation. owner is rejected by the engine.")]
    pub relation: RelationArg,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryUnshareArgs {
    #[schemars(
        description = "Memory reference: F:<uuid>, A:<uuid>, P:<uuid>, raw uuid, or handle."
    )]
    pub memory: String,
    #[schemars(description = "Grant subject: {subject_kind, subject_id}.")]
    pub subject: GrantSubjectArg,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemorySetVisibilityArgs {
    #[schemars(
        description = "Memory reference: F:<uuid>, A:<uuid>, P:<uuid>, raw uuid, or handle."
    )]
    pub memory: String,
    #[schemars(description = "New visibility: private or public.")]
    pub visibility: VisibilityArg,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryListSharesArgs {
    #[schemars(
        description = "Memory reference: F:<uuid>, A:<uuid>, P:<uuid>, raw uuid, or handle."
    )]
    pub memory: String,
    #[serde(default)]
    #[schemars(description = "Owner space key from core_memory_spaces. Omit for current owner.")]
    pub space: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MemoryListSharesOutput {
    pub memory: String,
    pub space: String,
    pub grants: Vec<GrantOutput>,
}

#[derive(Debug, Default)]
pub struct CoreMemoryTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CoreMemoryArgs {
    Share(MemoryShareArgs),
    Unshare(MemoryUnshareArgs),
    SetVisibility(MemorySetVisibilityArgs),
    ListShares(MemoryListSharesArgs),
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CoreMemoryOutput {
    Status(StatusOutput),
    ListShares(MemoryListSharesOutput),
}

impl McpTool for CoreMemoryTool {
    const NAME: &'static str = "core_memory";
    const DESCRIPTION: &'static str =
        "Memory access dispatcher — share/unshare/set_visibility/list_shares.";
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[
        McpActionArgSpec {
            action: "share",
            allowed_fields: &["memory", "subject", "relation"],
            required_fields: &["memory", "subject", "relation"],
        },
        McpActionArgSpec {
            action: "unshare",
            allowed_fields: &["memory", "subject"],
            required_fields: &["memory", "subject"],
        },
        McpActionArgSpec {
            action: "set_visibility",
            allowed_fields: &["memory", "visibility"],
            required_fields: &["memory", "visibility"],
        },
        McpActionArgSpec {
            action: "list_shares",
            allowed_fields: &["memory", "space"],
            required_fields: &["memory"],
        },
    ];
    type Args = CoreMemoryArgs;
    type Output = CoreMemoryOutput;

    fn call(
        ctx: McpToolCtx,
        args: CoreMemoryArgs,
    ) -> BoxFuture<'static, Result<CoreMemoryOutput, McpToolError>> {
        Box::pin(async move {
            match args {
                CoreMemoryArgs::Share(args) => share(ctx, args).await.map(CoreMemoryOutput::Status),
                CoreMemoryArgs::Unshare(args) => {
                    unshare(ctx, args).await.map(CoreMemoryOutput::Status)
                }
                CoreMemoryArgs::SetVisibility(args) => set_visibility(ctx, args)
                    .await
                    .map(CoreMemoryOutput::Status),
                CoreMemoryArgs::ListShares(args) => list_shares(ctx, args)
                    .await
                    .map(CoreMemoryOutput::ListShares),
            }
        })
    }
}

async fn share(ctx: McpToolCtx, args: MemoryShareArgs) -> Result<StatusOutput, McpToolError> {
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    engine
        .share_entry(
            &ctx.authz,
            resolve_memory_reference(&ctx, &args.memory)?,
            parse_grant_subject(args.subject)?,
            args.relation.into(),
        )
        .await?;
    Ok(StatusOutput { ok: true })
}

async fn unshare(ctx: McpToolCtx, args: MemoryUnshareArgs) -> Result<StatusOutput, McpToolError> {
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    engine
        .unshare_entry(
            &ctx.authz,
            resolve_memory_reference(&ctx, &args.memory)?,
            parse_grant_subject(args.subject)?,
        )
        .await?;
    Ok(StatusOutput { ok: true })
}

async fn set_visibility(
    ctx: McpToolCtx,
    args: MemorySetVisibilityArgs,
) -> Result<StatusOutput, McpToolError> {
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    engine
        .set_entry_visibility(
            &ctx.authz,
            resolve_memory_reference(&ctx, &args.memory)?,
            args.visibility.into(),
        )
        .await?;
    Ok(StatusOutput { ok: true })
}

async fn list_shares(
    ctx: McpToolCtx,
    args: MemoryListSharesArgs,
) -> Result<MemoryListSharesOutput, McpToolError> {
    let memory_id = resolve_memory_reference(&ctx, &args.memory)?;
    let memory = args.memory;
    let space = super::super::memory_spaces::resolve_space_owner(
        &ctx,
        args.space.as_deref(),
        super::super::memory_spaces::SpaceDefault::Current,
    )?;
    let engine = ctx
        .engine()
        .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
    let grant_rows = engine
        .list_grants(&ctx.authz, space.owner, GrantResource::Memory(memory_id))
        .await?;
    let grants = grant_rows.iter().map(format_grant).collect();
    Ok(MemoryListSharesOutput {
        memory,
        space: space.key,
        grants,
    })
}

fn resolve_memory_reference(ctx: &McpToolCtx, raw: &str) -> Result<MemoryId, McpToolError> {
    match ctx.resolve_memory(raw) {
        Ok(memory_id) => Ok(memory_id),
        Err(resolve_err) => raw
            .parse::<uuid::Uuid>()
            .map(MemoryId::new)
            .map_err(|_| resolve_err),
    }
}
