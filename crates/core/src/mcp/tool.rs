use futures::future::BoxFuture;

use crate::ToolCtx;

use super::{McpToolCaller, McpToolCtx, McpToolError, McpToolPresentation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpToolOrigin {
    Substrate,
    Flavor(String),
}

#[derive(Clone)]
pub struct McpToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub origin: McpToolOrigin,
    pub produces_schema_ids: &'static [&'static str],
    pub args_schema: serde_json::Value,
    /// JSON Schema for what the tool answers with, derived from its `Output`
    /// type. `produces_schema_ids` names the *registry* payloads a tool
    /// writes; this describes the tool's own reply envelope, which is a
    /// different thing and was previously undescribed.
    pub output_schema: serde_json::Value,
    pub action_arg_specs: &'static [McpActionArgSpec],
    /// What the tool declared about its own behaviour, or `None` when it
    /// declared nothing. Substrate tools may still resolve through
    /// `core_tool_annotations`; a flavor tool has no other route.
    pub annotations: Option<crate::mcp::McpToolAnnotations>,
    pub call: McpCallFn,
}

impl McpToolDescriptor {
    /// What this tool does: its own declaration, then the core manifest.
    ///
    /// One resolution order for the whole substrate. Four places needed the
    /// answer — the call gate (`ScopeGateBehavior::enforce_owner_role`), the
    /// visibility gate and the `tools/list` projection in the MCP adapter,
    /// and the embedded host's tool listing — and each had its own copy of
    /// this two-step. They are supposed to agree, and one of them did not:
    /// the visibility gate asked only `core_tool_annotations`, a table over
    /// *core* names, so a read-only principal saw no flavor tool at all.
    ///
    /// `FlavorRegistry::try_freeze` guarantees this returns `Some` for every
    /// registered tool.
    #[must_use]
    pub fn resolved_annotations(&self) -> Option<crate::mcp::McpToolAnnotations> {
        self.annotations
            .or_else(|| crate::mcp::core_tool_annotations(self.name))
    }

    /// Whether the owner-role gate should treat this tool as a read.
    ///
    /// Silence means write. A tool that has not said what it does may well
    /// write, and guessing "read" would hand a viewer a mutation.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.resolved_annotations()
            .and_then(|annotations| annotations.read_only)
            .unwrap_or(false)
    }
}

impl std::fmt::Debug for McpToolDescriptor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpToolDescriptor")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("origin", &self.origin)
            .field("produces_schema_ids", &self.produces_schema_ids)
            .field("args_schema", &self.args_schema)
            .field("output_schema", &self.output_schema)
            .field("action_arg_specs", &self.action_arg_specs)
            .field("annotations", &self.annotations)
            .field("call", &"<callable>")
            .finish()
    }
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
pub(crate) fn prepare_flat_tool_args(
    tool_name: &str,
    properties: &[String],
    args: &mut serde_json::Value,
) -> Result<(), McpToolError> {
    coerce_space_aliases(tool_name, args, properties)?;
    let object = args.as_object().ok_or_else(|| {
        McpToolError::InvalidInput(format!("{tool_name} arguments must be a JSON object"))
    })?;
    let mut unexpected = object
        .keys()
        .filter(|field| !properties.iter().any(|property| property == field.as_str()))
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

/// Reconcile the `space` (scalar) vs `spaces` (array) argument names so a
/// mismatched name is coerced, not silently ignored. Driven by the tool's own
/// schema: a tool declaring `spaces` accepts a scalar `space`; a tool declaring
/// `space` accepts a scalar `spaces` or a single-element `spaces` array.
fn coerce_space_aliases(
    tool_name: &str,
    args: &mut serde_json::Value,
    properties: &[String],
) -> Result<(), McpToolError> {
    let Some(object) = args.as_object_mut() else {
        return Ok(());
    };
    let has_spaces = properties.iter().any(|property| property == "spaces");
    let has_space = properties.iter().any(|property| property == "space");
    if has_space
        && !has_spaces
        && let Some(count) = object
            .get("spaces")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len)
        && count > 1
    {
        return Err(McpToolError::InvalidInput(format!(
            "{tool_name} accepts a single `space`; received `spaces` array with {count} elements"
        )));
    }
    if has_spaces
        && !has_space
        && !object.contains_key("spaces")
        && let Some(value) = object.remove("space")
    {
        object.insert("spaces".to_string(), serde_json::Value::Array(vec![value]));
    } else if has_space
        && !has_spaces
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
    Ok(())
}

