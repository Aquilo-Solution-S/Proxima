//! Edge relation registry — typing layer for `proxima_core.edges`.
//!
//! Mirror of the schema registry on the edge layer. The substrate
//! enforces a closed `RelationClass` (the abstract role an edge plays
//! in A/P traversal); flavors author concrete relations as
//! `RelationDescriptor`s and optionally attach an `EdgePayload`
//! schema for typed per-edge state.
//!
//! See docs/02 §"Relation registry" + §"Typed edge payloads" and
//! docs/03 §`EdgePayload`.
//!
//! `RelationClass` is closed by design: substrate-level traversal
//! contracts (A→P retrieval, supersession bookkeeping, provenance
//! walking) require a fixed vocabulary. Flavors differentiate within
//! a class via the `relation: text` discriminator on the edge row.
//!
use std::collections::BTreeSet;

use crate::verbs::schema::FlavorRegistryFrozen;
use crate::{CapabilityTag, SchemaId, SchemaVersion};

pub const CORE_DERIVED_FROM_RELATION: &str = "core/derived-from";
pub const CORE_SUPERSEDES_RELATION: &str = "core/supersedes";
pub const CORE_INSPIRES_RELATION: &str = "core/inspires";
pub const CORE_AUTHORED_RELATION: &str = "core/authored";
pub const CORE_DEPENDS_ON_RELATION: &str = "core/depends-on";

/// Closed substrate vocabulary for the abstract role an edge plays
/// in A/P traversal. The five variants below are the only edge
/// classes the substrate understands; flavors pick a class and
/// differentiate via the `relation: text` column.
///
/// Discriminator values match the SQL CHECK on
/// `proxima_core.edges.relation_class`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(type_name = "proxima_core.relation_class")]
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
    /// `PerspectiveLink` — causa-proxima carrier (causal interpretation).
    Causal,
    /// `PerspectiveLink` — non-causal interpretation.
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

/// Rust mirror of `proxima_core.edge_authorship_kind`. Tags which
/// operator/agent authored an edge row. See
/// `crates/storage-pg/migrations/20260516000010_baseline.sql` for the
/// canonical variant set.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type,
)]
#[sqlx(type_name = "proxima_core.edge_authorship_kind")]
pub enum EdgeAuthorshipKind {
    EventSource,
    OperatorFtoA,
    OperatorAtoP,
    OperatorAtoGoal,
    PerspectiveLink,
    User,
    Engine,
    ExternalAgent,
}

impl EdgeAuthorshipKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EventSource => "EventSource",
            Self::OperatorFtoA => "OperatorFtoA",
            Self::OperatorAtoP => "OperatorAtoP",
            Self::OperatorAtoGoal => "OperatorAtoGoal",
            Self::PerspectiveLink => "PerspectiveLink",
            Self::User => "User",
            Self::Engine => "Engine",
            Self::ExternalAgent => "ExternalAgent",
        }
    }
}

/// Bit mask over edge endpoint kinds admitted by a relation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EntityKindMask(u8);

impl EntityKindMask {
    const FACT: u8 = 0b0001;
    const ABSTRACTION: u8 = 0b0010;
    const PERSPECTIVE: u8 = 0b0100;
    const GOAL: u8 = 0b1000;

    #[must_use]
    pub const fn fact() -> Self {
        Self(Self::FACT)
    }

    #[must_use]
    pub const fn abstraction() -> Self {
        Self(Self::ABSTRACTION)
    }

    #[must_use]
    pub const fn perspective() -> Self {
        Self(Self::PERSPECTIVE)
    }

    #[must_use]
    pub const fn goal() -> Self {
        Self(Self::GOAL)
    }

    #[must_use]
    pub const fn memory() -> Self {
        Self(Self::FACT | Self::ABSTRACTION | Self::PERSPECTIVE)
    }

    #[must_use]
    pub const fn fact_abstraction() -> Self {
        Self(Self::FACT | Self::ABSTRACTION)
    }

