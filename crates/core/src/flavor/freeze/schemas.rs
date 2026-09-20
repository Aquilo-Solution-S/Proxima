//! Freeze checks that read the registry's own declarations — schemas,
//! scopes, capability tags, flavor descriptors — rather than any contract's
//! account of them.
//!
//! These are the checks with no cross-check partner: what they compare is
//! two halves of one registration, so a disagreement is internal drift in
//! the composed binary rather than a flavor contradicting its own contract.

use super::{
    BTreeSet, CapabilityTag, FlavorRegistry, FlavorRegistryError, PayloadKind,
    SchemaCapabilityTags, SchemaId, SchemaVersion,
};

impl FlavorRegistry {
    /// A listenable schema's captured events name a catalog entry that has
    /// to resolve.
    ///
    /// The pairing is a build-time fact about the composed binary, so it is
    /// refused at freeze rather than at the first write — the alternative is
    /// a deployment that emits events pointing at a `dataschema` which
    /// resolves to nothing, for the lifetime of every one of those events.
    /// The check reads `protocol_ingress`, because that is where
    /// `json_schema()` was recorded at registration.
    pub(super) fn validate_listenable_schemas_publish_a_json_schema(
        &self,
    ) -> Result<(), FlavorRegistryError> {
        for schema in &self.schemas {
            if !schema.listenable {
                continue;
            }
            let has_json_schema = self.protocol_ingress.iter().any(|entry| {
                entry.schema_id == schema.schema_id
                    && entry.schema_version == schema.schema_version
                    && entry.kind == schema.kind
                    && entry.json_schema.is_some()
            });
            if !has_json_schema {
                return Err(FlavorRegistryError::ListenableWithoutSchema {
                    schema_id: schema.schema_id.clone(),
                });
            }
        }
        Ok(())
    }

    /// One declaration per lifecycle scope, and one for every scope a
    /// payload names.
    ///
    /// Both halves of the scope contract are declarations, and each is
    /// useless without the other: a payload that names a kind no contract
    /// declares cannot be fenced (nothing says where its registry lives),
    /// and a kind two contracts declare cannot be fenced either (nothing
    /// says which). Both are build-time facts about the composed binary, so
    /// both are refused here rather than at the first write into the scope.
    ///
    /// The declaration's shape is checked in the same pass: core cannot
    /// reach the backend's identifier validator, but it can insist on a
    /// schema-qualified table and three named columns, which is what turns
    /// a typo into a refusal naming the flavor instead of a splice failure
    /// naming a string.
    pub(super) fn validate_scope_declarations(&self) -> Result<(), FlavorRegistryError> {
        let mut declared: std::collections::HashMap<crate::ScopeKind, &'static str> =
            std::collections::HashMap::new();
        for contract in &self.contracts {
            for decl in contract.scopes {
                if let Some(message) = decl.shape_error() {
                    return Err(FlavorRegistryError::InvalidScopeDeclaration {
                        flavor_id: contract.flavor_id,
                        kind: decl.kind,
                        message,
                    });
                }
                if let Some(first_flavor_id) = declared.insert(decl.kind, contract.flavor_id) {
                    return Err(FlavorRegistryError::DuplicateScopeDeclaration {
                        kind: decl.kind,
                        first_flavor_id,
                        conflicting_flavor_id: contract.flavor_id,
                    });
                }
            }
        }
        for (schema_id, kind) in &self.scoped_schemas {
            if !declared.contains_key(kind) {
                return Err(FlavorRegistryError::ScopeNotDeclared {
                    schema_id: schema_id.clone(),
                    kind: *kind,
                });
            }
        }
        Ok(())
    }

    /// Every recipe against the answer the embedding lane will act on.
    ///
    /// `EmbeddingRecipe` is the declaration. The two functions called here
    /// are the behaviour: `contract_embed_units` is the sole producer of
    /// what the drain reads, and `non_embeddable_schema_ids` is the exact
    /// list the enqueue queries bind and `schema_is_embeddable` answers
    /// from. Comparing the declaration to the behaviour is what makes the
    /// recipe govern rather than describe — a second declaration to compare
    /// against would only have to be kept equal in its turn.
    ///
    /// Both sides are judged per SCHEMA ID, not per registration, because
    /// the exclusion list is: the column it is bound against carries no
    /// kind. So a `Never` arm on an id any registration resolves a unit for
    /// is refused — the reason could never be applied — while a `Units` arm
    /// that resolves to nothing is refused only when no registration of that
    /// id resolves one either. A no-table `Units` arm shadowed by a sibling
    /// registration that does resolve a unit is ACCEPTED, and correctly: the
    /// drain matches units by id, so it finds the sibling's and has text.
    pub(super) fn validate_embedding_recipes_match_behavior(
        &self,
    ) -> Result<(), FlavorRegistryError> {
        let units = crate::verbs::schema::contract_embed_units(&self.contracts)?;
        let non_embeddable =
            crate::verbs::schema::non_embeddable_schema_ids(&self.contracts, &units);
        for contract in &self.contracts {
            for schema in contract.schemas {
                let schema_id = schema.schema_id();
                let machinery_embeds = !non_embeddable
                    .iter()
                    .any(|excluded| excluded == schema_id.as_str());
                if schema.embedding.is_never() == machinery_embeds {
                    return Err(FlavorRegistryError::EmbeddabilityDisagreement {
                        flavor_id: contract.flavor_id,
                        schema_id,
                        recipe_is_never: schema.embedding.is_never(),
                        machinery_embeds,
                    });
                }
            }
        }
        Ok(())
    }

    pub(super) fn validate_schema_capability_tags_resolve(
        &self,
    ) -> Result<(), FlavorRegistryError> {
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

    /// Cross-check: every `FlavorDescriptor::flavor_id` is unique.
    pub(super) fn validate_flavor_descriptors(&self) -> Result<(), FlavorRegistryError> {
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
