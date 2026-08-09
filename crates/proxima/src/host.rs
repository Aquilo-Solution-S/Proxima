//! Host-facing facade exports.

pub use crate::app::{AppContext, AppInfo, Authz, FlavorApp};
pub use crate::config::EmbedConfig;
pub use crate::core_mcp::{CoreMcpError, CoreMcpErrorKind, CoreMcpTools, CoreToolInfo};
pub use crate::migrations::{
    MigrationError, MigrationRunReport, NamedMigrator, preflight_without_migrations,
    run_core_and_flavor_migrations,
};
pub use crate::runtime::{
    BuiltProxima, Proxima, RunningProxima, layered_router, layered_router_with_revalidation, run,
};
pub use crate::runtime_config::{
    McpSettings, ProximaError, RuntimeBuilder, RuntimeConfig, RuntimeParts,
};
/// The S3-backed cited-blob lane.
///
/// [`BuiltProxima::blobs`] is a `pub` field of type `Option<CitedBlobStore>`
/// and [`crate::Proxima::s3`] is a `pub` method taking `S3RuntimeConfig`, so
/// both types were already part of the public surface — just not nameable
/// from `proxima`. A flavor could reach them by inference and could not
/// write either one in a signature, store one in a struct, or configure S3
/// programmatically; `S3RuntimeConfig::from_env` was the only route in, and
/// it reads process environment a library has no business requiring.
///
/// `BlobError` comes with them because `from_env` returns it.
pub use proxima_blob_s3::{BlobError, CitedBlobStore, S3RuntimeConfig};
/// Compliance erase surface. Note that [`ComplianceEraseTarget`]'s
/// variants take id newtypes rather than bare UUIDs and strings:
/// `GroupOwner` takes a [`GroupId`], the two source-scope variants add a
/// [`SourceId`], and the personal variants take a [`UserId`]. All of
/// those are re-exported below, so every variant of this enum is
/// constructible by a host depending on `proxima` alone — an exported
/// enum whose variants cannot be built is not actually exported.
pub use proxima_core::compliance::{
    ComplianceEraseCounts, ComplianceEraseOutcome, ComplianceEraseRefusal, ComplianceEraseRequest,
    ComplianceEraseTarget,
};
pub use proxima_core::cursor::Cursor;
/// The read verb a flavor searches its own corpus with.
///
/// [`proxima_core::Engine::search`] was already public, but every type in
/// its signature was off the facade — so a flavor could write a corpus and
/// had no sanctioned way to query it. Its own MCP tools would have had to
/// re-implement search against raw SQL, which is exactly the coupling the
/// tiered facade exists to prevent.
///
/// [`MemorySearchRequest::tags`] is worth knowing about: it is the only
/// predicate that narrows a search to a subset of a corpus. `schema_id` is
/// exact-match and there is no per-column filter, so a flavor that wants
/// "search inside this book" declares a `tag_column` on its projection and
/// filters here.
pub use proxima_core::engine::{
    ListWakeCandidatesReadRequest, ListWakeCandidatesReadResponse, SearchReadRequest,
    SearchReadResponse,
};
pub use proxima_core::error::ProtocolError;
pub use proxima_core::llm;
/// [`EmbedCaps`] is the second parameter of
/// [`OpenAiCompatEmbeddingClient::new`], so without it on the facade that
/// constructor is unspellable and `mistral()` is the only embedding client
/// a host depending on `proxima` alone can build. That rules out every
/// other OpenAI-compatible endpoint — a local Ollama, a self-hosted
/// inference server, any provider needing `matryoshka` to return
/// [`llm::EMBEDDING_DIM`] rather than its native width.
pub use proxima_core::models::EmbedCaps;
/// Cited-blob reconciliation surfaces.
///
/// Global [`CitedBlobStore::reconcile_all`] requires the booted runtime's
/// [`crate::SystemAuthority`] and returns the operator DTO, including raw
/// locator samples needed for restore work. Flavor tools use the separately
/// authorized owner port/service; its DTO carries cited-object ids and counts
/// but never bucket names or object keys.
pub use proxima_core::storage_ports::{
    CitedBlobMissingObject, CitedBlobOwnerMissingObject, CitedBlobOwnerReconcileOutcome,
    CitedBlobOwnerReconcilePort, CitedBlobOwnerReconcileService, CitedBlobReconcileOutcome,
    MAX_RECONCILE_SAMPLE,
};
pub use proxima_core::verbs::change_history::{ChangeHistoryRequest, ChangeHistoryResponse};
pub use proxima_core::verbs::fact_ingest::{
    CitationSpec, FactIngestOutcome, FactReceiptDraft, FactWriteCommand,
};
pub use proxima_core::verbs::goal_write::{
    GoalAssignmentTarget, GoalAuthorship, GoalCreateRequest, GoalEvidenceRef, GoalPayloadWrite,
    GoalState, GoalWakeConfigWrite, GoalWakeToolId, GoalWakeTrigger, GoalWriteBuildError,
    GoalWriteOutcome, IdempotencyKey, MAX_GOAL_TEXT_CHARS, MAX_GOAL_TITLE_CHARS,
    MAX_WAKE_TOOL_ID_CHARS, OperatorKind, SystemOrigin,
};
// `GoalWriteBuildError`'s variants carry this, so a host that matches on
// them cannot bind the payload without being able to name its type. An
// unnameable type in a public signature is the usual shape of an
// out-of-tree blocker, so it is re-exported beside the error itself.
pub use proxima_core::text_bounds::{TrimmedLenViolation, check_trimmed_len};
pub use proxima_core::verbs::mcp_call_history::{
    MAX_MCP_CALL_HISTORY_LIMIT, McpCallHistoryRequest, McpCallHistoryResponse, McpCallRecord,
};
pub use proxima_core::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
pub use proxima_core::verbs::query::{
    DEFAULT_HYBRID_SEMANTIC_WEIGHT, EdgeExistsRequest, EdgeExistsResponse, EdgeFilter,
    EdgeReadCursor, EdgeReadRequest, EdgeReadResponse, EntityKind, FactCitationReadback,
    MAX_SEARCH_PAGE_LIMIT, MemoryLineageDirection, MemoryLineageEdge, MemoryLineageNode,
    MemoryLineageRequest, MemoryLineageResponse, MemoryRow, MemorySearchPage, MemorySearchRequest,
    MemorySearchResult, QueryRequest, QueryResponse, SearchCursor, SearchMode, SearchOrder,
    SupersessionStatus, TagMatch, TombstoneFilter,
};
pub use proxima_core::verbs::schema::{PayloadKind, SchemaRequest, SchemaResponse};
pub use proxima_core::{
    AuthPath, Authenticator, AuthzContext, EmbeddingAnnObservability, EmbeddingJobBacklog,
    EmbeddingOrphanCounts, EmbeddingOrphanSweepOutcome, EmbeddingRecallCanary, Engine,
    EngineHandle, FlavorRegistryFrozen, FlavorServiceError, FlavorServices, GoalWakeCandidate,
    GoalWakeHardMemory, GroupId, MemoryId, Owner, OwnerAccessPort, OwnerExternalKeyParseError,
    OwnerRef, OwnerRefKind, Relation, Role, SourceBatchId, SourceId, StorageError, ToolScope,
    UserId, canonical_json_bytes, parse_external_key, provider_safe_tool_name,
};
/// The three citation schema ids [`CitationSpec`] is written with:
/// `UPLOADED_BLOB_SCHEMA_ID` names the cited object, and the other two
/// name the locator mapping through which a Fact cites it — the whole
/// object, or a page span within it.
///
/// A flavor citing an uploaded blob names a mapping id in every
/// `CitationSpec::v1` call. `CitationSpec::v1` takes `impl Into<String>`,
/// so leaving two of the three off the facade did not block anything —
/// it silently pushed flavors onto bare string literals that no compiler
/// could check against a rename of the constant they duplicate.
pub use proxima_core::{
    UPLOADED_BLOB_PAGE_SPAN_SCHEMA_ID, UPLOADED_BLOB_SCHEMA_ID, UPLOADED_BLOB_WHOLE_SCHEMA_ID,
};
#[cfg(feature = "openai-compat-embed")]
pub use proxima_llm_openai_compat::{
    MISTRAL_EMBED_BASE_URL, MISTRAL_EMBED_MODEL, OpenAiCompatConfig, OpenAiCompatEmbeddingClient,
};
pub use proxima_mcp_server::selfdoc::{build_instructions, how_to_markdown};
pub use proxima_mcp_server::{HostAllowlist, McpAuthContext, ResourceServerMetadata};
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
