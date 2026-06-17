//! Core long-term memory payloads and relations.

pub mod payloads;

pub use payloads::{AgentDerivationV1, AgentLinkV1, AgentNoteV1, Speaker, UtteranceV1};

use crate::{
    AuthorshipKindMask, EndpointBinding, EntityKindMask, FlavorRegistry, RelationClass,
    RelationDescriptor, SchemaId, SchemaRef, SchemaVersion,
};

pub const AGENT_LINK_RELATION: &str = "core/agent-link-refers-to";

pub(crate) fn register_all(registry: &mut FlavorRegistry) {
    registry.add_fact_schema::<AgentNoteV1>();
    registry.add_fact_schema::<UtteranceV1>();
    registry.add_abstraction_schema::<AgentDerivationV1>();
    registry.add_perspective_schema::<AgentDerivationV1>();
    registry.add_edge_schema::<AgentLinkV1>();
    registry.add_relation(RelationDescriptor::typed(
        AGENT_LINK_RELATION,
        RelationClass::Interpretive,
        SchemaRef::new(
            SchemaId::new("core/agent-link-v1".into()),
            SchemaVersion::new(1),
        ),
        EndpointBinding::Pin,
        EndpointBinding::Pin,
        EntityKindMask::abstraction_perspective(),
        EntityKindMask::memory(),
        AuthorshipKindMask::external_agent(),
    ));
}
