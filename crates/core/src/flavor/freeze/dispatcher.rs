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
    /// Cross-check: the owner-role gate can classify every registered flat
    /// tool.
    ///
    /// `ScopeGateBehavior::enforce_owner_role` demands WRITE when it cannot
    /// tell. Same two steps, same order: the tool's `ANNOTATIONS`, then the
    /// core manifest. Unclassified is silently a write. Dispatcher actions
    /// missing per-action annotations classify as writes.
    pub(super) fn validate_tools_declare_behavior(&self) -> Result<(), FlavorRegistryError> {
        for tool in &self.mcp_tools {
            if tool.action_arg_specs.is_empty()
                && tool.annotations.is_none()
                && crate::mcp::core_tool_annotations(tool.name).is_none()
            {
                return Err(FlavorRegistryError::UndeclaredToolBehavior { name: tool.name });
            }
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
            // Absent and malformed are different answers. Reading them as one
            // — `.and_then(Value::as_object)` — lets a schema that *carries*
            // the extension, and so may well be read as a dispatcher by
            // anything less forgiving, pass here as a flat tool. The derive
            // always writes an object; a hand-written `JsonSchema` need not.
            let extension = match tool.args_schema.get("x-proxima-actions") {
                Some(serde_json::Value::Object(extension)) => Some(extension),
                Some(malformed) => {
                    return Err(FlavorRegistryError::InvalidActionSpecs {
                        name: tool.name,
                        message: format!(
                            "its schema carries a malformed `x-proxima-actions` extension: \
                             expected an object keyed by action name, found {malformed}. \
                             Nothing can enumerate the actions of an extension it cannot read"
                        ),
                    });
                }
                None => None,
            };
            let Some(extension) = extension else {
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
            // The flattener writes `required = [discriminator]`, so the tag
            // name is readable straight off the normalized schema.
            let discriminator = tool
                .args_schema
                .get("required")
                .and_then(serde_json::Value::as_array)
                .and_then(|required| required.first())
                .and_then(serde_json::Value::as_str);
            if discriminator != Some("action") {
                return Err(FlavorRegistryError::InvalidActionSpecs {
                    name: tool.name,
                    message: format!(
                        "dispatcher discriminator is {discriminator:?}; a dispatcher must tag on \
                         `action` (#[serde(tag = \"action\")]) because scope keys, the scope \
                         gate, and the REST action routes all read that field"
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
            let derived = extension
                .keys()
                .map(String::as_str)
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
            validate_action_field_sets(tool, extension)?;
        }
        Ok(())
    }
}

/// The per-action half of [`FlavorRegistry::validate_dispatcher_action_specs`]:
/// each spec's field lists against the ones the schema derived for that action.
/// The action sets are known to agree by the time this runs, so `extension`
/// has a key for every spec.
fn validate_action_field_sets(
    tool: &McpToolDescriptor,
    extension: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), FlavorRegistryError> {
    for spec in tool.action_arg_specs {
        let meta = action_metadata(tool, extension, spec)?;
        let argument_schema = action_argument_schema(tool, meta, spec)?;
        let derived = derived_action_fields(tool, spec, argument_schema)?;
        for (key, fields) in [
            ("allowed_fields", spec.allowed_fields),
            ("required_fields", spec.required_fields),
        ] {
            let expected = if key == "allowed_fields" {
                &derived.allowed
            } else {
                &derived.required
            };
            let expected_fields = expected.iter().map(String::as_str).collect::<BTreeSet<_>>();
            validate_declared_action_fields(tool, spec, key, fields, &expected_fields)?;
            validate_action_metadata_fields(tool, spec, key, meta, &expected_fields)?;
        }
        validate_derived_required_are_allowed(tool, spec, &derived)?;
    }
    Ok(())
}

/// The derived metadata block for one action. The action sets agreeing
/// guarantees the key is present, not that it holds an object.
fn action_metadata<'a>(
    tool: &McpToolDescriptor,
    extension: &'a serde_json::Map<String, serde_json::Value>,
    spec: &crate::mcp::McpActionArgSpec,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, FlavorRegistryError> {
    extension
        .get(spec.action)
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| FlavorRegistryError::InvalidActionSpecs {
            name: tool.name,
            message: format!("action {} metadata must be an object", spec.action),
        })
}

/// The `argument_schema` an action's derived metadata advertises.
fn action_argument_schema<'a>(
    tool: &McpToolDescriptor,
    meta: &'a serde_json::Map<String, serde_json::Value>,
    spec: &crate::mcp::McpActionArgSpec,
) -> Result<&'a serde_json::Value, FlavorRegistryError> {
    meta.get("argument_schema")
        .ok_or_else(|| FlavorRegistryError::InvalidActionSpecs {
            name: tool.name,
            message: format!("action {} metadata is missing argument_schema", spec.action),
        })
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

/// The same field list as the derived metadata spells it, against the set the
/// schema derives: the metadata is what the wire reads, so it drifting from
/// the schema is as wrong as the spec drifting from it.
fn validate_action_metadata_fields(
    tool: &McpToolDescriptor,
    spec: &crate::mcp::McpActionArgSpec,
    key: &str,
    meta: &serde_json::Map<String, serde_json::Value>,
    expected_fields: &BTreeSet<&str>,
) -> Result<(), FlavorRegistryError> {
    let Some(metadata_fields) = meta.get(key).and_then(serde_json::Value::as_array) else {
        return Err(FlavorRegistryError::InvalidActionSpecs {
            name: tool.name,
            message: format!("action {} metadata is missing {key}", spec.action),
        });
    };
    let mut metadata_names = BTreeSet::new();
    for field in metadata_fields {
        let Some(field) = field.as_str() else {
            return Err(FlavorRegistryError::InvalidActionSpecs {
                name: tool.name,
                message: format!(
                    "action {} metadata {key} must contain only strings",
                    spec.action
                ),
            });
        };
        if field.is_empty() || field == "action" || !metadata_names.insert(field) {
            return Err(FlavorRegistryError::InvalidActionSpecs {
                name: tool.name,
                message: format!(
                    "action {} metadata {key} contains an empty, root action, or duplicate field",
                    spec.action
                ),
            });
        }
    }
    if &metadata_names != expected_fields {
        return Err(FlavorRegistryError::InvalidActionSpecs {
            name: tool.name,
            message: format!(
                "action {} metadata {key} drifts from argument_schema {expected_fields:?}",
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
