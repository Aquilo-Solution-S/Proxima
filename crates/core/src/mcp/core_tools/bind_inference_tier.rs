//! `core/bind_inference_tier` — wraps Engine's same-name verb.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
use crate::auth::Credentials;
use crate::mcp::core_tools::audit::{AuditEmit, emit_personality_config_changed};
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedSubject, PersonalityConfigChangedVerb,
};
use crate::mcp::{McpToolCtx, McpToolError};
use crate::{BindInferenceTierRequest, ModelTier};

#[derive(Debug, Default)]
pub struct BindInferenceTierTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BindInferenceTierArgs {
    pub tier: String,
    pub target_ref: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct BindInferenceTierOutput {
    pub tier: String,
    pub target_ref: String,
    pub audit_emit_failed: Option<String>,
}

impl McpTool for BindInferenceTierTool {
    const NAME: &'static str = "core/bind_inference_tier";
    const DESCRIPTION: &'static str = "Bind a model tier to an inference target_ref.";
    type Args = BindInferenceTierArgs;
    type Output = BindInferenceTierOutput;

    fn call(
        ctx: McpToolCtx,
        args: BindInferenceTierArgs,
    ) -> BoxFuture<'static, Result<BindInferenceTierOutput, McpToolError>> {
        Box::pin(async move {
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
            let tier: ModelTier = match args.tier.as_str() {
                "fast" => ModelTier::Fast,
                "standard" => ModelTier::Standard,
                "deep" => ModelTier::Deep,
                _ => {
                    return Err(McpToolError::InvalidInput(format!(
                        "tier must be 'fast', 'standard', or 'deep', got '{}'",
                        args.tier
                    )));
                }
            };
            let target_ref = args.target_ref.clone();
            let req = BindInferenceTierRequest {
                owner: ctx.owner.clone(),
                tier,
                target_ref: target_ref.clone(),
            };
            let _resp = engine
                .bind_inference_tier(&Credentials::None, &req)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            let subject_id = format!("{:?}::{}", tier, target_ref);
            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::BindInferenceTier,
                PersonalityConfigChangedSubject::TierBinding(subject_id),
                None,
                Some(PersonalityConfigChangeSnapshot::TierBinding {
                    tier: format!("{:?}", tier),
                    target_ref: target_ref.clone(),
                }),
            )
            .await;
            let audit_emit_failed = match audit {
                AuditEmit::Ok => None,
                AuditEmit::Failed { reason } => Some(reason),
            };
            Ok(BindInferenceTierOutput {
                tier: args.tier,
                target_ref,
                audit_emit_failed,
            })
        })
    }
}
