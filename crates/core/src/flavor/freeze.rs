use super::{
    BTreeSet, CapabilityTag, FlavorRegistry, FlavorRegistryError, FlavorRegistryFrozen,
    PayloadKind, SchemaCapabilityTags, SchemaId, SchemaVersion,
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
        // Every schema is either typed (a protocol-ingress parser) or
        // opaque. A typed schema whose ingress parser was dropped would
        // make `ingest_protocol_payload` silently accept any payload —
        // catch the drift here, not at first write.
        for schema in &self.schemas {
            let has_ingress = self.protocol_ingress.iter().any(|v| {
                v.schema_id == schema.schema_id
                    && v.schema_version == schema.schema_version
                    && v.kind == schema.kind
            });
            if schema.has_typed_ingress != has_ingress {
                return Err(FlavorRegistryError::SchemaIngressMismatch {
                    schema_id: schema.schema_id.clone(),
                    schema_version: schema.schema_version,
                    kind: schema.kind,
                });
            }
        }
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
        let mut seen_tools = std::collections::HashSet::new();
        for tool in &self.mcp_tools {
            if !seen_tools.insert(tool.name) {
                return Err(FlavorRegistryError::DuplicateTool { name: tool.name });
            }
        }
        self.validate_tools_declare_behavior()?;
        self.validate_dispatcher_action_specs()?;
        let mut seen_dependency_rules: std::collections::HashSet<&str> =
            std::collections::HashSet::new();
        for (schema_id, _) in &self.dependency_satisfaction_rules {
            if !seen_dependency_rules.insert(schema_id.as_str()) {
                return Err(FlavorRegistryError::DuplicateDependencyRule {
                    schema_id: schema_id.clone(),
                });
            }
        }
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

    /// Cross-check: the owner-role gate can classify every registered tool.
    ///
    /// `ScopeGateBehavior::enforce_owner_role` asks whether a tool is
    /// read-only and demands WRITE when it cannot tell. It resolves that in
    /// two steps — the tool's own `ANNOTATIONS`, then the core manifest —
    /// and this checks the same two, in the same order. A tool neither
    /// answers is not merely undocumented: it is silently reclassified as a
    /// write, and the symptom is a viewer refused a read with no stated
    /// cause. That is exactly what happened to every `proxima-code_*` tool
    /// before `ANNOTATIONS` existed.
    ///
    /// Boot is the right place to say so. The alternative is a compile-time
    /// requirement Rust cannot express (a trait const with a default is
    /// always satisfiable) or a per-flavor test each flavor has to remember
    /// to write.
    fn validate_tools_declare_behavior(&self) -> Result<(), FlavorRegistryError> {
        for tool in &self.mcp_tools {
            if tool.annotations.is_none() && crate::mcp::core_tool_annotations(tool.name).is_none()
            {
                return Err(FlavorRegistryError::UndeclaredToolBehavior { name: tool.name });
            }
        }
        Ok(())
    }

    /// Cross-check: a dispatcher's declared actions and its derived schema
    /// describe the same dispatcher.
    ///
    /// A dispatcher used to be described in three places that nothing tied
    /// together: `ACTION_ARG_SPECS` (what the argument validator and the REST
    /// router read), the schemars-derived `x-proxima-actions` extension (what
    /// MCP clients read), and the substrate-only `CoreActionMeta` tables (what
    /// the scope gate read). Substrate tools kept all three in step by hand;
    /// a flavor tool could not write the third at all. `McpToolDescriptor::
    /// action_arg_specs` is now the one enumeration, and this is what makes
    /// declaring it non-optional for anything that *looks* like a dispatcher
    /// to a client.
    ///
    /// The discriminator must literally be `action`. That is not cosmetic:
    /// `ToolScope` keys are spelled `"{tool}:{action}"`, `validate_action_args`
    /// and `ScopeGateBehavior::enforce_scope` both read `args["action"]`, and
    /// the REST narrowed route injects `"action"` into the body before
    /// dispatch. A dispatcher tagged on anything else would be enumerated
    /// correctly and then gated, validated, and routed as if it had no
    /// actions at all.
    fn validate_dispatcher_action_specs(&self) -> Result<(), FlavorRegistryError> {
        for tool in &self.mcp_tools {
            let extension = tool
                .args_schema
                .get("x-proxima-actions")
                .and_then(serde_json::Value::as_object);
            let Some(extension) = extension else {
                if tool.action_arg_specs.is_empty() {
                    continue;
                }
                return Err(FlavorRegistryError::InvalidActionSpecs {
                    name: tool.name,
                    message: "declares ACTION_ARG_SPECS but its `Args` is not an internally \
                              tagged enum, so there is no action schema to validate against"
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
                                "action `{}` declares {key} {declared_fields:?} but the derived \
                                 schema says {derived_fields:?}",
                                spec.action
                            ),
                        });
                    }
                }
            }
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