    #[must_use]
    pub const fn abstraction_perspective() -> Self {
        Self(Self::ABSTRACTION | Self::PERSPECTIVE)
    }

    #[must_use]
    pub const fn abstraction_perspective_goal() -> Self {
        Self(Self::ABSTRACTION | Self::PERSPECTIVE | Self::GOAL)
    }

    #[must_use]
    pub const fn fact_abstraction_goal() -> Self {
        Self(Self::FACT | Self::ABSTRACTION | Self::GOAL)
    }

    #[must_use]
    pub const fn all() -> Self {
        Self(Self::FACT | Self::ABSTRACTION | Self::PERSPECTIVE | Self::GOAL)
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn contains_fact(self) -> bool {
        self.0 & Self::FACT != 0
    }

    #[must_use]
    pub fn contains_str(self, kind: &str) -> bool {
        match kind {
            "Fact" => self.0 & Self::FACT != 0,
            "Abstraction" => self.0 & Self::ABSTRACTION != 0,
            "Perspective" => self.0 & Self::PERSPECTIVE != 0,
            "Goal" => self.0 & Self::GOAL != 0,
            _ => false,
        }
    }
}

/// Bit mask over `proxima_core.edges.authorship_kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AuthorshipKindMask(u16);

impl AuthorshipKindMask {
    const EVENT_SOURCE: u16 = 0b0000_0001;
    const OPERATOR_F_TO_A: u16 = 0b0000_0010;
    const OPERATOR_A_TO_P: u16 = 0b0000_0100;
    const OPERATOR_A_TO_GOAL: u16 = 0b0000_1000;
    const PERSPECTIVE_LINK: u16 = 0b0001_0000;
    const USER: u16 = 0b0010_0000;
    const ENGINE: u16 = 0b0100_0000;
    const EXTERNAL_AGENT: u16 = 0b1000_0000;

    #[must_use]
    pub const fn event_source() -> Self {
        Self(Self::EVENT_SOURCE)
    }

    #[must_use]
    pub const fn operator_f_to_a() -> Self {
        Self(Self::OPERATOR_F_TO_A)
    }

    #[must_use]
    pub const fn operator_a_to_p() -> Self {
        Self(Self::OPERATOR_A_TO_P)
    }

    #[must_use]
    pub const fn operator_a_to_goal() -> Self {
        Self(Self::OPERATOR_A_TO_GOAL)
    }

    #[must_use]
    pub const fn perspective_link() -> Self {
        Self(Self::PERSPECTIVE_LINK)
    }

    #[must_use]
    pub const fn user() -> Self {
        Self(Self::USER)
    }

    #[must_use]
    pub const fn engine() -> Self {
        Self(Self::ENGINE)
    }

    #[must_use]
    pub const fn external_agent() -> Self {
        Self(Self::EXTERNAL_AGENT)
    }

    #[must_use]
    pub const fn operator() -> Self {
        Self(Self::OPERATOR_F_TO_A | Self::OPERATOR_A_TO_P | Self::OPERATOR_A_TO_GOAL)
    }

