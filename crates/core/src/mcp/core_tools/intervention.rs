//! `core/emit_intervention_decision` — Wake Supervisor-authored intervention decision Fact.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::Storage;
use crate::intervention::{
    EmitInterventionDecisionInput, InterventionDecisionKind, InterventionDecisionV1,
    LoadedInterventionRequest,
};
use crate::mcp::{McpTool, McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct EmitInterventionDecisionTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmitInterventionDecisionArgs {
    #[schemars(
        description = "`F...` intervention-requested Fact memory handle that woke the Wake Supervisor."
    )]
    pub intervention_request: String,
    #[schemars(
        description = "Decision to emit for the intervention request: continue, stop, redirect, decompose, or accept_terminal."
    )]
    pub decision: InterventionDecisionKind,
    #[schemars(
        description = "Optional extra wake rounds to grant for a continue decision. Omit or null for stop, redirect, decompose, or accept_terminal."
    )]
    #[serde(default)]
    pub grant_rounds: Option<u16>,
    #[schemars(
        description = "Optional `I...` Personality handle to redirect to. Required only for redirect; omit or null otherwise."
    )]
    #[serde(default)]
    pub redirect_personality: Option<String>,
    #[schemars(description = "Evidence-based rationale for the decision, 1 to 20000 chars.")]
    pub rationale: String,
    #[schemars(
        description = "Stable idempotency key for this intervention decision. Reuse only for exact replay."
    )]
    pub idempotency_key: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EmitInterventionDecisionOutput {
    pub intervention_decision: String,
    pub decision: InterventionDecisionKind,
}

impl McpTool for EmitInterventionDecisionTool {
    const NAME: &'static str = "core/emit_intervention_decision";
    const DESCRIPTION: &'static str = "Emit a typed InterventionDecision for a InterventionRequested Fact targeted at caller Self.";
    type Args = EmitInterventionDecisionArgs;
    type Output = EmitInterventionDecisionOutput;

    fn call(
        ctx: McpToolCtx,
        args: EmitInterventionDecisionArgs,
    ) -> BoxFuture<'static, Result<EmitInterventionDecisionOutput, McpToolError>> {
        Box::pin(async move {
            let intervention_request = ctx.resolve_fact_memory(&args.intervention_request)?;
            if args.rationale.trim().is_empty() {
                return Err(McpToolError::InvalidInput("rationale is empty".into()));
            }
            if args.idempotency_key.trim().is_empty() {
                return Err(McpToolError::InvalidInput(
                    "idempotency_key is empty".into(),
                ));
            }
            let caller_self = ctx.caller_self_perspective.ok_or_else(|| {
                McpToolError::InvalidInput("caller_self_perspective required".into())
            })?;
            let loaded = require_storage(&ctx)?
                .load_intervention_request(&ctx.owner, intervention_request)
                .await?
                .ok_or_else(|| {
                    McpToolError::InvalidInput(
                        "intervention_request is not a InterventionRequested Fact for this owner"
                            .into(),
                    )
                })?;
            if !require_storage(&ctx)?
                .is_intervention_supervisor(
                    &ctx.owner,
                    caller_self,
                    loaded.target_intervention_personality_instance_id,
                )
                .await?
            {
                return Err(McpToolError::InvalidInput(
                    "caller Self is not the targeted Wake Supervisor".into(),
                ));
            }
            validate_decision_shape(&ctx, &args, &loaded).await?;
            let redirect_personality_instance_id = args
                .redirect_personality
                .as_deref()
                .map(|raw| ctx.resolve_personality(raw).map(|id| id.into_inner()))
                .transpose()?;
            let decision = args.decision;
            let payload = InterventionDecisionV1 {
                intervention_request_memory_id: loaded.memory_id.into_inner(),
                decision,
                grant_rounds: args.grant_rounds,
                redirect_personality_instance_id,
                rationale: args.rationale,
                idempotency_key: args.idempotency_key,
                decided_at: OffsetDateTime::now_utc(),
            };
            if let Some(existing) = require_storage(&ctx)?
                .existing_intervention_decision(
                    &ctx.owner,
                    loaded.memory_id,
                    &payload.idempotency_key,
                )
                .await?
            {
                return Ok(EmitInterventionDecisionOutput {
                    intervention_decision: ctx.format_fact_memory(existing),
                    decision,
                });
            }
            let outcome = require_storage(&ctx)?
                .emit_intervention_decision_atomic(
                    &ctx.registry,
                    &EmitInterventionDecisionInput {
                        owner: ctx.owner.clone(),
                        payload,
                        caller_self,
                    },
                )
                .await?;
            Ok(EmitInterventionDecisionOutput {
                intervention_decision: ctx.format_fact_memory(outcome.memory_id),
                decision,
            })
        })
    }
}

async fn validate_decision_shape(
    ctx: &McpToolCtx,
    args: &EmitInterventionDecisionArgs,
    loaded: &LoadedInterventionRequest,
) -> Result<(), McpToolError> {
    match args.decision {
        InterventionDecisionKind::Continue => {
            let Some(grant_rounds) = args.grant_rounds else {
                return Err(McpToolError::InvalidInput(
                    "continue requires grant_rounds".into(),
                ));
            };
            if i32::from(grant_rounds) == 0
                || i32::from(grant_rounds) > loaded.intervention_extension_rounds
            {
                return Err(McpToolError::InvalidInput(
                    "grant_rounds exceeds request extension".into(),
                ));
            }
            let prior = require_storage(ctx)?
                .prior_continue_grant_rounds(&ctx.owner, loaded.memory_id)
                .await?;
            if prior + i64::from(grant_rounds) > i64::from(loaded.intervention_hard_cap_rounds) {
                return Err(McpToolError::InvalidInput(
                    "grant_rounds exceeds request hard cap".into(),
                ));
            }
        }
        InterventionDecisionKind::Redirect => {
            if args.redirect_personality.is_none() {
                return Err(McpToolError::InvalidInput(
                    "redirect requires redirect_personality".into(),
                ));
            }
        }
        _ => {
            if args.grant_rounds.is_some() {
                return Err(McpToolError::InvalidInput(
                    "grant_rounds is only valid for continue".into(),
                ));
            }
        }
    }
    Ok(())
}

/// Borrow the engine-backed `Storage` handle the intervention tool needs.
///
/// `McpToolCtx::engine` is `None` only in test scaffolds without a wired
/// engine; the intervention tool always requires one.
fn require_storage(ctx: &McpToolCtx) -> Result<&dyn Storage, McpToolError> {
    ctx.storage().ok_or_else(|| {
        McpToolError::Other("intervention tools require an attached engine".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervention_decision_output_is_record_only() {
        let output = EmitInterventionDecisionOutput {
            intervention_decision: "F1".into(),
            decision: InterventionDecisionKind::Continue,
        };

        let value = serde_json::to_value(output).expect("serialize output");

        assert_eq!(value["intervention_decision"], "F1");
        assert_eq!(value["decision"], "continue");
        assert!(value.get("continuation_applied").is_none());
        assert!(value.get("continuation_note").is_none());
    }
}
