//! Substrate MCP flavor — agent-authored Facts, A/P rows, and edges.

pub mod migrations;
pub mod payloads;
pub mod tools;

pub use migrations::migrator;
pub use payloads::{AgentDerivationV1, AgentLinkV1, AgentNoteV1};

use proxima_core::{
    AuthorshipKindMask, EntityKindMask, RelationClass, RelationDescriptor, SchemaId, SchemaRef,
    SchemaVersion,
};

pub const AGENT_LINK_RELATION: &str = "proxima-mcp/agent-link-refers-to";

proxima_core::proxima_flavor! {
    name = "proxima-mcp",
    display_name = "MCP",
    fact_schemas = [
        payloads::AgentNoteV1,
    ],
    abstraction_schemas = [
        payloads::AgentDerivationV1,
    ],
    perspective_schemas = [
        payloads::AgentDerivationV1,
    ],
    edge_schemas = [
        payloads::AgentLinkV1,
    ],
    relations = [
        RelationDescriptor::typed(
            AGENT_LINK_RELATION,
            RelationClass::Interpretive,
            SchemaRef::new(
                SchemaId::new("proxima-mcp/agent-link-v1".into()),
                SchemaVersion::new(1),
            ),
            EntityKindMask::abstraction_perspective(),
            EntityKindMask::memory(),
            AuthorshipKindMask::external_agent(),
        ),
    ],
    mcp_tools = [
        tools::search::SearchGraphTool,
        tools::search::OpenTool,
        tools::remember::RememberTool,
        tools::derive::DeriveTool,
        tools::link::LinkTool,
    ],
}
