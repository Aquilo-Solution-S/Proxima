use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::verbs::goal_write::GoalState;
use schemars::JsonSchema;
use serde::Deserialize;

use super::accept::{AcceptArgs, AcceptOutput, accept_goal};
use super::util::GoalPayloadInput;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ModifyArgs {
    #[schemars(
        description = "`G...` Goal handle for the Proposed Goal to accept with modifications."
    )]
    pub proposal: String,
    #[schemars(description = "Replacement typed Goal payload for the accepted Active Goal.")]
    pub payload: GoalPayloadInput,
    #[schemars(
        description = "Optional replacement evidence memory handles (`F...` Fact or `A...` Abstraction). Omit or null to copy proposal evidence; use `[]` to clear evidence."
    )]
    pub evidence: Option<Vec<String>>,
    #[schemars(
        description = "Optional stable idempotency key. Omit or null to derive a fresh request id."
    )]
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