type McpCall = dyn Fn(McpToolCtx, serde_json::Value) -> BoxFuture<'static, Result<serde_json::Value, McpToolError>>
    + Send
    + Sync;

/// Copyable handle to a capture-capable, process-lifetime registered tool call.
pub type McpCallFn = &'static McpCall;

pub trait McpTool: Send + Sync + 'static {
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[];
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[];
    /// MCP behaviour hints for this tool. See [`crate::Tool::ANNOTATIONS`]
    /// — a tool that declares nothing is treated as a write.
    const ANNOTATIONS: Option<crate::mcp::McpToolAnnotations> = None;

    type Args: serde::de::DeserializeOwned + schemars::JsonSchema + Send + 'static;
    /// See [`crate::Tool::Output`] — the manifest derives an output schema
    /// from this type just as it derives the argument schema from `Args`.
    type Output: serde::Serialize + schemars::JsonSchema + Send + 'static;

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
    use crate::mcp::{McpToolError, McpToolErrorKind};

    #[test]
    fn flat_tool_rejects_unknown_field() {
        let mut args = serde_json::json!({ "query": "x", "spaces": [], "bogus": 1 });
        let err = prepare_flat_tool_args(
            "core_search_memories",
            &["query".to_string(), "spaces".to_string()],
            &mut args,
        )
        .expect_err("unknown field rejected");
        assert!(
            matches!(err, McpToolError::InvalidInput(ref m) if m.contains("does not accept field(s): bogus")),
            "got {err:?}",
        );
    }

    #[test]
    fn scalar_space_alias_coerces_to_spaces_array() {
        let mut args = serde_json::json!({ "query": "x", "space": "team" });
        prepare_flat_tool_args(
            "core_search_memories",
            &["query".to_string(), "spaces".to_string()],
            &mut args,
        )
        .expect("space alias accepted");
        assert_eq!(args["spaces"], serde_json::json!(["team"]));
        assert!(args.get("space").is_none(), "alias key removed: {args}");
    }

    #[test]
    fn multi_element_spaces_alias_is_rejected_for_scalar_space() {
        let mut args = serde_json::json!({ "body": "b", "spaces": ["team", "other"] });
        let err = prepare_flat_tool_args(
            "core_remember",
            &["body".to_string(), "space".to_string()],
            &mut args,
        )
        .expect_err("multiple spaces rejected for a scalar-space tool");
        assert_eq!(err.kind(), McpToolErrorKind::InvalidInput);
        assert!(
            matches!(err, McpToolError::InvalidInput(ref message)
                if message == "core_remember accepts a single `space`; received `spaces` array with 2 elements"),
            "got {err:?}",
        );
    }

    #[test]
    fn single_element_spaces_alias_coerces_to_scalar_space() {
        let mut args = serde_json::json!({ "body": "b", "spaces": ["team"] });
        prepare_flat_tool_args(
            "core_remember",
            &["body".to_string(), "space".to_string()],
            &mut args,
        )
        .expect("single spaces alias accepted");
        assert_eq!(args["space"], serde_json::json!("team"));
        assert!(args.get("spaces").is_none(), "alias key removed: {args}");
    }

    #[test]
    fn scalar_spaces_alias_coerces_to_scalar_space() {
        let mut args = serde_json::json!({ "body": "b", "spaces": "team" });
        prepare_flat_tool_args(
            "core_remember",
            &["body".to_string(), "space".to_string()],
            &mut args,
        )
        .expect("scalar spaces alias accepted");
        assert_eq!(args["space"], serde_json::json!("team"));
    }

    #[test]
    fn known_fields_pass() {
        let mut args = serde_json::json!({ "query": "x", "spaces": ["a"] });
        prepare_flat_tool_args(
            "core_search_memories",
            &["query".to_string(), "spaces".to_string()],
            &mut args,
        )
        .expect("known fields accepted");
    }
}
