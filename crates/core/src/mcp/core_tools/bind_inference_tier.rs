//! `core/bind_inference_tier` — wraps Engine's same-name verb.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::McpTool;
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
                principal: ctx.owner.principal.clone(),
                org_id: None,
                tier,
                target_ref: target_ref.clone(),
            };
            let _resp = engine
                .bind_inference_tier(&ctx.authz, &req)
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            let subject_id = format!("{tier:?}::{target_ref}");
            let audit = emit_personality_config_changed(
                &ctx,
                PersonalityConfigChangedVerb::BindInferenceTier,
                PersonalityConfigChangedSubject::TierBinding(subject_id),
                None,
                Some(PersonalityConfigChangeSnapshot::TierBinding {
                    tier: format!("{tier:?}"),
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::authz::{AuthPath, AuthzContext, RoleSet};
    use crate::mcp::{HandleTable, McpAuthorContext, OutputMode};
    use crate::verbs::query::MemoryStore;
    use crate::{Engine, FlavorRegistry, OrgId, Owner, Principal, UserId};

    fn make_ctx() -> McpToolCtx {
        let owner = Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        };
        let pool = sqlx::PgPool::connect_lazy("postgres://x/x").expect("lazy");
        McpToolCtx {
            pool,
            owner: owner.clone(),
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: Some(Arc::new(HandleTable::new())),
            mode: OutputMode::Handles,
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "t".into(),
                client_name: "t".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            engine: Some(Arc::new(Engine::new(
                FlavorRegistry::new().freeze(),
                MemoryStore::new(),
            ))),
        }
    }

    #[tokio::test]
    async fn wake_shaped_context_is_denied_admin_verbs() {
        let mut ctx = make_ctx();
        ctx.authz.capabilities.roles = RoleSet {
            graph_read: true,
            graph_write: true,
            source_ingest: false,
            admin: false,
        };
        let err = BindInferenceTierTool::call(
            ctx,
            BindInferenceTierArgs {
                tier: "fast".into(),
                target_ref: "t".into(),
            },
        )
        .await
        .expect_err("non-admin context must be denied");
        assert!(err.to_string().contains("requires admin role"));
    }
}
