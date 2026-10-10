//! Freeze checks over MCP tool descriptors: that every tool declares a
//! behavior, and that a dispatcher tool's per-action specs agree with the
//! argument schema its own registration derived.
//!
//! A dispatcher flattens every action into one argument schema, so the
//! per-action field lists and the schema are two descriptions of the same
//! object written apart from each other. Validating them here means a
//! mismatch is a build-time refusal rather than a `-32602` at the first
//! call that happens to name the drifted action.

use super::{
    BTreeSet, FlavorRegistry, FlavorRegistryError, McpToolDescriptor, analyze_root_fields,
    schema_contains_defs, schema_contains_ref, validate_closed_root_schema,
};

impl FlavorRegistry {
    /// Cross-check: every registered tool has exactly one behaviour
    /// declaration.
    ///
    /// A flat tool's is its `EFFECT`; without it the owner-role gate cannot
    /// tell a read from a write and demands write access. A dispatcher's is
    /// one `effect` per action, and a tool-level `EFFECT` beside them would
    /// be a second answer nothing reads.
    pub(super) fn validate_tools_declare_behavior(&self) -> Result<(), FlavorRegistryError> {
        for tool in &self.mcp_tools {
            let dispatcher =
                !tool.action_arg_specs.is_empty() || !tool.argv_action_specs.is_empty();
            match (dispatcher, tool.effect) {
                (false, None) => {
                    return Err(FlavorRegistryError::UndeclaredToolBehavior { name: tool.name });
                }
                (true, Some(_)) => {
                    return Err(FlavorRegistryError::DispatcherToolEffect { name: tool.name });
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Cross-check: every tool name and every action name is a scope key.
    ///
    /// The scope gate and every palette identify a tool by
    /// [`ScopeKey`](crate::ScopeKey)s built from these names
    /// ([`McpToolDescriptor::palette_keys`] trusts this ran). An action outside
    /// the grammar, or a dispatcher called `resource`, is refused here and
    /// not at the first palette build.
    pub(super) fn validate_scope_keys(&self) -> Result<(), FlavorRegistryError> {
        for tool in &self.mcp_tools {
            tool.scope_keys()
                .map_err(|reason| FlavorRegistryError::InvalidScopeKey {
                    name: tool.name,
                    reason,
                })?;
        }
        Ok(())
    }

    /// Cross-check: a dispatcher's declared actions and its derived schema
    /// describe the same dispatcher.
    ///
    /// `McpToolDescriptor::action_arg_specs` is the one enumeration.
    /// Discriminator must be `action`: `ToolScope` keys are `"{tool}:{action}"`,
    /// validators and the scope gate read `args["action"]`, REST injects
    /// `"action"` before dispatch.
    pub(super) fn validate_dispatcher_action_specs(&self) -> Result<(), FlavorRegistryError> {
        for tool in &self.mcp_tools {
            let Some(dispatcher) = &tool.dispatcher_schema else {
                if tool.action_arg_specs.is_empty() {
                    continue;
                }
                return Err(FlavorRegistryError::InvalidActionSpecs {
                    name: tool.name,
                    message: "declares ACTION_ARG_SPECS but its `Args` produced no action \
                              schema: either it is not an internally tagged enum, or one of \
                              its variants did not flatten — the flattener needs every variant \
                              to be an object schema carrying a string `const` at the \
                              discriminator. Either way there is nothing to validate against"
                        .to_string(),
                });
            };
            if tool.action_arg_specs.is_empty() {
                return Err(FlavorRegistryError::DispatcherWithoutActionSpecs { name: tool.name });
            }
            if dispatcher.discriminator != "action" {
                return Err(FlavorRegistryError::InvalidActionSpecs {
                    name: tool.name,
                    message: format!(
                        "dispatcher discriminator is {:?}; a dispatcher must tag on \
                         `action` (#[serde(tag = \"action\")]) because scope keys, the scope \
                         gate, and the REST action routes all read that field",
                        dispatcher.discriminator
                    ),
                });
            }
            let declared = tool
                .action_arg_specs
                .iter()
                .map(|spec| spec.action)
                .collect::<BTreeSet<_>>();
            // Length before set equality: a set collapses duplicate action
            // names. The later spec is never read (`validate_action_args`
            // and the scope gate take the first match).
            if tool.action_arg_specs.len() != declared.len() {
                return Err(FlavorRegistryError::InvalidActionSpecs {
                    name: tool.name,
                    message: format!(
                        "ACTION_ARG_SPECS holds {} specs naming {} distinct actions, so an \
                         action name is duplicated; the later spec for a duplicate action is \
                         never read",
                        tool.action_arg_specs.len(),
                        declared.len()
                    ),
                });
            }
            let derived = dispatcher
                .actions
                .iter()
                .map(|action| action.action.as_str())
                .collect::<BTreeSet<_>>();
            if declared != derived {
                return Err(FlavorRegistryError::InvalidActionSpecs {
                    name: tool.name,
                    message: format!(
                        "ACTION_ARG_SPECS names {declared:?} but the derived schema names \
                         {derived:?}"
                    ),
                });
            }
            validate_action_field_sets(tool, dispatcher)?;
        }
        Ok(())
    }
}

/// The per-action half of [`FlavorRegistry::validate_dispatcher_action_specs`]:
/// each spec's field lists against the ones the schema derived for that action.
/// The action sets are known to agree by the time this runs, so `dispatcher`
/// has an action for every spec.
fn validate_action_field_sets(
    tool: &McpToolDescriptor,
    dispatcher: &crate::mcp::McpDispatcherSchema,
) -> Result<(), FlavorRegistryError> {
    for spec in tool.action_arg_specs {
        let argument_schema = &dispatcher
            .action(spec.action)
            .expect("the action sets agree")
            .argument_schema;
        let derived = derived_action_fields(tool, spec, argument_schema)?;
        validate_declared_action_fields(
            tool,
            spec,
            "allowed_fields",
            spec.allowed_fields,
            &derived.allowed.iter().map(String::as_str).collect(),
        )?;
        validate_declared_action_fields(
            tool,
            spec,
            "required_fields",
            spec.required_fields,
            &derived.required.iter().map(String::as_str).collect(),
        )?;
        validate_derived_required_are_allowed(tool, spec, &derived)?;
    }
    Ok(())
}

/// The field vocabulary an action's `argument_schema` describes, once the
/// schema is known to be closed and ref-free: a `$ref` or `$defs` would put
/// part of the vocabulary in a subtree the dispatcher never resolves.
fn derived_action_fields(
    tool: &McpToolDescriptor,
    spec: &crate::mcp::McpActionArgSpec,
    argument_schema: &serde_json::Value,
) -> Result<crate::mcp::schema::RootFieldAnalysis, FlavorRegistryError> {
    validate_closed_root_schema(argument_schema).map_err(|message| {
        FlavorRegistryError::InvalidActionSpecs {
            name: tool.name,
            message: format!(
                "action {} argument_schema is invalid: {message}",
                spec.action
            ),
        }
    })?;
    if schema_contains_ref(argument_schema) || schema_contains_defs(argument_schema) {
        return Err(FlavorRegistryError::InvalidActionSpecs {
            name: tool.name,
            message: format!("action {} argument_schema must be ref-free", spec.action),
        });
    }
    analyze_root_fields(argument_schema).map_err(|message| {
        FlavorRegistryError::InvalidActionSpecs {
            name: tool.name,
            message: format!(
                "action {} argument_schema is invalid: {message}",
                spec.action
            ),
        }
    })
}

/// One declared field list on the spec (`allowed_fields` or `required_fields`)
/// against the set the schema derives.
fn validate_declared_action_fields(
    tool: &McpToolDescriptor,
    spec: &crate::mcp::McpActionArgSpec,
    key: &str,
    fields: &[&str],
    expected_fields: &BTreeSet<&str>,
) -> Result<(), FlavorRegistryError> {
    let declared_fields = fields.iter().copied().collect::<BTreeSet<_>>();
    if fields
        .iter()
        .any(|field| field.is_empty() || *field == "action")
    {
        return Err(FlavorRegistryError::InvalidActionSpecs {
            name: tool.name,
            message: format!(
                "action {} declares an empty or root action field in {key}",
                spec.action
            ),
        });
    }
    if fields.len() != declared_fields.len() {
        return Err(FlavorRegistryError::InvalidActionSpecs {
            name: tool.name,
            message: format!("action {} contains duplicate fields in {key}", spec.action),
        });
    }
    if &declared_fields != expected_fields {
        return Err(FlavorRegistryError::InvalidActionSpecs {
            name: tool.name,
            message: format!(
                "action {} declares {key} {declared_fields:?} but the argument_schema \
                 says {expected_fields:?}",
                spec.action
            ),
        });
    }
    Ok(())
}

/// The schema's own two sets against each other: a required field the schema
/// does not allow is a call surface nothing can satisfy.
fn validate_derived_required_are_allowed(
    tool: &McpToolDescriptor,
    spec: &crate::mcp::McpActionArgSpec,
    derived: &crate::mcp::schema::RootFieldAnalysis,
) -> Result<(), FlavorRegistryError> {
    if !derived
        .required
        .iter()
        .all(|field| derived.allowed.iter().any(|name| name == field))
    {
        return Err(FlavorRegistryError::InvalidActionSpecs {
            name: tool.name,
            message: format!(
                "action {} argument_schema required fields are not allowed",
                spec.action
            ),
        });
    }
    Ok(())
}
