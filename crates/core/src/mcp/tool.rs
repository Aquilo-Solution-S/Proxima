use futures::future::BoxFuture;

use crate::ToolCtx;

use super::{McpToolCaller, McpToolCtx, McpToolError, McpToolPresentation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpToolOrigin {
    Substrate,
    Flavor(String),
}

#[derive(Debug, Clone)]
pub struct McpToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub origin: McpToolOrigin,
    pub produces_schema_ids: &'static [&'static str],
    pub args_schema: serde_json::Value,
    pub action_arg_specs: &'static [McpActionArgSpec],
    pub call: McpCallFn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpActionArgSpec {
    pub action: &'static str,
    pub allowed_fields: &'static [&'static str],
    pub required_fields: &'static [&'static str],
}

pub(crate) fn validate_action_args(
    tool_name: &str,
    specs: &[McpActionArgSpec],
    args: &serde_json::Value,
) -> Result<(), McpToolError> {
    if specs.is_empty() {
        return Ok(());
    }
    let object = args.as_object().ok_or_else(|| {
        McpToolError::InvalidInput(format!("{tool_name} arguments must be a JSON object"))
    })?;
    let action = object
        .get("action")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            McpToolError::InvalidInput(format!(
                "{tool_name} arguments must include string field `action`"
            ))
        })?;
    let spec = specs
        .iter()
        .find(|spec| spec.action == action)
        .ok_or_else(|| {
            let supported = specs
                .iter()
                .map(|spec| spec.action)
                .collect::<Vec<_>>()
                .join(", ");
            McpToolError::InvalidInput(format!(
                "{tool_name} action `{action}` is not supported; expected one of: {supported}"
            ))
        })?;

    let allowed = spec
        .allowed_fields
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let mut unexpected = object
        .keys()
        .filter(|field| field.as_str() != "action" && !allowed.contains(field.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        unexpected.sort();
        return Err(McpToolError::InvalidInput(format!(
            "{tool_name} action `{action}` does not accept field(s): {}",
            unexpected.join(", ")
        )));
    }

    let missing = spec
        .required_fields
        .iter()
        .copied()
        .filter(|field| !object.contains_key(*field))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(McpToolError::InvalidInput(format!(
            "{tool_name} action `{action}` requires field(s): {}",
            missing.join(", ")
        )));
    }
    Ok(())
}

pub type McpCallFn = fn(
    McpToolCtx,
    serde_json::Value,
) -> BoxFuture<'static, Result<serde_json::Value, McpToolError>>;

pub trait McpTool: Send + Sync + 'static {
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[];
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[];

    type Args: serde::de::DeserializeOwned + schemars::JsonSchema + Send + 'static;
    type Output: serde::Serialize + Send + 'static;

    fn call(
        ctx: McpToolCtx,
        args: Self::Args,
    ) -> BoxFuture<'static, Result<Self::Output, McpToolError>>;
}

impl<T> McpTool for T
where
    T: crate::Tool,
{
    const NAME: &'static str = T::NAME;
    const DESCRIPTION: &'static str = T::DESCRIPTION;
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = T::PRODUCES_SCHEMA_IDS;

    type Args = T::Args;
    type Output = T::Output;

    fn call(
        ctx: McpToolCtx,
        args: Self::Args,
    ) -> BoxFuture<'static, Result<Self::Output, McpToolError>> {
        let presentation = McpToolPresentation::from_ctx(&ctx);
        let caller = McpToolCaller::from_ctx(&ctx);
        let mut services = ctx.extensions.into_tool_services();
        services.insert(presentation);
        services.insert(caller);
        let tool_ctx = ToolCtx::from_parts(
            ctx.owner,
            ctx.authz,
            ctx.registry,
            ctx.caller_self_perspective,
            services,
            ctx.engine,
        );
        Box::pin(async move { T::call(tool_ctx, args).await.map_err(Into::into) })
    }
}
