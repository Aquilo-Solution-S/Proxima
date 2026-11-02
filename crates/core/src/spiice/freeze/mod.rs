//! Freezing the registry: every check a composed binary must pass before
//! its spiice registry is sealed for runtime use.
//!
//! [`SpiiceRegistry::try_freeze`] is the whole entry point, and it is kept
//! here alone so the order of the checks stays readable in one screen. The
//! checks themselves live beside the question they answer:
//!
//! | module | question |
//! |---|---|
//! | [`schemas`] | do the registry's own declarations agree with each other? |
//! | [`contracts`] | does each spiice's contract match what it registered? |
//! | [`legs`] | is every declared surface reached by erase, transfer and forget? |
//! | [`projection`] | is the search projection uniform, and can a query reach it? |
//! | [`dispatcher`] | do a dispatcher tool's action specs match its argument schema? |
//!
//! Nothing here is reachable after the freeze: a `SpiiceRegistryFrozen` is
//! the proof that all of it passed, which is why the checks may be as
//! expensive as they need to be and why a failure is a refusal to boot
//! rather than a runtime error some request discovers.

use super::{
    BTreeSet, CapabilityTag, McpToolDescriptor, PayloadKind, SchemaCapabilityTags, SchemaId,
    SchemaVersion, SpiiceRegistry, SpiiceRegistryError, SpiiceRegistryFrozen,
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

impl SpiiceRegistry {
    /// Validate the registry and seal it for runtime use.
    /// # Errors
    ///
    /// Returns typed registry errors for invalid descriptors, unregistered
    /// references, ingress mismatches, unsatisfiable tags, and duplicate ids.
    pub fn try_freeze(self) -> Result<SpiiceRegistryFrozen, SpiiceRegistryError> {
        self.validate_schema_capability_tags_resolve()?;
        self.validate_spiice_descriptors()?;
        let mut seen_schemas = std::collections::HashSet::new();
        for schema in &self.schemas {
            if !seen_schemas.insert((schema.schema_id.clone(), schema.schema_version, schema.kind))
            {
                return Err(SpiiceRegistryError::DuplicateSchema {
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
                return Err(SpiiceRegistryError::DuplicateMemorySchemaSelector {
                    schema_id: schema.schema_id.clone(),
                    kind: schema.kind,
                    first_version,
                    conflicting_version: schema.schema_version,
                });
            }
        }
        // Every Memory/Goal schema is typed; only cited-object schemas may be
        // opaque. A typed descriptor whose ingress parser was dropped is
        // unusable at the protocol boundary, so catch that internal drift
        // at startup rather than at first write.
        for schema in &self.schemas {
            if !schema.has_typed_ingress && schema.kind != PayloadKind::CitedObject {
                return Err(SpiiceRegistryError::OpaqueSchemaKind {
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
                return Err(SpiiceRegistryError::SchemaIngressMismatch {
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
                return Err(SpiiceRegistryError::SchemaIngressMismatch {
                    schema_id: entry.schema_id.clone(),
                    schema_version: entry.schema_version,
                    kind: entry.kind,
                });
            }
        }
        let mut seen_tools = std::collections::HashSet::new();
        for tool in &self.mcp_tools {
            if !seen_tools.insert(tool.name) {
                return Err(SpiiceRegistryError::DuplicateTool { name: tool.name });
            }
        }
        self.validate_listenable_schemas_publish_a_json_schema()?;
        self.validate_tools_declare_behavior()?;
        self.validate_dispatcher_action_specs()?;
        self.validate_scope_keys()?;
        self.validate_titles()?;
        self.validate_argv_action_names()?;
        self.validate_contracts()?;
        self.validate_scope_declarations()?;
        self.validate_embedding_recipes_match_behavior()?;
        SpiiceRegistryFrozen::from_registry(self)
    }

    #[must_use]
    #[doc(hidden)]
    pub fn freeze_or_panic_for_tests(self) -> SpiiceRegistryFrozen {
        self.try_freeze()
            .expect("spiice registry must be valid before freeze")
    }
}
