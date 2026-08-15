use super::{
    BTreeSet, CapabilityTag, FlavorRegistry, FlavorRegistryError, FlavorRegistryFrozen,
    McpToolDescriptor, PayloadKind, SchemaCapabilityTags, SchemaId, SchemaVersion,
};

impl FlavorRegistry {
    /// Validate the registry and seal it for runtime use.
    /// # Errors
    ///
    /// Returns typed registry errors for invalid descriptors, unregistered
    /// references, ingress mismatches, unsatisfiable tags, and duplicate ids.
    pub fn try_freeze(self) -> Result<FlavorRegistryFrozen, FlavorRegistryError> {
        self.validate_schema_capability_tags_resolve()?;
        self.validate_flavor_descriptors()?;
        let mut seen_schemas = std::collections::HashSet::new();
        for schema in &self.schemas {
            if !seen_schemas.insert((schema.schema_id.clone(), schema.schema_version, schema.kind))
            {
                return Err(FlavorRegistryError::DuplicateSchema {
                    schema_id: schema.schema_id.clone(),
                    schema_version: schema.schema_version,
                    kind: schema.kind,
                });
            }
        }
        // Every Memory/Goal schema is typed; only citation schemas may be
        // opaque. A typed descriptor whose ingress parser was dropped is
        // unusable at the protocol boundary, so catch that internal drift
        // at startup rather than at first write.
        for schema in &self.schemas {
            if !schema.has_typed_ingress
                && !matches!(
                    schema.kind,
                    PayloadKind::CitedObject | PayloadKind::CitationMapping
                )
            {
                return Err(FlavorRegistryError::OpaqueSchemaKind {
                    schema_id: schema.schema_id.clone(),
                    schema_version: schema.schema_version,
                    kind: schema.kind,
                });
            }
            let ingress_count = self
                .protocol_ingress
                .iter()
                .filter(|entry| {
                    entry.schema_id == schema.schema_id
                        && entry.schema_version == schema.schema_version
                        && entry.kind == schema.kind
                })
                .count();
            let expected_ingress_count = usize::from(schema.has_typed_ingress);
            if ingress_count != expected_ingress_count {
                return Err(FlavorRegistryError::SchemaIngressMismatch {
                    schema_id: schema.schema_id.clone(),
                    schema_version: schema.schema_version,
                    kind: schema.kind,
                });
            }
        }
        for entry in &self.protocol_ingress {
            let resolves_to_typed_schema = self.schemas.iter().any(|schema| {
                schema.schema_id == entry.schema_id
                    && schema.schema_version == entry.schema_version
                    && schema.kind == entry.kind
                    && schema.has_typed_ingress
            });
            if !resolves_to_typed_schema {
                return Err(FlavorRegistryError::SchemaIngressMismatch {
                    schema_id: entry.schema_id.clone(),
                    schema_version: entry.schema_version,
                    kind: entry.kind,
                });
            }
        }
        let mut seen_tools = std::collections::HashSet::new();
        for tool in &self.mcp_tools {
            if !seen_tools.insert(tool.name) {
                return Err(FlavorRegistryError::DuplicateTool { name: tool.name });
            }
        }
        self.validate_tools_declare_behavior()?;
        self.validate_dispatcher_action_specs()?;
        Ok(FlavorRegistryFrozen::from_registry(self))
    }

    #[must_use]
    #[doc(hidden)]
    pub fn freeze_or_panic_for_tests(self) -> FlavorRegistryFrozen {
        self.try_freeze()
            .expect("flavor registry must be valid before freeze")
    }

    fn validate_schema_capability_tags_resolve(&self) -> Result<(), FlavorRegistryError> {
        for binding in &self.schema_capability_tags {
            if !self.schemas.iter().any(|schema| {
                schema.schema_id == binding.schema_id
                    && schema.schema_version == binding.schema_version
                    && schema.kind == binding.kind
            }) {
                return Err(FlavorRegistryError::UnregisteredSchemaCapabilityTags {
                    schema_id: binding.schema_id.clone(),
                    schema_version: binding.schema_version,
                    kind: binding.kind,
                });
            }
        }
        Ok(())
    }

    /// Cross-check: the owner-role gate can classify every registered flat
    /// tool.
    ///
    /// `ScopeGateBehavior::enforce_owner_role` demands WRITE when it cannot
    /// tell. Same two steps, same order: the tool's `ANNOTATIONS`, then the
    /// core manifest. Unclassified is silently a write. Dispatcher actions
    /// missing per-action annotations classify as writes.
    fn validate_tools_declare_behavior(&self) -> Result<(), FlavorRegistryError> {
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
    fn validate_dispatcher_action_specs(&self) -> Result<(), FlavorRegistryError> {
        for tool in &self.mcp_tools {
            // Absent and malformed are different answers. Reading them as one
            // — `.and_then(Value::as_object)` — let a schema that *carries*
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

    /// Cross-check: every `FlavorDescriptor::flavor_id` is unique.
    fn validate_flavor_descriptors(&self) -> Result<(), FlavorRegistryError> {
        let mut seen_ids = std::collections::HashSet::new();
        for flavor in &self.flavors {
            if !seen_ids.insert(flavor.flavor_id.as_str()) {
                return Err(FlavorRegistryError::DuplicateFlavor {
                    flavor_id: flavor.flavor_id.clone(),
                });
            }
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
        let meta = &extension[spec.action];
        for (key, fields) in [
            ("allowed_fields", spec.allowed_fields),
            ("required_fields", spec.required_fields),
        ] {
            let declared_fields = fields.iter().copied().collect::<BTreeSet<_>>();
            let derived_fields = meta
                .get(key)
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            if declared_fields != derived_fields {
                return Err(FlavorRegistryError::InvalidActionSpecs {
                    name: tool.name,
                    message: format!(
                        "action `{}` declares {key} {declared_fields:?} but the derived schema \
                         says {derived_fields:?}",
                        spec.action
                    ),
                });
            }
        }
    }
    Ok(())
}

pub(crate) fn schema_capability_map(
    bindings: &[SchemaCapabilityTags],
) -> std::collections::HashMap<(SchemaId, SchemaVersion, PayloadKind), BTreeSet<CapabilityTag>> {
    let mut out: std::collections::HashMap<_, BTreeSet<CapabilityTag>> =
        std::collections::HashMap::new();
    for binding in bindings {
        out.entry((
            binding.schema_id.clone(),
            binding.schema_version,
            binding.kind,
        ))
        .or_default()
        .extend(binding.tags.iter().cloned());
    }
    out
}
