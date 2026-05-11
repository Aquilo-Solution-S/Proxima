//! Goal flavor — typed goal payloads, `MotivatedBy` edge, and MCP goal tools.

pub mod migrations;
pub mod payloads;
pub mod relations;
pub mod tools;

pub use migrations::migrator;
pub use payloads::{GoalAchievedV1, GoalActivatedV1, GoalProposedV1, SimpleTextGoalV1, TaskGoalV1};
pub use relations::{MOTIVATED_BY_RELATION, descriptor as motivated_by_descriptor};

proxima_core::proxima_flavor! {
    name = "proxima-goal",
    display_name = "Goal",
    fact_schemas = [
        payloads::GoalProposedV1,
        payloads::GoalActivatedV1,
        payloads::GoalAchievedV1,
    ],
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
    mcp_tools = [
        tools::ProposeTool,
        tools::AcceptTool,
        tools::ModifyTool,
        tools::DeclineTool,
        tools::MarkAchievedTool,
    ],
}
