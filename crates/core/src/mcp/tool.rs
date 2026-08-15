use futures::future::BoxFuture;

use crate::{ToolCaller, ToolCtx};

use super::{McpToolCtx, McpToolError, McpToolPresentation};

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
    /// JSON Schema for the tool's reply envelope. `produces_schema_ids` names
    /// the registry payloads it writes — a different thing.
    pub output_schema: serde_json::Value,
    pub action_arg_specs: &'static [McpActionArgSpec],
    /// What a flat tool declared about its own behaviour, or `None` when it
    /// declared nothing. Substrate flat tools may still resolve through
    /// `core_tool_annotations`. Dispatcher behavior lives on
    /// `action_arg_specs`; this field is not an action fallback.
    pub annotations: Option<crate::mcp::McpToolAnnotations>,
    pub call: McpCallFn,
}

impl McpToolDescriptor {
    /// What this tool does as a whole.
    ///
    /// A dispatcher's action specs are the authority, so its whole-tool answer
    /// is a conservative aggregate: read-only only when every action is an
    /// explicit read, and other hints only when every action agrees. Flat
    /// tools resolve their own declaration, then the core manifest.
    ///
    /// `FlavorRegistry::try_freeze` guarantees this returns `Some` for every
    /// registered flat tool. A dispatcher always returns an aggregate.
    #[must_use]
    pub fn resolved_annotations(&self) -> Option<crate::mcp::McpToolAnnotations> {
        if !self.action_arg_specs.is_empty() {
            let first = self.action_arg_specs.first()?;
            let common = |field: fn(crate::mcp::McpToolAnnotations) -> Option<bool>| {
                let first = first.annotations.and_then(field);
                self.action_arg_specs
                    .iter()
                    .all(|spec| spec.annotations.and_then(field) == first)
                    .then_some(first)
                    .flatten()
            };
            return Some(crate::mcp::McpToolAnnotations {
                read_only: Some(
                    self.action_arg_specs.iter().all(|spec| {
                        spec.annotations.and_then(|value| value.read_only) == Some(true)
                    }),
                ),
                destructive: common(|value| value.destructive),
                idempotent: common(|value| value.idempotent),
                open_world: common(|value| value.open_world),
            });
        }
        self.annotations
            .or_else(|| crate::mcp::core_tool_annotations(self.name))
    }

    /// The descriptor-owned contract for one dispatcher action.
    #[must_use]
    pub fn action_arg_spec(&self, action: &str) -> Option<&McpActionArgSpec> {
        self.action_arg_specs
            .iter()
            .find(|spec| spec.action == action)
    }

    /// What one dispatcher action declares about its behaviour.
    ///
    /// There is deliberately no tool-level fallback. A dispatcher action is
    /// a separately gated call surface, and inheriting the parent declaration
    /// would make a later write action read-only under a read-only parent.
    /// Silence therefore stays fail-closed and is interpreted as a write by
    /// [`Self::action_is_read_only`].
    #[must_use]
    pub fn resolved_action_annotations(
        &self,
        action: &str,
    ) -> Option<crate::mcp::McpToolAnnotations> {
        self.action_arg_spec(action)
            .and_then(|spec| spec.annotations)
    }

    /// Whether the owner-role gate should treat one dispatcher action as a
    /// read. Missing specs and missing annotations are writes.
    #[must_use]
    pub fn action_is_read_only(&self, action: &str) -> bool {
        self.resolved_action_annotations(action)
            .and_then(|annotations| annotations.read_only)
            .unwrap_or(false)
    }

