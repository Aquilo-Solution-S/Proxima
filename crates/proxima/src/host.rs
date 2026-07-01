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
    AuthPath, AuthzContext, Engine, EngineHandle, MemoryId, Owner, OwnerRef, Relation,
    SourceBatchId, StorageError, ToolScope, UPLOADED_BLOB_SCHEMA_ID, UserId, canonical_json_bytes,
    provider_safe_tool_name,
};
#[cfg(feature = "openai-compat-embed")]
pub use proxima_llm_openai_compat::{
    MISTRAL_EMBED_BASE_URL, MISTRAL_EMBED_MODEL, OpenAiCompatConfig, OpenAiCompatEmbeddingClient,
};
pub use proxima_mcp_server::selfdoc::{build_instructions, how_to_markdown};
pub use proxima_mcp_server::{McpAuthContext, ResourceServerMetadata};
#[cfg(feature = "testkit")]
pub use proxima_pg_testkit as testkit;
