use super::{
    BTreeSet, CapabilityTag, FlavorRegistry, FlavorRegistryError, FlavorRegistryFrozen,
    PayloadKind, RelationDescriptor, SchemaCapabilityTags, SchemaId, SchemaVersion,
};

impl FlavorRegistry {
    /// Validate the registry and seal it for runtime use.
    /// # Errors
    ///
    /// Returns typed registry errors for invalid descriptors, unregistered
    /// references, ingress mismatches, unsatisfiable tags, and duplicate ids.
    pub fn try_freeze(self) -> Result<FlavorRegistryFrozen, FlavorRegistryError> {
        for rel in &self.relations {
            if let Err(message) = rel.validate_descriptor() {
                return Err(FlavorRegistryError::InvalidRelationDescriptor {
                    relation: rel.relation.clone(),
                    message,
                });
            }
            if let Some(payload_schema) = &rel.payload_schema
                && !self.schemas.iter().any(|s| {
                    s.kind == PayloadKind::Edge
                        && s.schema_id == payload_schema.schema_id
                        && s.schema_version == payload_schema.schema_version
                })
            {
                return Err(FlavorRegistryError::UnregisteredRelationPayload {
                    relation: rel.relation.clone(),
                    schema_id: payload_schema.schema_id.clone(),
                    schema_version: payload_schema.schema_version,
                });
            }
        }
        self.validate_schema_capability_tags_resolve()?;
        self.validate_required_relation_tags_satisfiable()?;
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
        let mut seen_relations = std::collections::HashSet::new();
        for rel in &self.relations {
            if !seen_relations.insert(rel.relation.clone()) {
                return Err(FlavorRegistryError::DuplicateRelation {
                    relation: rel.relation.clone(),
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

    fn validate_required_relation_tags_satisfiable(&self) -> Result<(), FlavorRegistryError> {
        let declared = schema_capability_map(&self.schema_capability_tags);
        for relation in &self.relations {
            self.validate_relation_side_tags_satisfiable(
                relation,
                "source",
                relation.source_kind_mask,
                &relation.source_required_tags,
                &declared,
            )?;
            self.validate_relation_side_tags_satisfiable(
                relation,
                "target",
                relation.target_kind_mask,
                &relation.target_required_tags,
                &declared,
            )?;
        }
        Ok(())
    }

    fn validate_relation_side_tags_satisfiable(
        &self,
        relation: &RelationDescriptor,
        side: &'static str,
        kind_mask: crate::EntityKindMask,
        required_tags: &BTreeSet<CapabilityTag>,
        declared: &std::collections::HashMap<
            (SchemaId, SchemaVersion, PayloadKind),
            BTreeSet<CapabilityTag>,
        >,
    ) -> Result<(), FlavorRegistryError> {
        if required_tags.is_empty() {
            return Ok(());
        }
        let admitted = self.schemas.iter().any(|schema| {
            payload_kind_admitted_by_mask(schema.kind, kind_mask)
                && declared
                    .get(&(schema.schema_id.clone(), schema.schema_version, schema.kind))
                    .is_some_and(|tags| required_tags.is_subset(tags))
        });
        if !admitted {
            return Err(FlavorRegistryError::UnsatisfiableRelationTags {
                relation: relation.relation.clone(),
                side,
            });
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

fn payload_kind_admitted_by_mask(kind: PayloadKind, mask: crate::EntityKindMask) -> bool {
    match kind {
        PayloadKind::Fact => mask.contains_str("Fact"),
        PayloadKind::Abstraction => mask.contains_str("Abstraction"),
        PayloadKind::Perspective => mask.contains_str("Perspective"),
        PayloadKind::Goal => mask.contains_str("Goal"),
        PayloadKind::Edge | PayloadKind::CitedObject | PayloadKind::CitationMapping => false,
    }
}
