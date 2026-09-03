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

fn serialize_tool_output<T: serde::Serialize>(
    output: T,
) -> Result<serde_json::Value, McpToolError> {
    serde_json::to_value(output)
        .map_err(|err| McpToolError::Other(format!("serialize tool output: {err}")))
}

impl FlavorRegistry {
    /// Register a flavor-shipped [`Tool`] under `expected_prefix`.
    /// Same path as [`Self::try_add_mcp_tool`] (blanket `impl<T: Tool> McpTool
    /// for T`). A second body would let a flavor dispatcher land with empty
    /// `action_arg_specs` and validate as flat.
    ///
    /// # Errors
    ///
    /// Returns `InvalidToolName` when the tool name does not match the expected
    /// prefix or provider-safe form.
    pub fn try_add_tool<T: Tool>(
        &mut self,
        expected_prefix: &str,
    ) -> Result<(), FlavorRegistryError> {
        self.try_add_mcp_tool::<T>(expected_prefix)
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
        // One action vocabulary per tool, decided here rather than at the
        // first call: were both spec kinds live, the scope gate, the
        // catalog, and the validator would each have to pick which one
        // names an action, and any disagreement would be a runtime
        // surprise instead of a registration error.
        if !T::ACTION_ARG_SPECS.is_empty() && !T::ARGV_ACTION_SPECS.is_empty() {
            return Err(FlavorRegistryError::ConflictingActionVocabularies { name: T::NAME });
        }
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
                // argv-keyed tools resolve the action key (closed set — argv
                // matching no declared prefix is refused here) and leave flag
                // validation to their own dispatch; flat McpTools run the flat
                // unknown-field + space-alias guard instead of silently
                // accepting unknown fields.
                if !T::ACTION_ARG_SPECS.is_empty() {
                    validate_action_args(T::NAME, T::ACTION_ARG_SPECS, &args)?;
                } else if !T::ARGV_ACTION_SPECS.is_empty() {
                    crate::mcp::resolve_argv_action(T::NAME, T::ARGV_ACTION_SPECS, &args)?;
                } else {
                    prepare_flat_tool_args(T::NAME, &properties, &mut args)?;
                }
                let typed: T::Args = serde_json::from_value(args)
                    .map_err(|e| McpToolError::InvalidInput(e.to_string()))?;
                let output = T::call(ctx, typed).await?;
                serialize_tool_output(output)
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
            argv_action_specs: T::ARGV_ACTION_SPECS,
            annotations: <T as McpTool>::ANNOTATIONS,
            audience: <T as McpTool>::AUDIENCE,
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

#[cfg(test)]
mod tests {
    use serde::Serializer;

    use super::serialize_tool_output;
    use crate::mcp::{McpToolError, McpToolErrorKind};

    struct FailingOutput;

    impl serde::Serialize for FailingOutput {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Err(serde::ser::Error::custom(
                "fixture secret from output serializer",
            ))
        }
    }

    #[test]
    fn output_serialization_failure_is_internal_and_redacted() {
        let err = serialize_tool_output(FailingOutput)
            .expect_err("a failing output serializer must fail the tool call");
        assert!(
            matches!(
                &err,
                McpToolError::Other(message)
                    if message == "serialize tool output: fixture secret from output serializer"
            ),
            "the internal diagnostic must retain enough context for server logs: {err:?}",
        );
        assert_eq!(err.kind(), McpToolErrorKind::Internal);
        assert_eq!(err.client_message(), "internal server error");
    }
}

#[cfg(test)]
mod action_vocabulary_tests {
    use std::sync::Arc;

    use futures::future::BoxFuture;

    use crate::mcp::{
        McpActionArgSpec, McpArgvActionSpec, McpAuthorContext, McpTool, McpToolAnnotations,
        McpToolAudience, McpToolCtx, McpToolError, McpToolErrorKind,
    };
    use crate::{
        AuthPath, AuthzContext, FlavorRegistry, FlavorRegistryError, FlavorServices, OwnerRef,
        UserId,
    };

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
    struct ArgvArgs {
        #[schemars(description = "command words followed by flags")]
        argv: Vec<String>,
    }

