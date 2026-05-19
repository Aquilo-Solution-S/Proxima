use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::verbs::goal_write::GoalState;
use schemars::JsonSchema;
use serde::Deserialize;

use super::accept::{AcceptArgs, AcceptOutput, accept_goal};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeclineArgs {
    #[schemars(description = "`G...` Goal handle for the Proposed Goal to decline.")]
    pub proposal: String,
    #[schemars(
        description = "Optional stable idempotency key. Omit or null to derive a fresh request id."
    )]
    pub idempotency_key: Option<String>,
}

#[derive(Debug)]
pub struct DeclineTool;

impl McpTool for DeclineTool {
    const NAME: &'static str = "proxima-goal/goal_decline";
    const DESCRIPTION: &'static str = "Decline a Proposed Goal and mark it Rejected.";
    type Args = DeclineArgs;
    type Output = AcceptOutput;

    fn call(
        ctx: McpToolCtx,
        args: DeclineArgs,
    ) -> futures::future::BoxFuture<'static, Result<AcceptOutput, McpToolError>> {
        Box::pin(async move {
            accept_goal(
                ctx,
                AcceptArgs {
                    proposal: args.proposal,
                    payload: None,
                    evidence: None,
                    target_personality: None,
                    idempotency_key: args.idempotency_key,
                },
                GoalState::Rejected,
            )
            .await
        })
    }
}
