//! Edge relation registry — typing layer for `proxima_core.edges`.
//!
//! Mirror of the schema registry on the edge layer. The substrate
//! enforces a closed `RelationClass` (the abstract role an edge plays
//! in A/P traversal); flavors author concrete relations as
//! `RelationDescriptor`s and optionally attach an `EdgePayload`
//! schema for typed per-edge state.
//!
//! See docs/02 §"Relation registry" + §"Typed edge payloads" and
//! docs/03 §EdgePayload.
//!
//! `RelationClass` is closed by design: substrate-level traversal
//! contracts (A→P retrieval, supersession bookkeeping, provenance
//! walking) require a fixed vocabulary. Flavors differentiate within
//! a class via the `relation: text` discriminator on the edge row.
//!
//! v1 keeps `RelationDescriptor` minimal — the doc-illustrative
//! `source_kind_mask` / `target_kind_mask` / `authorship_mask` fields
//! are not yet modeled in Rust because the substrate already enforces
//! endpoint-kind validity through SQL CHECK constraints on
//! `proxima_core.edges`. Add them when a relation needs runtime mask
//! validation in core code (e.g. cross-class authorship rules).

use crate::{SchemaId, SchemaVersion};

pub const CORE_DERIVED_FROM_RELATION: &str = "core/derived-from";
pub const CORE_SUPERSEDES_RELATION: &str = "core/supersedes";
pub const CORE_INSPIRES_RELATION: &str = "core/inspires";
pub const CORE_AUTHORED_RELATION: &str = "core/authored";

/// Closed substrate vocabulary for the abstract role an edge plays
/// in A/P traversal. The five variants below are the only edge
/// classes the substrate understands; flavors pick a class and
/// differentiate via the `relation: text` column.
///
/// Discriminator values match the SQL CHECK on
/// `proxima_core.edges.relation_class`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RelationClass {
    /// EventSource-authored edges shaped from payload structure
    /// (e.g. `commit→parent_commit`, `chunk→file_revision`).
    Structural,
    /// Operator-authored edges produced during consolidation
    /// (e.g. `core/derived-from` from F→A).
    Provenance,
    /// Engine-authored edges marking a re-derivation supersedes the
    /// prior head.
    Supersession,
    /// PerspectiveLink — causa-proxima carrier (causal interpretation).
    Causal,
    /// PerspectiveLink — non-causal interpretation.
    Interpretive,
}

impl RelationClass {
    /// SQL discriminator. Stable contract — must match the CHECK on
    /// `proxima_core.edges.relation_class`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Structural => "Structural",
            Self::Provenance => "Provenance",
            Self::Supersession => "Supersession",
            Self::Causal => "Causal",
            Self::Interpretive => "Interpretive",
        }
    }
}

/// Reference to a registered schema by `(id, version)`. Used by
/// `RelationDescriptor::payload_schema` to point at the `EdgePayload`
/// schema a relation's edges carry, when typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaRef {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
}

impl SchemaRef {
    #[must_use]
    pub fn new(schema_id: SchemaId, schema_version: SchemaVersion) -> Self {
        Self {
            schema_id,
            schema_version,
        }
    }
}

/// Build-time descriptor for a registered relation. Authored by the
/// flavor that owns the relation; consumed by:
///
/// - the atomic edge-write verb, which reads `payload_schema` to
///   decide whether to write a typed sidecar in the same transaction;
/// - `Schema` introspection, surfacing the registered relations
///   alongside payload schemas.
#[derive(Clone, Debug)]
pub struct RelationDescriptor {
    /// Flavor-qualified relation id, e.g. `"proxima-code/calls"`.
    /// Stored verbatim in `proxima_core.edges.relation`.
    pub relation: String,
    /// Closed substrate class — what role this edge plays in A/P
    /// traversal. Stored as `RelationClass::as_str()` in
    /// `proxima_core.edges.relation_class`.
    pub class: RelationClass,
    /// Some(SchemaRef) iff edges of this relation carry a typed
    /// `EdgePayload` sidecar. None for substrate-only relations
    /// (e.g. `core/derived-from` carries all needed state on the
    /// edge row itself).
    pub payload_schema: Option<SchemaRef>,
}

impl RelationDescriptor {
    /// Untyped relation — substrate-only (no `EdgePayload` sidecar).
    #[must_use]
    pub fn substrate(relation: impl Into<String>, class: RelationClass) -> Self {
        Self {
            relation: relation.into(),
            class,
            payload_schema: None,
        }
    }

    /// Typed relation — edges of this relation carry an
    /// `EdgePayload` sidecar keyed on `edge_id`.
    #[must_use]
    pub fn typed(
        relation: impl Into<String>,
        class: RelationClass,
        payload_schema: SchemaRef,
    ) -> Self {
        Self {
            relation: relation.into(),
            class,
            payload_schema: Some(payload_schema),
        }
    }
}

#[must_use]
pub fn core_relation_descriptors() -> Vec<RelationDescriptor> {
    vec![
        RelationDescriptor::substrate(CORE_DERIVED_FROM_RELATION, RelationClass::Provenance),
        RelationDescriptor::substrate(CORE_SUPERSEDES_RELATION, RelationClass::Supersession),
        RelationDescriptor::substrate(CORE_INSPIRES_RELATION, RelationClass::Causal),
        RelationDescriptor::substrate(CORE_AUTHORED_RELATION, RelationClass::Causal),
    ]
}

/// Relation resolved from the immutable `FlavorRegistryFrozen` for an
/// edge write. Carries the descriptor plus the typed edge sidecar
/// table when the descriptor references an `EdgePayload` schema.
#[derive(Clone, Copy, Debug)]
pub struct RegisteredRelation<'a> {
    pub descriptor: &'a RelationDescriptor,
    pub payload_sidecar_table: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    use super::{
        CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION, CORE_INSPIRES_RELATION,
        CORE_SUPERSEDES_RELATION, RelationClass, core_relation_descriptors,
    };

    fn descriptor_for(relation: &str) -> Option<RelationClass> {
        core_relation_descriptors()
            .into_iter()
            .find(|d| d.relation == relation)
            .map(|d| d.class)
    }

    #[test]
    fn core_authored_is_registered_as_causal() {
        assert_eq!(
            descriptor_for(CORE_AUTHORED_RELATION),
            Some(RelationClass::Causal),
            "core/authored must be registered with class Causal so it shares \
             the substrate causal-edge vocabulary with core/inspires",
        );
    }

    #[test]
    fn pre_existing_core_relations_unchanged() {
        assert_eq!(
            descriptor_for(CORE_DERIVED_FROM_RELATION),
            Some(RelationClass::Provenance),
        );
        assert_eq!(
            descriptor_for(CORE_SUPERSEDES_RELATION),
            Some(RelationClass::Supersession),
        );
        assert_eq!(
            descriptor_for(CORE_INSPIRES_RELATION),
            Some(RelationClass::Causal),
        );
    }
}