    #[must_use]
    pub const fn core() -> Self {
        Self(Self::USER | Self::ENGINE)
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub fn contains_str(self, kind: &str) -> bool {
        match kind {
            "EventSource" => self.0 & Self::EVENT_SOURCE != 0,
            "OperatorFtoA" => self.0 & Self::OPERATOR_F_TO_A != 0,
            "OperatorAtoP" => self.0 & Self::OPERATOR_A_TO_P != 0,
            "OperatorAtoGoal" => self.0 & Self::OPERATOR_A_TO_GOAL != 0,
            "PerspectiveLink" => self.0 & Self::PERSPECTIVE_LINK != 0,
            "User" => self.0 & Self::USER != 0,
            "Engine" => self.0 & Self::ENGINE != 0,
            "ExternalAgent" => self.0 & Self::EXTERNAL_AGENT != 0,
            _ => false,
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

/// Durable endpoint binding for a relation side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EndpointBinding {
    /// Resolve the side through a `fact_entity_id` head pointer.
    FollowHead,
    /// Pin the side to the exact memory or goal row written on the edge.
    Pin,
}

impl EndpointBinding {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FollowHead => "FollowHead",
            Self::Pin => "Pin",
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
    /// Entity kinds permitted as edge source endpoints.
    pub source_kind_mask: EntityKindMask,
    /// Entity kinds permitted as edge target endpoints.
    pub target_kind_mask: EntityKindMask,
    /// Durable endpoint binding required for the source side.
    pub source_binding: EndpointBinding,
    /// Durable endpoint binding required for the target side.
    pub target_binding: EndpointBinding,
    /// Edge authorship kinds permitted for this relation.
    pub authorship_mask: AuthorshipKindMask,
    /// Capability tags required on the source endpoint schema.
    pub source_required_tags: BTreeSet<CapabilityTag>,
    /// Capability tags required on the target endpoint schema.
    pub target_required_tags: BTreeSet<CapabilityTag>,
    /// Some(SchemaRef) iff edges of this relation carry a typed
    /// `EdgePayload` sidecar. None for substrate-only relations
    /// (e.g. `core/derived-from` carries all needed state on the
    /// edge row itself).
    pub payload_schema: Option<SchemaRef>,
}

impl RelationDescriptor {
    /// Untyped relation — substrate-only (no `EdgePayload` sidecar).
    #[must_use]
    pub fn substrate(
        relation: impl Into<String>,
        class: RelationClass,
        source_binding: EndpointBinding,
        target_binding: EndpointBinding,
        source_kind_mask: EntityKindMask,
        target_kind_mask: EntityKindMask,
        authorship_mask: AuthorshipKindMask,
    ) -> Self {
        Self {
            relation: relation.into(),
            class,
            source_kind_mask,
            target_kind_mask,
            source_binding,
            target_binding,
            authorship_mask,
            source_required_tags: BTreeSet::new(),
            target_required_tags: BTreeSet::new(),
            payload_schema: None,
        }
    }

    /// Typed relation — edges of this relation carry an
    /// `EdgePayload` sidecar keyed on `edge_id`.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn typed(
        relation: impl Into<String>,
        class: RelationClass,
        payload_schema: SchemaRef,
        source_binding: EndpointBinding,
        target_binding: EndpointBinding,
        source_kind_mask: EntityKindMask,
        target_kind_mask: EntityKindMask,
        authorship_mask: AuthorshipKindMask,
    ) -> Self {
        Self {
            relation: relation.into(),
            class,
            source_kind_mask,
            target_kind_mask,
            source_binding,
            target_binding,
            authorship_mask,
            source_required_tags: BTreeSet::new(),
            target_required_tags: BTreeSet::new(),
            payload_schema: Some(payload_schema),
        }
    }

    /// Add endpoint capability constraints to an existing descriptor.
    ///
    /// # Panics
    ///
    /// Panics if any tag is invalid.
    #[must_use]
    pub fn with_required_tags(mut self, source: &[&str], target: &[&str]) -> Self {
        self.source_required_tags = parse_required_tags(&self.relation, "source", source);
        self.target_required_tags = parse_required_tags(&self.relation, "target", target);
        self
    }

    /// Untyped relation with endpoint capability constraints.
    ///
    /// # Panics
    ///
    /// Panics if any tag is invalid.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn substrate_with_required_tags(
        relation: impl Into<String>,
        class: RelationClass,
        source_binding: EndpointBinding,
        target_binding: EndpointBinding,
        source_kind_mask: EntityKindMask,
        target_kind_mask: EntityKindMask,
        authorship_mask: AuthorshipKindMask,
        source_required_tags: &[&str],
        target_required_tags: &[&str],
    ) -> Self {
        Self::substrate(
            relation,
            class,
            source_binding,
            target_binding,
            source_kind_mask,
            target_kind_mask,
            authorship_mask,
        )
        .with_required_tags(source_required_tags, target_required_tags)
    }

    /// Typed relation with endpoint capability constraints.
    ///
    /// # Panics
    ///
    /// Panics if any tag is invalid.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn typed_with_required_tags(
        relation: impl Into<String>,
        class: RelationClass,
        payload_schema: SchemaRef,
        source_binding: EndpointBinding,
        target_binding: EndpointBinding,
        source_kind_mask: EntityKindMask,
        target_kind_mask: EntityKindMask,
        authorship_mask: AuthorshipKindMask,
        source_required_tags: &[&str],
        target_required_tags: &[&str],
    ) -> Self {
        Self::typed(
            relation,
            class,
            payload_schema,
            source_binding,
            target_binding,
            source_kind_mask,
            target_kind_mask,
            authorship_mask,
        )
        .with_required_tags(source_required_tags, target_required_tags)
    }

