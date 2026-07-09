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

/// Validate a *flat* (non-dispatcher) MCP tool's arguments: coerce the
/// `space`/`spaces` arity aliases against the tool's schema, then reject any
/// top-level key not declared as a schema property. Dispatcher tools run
/// [`validate_action_args`] instead; flat tools previously short-circuited on
/// empty specs and silently accepted (and dropped) unknown fields — a
/// mistyped `space` on `core_search_memories` searched the wrong owner.
pub(crate) fn prepare_flat_tool_args<A: schemars::JsonSchema>(
    tool_name: &str,
    args: &mut serde_json::Value,
) -> Result<(), McpToolError> {
    let properties = flat_tool_property_names::<A>();
    coerce_space_aliases(args, &properties);
    let object = args.as_object().ok_or_else(|| {
        McpToolError::InvalidInput(format!("{tool_name} arguments must be a JSON object"))
    })?;
    let mut unexpected = object
        .keys()
        .filter(|field| !properties.contains(field.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        unexpected.sort();
        return Err(McpToolError::InvalidInput(format!(
            "{tool_name} does not accept field(s): {}",
            unexpected.join(", ")
        )));
    }
    Ok(())
}

/// Top-level property names advertised by the flat tool's `Args` schema. This
/// is the single source of truth for the unknown-field guard (no hardcoded
/// field list to drift from the struct).
fn flat_tool_property_names<A: schemars::JsonSchema>() -> std::collections::BTreeSet<String> {
    super::schema::mcp_tool_schema::<A>()
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}

/// Reconcile the `space` (scalar) vs `spaces` (array) argument names so a
/// mismatched name is coerced, not silently ignored. Driven by the tool's own
/// schema: a tool declaring `spaces` accepts a scalar `space`; a tool declaring
/// `space` accepts a `spaces` scalar-or-array (first element wins for arrays).
fn coerce_space_aliases(
    args: &mut serde_json::Value,
    properties: &std::collections::BTreeSet<String>,
) {
    let Some(object) = args.as_object_mut() else {
        return;
    };
    if properties.contains("spaces")
        && !properties.contains("space")
        && !object.contains_key("spaces")
        && let Some(value) = object.remove("space")
    {
        object.insert("spaces".to_string(), serde_json::Value::Array(vec![value]));
    } else if properties.contains("space")
        && !properties.contains("spaces")
        && !object.contains_key("space")
        && let Some(value) = object.remove("spaces")
    {
        let scalar = match value {
            serde_json::Value::Array(items) => items.into_iter().next(),
            other => Some(other),
        };
        if let Some(scalar) = scalar {
            object.insert("space".to_string(), scalar);
        }
    }
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

#[cfg(test)]
mod flat_tool_tests {
    use super::prepare_flat_tool_args;
    use crate::mcp::McpToolError;
    use schemars::JsonSchema;

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct SearchLike {
        #[schemars(description = "the query")]
        query: String,
        #[schemars(description = "spaces")]
        spaces: Vec<String>,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct RememberLike {
        #[schemars(description = "body")]
        body: String,
        #[schemars(description = "space")]
        space: Option<String>,
    }

    #[test]
    fn flat_tool_rejects_unknown_field() {
        let mut args = serde_json::json!({ "query": "x", "spaces": [], "bogus": 1 });
        let err = prepare_flat_tool_args::<SearchLike>("core_search_memories", &mut args)
            .expect_err("unknown field rejected");
        assert!(
            matches!(err, McpToolError::InvalidInput(ref m) if m.contains("does not accept field(s): bogus")),
            "got {err:?}",
        );
    }

    #[test]
    fn scalar_space_alias_coerces_to_spaces_array() {
        let mut args = serde_json::json!({ "query": "x", "space": "team" });
        prepare_flat_tool_args::<SearchLike>("core_search_memories", &mut args)
            .expect("space alias accepted");
        assert_eq!(args["spaces"], serde_json::json!(["team"]));
        assert!(args.get("space").is_none(), "alias key removed: {args}");
    }

    #[test]
    fn spaces_alias_coerces_to_scalar_space() {
        let mut args = serde_json::json!({ "body": "b", "spaces": ["team", "other"] });
        prepare_flat_tool_args::<RememberLike>("core_remember", &mut args)
            .expect("spaces alias accepted");
        assert_eq!(args["space"], serde_json::json!("team"));
        assert!(args.get("spaces").is_none(), "alias key removed: {args}");

        let mut scalar = serde_json::json!({ "body": "b", "spaces": "team" });
        prepare_flat_tool_args::<RememberLike>("core_remember", &mut scalar)
            .expect("scalar spaces alias accepted");
        assert_eq!(scalar["space"], serde_json::json!("team"));
    }

    #[test]
    fn known_fields_pass() {
        let mut args = serde_json::json!({ "query": "x", "spaces": ["a"] });
        prepare_flat_tool_args::<SearchLike>("core_search_memories", &mut args)
            .expect("known fields accepted");
    }
}
