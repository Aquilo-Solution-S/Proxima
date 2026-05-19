//! `core/register_inference_target` — wraps Engine's same-name verb.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::auth::Credentials;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    InferenceTargetConfigSnapshot, PersonalityConfigChangeSnapshot,
    PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};
use crate::{InferenceTargetConfig, RegisterInferenceTargetRequest};

#[derive(Debug, Default)]
pub struct RegisterInferenceTargetTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RegisterInferenceTargetArgs {
    pub target_ref: String,
    /// Provider config — accepted as JSON; deserialised into the typed
    /// `InferenceTargetConfig` (`mistral_chat`, `openai_chat`, or
    /// `openai_responses`).
    pub config: serde_json::Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RegisterInferenceTargetOutput {
    pub target_ref: String,
    pub idempotent_replay: bool,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for RegisterInferenceTargetTool {
    const NAME: &'static str = "core/register_inference_target";
    const DESCRIPTION: &'static str = "Register an inference target. Idempotent on target_ref.";
    type Args = RegisterInferenceTargetArgs;
    type Output = RegisterInferenceTargetOutput;

    fn call(
        ctx: McpToolCtx,
        args: RegisterInferenceTargetArgs,
    ) -> BoxFuture<'static, Result<RegisterInferenceTargetOutput, McpToolError>> {
        Box::pin(async move {
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
            let target_ref = args.target_ref.clone();
            let config: InferenceTargetConfig = serde_json::from_value(args.config.clone())
                .map_err(|e| McpToolError::InvalidInput(format!("config: {e}")))?;
            let audit_config = InferenceTargetConfigSnapshot::from(&config);
            let req = RegisterInferenceTargetRequest {
                owner: ctx.owner.clone(),
                target_ref: target_ref.clone(),
                config,
            };
            let resp = engine
                .register_inference_target(&Credentials::None, &req)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::RegisterInferenceTarget,
                PersonalityConfigChangedSubject::InferenceTarget(target_ref.clone()),
                None,
                Some(PersonalityConfigChangeSnapshot::InferenceTarget {
                    config: audit_config,
                }),
            )
            .await;
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => Some(reason),
            };
            Ok(RegisterInferenceTargetOutput {
                target_ref: resp.target_ref,
                idempotent_replay: resp.idempotent_replay,
                audit_emit_failed,
            })
        })
    }
}
