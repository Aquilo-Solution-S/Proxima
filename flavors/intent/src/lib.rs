//! Intent flavor — typed Vision Brief abstractions for goal interpretation.

pub mod migrations;
pub mod payloads;

pub use migrations::migrator;
pub use payloads::{VisionAmbitionLevel, VisionBriefV1};

proxima_core::proxima_flavor! {
    name = "proxima-intent",
    display_name = "Intent",
    fact_schemas = [],
    abstraction_schemas = [
        payloads::VisionBriefV1,
    ],
    perspective_schemas = [],
    goal_schemas = [],
    edge_schemas = [],
    relations = [],
    mcp_tools = [],
}
