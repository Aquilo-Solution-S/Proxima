//! Host-facing facade exports.

pub use crate::app::{AppContext, AppInfo, Authz, FlavorApp};
pub use crate::config::EmbedConfig;
pub use crate::core_mcp::{CoreMcpError, CoreMcpErrorKind, CoreMcpTools, CoreToolInfo};
pub use crate::migrations::{
    MigrationError, MigrationRunReport, NamedMigrator, run_core_and_flavor_migrations,
};
pub use crate::runtime::{
    BuiltProxima, Proxima, RunningProxima, layered_router, layered_router_with_revalidation, run,
};
pub use crate::runtime_config::{
    McpSettings, ProximaError, RuntimeBuilder, RuntimeConfig, RuntimeParts,
};
pub use proxima_core::compliance::{
    ComplianceEraseCounts, ComplianceEraseOutcome, ComplianceEraseRefusal, ComplianceEraseRequest,
    ComplianceEraseTarget,
};
pub use proxima_core::cursor::Cursor;
pub use proxima_core::error::ProtocolError;
pub use proxima_core::llm;
pub use proxima_core::verbs::change_history::{ChangeHistoryRequest, ChangeHistoryResponse};
pub use proxima_core::verbs::fact_ingest::{
    CitationSpec, FactIngestOutcome, FactReceiptDraft, FactWriteCommand,
};
pub use proxima_core::verbs::goal_write::{
    GoalAssignmentTarget, GoalAuthorship, GoalCreateRequest, GoalEvidenceRef, GoalPayloadWrite,
    GoalState, GoalWriteBuildError, GoalWriteOutcome, IdempotencyKey, MAX_GOAL_TEXT_CHARS,
    MAX_GOAL_TITLE_CHARS, OperatorKind, SystemOrigin,
};
pub use proxima_core::verbs::mcp_call_history::{
    MAX_MCP_CALL_HISTORY_LIMIT, McpCallHistoryRequest, McpCallHistoryResponse, McpCallRecord,
};
pub use proxima_core::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
pub use proxima_core::verbs::query::{
    EdgeExistsRequest, EdgeExistsResponse, EdgeFilter, EdgeReadRequest, EdgeReadResponse, EdgeRow,
    EntityKind, FactCitationReadback, MemoryLineageDirection, MemoryLineageEdge, MemoryLineageNode,
    MemoryLineageRequest, MemoryLineageResponse, MemoryRow, QueryRequest, QueryResponse,
    SupersessionStatus, TombstoneFilter,
};
pub use proxima_core::verbs::schema::{
    PayloadKind, RelationInfo, RelationPayloadSchemaRef, SchemaRequest, SchemaResponse,
};
pub use proxima_core::{
    AuthPath, AuthzContext, EmbeddingAnnObservability, EmbeddingJobBacklog, EmbeddingOrphanCounts,
    EmbeddingOrphanSweepOutcome, EmbeddingRecallCanary, Engine, EngineHandle, FlavorRegistryFrozen,
    MemoryId, Owner, OwnerAccessPort, OwnerExternalKeyParseError, OwnerRef, Relation,
    SourceBatchId, StorageError, ToolScope, UPLOADED_BLOB_SCHEMA_ID, UserId, canonical_json_bytes,
    parse_external_key, provider_safe_tool_name,
};
#[cfg(feature = "openai-compat-embed")]
pub use proxima_llm_openai_compat::{
    MISTRAL_EMBED_BASE_URL, MISTRAL_EMBED_MODEL, OpenAiCompatConfig, OpenAiCompatEmbeddingClient,
};
pub use proxima_mcp_server::selfdoc::{build_instructions, how_to_markdown};
pub use proxima_mcp_server::{McpAuthContext, ResourceServerMetadata};
#[cfg(feature = "testkit")]
pub use proxima_pg_testkit as testkit;
/// Stable exported Postgres `OwnerAccessPort` adapter for embedding hosts
/// (see [`proxima_storage_pg::PgOwnerAccessResolver`]).
pub use proxima_storage_pg::PgOwnerAccessResolver;
/// Cancellation token type used by [`BuiltProxima::cancel`] and
/// [`RunningProxima::cancel`].
pub use tokio_util::sync::CancellationToken;

/// Derive an agent-safe MCP tool palette from the frozen registry, excluding
/// every id in `exclude`. Action-scoped tools expand to `tool:action`
/// granularity (Proxima's scope gate authorizes them at that granularity),
/// so excluding a tool's name also excludes every one of its actions in one
/// step — nothing is emitted for an excluded `tool.name` at all, so a newly
/// added action on an already-excluded tool can never silently bypass the
/// exclusion list.
#[must_use]
pub fn tool_palette_excluding(registry: &FlavorRegistryFrozen, exclude: &[&str]) -> ToolScope {
    let excluded: std::collections::HashSet<&str> = exclude.iter().copied().collect();
    let mut entries = Vec::new();
    for tool in registry.list_mcp_tools() {
        if excluded.contains(tool.name) {
            continue;
        }
        if tool.action_arg_specs.is_empty() {
            entries.push(tool.name.to_string());
        } else {
            entries.extend(
                tool.action_arg_specs
                    .iter()
                    .map(|action| format!("{}:{}", tool.name, action.action)),
            );
        }
    }
    entries.sort();
    entries.dedup();
    ToolScope::Palette(entries)
}

#[cfg(test)]
mod tests {
    use super::{FlavorRegistryFrozen, ToolScope, tool_palette_excluding};
    use proxima_core::FlavorRegistry;
    use proxima_core::mcp::McpTool;
    use proxima_core::mcp::core_tools::{CoreGoalTool, SearchMemoriesTool};

    // `FlavorRegistry::default()` already registers every substrate tool
    // (see `core_tools::register_all`), including the action-scoped
    // `CoreGoalTool` and the flat `SearchMemoriesTool` used below — no
    // flavor needs to be linked in for this palette-derivation test.
    fn registry() -> FlavorRegistryFrozen {
        FlavorRegistry::new().freeze_or_panic_for_tests()
    }

    #[test]
    fn excluding_an_action_scoped_tool_removes_every_action_entry() {
        let registry = registry();

        let scope = tool_palette_excluding(&registry, &[CoreGoalTool::NAME]);

        let ToolScope::Palette(entries) = &scope else {
            panic!("expected a palette scope")
        };
        assert!(!entries.iter().any(|entry| entry == CoreGoalTool::NAME));
        let action_prefix = format!("{}:", CoreGoalTool::NAME);
        assert!(
            !entries
                .iter()
                .any(|entry| entry.starts_with(&action_prefix)),
            "excluding the tool name must also exclude every tool:action expansion"
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry == SearchMemoriesTool::NAME)
        );
    }

    #[test]
    fn unexcluded_action_scoped_tool_keeps_every_action() {
        let registry = registry();

        let scope = tool_palette_excluding(&registry, &[]);

        assert!(scope.allows_action(CoreGoalTool::NAME, "set"));
        assert!(scope.allows(SearchMemoriesTool::NAME));
        assert!(
            !scope.allows(CoreGoalTool::NAME),
            "flat entry must not leak for an action-scoped tool"
        );
    }

    #[test]
    fn excluding_an_unrelated_tool_keeps_others_untouched() {
        let registry = registry();

        let scope = tool_palette_excluding(&registry, &["some_other_tool"]);

        assert!(scope.allows_action(CoreGoalTool::NAME, "set"));
        assert!(scope.allows(SearchMemoriesTool::NAME));
    }
}
