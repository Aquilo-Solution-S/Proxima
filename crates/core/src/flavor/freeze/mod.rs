//! Freezing the registry: every check a composed binary must pass before
//! its flavor registry is sealed for runtime use.
//!
//! [`FlavorRegistry::try_freeze`] is the whole entry point, and it is kept
//! here alone so the order of the checks stays readable in one screen. The
//! checks themselves live beside the question they answer:
//!
//! | module | question |
//! |---|---|
//! | [`schemas`] | do the registry's own declarations agree with each other? |
//! | [`contracts`] | does each flavor's contract match what it registered? |
//! | [`legs`] | is every declared surface reached by erase, transfer and forget? |
//! | [`projection`] | is the search projection uniform, and can a query reach it? |
//! | [`dispatcher`] | do a dispatcher tool's action specs match its argument schema? |
//!
//! Nothing here is reachable after the freeze: a `FlavorRegistryFrozen` is
//! the proof that all of it passed, which is why the checks may be as
//! expensive as they need to be and why a failure is a refusal to boot
//! rather than a runtime error some request discovers.

use super::{
    BTreeSet, CapabilityTag, FlavorRegistry, FlavorRegistryError, FlavorRegistryFrozen,
    McpToolDescriptor, PayloadKind, SchemaCapabilityTags, SchemaId, SchemaVersion,
};
use crate::mcp::schema::{
    analyze_root_fields, schema_contains_defs, schema_contains_ref, validate_closed_root_schema,
};

mod contracts;
mod dispatcher;
mod legs;
mod projection;
mod schemas;

#[cfg(test)]
mod tests;

pub(crate) use schemas::schema_capability_map;

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
        let mut seen_memory_selectors = std::collections::HashMap::new();
        for schema in &self.schemas {
            if !matches!(
                schema.kind,
                PayloadKind::Fact | PayloadKind::Abstraction | PayloadKind::Perspective
            ) {
                continue;
            }
            if let Some(first_version) = seen_memory_selectors.insert(
                (schema.kind, schema.schema_id.clone()),
                schema.schema_version,
            ) {
                return Err(FlavorRegistryError::DuplicateMemorySchemaSelector {
                    schema_id: schema.schema_id.clone(),
                    kind: schema.kind,
                    first_version,
                    conflicting_version: schema.schema_version,
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
        self.validate_listenable_schemas_publish_a_json_schema()?;
        self.validate_tools_declare_behavior()?;
        self.validate_dispatcher_action_specs()?;
        self.validate_contracts()?;
        self.validate_scope_declarations()?;
        self.validate_embedding_recipes_match_behavior()?;
        FlavorRegistryFrozen::from_registry(self)
    }

    #[must_use]
    #[doc(hidden)]
    pub fn freeze_or_panic_for_tests(self) -> FlavorRegistryFrozen {
        self.try_freeze()
            .expect("flavor registry must be valid before freeze")
    }
}