    /// Validate descriptor-local masks against a proposed edge shape.
    ///
    /// Storage also enforces universal endpoint truth and layer rules; this
    /// check gives callers deterministic relation-level failures before SQL.
    ///
    /// # Errors
    /// Returns an error string when the source kind, target kind, authorship,
    /// or class-specific Fact-to-Fact rule is not admitted by the descriptor.
    pub fn validate_edge_shape(
        &self,
        source_kind: &str,
        source_endpoint: EndpointBinding,
        target_kind: &str,
        target_endpoint: EndpointBinding,
        authorship_kind: &str,
    ) -> Result<(), String> {
        self.validate_descriptor()?;
        self.validate_endpoint_binding(
            "source",
            self.source_binding,
            source_endpoint,
            source_kind,
        )?;
        self.validate_endpoint_binding(
            "target",
            self.target_binding,
            target_endpoint,
            target_kind,
        )?;
        if !self.source_kind_mask.contains_str(source_kind) {
            return Err(format!(
                "relation {} rejects source kind {}",
                self.relation, source_kind
            ));
        }
        if !self.target_kind_mask.contains_str(target_kind) {
            return Err(format!(
                "relation {} rejects target kind {}",
                self.relation, target_kind
            ));
        }
        if !self.authorship_mask.contains_str(authorship_kind) {
            return Err(format!(
                "relation {} rejects authorship kind {}",
                self.relation, authorship_kind
            ));
        }
        if source_kind == "Fact"
            && target_kind == "Fact"
            && matches!(
                self.class,
                RelationClass::Causal | RelationClass::Interpretive
            )
        {
            return Err(format!(
                "relation {} cannot author semantic Fact-to-Fact edges",
                self.relation
            ));
        }
        if source_kind == "Fact"
            && target_kind == "Fact"
            && self.class == RelationClass::Supersession
        {
            return Err(format!("relation {} cannot supersede Facts", self.relation));
        }
        if self.relation == CORE_SUPERSEDES_RELATION && source_kind != target_kind {
            return Err("core/supersedes requires matching source and target kinds".to_string());
        }
        Ok(())
    }

    fn validate_endpoint_binding(
        &self,
        side: &str,
        declared: EndpointBinding,
        actual: EndpointBinding,
        kind: &str,
    ) -> Result<(), String> {
        if declared != actual {
            return Err(format!(
                "relation {} expects {} endpoint {}, got {}",
                self.relation,
                side,
                declared.as_str(),
                actual.as_str(),
            ));
        }
        if actual == EndpointBinding::FollowHead && kind != "Fact" {
            return Err(format!(
                "relation {} requires {} FollowHead endpoint to be Fact, got {}",
                self.relation, side, kind,
            ));
        }
        Ok(())
    }

