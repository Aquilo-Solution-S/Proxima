use super::{
    FlavorRegistry, FlavorRegistryError, McpCallFn, McpTool, McpToolDescriptor, McpToolError,
    McpToolOrigin, Tool, mcp_tool_schema, validate_action_args,
};

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
        let call: McpCallFn = |ctx, args| {
            Box::pin(async move {
                validate_action_args(T::NAME, &[], &args)?;
                let typed: T::Args = serde_json::from_value(args)
                    .map_err(|e| McpToolError::InvalidInput(e.to_string()))?;
                let output = <T as McpTool>::call(ctx, typed).await?;
                serde_json::to_value(output).map_err(|e| McpToolError::InvalidInput(e.to_string()))
            })
        };
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
            action_arg_specs: &[],
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
        let call: McpCallFn = |ctx, args| {
            Box::pin(async move {
                validate_action_args(T::NAME, T::ACTION_ARG_SPECS, &args)?;
                let typed: T::Args = serde_json::from_value(args)
                    .map_err(|e| McpToolError::InvalidInput(e.to_string()))?;
                let output = T::call(ctx, typed).await?;
                serde_json::to_value(output).map_err(|e| McpToolError::InvalidInput(e.to_string()))
            })
        };
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
            action_arg_specs: T::ACTION_ARG_SPECS,
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
