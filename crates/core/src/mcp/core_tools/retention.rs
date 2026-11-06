//! `core_retention` and `core_set_retention`: an owner's retention per
//! schema (docs/data-and-erasure.md): members read, owner admins set.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::memory_spaces::MemorySpaceKey;
use crate::mcp::{McpTool, McpToolAudience, McpToolCtx, McpToolError, Replay, ToolEffect};
use crate::protocol::tool as protocol_tool;
use crate::{KeepDays, OwnerId, SchemaId};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RetentionArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RetentionOutput {
    pub retention: Vec<RetentionRow>,
}

#[derive(Debug, Serialize, PartialEq, Eq, JsonSchema)]
pub struct RetentionRow {
    pub space: String,
    pub schema_id: String,
    pub keep_days: u16,
}

/// The read: the rows of every owner the caller may read.
#[derive(Debug)]
pub struct RetentionTool;

impl McpTool for RetentionTool {
    const NAME: &'static str = protocol_tool::CORE_RETENTION;
    const TITLE: &'static str = "Retention";
    const DESCRIPTION: &'static str =
        "Days each schema's memory is kept in spaces you belong to; unlisted keeps for good.";
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::ReadOnly);
    const AUDIENCE: McpToolAudience = McpToolAudience::Owner;
    type Args = RetentionArgs;
    type Output = RetentionOutput;

    fn call(
        ctx: McpToolCtx,
        _args: RetentionArgs,
    ) -> BoxFuture<'static, Result<RetentionOutput, McpToolError>> {
        Box::pin(async move {
            let engine = ctx.require_engine()?;
            let rows = engine.read_retention(&ctx.authz).await?;
            Ok(RetentionOutput {
                retention: rows
                    .into_iter()
                    .map(|row| RetentionRow {
                        space: row.owner.to_string(),
                        schema_id: row.schema.into_inner(),
                        keep_days: row.keep_days.get(),
                    })
                    .collect(),
            })
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetRetentionArgs {
    /// `current` (default) or a space UUID you administer.
    #[serde(default)]
    pub space: Option<String>,
    pub schema_id: String,
    /// Omit to keep for good.
    #[serde(default)]
    #[schemars(range(min = 1, max = 36500))]
    pub keep_days: Option<i64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetRetentionOutput {
    pub ok: bool,
}

/// The set: one row of one owner, removed when no days are given.
#[derive(Debug)]
pub struct SetRetentionTool;

impl McpTool for SetRetentionTool {
    const NAME: &'static str = protocol_tool::CORE_SET_RETENTION;
    const TITLE: &'static str = "Set retention";
    const DESCRIPTION: &'static str =
        "Keep a schema's memory in a space for keep_days; older memory is erased.";
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::Destructive(Replay::Idempotent));
    const AUDIENCE: McpToolAudience = McpToolAudience::Owner;
    type Args = SetRetentionArgs;
    type Output = SetRetentionOutput;

    fn call(
        ctx: McpToolCtx,
        args: SetRetentionArgs,
    ) -> BoxFuture<'static, Result<SetRetentionOutput, McpToolError>> {
        Box::pin(async move {
            let owner = resolve_space(&ctx, args.space.as_deref())?;
            let keep_days = args
                .keep_days
                .map(KeepDays::new)
                .transpose()
                .map_err(|err| McpToolError::InvalidInput(err.to_string()))?;
            if args.schema_id.is_empty() {
                return Err(McpToolError::InvalidInput("schema_id is empty".into()));
            }
            let engine = ctx.require_engine()?;
            engine
                .set_retention(
                    &ctx.authz,
                    &owner,
                    &SchemaId::new(args.schema_id),
                    keep_days,
                )
                .await?;
            Ok(SetRetentionOutput { ok: true })
        })
    }
}

/// The named space, structurally: whether the caller administers it is the
/// engine's owner-admin gate to say, as a `Forbidden`.
fn resolve_space(ctx: &McpToolCtx, raw: Option<&str>) -> Result<OwnerId, McpToolError> {
    match MemorySpaceKey::parse(raw.unwrap_or("current")) {
        Some(MemorySpaceKey::Current) => Ok(ctx.owner),
        Some(MemorySpaceKey::Owner(owner)) => Ok(owner),
        None => Err(McpToolError::InvalidInput(format!(
            "unknown memory space: {}",
            raw.unwrap_or_default()
        ))),
    }
}