    /// Validate mask shape independent of a concrete edge.
    ///
    /// # Errors
    /// Returns an error string for empty masks or descriptors that would admit
    /// direct semantic Fact-to-Fact edges.
    pub fn validate_descriptor(&self) -> Result<(), String> {
        if self.source_kind_mask.is_empty() {
            return Err(format!(
                "relation {} has empty source kind mask",
                self.relation
            ));
        }
        if self.target_kind_mask.is_empty() {
            return Err(format!(
                "relation {} has empty target kind mask",
                self.relation
            ));
        }
        if self.authorship_mask.is_empty() {
            return Err(format!(
                "relation {} has empty authorship mask",
                self.relation
            ));
        }
        if self.source_binding == EndpointBinding::FollowHead
            && !self.source_kind_mask.contains_fact()
        {
            return Err(format!(
                "relation {} has source FollowHead binding without Fact source mask",
                self.relation
            ));
        }
        if self.target_binding == EndpointBinding::FollowHead
            && !self.target_kind_mask.contains_fact()
        {
            return Err(format!(
                "relation {} has target FollowHead binding without Fact target mask",
                self.relation
            ));
        }
        if self.source_kind_mask.contains_fact()
            && self.target_kind_mask.contains_fact()
            && matches!(
                self.class,
                RelationClass::Causal | RelationClass::Interpretive
            )
        {
            return Err(format!(
                "relation {} admits semantic Fact-to-Fact edges",
                self.relation
            ));
        }
        Ok(())
    }
}

#[must_use]
pub fn core_relation_descriptors() -> Vec<RelationDescriptor> {
    vec![
        RelationDescriptor::substrate(
            CORE_DERIVED_FROM_RELATION,
            RelationClass::Provenance,
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::all(),
            EntityKindMask::fact_abstraction_goal(),
            AuthorshipKindMask::event_source()
                .union(AuthorshipKindMask::operator())
                .union(AuthorshipKindMask::engine())
                .union(AuthorshipKindMask::external_agent()),
        ),
        RelationDescriptor::substrate(
            CORE_SUPERSEDES_RELATION,
            RelationClass::Supersession,
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::abstraction_perspective_goal(),
            EntityKindMask::abstraction_perspective_goal(),
            AuthorshipKindMask::core(),
        ),
        RelationDescriptor::substrate(
            CORE_INSPIRES_RELATION,
            RelationClass::Causal,
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::goal(),
            EntityKindMask::perspective(),
            AuthorshipKindMask::user().union(AuthorshipKindMask::external_agent()),
        ),
        RelationDescriptor::substrate(
            CORE_AUTHORED_RELATION,
            RelationClass::Causal,
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::perspective(),
            EntityKindMask::memory(),
            AuthorshipKindMask::engine().union(AuthorshipKindMask::external_agent()),
        ),
        RelationDescriptor::substrate(
            CORE_DEPENDS_ON_RELATION,
            RelationClass::Structural,
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::memory(),
            EntityKindMask::memory(),
            AuthorshipKindMask::engine().union(AuthorshipKindMask::external_agent()),
        ),
    ]
}

/// Relation resolved from the immutable `FlavorRegistryFrozen` for an
/// edge write. Carries the descriptor plus the typed edge sidecar
/// table when the descriptor references an `EdgePayload` schema.
#[derive(Clone, Copy, Debug)]
pub struct RegisteredRelation<'a> {
    pub descriptor: &'a RelationDescriptor,
    pub payload_sidecar_table: Option<&'a str>,
    pub(crate) registry: &'a FlavorRegistryFrozen,
}

impl RegisteredRelation<'_> {
    #[must_use]
    pub fn registry(&self) -> &FlavorRegistryFrozen {
        self.registry
    }
}

