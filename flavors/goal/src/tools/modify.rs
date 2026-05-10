use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::verbs::goal_write::GoalState;
use schemars::JsonSchema;
use serde::Deserialize;

use super::accept::{AcceptArgs, AcceptOutput, accept_goal};
use super::util::GoalPayloadInput;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ModifyArgs {
    pub proposal: String,
    pub payload: GoalPayloadInput,
    pub evidence: Option<Vec<String>>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug)]
pub struct ModifyTool;

impl McpTool for ModifyTool {
    const NAME: &'static str = "proxima-goal/goal_modify";
    const DESCRIPTION: &'static str =
        "Accept a proposal as an Active Goal with modified payload or evidence.";
    type Args = ModifyArgs;
    type Output = AcceptOutput;

    fn call(
        ctx: McpToolCtx,
        args: ModifyArgs,
    ) -> futures::future::BoxFuture<'static, Result<AcceptOutput, McpToolError>> {
        Box::pin(async move {
            accept_goal(
                ctx,
                AcceptArgs {
                    proposal: args.proposal,
                    payload: Some(args.payload),
                    evidence: args.evidence,
                    target_personality: None,
                    idempotency_key: args.idempotency_key,
                },
                GoalState::Active,
            )
            .await
        })
    }
}