    /// Client-facing prose for one dispatcher action.
    ///
    /// Substrate actions keep their curated manifest description. Flavor
    /// actions have no substrate entry, so their enum-variant doc comment is
    /// read from the schema-derived `x-proxima-actions` extension instead.
    #[must_use]
    pub fn resolved_action_description(&self, action: &str) -> Option<&str> {
        crate::mcp::core_action_meta(self.name, action)
            .map(|meta| meta.description)
            .or_else(|| {
                self.args_schema
                    .get("x-proxima-actions")
                    .and_then(serde_json::Value::as_object)
                    .and_then(|actions| actions.get(action))
                    .and_then(|metadata| metadata.get("description"))
                    .and_then(serde_json::Value::as_str)
            })
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
    /// Behaviour of this action. `None` is deliberately a write: callers
    /// must opt into read authorization and retry-safe `QUERY` exposure.
    pub annotations: Option<crate::mcp::McpToolAnnotations>,
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
/// [`validate_action_args`] instead. A mistyped `space` on
/// `core_search_memories` would otherwise search the wrong owner.
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
    /// The actions this tool dispatches, or `&[]` for a flat tool. See
    /// [`crate::Tool::ACTION_ARG_SPECS`] — this is the single enumeration of
    /// a dispatcher's action set, and the blanket impl below forwards it
    /// from `Tool` so a flavor dispatcher declares it in exactly one place.
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[];
    /// MCP behaviour hints for a flat tool. See [`crate::Tool::ANNOTATIONS`].
    /// Dispatchers ignore this parent declaration and resolve only from their
    /// action specs. Forwarded from `Tool` by the blanket impl below.
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
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = <T as crate::Tool>::ACTION_ARG_SPECS;
    const ANNOTATIONS: Option<crate::mcp::McpToolAnnotations> = <T as crate::Tool>::ANNOTATIONS;

    type Args = T::Args;
    type Output = T::Output;

    fn call(
        ctx: McpToolCtx,
        args: Self::Args,
    ) -> BoxFuture<'static, Result<Self::Output, McpToolError>> {
        let presentation = McpToolPresentation::from_ctx(&ctx);
        let caller = ToolCaller::new(
            ctx.author.model_id.clone(),
            ctx.author.client_name.clone(),
            ctx.author.client_version.clone(),
        );
        let mut services = ctx.services.into_tool_services();
        services.insert(presentation);
        let tool_ctx = ToolCtx::from_parts(
            ctx.owner,
            ctx.authz,
            ctx.registry,
            Some(caller),
            ctx.caller_self_perspective,
            services,
            ctx.engine,
        );
        Box::pin(async move { T::call(tool_ctx, args).await.map_err(Into::into) })
    }
}

#[cfg(test)]
mod flat_tool_tests {
    use std::sync::Arc;

    use futures::future::BoxFuture;

    use super::{McpTool, prepare_flat_tool_args};
    use crate::mcp::{McpAuthorContext, McpToolCtx, McpToolError, McpToolErrorKind};
    use crate::{
        AuthPath, AuthzContext, FlavorRegistry, FlavorServices, MemoryId, OwnerRef, Tool, ToolCtx,
        ToolError, UserId,
    };

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
    struct CallerArgs {}

    #[derive(Debug, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
    struct CallerOutput {
        model_id: String,
        client_name: String,
        client_version: String,
        caller_self_perspective: Option<String>,
    }

    struct CallerTool;

    impl Tool for CallerTool {
        const NAME: &'static str = "proxima-test_caller";
        const DESCRIPTION: &'static str = "Echo generic caller context.";

        type Args = CallerArgs;
        type Output = CallerOutput;

        fn call(
            ctx: ToolCtx,
            _args: Self::Args,
        ) -> BoxFuture<'static, Result<Self::Output, ToolError>> {
            Box::pin(async move {
                let caller = ctx
                    .caller()
                    .ok_or_else(|| ToolError::Other("caller metadata missing".into()))?;
                Ok(CallerOutput {
                    model_id: caller.model_id.clone(),
                    client_name: caller.client_name.clone(),
                    client_version: caller.client_version.clone(),
                    caller_self_perspective: ctx
                        .caller_self_perspective()
                        .map(|id| id.into_inner().to_string()),
                })
            })
        }
    }

    #[tokio::test]
    async fn generic_adapter_maps_complete_caller_and_keeps_self_separate() {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let caller_self_perspective = MemoryId::new(uuid::Uuid::now_v7());
        let ctx = McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "planner/model".into(),
                client_name: "planner-client".into(),
                client_version: "2.4.1".into(),
                caller_self_perspective: Some(caller_self_perspective),
            },
            caller_self_perspective: Some(caller_self_perspective),
            services: FlavorServices::default(),
            engine: None,
        };

        let output = <CallerTool as McpTool>::call(ctx, CallerArgs {})
            .await
            .expect("adapter supplies caller context");

        assert_eq!(
            output,
            CallerOutput {
                model_id: "planner/model".into(),
                client_name: "planner-client".into(),
                client_version: "2.4.1".into(),
                caller_self_perspective: Some(caller_self_perspective.into_inner().to_string()),
            }
        );
    }

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