fn parse_required_tags(relation: &str, side: &str, tags: &[&str]) -> BTreeSet<CapabilityTag> {
    tags.iter()
        .map(|tag| {
            CapabilityTag::parse(*tag).unwrap_or_else(|err| {
                panic!("RelationDescriptor {relation:?} has invalid {side} capability tag: {err}")
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION, CORE_INSPIRES_RELATION,
        CORE_SUPERSEDES_RELATION, EndpointBinding, EntityKindMask, RelationClass, SchemaId,
        SchemaRef, SchemaVersion, core_relation_descriptors,
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

    #[test]
    fn causal_descriptor_cannot_admit_fact_to_fact() {
        let descriptor = super::RelationDescriptor::substrate(
            "test/bad",
            RelationClass::Causal,
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::fact(),
            EntityKindMask::fact(),
            super::AuthorshipKindMask::perspective_link(),
        );
        assert!(descriptor.validate_descriptor().is_err());
    }

    #[test]
    fn untagged_relations_have_empty_required_tags() {
        let substrate = super::RelationDescriptor::substrate(
            "test/untagged-substrate",
            RelationClass::Structural,
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::fact(),
            EntityKindMask::fact(),
            super::AuthorshipKindMask::external_agent(),
        );
        assert!(substrate.source_required_tags.is_empty());
        assert!(substrate.target_required_tags.is_empty());

        let typed = super::RelationDescriptor::typed(
            "test/untagged-typed",
            RelationClass::Structural,
            SchemaRef::new(SchemaId::new("test/edge-v1".into()), SchemaVersion::new(1)),
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::fact(),
            EntityKindMask::fact(),
            super::AuthorshipKindMask::external_agent(),
        );
        assert!(typed.source_required_tags.is_empty());
        assert!(typed.target_required_tags.is_empty());
    }

    #[test]
    fn tagged_descriptor_preserves_shape_validation() {
        let descriptor = super::RelationDescriptor::substrate(
            "test/assigned-to",
            RelationClass::Structural,
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::fact(),
            EntityKindMask::goal(),
            super::AuthorshipKindMask::external_agent(),
        )
        .with_required_tags(&["task"], &["actor"]);

        descriptor
            .validate_edge_shape(
                "Fact",
                EndpointBinding::Pin,
                "Goal",
                EndpointBinding::Pin,
                "ExternalAgent",
            )
            .expect("capability tags do not change relation shape validation");
        assert_eq!(
            descriptor
                .source_required_tags
                .iter()
                .map(super::CapabilityTag::as_str)
                .collect::<Vec<_>>(),
            ["task"],
        );
        assert_eq!(
            descriptor
                .target_required_tags
                .iter()
                .map(super::CapabilityTag::as_str)
                .collect::<Vec<_>>(),
            ["actor"],
        );
    }

    #[test]
    fn core_authored_allows_perspective_to_abstraction() {
        let descriptor = core_relation_descriptors()
            .into_iter()
            .find(|d| d.relation == CORE_AUTHORED_RELATION)
            .expect("core/authored descriptor");
        descriptor
            .validate_edge_shape(
                "Perspective",
                EndpointBinding::Pin,
                "Abstraction",
                EndpointBinding::Pin,
                "Engine",
            )
            .expect("Perspective can frame an Abstraction");
    }

    #[test]
    fn pin_descriptors_reject_follow_head_endpoint_shape() {
        let substrate = super::RelationDescriptor::substrate(
            "test/pin-substrate",
            RelationClass::Structural,
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::fact(),
            EntityKindMask::fact(),
            super::AuthorshipKindMask::event_source(),
        );
        assert!(
            substrate
                .validate_edge_shape(
                    "Fact",
                    EndpointBinding::FollowHead,
                    "Fact",
                    EndpointBinding::Pin,
                    "EventSource",
                )
                .is_err()
        );

        let typed = super::RelationDescriptor::typed(
            "test/pin-typed",
            RelationClass::Structural,
            SchemaRef::new(
                SchemaId::new("test/pin-edge-v1".into()),
                SchemaVersion::new(1),
            ),
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::fact(),
            EntityKindMask::fact(),
            super::AuthorshipKindMask::event_source(),
        );
        assert!(
            typed
                .validate_edge_shape(
                    "Fact",
                    EndpointBinding::Pin,
                    "Fact",
                    EndpointBinding::FollowHead,
                    "EventSource",
                )
                .is_err()
        );
    }
}