    const ARGV_SPECS: &[McpArgvActionSpec] = &[
        McpArgvActionSpec {
            action: "approval",
            argv_prefix: &["approval"],
            annotations: Some(McpToolAnnotations::new().read_only(true).open_world(false)),
            audience: McpToolAudience::Shared,
        },
        McpArgvActionSpec {
            action: "approval-decide",
            argv_prefix: &["approval", "decide"],
            annotations: None,
            audience: McpToolAudience::Owner,
        },
    ];

    /// An argv-keyed dispatcher: CLI-grammar args, actions derived from
    /// `argv` by longest-prefix match.
    struct ArgvTool;

    impl McpTool for ArgvTool {
        const NAME: &'static str = "proxima-stub_cli";
        const DESCRIPTION: &'static str = "An argv-keyed fixture dispatcher.";
        const ARGV_ACTION_SPECS: &'static [McpArgvActionSpec] = ARGV_SPECS;
        const ANNOTATIONS: Option<McpToolAnnotations> =
            Some(McpToolAnnotations::new().read_only(false).open_world(false));
        type Args = ArgvArgs;
        type Output = Vec<String>;
        fn call(
            _ctx: McpToolCtx,
            args: Self::Args,
        ) -> BoxFuture<'static, Result<Vec<String>, McpToolError>> {
            Box::pin(async move { Ok(args.argv) })
        }
    }

    /// A tool claiming both vocabularies at once.
    struct BothVocabularies;

