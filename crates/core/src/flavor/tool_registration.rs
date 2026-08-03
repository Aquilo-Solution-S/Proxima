use super::{
    FlavorRegistry, FlavorRegistryError, McpCallFn, McpTool, McpToolDescriptor, McpToolError,
    McpToolOrigin, Tool, mcp_output_schema, mcp_tool_schema, validate_action_args,
};
use crate::mcp::prepare_flat_tool_args;
use crate::mcp::schema::undescribed_property_names;
use futures::future::BoxFuture;

/// Top-level property names advertised by the flat tool's `Args` schema. This
/// is the single source of truth for the unknown-field guard (no hardcoded
/// field list to drift from the struct).
fn flat_tool_property_names(args_schema: &serde_json::Value) -> std::sync::Arc<[String]> {
    args_schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .map(|properties| properties.keys().cloned().collect::<Vec<_>>().into())
        .unwrap_or_default()
}

/// Warn (never fail) on MCP tool properties that ship without a description.
/// Downstream flavors must not be hard-blocked by this lint, so it only logs.
fn warn_undescribed_properties(tool_name: &str, args_schema: &serde_json::Value) {
    for field in undescribed_property_names(args_schema) {
        tracing::warn!(
            tool = tool_name,
            field = %field,
            "MCP tool property has no description",
        );
    }
}

impl FlavorRegistry {
    /// # Errors
    ///
    /// Returns `InvalidToolName` when the tool name does not match the expected
    /// prefix or provider-safe form.
    pub fn try_add_tool<T: Tool>(
        &mut self,
        expected_prefix: &str,
    ) -> Result<(), FlavorRegistryError> {
        let slash = format!("{expected_prefix}/");
        let under = format!("{expected_prefix}_");
        validate_tool_name(T::NAME, expected_prefix, &slash, &under)?;
        let args_schema = mcp_tool_schema::<T::Args>();
        let output_schema = mcp_output_schema::<T::Output>();
        warn_undescribed_properties(T::NAME, &args_schema);
        let properties = flat_tool_property_names(&args_schema);
        // Tool registrations live for the process lifetime; leaking the
        // closure preserves the descriptor's copyable call-handle semantics.
        let call: McpCallFn = Box::leak(Box::new(move |ctx, mut args| -> BoxFuture<'static, _> {
            let properties = properties.clone();
            Box::pin(async move {
                prepare_flat_tool_args(T::NAME, &properties, &mut args)?;
                let typed: T::Args = serde_json::from_value(args)
                    .map_err(|e| McpToolError::InvalidInput(e.to_string()))?;
                let output = <T as McpTool>::call(ctx, typed).await?;
                serde_json::to_value(output).map_err(|e| McpToolError::InvalidInput(e.to_string()))
            })
        }));
        self.mcp_tools.push(McpToolDescriptor {
            name: T::NAME,
            description: T::DESCRIPTION,
            origin: if expected_prefix == "core" {
                McpToolOrigin::Substrate
            } else {
                McpToolOrigin::Flavor(expected_prefix.to_string())
            },
            produces_schema_ids: T::PRODUCES_SCHEMA_IDS,
            args_schema,
            output_schema,
            action_arg_specs: &[],
            annotations: <T as Tool>::ANNOTATIONS,
            call,
        });
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_tool_or_panic_for_tests<T: Tool>(&mut self, expected_prefix: &str) {
        self.try_add_tool::<T>(expected_prefix)
            .expect("tool registration must be valid");
    }

    /// Register a flavor-shipped MCP tool under `expected_prefix`.
    #[doc(hidden)]
    pub fn try_add_mcp_tool<T: McpTool>(
        &mut self,
        expected_prefix: &str,
    ) -> Result<(), FlavorRegistryError> {
        let slash = format!("{expected_prefix}/");
        let under = format!("{expected_prefix}_");
        validate_tool_name(T::NAME, expected_prefix, &slash, &under)?;
        let args_schema = mcp_tool_schema::<T::Args>();
        let output_schema = mcp_output_schema::<T::Output>();
        warn_undescribed_properties(T::NAME, &args_schema);
        let properties = flat_tool_property_names(&args_schema);
        // Tool registrations live for the process lifetime; leaking the
        // closure preserves the descriptor's copyable call-handle semantics.
        let call: McpCallFn = Box::leak(Box::new(move |ctx, mut args| -> BoxFuture<'static, _> {
            let properties = properties.clone();
            Box::pin(async move {
                // Dispatcher tools (non-empty specs) run per-action validation;
                // flat McpTools run the flat unknown-field + space-alias guard
                // instead of silently accepting unknown fields.
                if T::ACTION_ARG_SPECS.is_empty() {
                    prepare_flat_tool_args(T::NAME, &properties, &mut args)?;
                } else {
                    validate_action_args(T::NAME, T::ACTION_ARG_SPECS, &args)?;
                }
                let typed: T::Args = serde_json::from_value(args)
                    .map_err(|e| McpToolError::InvalidInput(e.to_string()))?;
                let output = T::call(ctx, typed).await?;
                serde_json::to_value(output).map_err(|e| McpToolError::InvalidInput(e.to_string()))
            })
        }));
        self.mcp_tools.push(McpToolDescriptor {
            name: T::NAME,
            description: T::DESCRIPTION,
            origin: if expected_prefix == "core" {
                McpToolOrigin::Substrate
            } else {
                McpToolOrigin::Flavor(expected_prefix.to_string())
            },
            produces_schema_ids: T::PRODUCES_SCHEMA_IDS,
            args_schema,
            output_schema,
            action_arg_specs: T::ACTION_ARG_SPECS,
            annotations: <T as McpTool>::ANNOTATIONS,
            call,
        });
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_mcp_tool_or_panic_for_tests<T: McpTool>(&mut self, expected_prefix: &str) {
        self.try_add_mcp_tool::<T>(expected_prefix)
            .expect("MCP tool registration must be valid");
    }
}

fn validate_tool_name(
    name: &'static str,
    expected_prefix: &str,
    slash: &str,
    under: &str,
) -> Result<(), FlavorRegistryError> {
    if !(name.starts_with(slash) || name.starts_with(under)) {
        return Err(FlavorRegistryError::InvalidToolName {
            name,
            expected_prefix: expected_prefix.to_string(),
            message: format!("expected prefix {slash:?} or {under:?}"),
        });
    }
    let provider_safe = crate::mcp::provider_safe_tool_name(name);
    if name != provider_safe {
        return Err(FlavorRegistryError::InvalidToolName {
            name,
            expected_prefix: expected_prefix.to_string(),
            message: format!(
                "tool name must be provider-safe; normalized form is {provider_safe:?}"
            ),
        });
    }
    Ok(())
}
