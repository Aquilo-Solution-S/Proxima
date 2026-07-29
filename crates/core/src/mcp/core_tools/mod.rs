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

/// Reject `limit: 0` on any paged read.
///
/// The two ends of the range are not symmetric. A limit *above* the
/// maximum is clamped, because "as many as you will give me" is still the
/// caller's intent and the page they get answers it. Zero answers nothing:
/// it produces a well-formed empty page that is indistinguishable from
/// "nothing matched", so a mistyped bound reads as a real absence. Same
/// reasoning as `search_memories`' `body_max_chars`, which has rejected
/// zero for exactly this since it was written.
pub(crate) fn reject_zero_limit(limit: u32) -> Result<(), crate::mcp::McpToolError> {
    if limit == 0 {
        return Err(crate::mcp::McpToolError::InvalidInput(
            "limit must be at least 1".into(),
        ));
    }
    Ok(())
}

/// Resolve an optional wire `limit` to `1..=`[`MAX_PAGE_LIMIT`],
/// defaulting to [`DEFAULT_PAGE_LIMIT`] when omitted and rejecting zero
/// per [`reject_zero_limit`].
pub(crate) fn resolve_page_limit(limit: Option<u32>) -> Result<u32, crate::mcp::McpToolError> {
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);
    reject_zero_limit(limit)?;
    Ok(limit.min(MAX_PAGE_LIMIT))
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

#[cfg(test)]
mod page_limit_tests {
    use super::{DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT, reject_zero_limit, resolve_page_limit};

    #[test]
    fn zero_is_rejected_and_the_upper_bound_is_clamped() {
        assert!(reject_zero_limit(0).is_err());
        assert!(reject_zero_limit(1).is_ok());
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

    /// The MCP layer used to clamp `0` to `1`, which meant the engine's own
    /// `limit == 0` guards (`engine::query`, `engine::read_verbs`) could
    /// never fire through a tool call. Rejecting here keeps the two layers
    /// saying the same thing rather than one hiding the other.
    #[test]
    fn the_rejection_names_the_bound_it_wants() {
        let crate::mcp::McpToolError::InvalidInput(message) = reject_zero_limit(0).unwrap_err()
        else {
            panic!("a zero limit must be invalid input, not any other error kind");
        };
        assert!(
            message.contains("at least 1"),
            "the message must tell the caller the bound: {message}"
        );
    }
}
