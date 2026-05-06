//! Goal flavor — typed goal payloads, MotivatedBy edge, and MCP goal tools.

pub mod migrations;
pub mod payloads;
pub mod relations;
pub mod tools;

pub use migrations::migrator;
pub use payloads::{SimpleTextGoalV1, TaskGoalV1};
pub use relations::{MOTIVATED_BY_RELATION, descriptor as motivated_by_descriptor};

proxima_core::proxima_flavor! {
    name = "proxima-goal",
    fact_schemas = [],
    abstraction_schemas = [],
    perspective_schemas = [],
    goal_schemas = [
        payloads::SimpleTextGoalV1,
        payloads::TaskGoalV1,
    ],
    edge_schemas = [],
    relations = [
        relations::descriptor(),
    ],
    mcp_tools = [],
}
