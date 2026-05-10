//! `core/remove_inference_target` — wraps Engine's same-name verb.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::RemoveInferenceTargetRequest;
use crate::auth::Credentials;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct RemoveInferenceTargetTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveInferenceTargetArgs {
    pub target_ref: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RemoveInferenceTargetOutput {
    pub idempotent_replay: bool,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for RemoveInferenceTargetTool {
    const NAME: &'static str = "core/remove_inference_target";
    const DESCRIPTION: &'static str =
        "Remove an inference target by ref. Idempotent.";
    type Args = RemoveInferenceTargetArgs;
    type Output = RemoveInferenceTargetOutput;

    fn call(
        ctx: McpToolCtx,
        args: RemoveInferenceTargetArgs,
    ) -> BoxFuture<'static, Result<RemoveInferenceTargetOutput, McpToolError>> {
        Box::pin(async move {
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
            let target_ref = args.target_ref.clone();
            let req = RemoveInferenceTargetRequest {
                owner: ctx.owner.clone(),
                target_ref: target_ref.clone(),
            };
            let resp = engine
                .remove_inference_target(&Credentials::None, &req)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::RemoveInferenceTarget,
                PersonalityConfigChangedSubject::InferenceTarget(target_ref),
                Some(serde_json::Value::Null),
                None,
            )
            .await;
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => Some(reason),
            };
            Ok(RemoveInferenceTargetOutput {
                idempotent_replay: resp.idempotent_replay,
                audit_emit_failed,
            })
        })
    }
}
