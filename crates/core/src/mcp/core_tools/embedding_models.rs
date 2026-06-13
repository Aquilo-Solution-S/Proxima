//! Binary-wide embedding model settings over MCP.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::authz::Role;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::{EmbeddingModelConfig, EmbeddingModelRef, McpTool};

#[derive(Debug, Default)]
pub struct ListEmbeddingModelsTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListEmbeddingModelsArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListEmbeddingModelsOutput {
    pub models: Vec<EmbeddingModelConfig>,
}

impl McpTool for ListEmbeddingModelsTool {
    const NAME: &'static str = "core/list_embedding_models";
    const DESCRIPTION: &'static str =
        "List binary-wide embedding models registered for this Proxima runtime.";
    type Args = ListEmbeddingModelsArgs;
    type Output = ListEmbeddingModelsOutput;

    fn call(
        ctx: McpToolCtx,
        _args: ListEmbeddingModelsArgs,
    ) -> BoxFuture<'static, Result<ListEmbeddingModelsOutput, McpToolError>> {
        Box::pin(async move {
            let models = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?
                .list_embedding_models()
                .await
                .map_err(McpToolError::Storage)?;
            Ok(ListEmbeddingModelsOutput { models })
        })
    }
}

#[derive(Debug, Default)]
pub struct GetEmbeddingActiveTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetEmbeddingActiveArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetEmbeddingActiveOutput {
    pub active: Option<EmbeddingModelRef>,
}

impl McpTool for GetEmbeddingActiveTool {
    const NAME: &'static str = "core/get_embedding_active";
    const DESCRIPTION: &'static str =
        "Read the binary-wide active embedding model, if one is configured.";
    type Args = GetEmbeddingActiveArgs;
    type Output = GetEmbeddingActiveOutput;

    fn call(
        ctx: McpToolCtx,
        _args: GetEmbeddingActiveArgs,
    ) -> BoxFuture<'static, Result<GetEmbeddingActiveOutput, McpToolError>> {
        Box::pin(async move {
            let active = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?
                .get_embedding_active()
                .await
                .map_err(McpToolError::Storage)?;
            Ok(GetEmbeddingActiveOutput { active })
        })
    }
}

#[derive(Debug, Default)]
pub struct RegisterEmbeddingModelTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RegisterEmbeddingModelArgs {
    pub model: EmbeddingModelConfig,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RegisterEmbeddingModelOutput {
    pub model: EmbeddingModelRef,
}

impl McpTool for RegisterEmbeddingModelTool {
    const NAME: &'static str = "core/register_embedding_model";
    const DESCRIPTION: &'static str =
        "Register one binary-wide embedding model. Use core/set_embedding_active to activate it.";
    type Args = RegisterEmbeddingModelArgs;
    type Output = RegisterEmbeddingModelOutput;

    fn call(
        ctx: McpToolCtx,
        args: RegisterEmbeddingModelArgs,
    ) -> BoxFuture<'static, Result<RegisterEmbeddingModelOutput, McpToolError>> {
        Box::pin(async move {
            crate::engine::authorize(&ctx.authz, &ctx.owner.principal, Role::Admin)
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            let model_ref = EmbeddingModelRef {
                vendor: args.model.vendor.clone(),
                model_id: args.model.model_id.clone(),
            };
            ctx.storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?
                .register_embedding_model(args.model)
                .await
                .map_err(McpToolError::Storage)?;
            Ok(RegisterEmbeddingModelOutput { model: model_ref })
        })
    }
}

#[derive(Debug, Default)]
pub struct DeleteEmbeddingModelTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteEmbeddingModelArgs {
    pub vendor: String,
    pub model_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DeleteEmbeddingModelOutput {
    pub deleted: bool,
    pub hot_reload: Option<EmbeddingHotReloadOutput>,
    pub hot_reload_error: Option<String>,
}

impl McpTool for DeleteEmbeddingModelTool {
    const NAME: &'static str = "core/delete_embedding_model";
    const DESCRIPTION: &'static str =
        "Delete one binary-wide embedding model. If it was active, the running engine reloads.";
    type Args = DeleteEmbeddingModelArgs;
    type Output = DeleteEmbeddingModelOutput;

