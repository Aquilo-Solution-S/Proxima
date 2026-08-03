use super::{
    BTreeSet, CapabilityTag, FlavorRegistry, FlavorRegistryError, FlavorRegistryFrozen,
    McpToolDescriptor, McpToolOrigin, PayloadKind, SchemaCapabilityTags, SchemaId, SchemaVersion,
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
        self.validate_flavor_dispatcher_annotations()?;
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
            // Before the set comparison, because a set cannot report this:
            // two specs for one action collapse into one member and compare
            // equal to a correct derived key set. The second spec is dead —
            // `validate_action_args` and the scope gate both take the first
            // match — so its fields are silently never the contract.
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

    /// Cross-check: a flavor dispatcher does not declare itself read-only at
    /// tool level.
    ///
    /// `CoreActionMeta` is the only per-action annotation table and it is
    /// keyed by substrate tool name, so a flavor dispatcher has no per-action
    /// answer to give: its own `ANNOTATIONS` decide read from write for
    /// *every* action it dispatches. `read_only(true)` there is therefore not
    /// a claim about one action but about all of them, including the write
    /// action added to the enum next month — the owner-role gate would admit
    /// a viewer role to it, and the REST surface would advertise it as
    /// `QUERY`, which any proxy or client library may safely retry.
    ///
    /// The guard keys on *origin*, not on the annotation value, because the
    /// substrate legitimately has both shapes and both must keep freezing:
    /// `core_fact` is read-only at tool level, and `core_membership` is
    /// write/destructive at tool level with a read-only `list_members`
    /// rescued by `CoreActionMeta`. Until `McpActionArgSpec` carries an
    /// annotation slot of its own (docs/12 §Known gaps), the only honest
    /// tool-level answer for a flavor dispatcher is `read_only(false)`.
    fn validate_flavor_dispatcher_annotations(&self) -> Result<(), FlavorRegistryError> {
        for tool in &self.mcp_tools {
            if tool.action_arg_specs.is_empty() || !matches!(tool.origin, McpToolOrigin::Flavor(_))
            {
                continue;
            }
            // The tool's OWN declaration, not `resolved_annotations()`: the
            // only thing that fallback adds is the substrate manifest, a
            // table over core names this tool is not in. The two coincide for
            // a flavor tool; asking for the declaration names what is being
            // refused.
            if tool
                .annotations
                .and_then(|annotations| annotations.read_only)
                == Some(true)
            {
                return Err(FlavorRegistryError::InvalidActionSpecs {
                    name: tool.name,
                    message: "a dispatcher without per-action annotations cannot declare itself \
                              read-only at tool level: every action, including any write action \
                              added later, would inherit read-only owner-role gating and REST \
                              QUERY eligibility. Declare `read_only(false)`, or leave read_only \
                              unset, until per-action annotations exist"
                        .to_string(),
                });
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