    impl McpTool for BothVocabularies {
        const NAME: &'static str = "proxima-stub_both";
        const DESCRIPTION: &'static str = "A fixture declaring both spec kinds.";
        const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[McpActionArgSpec {
            action: "look",
            allowed_fields: &["id"],
            required_fields: &["id"],
            annotations: Some(McpToolAnnotations::new().read_only(true).open_world(false)),
            audience: McpToolAudience::Shared,
        }];
        const ARGV_ACTION_SPECS: &'static [McpArgvActionSpec] = &[McpArgvActionSpec {
            action: "look",
            argv_prefix: &["look"],
            annotations: None,
            audience: McpToolAudience::Shared,
        }];
        type Args = ArgvArgs;
        type Output = ();
        fn call(_: McpToolCtx, _: Self::Args) -> BoxFuture<'static, Result<(), McpToolError>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// A flat tool declaring the tool-level owner-only class. Written
    /// against `Tool`, not `McpTool`, so the round-trip below also proves
    /// the blanket impl forwards the new consts the way it forwards
    /// `ACTION_ARG_SPECS`.
    struct OwnerOnlyFlatTool;

    impl crate::Tool for OwnerOnlyFlatTool {
        const NAME: &'static str = "proxima-stub_admin";
        const DESCRIPTION: &'static str = "A flat fixture in the owner-only class.";
        const AUDIENCE: McpToolAudience = McpToolAudience::Owner;
        const ANNOTATIONS: Option<McpToolAnnotations> =
            Some(McpToolAnnotations::new().read_only(false).open_world(false));
        type Args = ArgvArgs;
        type Output = ();
        fn call(
            _: crate::ToolCtx,
            _: Self::Args,
        ) -> BoxFuture<'static, Result<(), crate::ToolError>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// Declaring both spec kinds is refused at registration, where the
    /// author reads the error — not at the first call that has to guess
    /// which vocabulary names an action.
    #[test]
    fn registration_rejects_a_tool_declaring_both_spec_kinds() {
        let mut registry = FlavorRegistry::new();
        let err = registry
            .try_add_mcp_tool::<BothVocabularies>("proxima-stub")
            .expect_err("both vocabularies must not register");
        assert!(
            matches!(
                err,
                FlavorRegistryError::ConflictingActionVocabularies {
                    name: "proxima-stub_both"
                }
            ),
            "got {err:?}",
        );
        // Positive control: the same grammar under one vocabulary registers.
        registry
            .try_add_mcp_tool::<ArgvTool>("proxima-stub")
            .expect("an argv-only tool registers");
    }

    /// The descriptor is the one artifact every seam reads, so the audience
    /// has to survive registration at both levels: per argv action and on
    /// the tool.
    #[test]
    fn descriptor_carries_the_audience_at_both_levels() {
        let mut registry = FlavorRegistry::new();
        registry.add_mcp_tool_or_panic_for_tests::<ArgvTool>("proxima-stub");
        registry.add_mcp_tool_or_panic_for_tests::<OwnerOnlyFlatTool>("proxima-stub");
        let frozen = registry.freeze_or_panic_for_tests();

        let argv_tool = frozen
            .mcp_tool(ArgvTool::NAME)
            .expect("argv tool registered");
        assert_eq!(argv_tool.argv_action_specs, ARGV_SPECS);
        assert_eq!(
            argv_tool.audience,
            McpToolAudience::Shared,
            "an undeclared tool-level audience stays Shared"
        );
        let by_action = |action: &str| {
            argv_tool
                .argv_action_specs
                .iter()
                .find(|spec| spec.action == action)
                .expect("declared action present")
                .audience
        };
        // Positive control and subject side by side: one action owner-only,
        // its sibling shared, so an audience that leaked tool-wide fails.
        assert_eq!(by_action("approval"), McpToolAudience::Shared);
        assert_eq!(by_action("approval-decide"), McpToolAudience::Owner);

        let flat = frozen
            .mcp_tool(OwnerOnlyFlatTool::NAME)
            .expect("flat tool registered");
        assert_eq!(
            flat.audience,
            McpToolAudience::Owner,
            "a flat tool's AUDIENCE declaration round-trips"
        );
        assert!(flat.argv_action_specs.is_empty());
    }

    /// The substrate's own membership dispatcher declares the owner
    /// audience — the descriptor statement hosts partition on instead of
    /// hardcoding the tool's name.
    #[test]
    fn core_membership_declares_the_owner_audience() {
        let frozen = FlavorRegistry::new().freeze_or_panic_for_tests();
        let membership = frozen
            .mcp_tool(crate::protocol::tool::CORE_MEMBERSHIP)
            .expect("core_membership is registered");
        assert_eq!(membership.audience, McpToolAudience::Owner);
    }

    fn test_ctx() -> McpToolCtx {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "test".into(),
                trusted_model_id: None,
                client_name: "test".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            services: FlavorServices::default(),
            engine: None,
        }
    }

    /// The registered call handle runs the same closed-set resolution the
    /// scope gate does: matched argv reaches the tool, unmatched argv is a
    /// validation error before decode.
    #[tokio::test]
    async fn dispatch_validates_argv_against_the_declared_commands() {
        let mut registry = FlavorRegistry::new();
        registry.add_mcp_tool_or_panic_for_tests::<ArgvTool>("proxima-stub");
        let frozen = registry.freeze_or_panic_for_tests();
        let descriptor = frozen.mcp_tool(ArgvTool::NAME).expect("registered");

        let output = (descriptor.call)(
            test_ctx(),
            serde_json::json!({ "argv": ["approval", "decide", "--id", "7"] }),
        )
        .await
        .expect("declared command dispatches");
        assert_eq!(
            output,
            serde_json::json!(["approval", "decide", "--id", "7"])
        );

        let err = (descriptor.call)(test_ctx(), serde_json::json!({ "argv": ["unknown"] }))
            .await
            .expect_err("unmatched argv is refused before the tool runs");
        assert_eq!(err.kind(), McpToolErrorKind::InvalidInput);
    }
}
