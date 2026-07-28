//! Substrate-shipped MCP tools registered into every composite binary.

pub mod citation_of_fact;
pub mod fact;
pub mod facts_citing_object;
pub mod search_memories;

pub mod get_graph;
pub mod get_memories;
pub mod get_memory;
pub mod goal;
pub mod goal_reads;
pub mod list_change_events;
pub mod list_edge_types;
pub mod list_schemas;
pub mod list_substrate_tools;
pub mod list_wake_candidates;
pub mod membership;
pub mod memory;
pub mod memory_spaces;
pub mod publish;
pub mod read_edges;
pub mod upload;
pub mod walk_memory_lineage;
pub(crate) mod wire_ref;

pub use fact::CoreFactTool;
pub use goal::CoreGoalTool;
pub use membership::CoreMembershipTool;
pub use memory::{DeriveTool, LinkTool, RecordUtteranceTool, RememberTool};
pub use memory_spaces::MemorySpacesTool;
pub use publish::CorePublishTool;
pub use search_memories::SearchMemoriesTool;
pub use upload::CoreUploadTool;

use crate::mcp::McpToolAnnotations;

/// Shared page bounds for the keyset-paginated read surfaces (edges,
/// goals, citations, membership, lineage, wake candidates). One pair of
/// constants so six tools cannot drift apart on the same contract;
/// `core_list_change_events` deliberately keeps its own larger bounds
/// (pull-log semantics, default 100 / max 1000).
pub(crate) const DEFAULT_PAGE_LIMIT: u32 = 50;
pub(crate) const MAX_PAGE_LIMIT: u32 = 200;

/// Clamp an optional wire `limit` to `1..=`[`MAX_PAGE_LIMIT`], defaulting
/// to [`DEFAULT_PAGE_LIMIT`] when omitted.
pub(crate) fn clamp_page_limit(limit: Option<u32>) -> u32 {
    limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT)
}

const READ_ONLY: McpToolAnnotations = McpToolAnnotations::new().read_only(true).open_world(false);
const WRITE_NON_IDEMPOTENT: McpToolAnnotations = McpToolAnnotations::new()
    .read_only(false)
    .destructive(false)
    .idempotent(false)
    .open_world(false);
const WRITE_IDEMPOTENT: McpToolAnnotations = McpToolAnnotations::new()
    .read_only(false)
    .destructive(false)
    .idempotent(true)
    .open_world(false);
const DESTRUCTIVE_NON_IDEMPOTENT: McpToolAnnotations = McpToolAnnotations::new()
    .read_only(false)
    .destructive(true)
    .idempotent(false)
    .open_world(false);
#[allow(dead_code)]
const DESTRUCTIVE_IDEMPOTENT: McpToolAnnotations = McpToolAnnotations::new()
    .read_only(false)
    .destructive(true)
    .idempotent(true)
    .open_world(false);

/// Register every substrate-shipped MCP tool into the `FlavorRegistry`.
/// Called from `FlavorRegistry::default()`.
pub(crate) fn register_all(
    registry: &mut crate::FlavorRegistry,
) -> Result<(), crate::FlavorRegistryError> {
    registry.try_add_mcp_tool::<SearchMemoriesTool>("core")?;
    registry.try_add_mcp_tool::<MemorySpacesTool>("core")?;
    registry.try_add_mcp_tool::<RememberTool>("core")?;
    registry.try_add_mcp_tool::<RecordUtteranceTool>("core")?;
    registry.try_add_mcp_tool::<DeriveTool>("core")?;
    registry.try_add_mcp_tool::<LinkTool>("core")?;
    registry.try_add_mcp_tool::<CoreGoalTool>("core")?;
    registry.try_add_mcp_tool::<CoreFactTool>("core")?;
    registry.try_add_mcp_tool::<CoreMembershipTool>("core")?;
    registry.try_add_mcp_tool::<CorePublishTool>("core")?;
    registry.try_add_mcp_tool::<CoreUploadTool>("core")?;
    Ok(())
}
