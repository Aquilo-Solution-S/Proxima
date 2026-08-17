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
pub mod list_schemas;
pub mod list_substrate_tools;
pub mod list_wake_candidates;
pub mod membership;
pub mod memory;
pub mod memory_spaces;
pub mod publish;
pub mod upload;
pub mod walk_memory_lineage;
pub(crate) mod wire_ref;

pub use fact::CoreFactTool;
pub use goal::CoreGoalTool;
pub use membership::CoreMembershipTool;
pub use memory::{DeriveTool, ForgetTool, InterpretTool, RecordUtteranceTool, RememberTool};
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

/// Resolve an optional wire `limit` to `1..=`[`MAX_PAGE_LIMIT`],
/// defaulting to [`DEFAULT_PAGE_LIMIT`] when omitted and rejecting zero
/// per [`crate::reject_zero_limit`], which is shared with the flavor tool
/// trait so both surfaces cannot drift on the same rule.
pub(crate) fn resolve_page_limit(limit: Option<u32>) -> Result<u32, crate::mcp::McpToolError> {
    crate::reject_zero_limit(limit)?;
    Ok(limit.unwrap_or(DEFAULT_PAGE_LIMIT).min(MAX_PAGE_LIMIT))
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

/// Register every substrate-shipped MCP tool into the `FlavorRegistry`.
/// Called from `FlavorRegistry::default()`.
pub(crate) fn register_all(
    registry: &mut crate::FlavorRegistry,
) -> Result<(), crate::FlavorRegistryError> {
    registry.try_add_mcp_tool::<SearchMemoriesTool>("core")?;
    registry.try_add_mcp_tool::<MemorySpacesTool>("core")?;
    registry.try_add_mcp_tool::<RememberTool>("core")?;
    registry.try_add_mcp_tool::<ForgetTool>("core")?;
    registry.try_add_mcp_tool::<RecordUtteranceTool>("core")?;
    registry.try_add_mcp_tool::<DeriveTool>("core")?;
    registry.try_add_mcp_tool::<InterpretTool>("core")?;
    registry.try_add_mcp_tool::<CoreGoalTool>("core")?;
    registry.try_add_mcp_tool::<CoreFactTool>("core")?;
    registry.try_add_mcp_tool::<CoreMembershipTool>("core")?;
    registry.try_add_mcp_tool::<CorePublishTool>("core")?;
    registry.try_add_mcp_tool::<CoreUploadTool>("core")?;
    Ok(())
}

#[cfg(test)]
mod page_limit_tests {
    use super::{DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT, resolve_page_limit};
    use crate::reject_zero_limit;

    #[test]
    fn zero_is_rejected_and_the_upper_bound_is_clamped() {
        assert!(reject_zero_limit(Some(0)).is_err());
        assert!(reject_zero_limit(Some(1)).is_ok());
        assert!(reject_zero_limit(None).is_ok());
        assert!(reject_zero_limit(Some(u32::MAX)).is_ok());
        assert!(resolve_page_limit(Some(0)).is_err());
        assert_eq!(resolve_page_limit(Some(1)).unwrap(), 1);
        assert_eq!(
            resolve_page_limit(Some(MAX_PAGE_LIMIT + 1)).unwrap(),
            MAX_PAGE_LIMIT,
        );
        assert_eq!(resolve_page_limit(Some(u32::MAX)).unwrap(), MAX_PAGE_LIMIT);
    }

    #[test]
    fn an_omitted_limit_is_the_default_not_zero() {
        assert_eq!(resolve_page_limit(None).unwrap(), DEFAULT_PAGE_LIMIT);
    }

    /// Zero limit is invalid input, and that mapping survives `From<ToolError>`
    /// into [`crate::mcp::McpToolError`].
    #[test]
    fn the_rejection_survives_the_hop_to_mcp_tool_error() {
        let crate::ToolError::InvalidInput(message) = reject_zero_limit(Some(0)).unwrap_err()
        else {
            panic!("a zero limit must be invalid input, not any other error kind");
        };
        assert!(
            message.contains("at least 1"),
            "the message must tell the caller the bound: {message}"
        );

        let promoted: crate::mcp::McpToolError = reject_zero_limit(Some(0)).unwrap_err().into();
        assert!(
            matches!(promoted, crate::mcp::McpToolError::InvalidInput(_)),
            "the shared helper must still read as invalid input on the MCP surface: {promoted:?}"
        );
    }
}