    fn call(
        ctx: McpToolCtx,
        args: DeleteEmbeddingModelArgs,
    ) -> BoxFuture<'static, Result<DeleteEmbeddingModelOutput, McpToolError>> {
        Box::pin(async move {
            crate::engine::authorize(&ctx.authz, &ctx.owner.principal, Role::Admin)
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            let storage = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?;
            let was_active = storage
                .get_embedding_active()
                .await
                .map_err(McpToolError::Storage)?
                .is_some_and(|active| {
                    active.vendor == args.vendor && active.model_id == args.model_id
                });
            let deleted = storage
                .delete_embedding_model(&args.vendor, &args.model_id)
                .await
                .map_err(McpToolError::Storage)?;
            let (hot_reload, hot_reload_error) = if deleted && was_active {
                reload_embedding(&ctx).await
            } else {
                (None, None)
            };
            Ok(DeleteEmbeddingModelOutput {
                deleted,
                hot_reload,
                hot_reload_error,
            })
        })
    }
}

#[derive(Debug, Default)]
pub struct SetEmbeddingActiveTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetEmbeddingActiveArgs {
    pub vendor: String,
    pub model_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetEmbeddingActiveOutput {
    pub active: EmbeddingModelRef,
    pub hot_reload: Option<EmbeddingHotReloadOutput>,
    pub hot_reload_error: Option<String>,
}

impl McpTool for SetEmbeddingActiveTool {
    const NAME: &'static str = "core/set_embedding_active";
    const DESCRIPTION: &'static str = "Set the binary-wide active embedding model and hot-reload the running engine when supported.";
    type Args = SetEmbeddingActiveArgs;
    type Output = SetEmbeddingActiveOutput;

    fn call(
        ctx: McpToolCtx,
        args: SetEmbeddingActiveArgs,
    ) -> BoxFuture<'static, Result<SetEmbeddingActiveOutput, McpToolError>> {
        Box::pin(async move {
            crate::engine::authorize(&ctx.authz, &ctx.owner.principal, Role::Admin)
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            ctx.storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?
                .set_embedding_active(&args.vendor, &args.model_id)
                .await
                .map_err(McpToolError::Storage)?;
            let (hot_reload, hot_reload_error) = reload_embedding(&ctx).await;
            Ok(SetEmbeddingActiveOutput {
                active: EmbeddingModelRef {
                    vendor: args.vendor,
                    model_id: args.model_id,
                },
                hot_reload,
                hot_reload_error,
            })
        })
    }
}

#[derive(Debug, Default)]
pub struct ClearEmbeddingActiveTool;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ClearEmbeddingActiveArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ClearEmbeddingActiveOutput {
    pub cleared: bool,
    pub hot_reload: Option<EmbeddingHotReloadOutput>,
    pub hot_reload_error: Option<String>,
}

impl McpTool for ClearEmbeddingActiveTool {
    const NAME: &'static str = "core/clear_embedding_active";
    const DESCRIPTION: &'static str = "Clear the binary-wide active embedding model and hot-reload the running engine when supported.";
    type Args = ClearEmbeddingActiveArgs;
    type Output = ClearEmbeddingActiveOutput;

    fn call(
        ctx: McpToolCtx,
        _args: ClearEmbeddingActiveArgs,
    ) -> BoxFuture<'static, Result<ClearEmbeddingActiveOutput, McpToolError>> {
        Box::pin(async move {
            crate::engine::authorize(&ctx.authz, &ctx.owner.principal, Role::Admin)
                .map_err(|e| McpToolError::Other(e.to_string()))?;
            let cleared = ctx
                .storage()
                .ok_or_else(|| McpToolError::Other("engine storage unavailable".into()))?
                .clear_embedding_active()
                .await
                .map_err(McpToolError::Storage)?;
            let (hot_reload, hot_reload_error) = reload_embedding(&ctx).await;
            Ok(ClearEmbeddingActiveOutput {
                cleared,
                hot_reload,
                hot_reload_error,
            })
        })
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EmbeddingHotReloadOutput {
    pub active: bool,
    pub model_id: Option<String>,
    pub dim: Option<usize>,
}

async fn reload_embedding(ctx: &McpToolCtx) -> (Option<EmbeddingHotReloadOutput>, Option<String>) {
    let Some(engine) = ctx.engine() else {
        return (None, Some("engine unavailable".into()));
    };
    match engine.reload_embedding_client(&ctx.owner).await {
        Ok(outcome) => (
            Some(EmbeddingHotReloadOutput {
                active: outcome.active,
                model_id: outcome.model_id,
                dim: outcome.dim,
            }),
            None,
        ),
        Err(err) => (None, Some(err.to_string())),
    }
}
