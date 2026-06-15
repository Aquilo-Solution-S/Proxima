//! `persist_mcp_call` verb typed surface.
//!
//! Engine-internal materialization of one MCP tool call as a Fact with
//! a content-addressed inline I/O citation object.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    CitationMappingPayload, CitedObjectPayload, EventId, FactPayload, MemoryId, Owner, SchemaId,
    SchemaVersion, SourceId, canonical_json_bytes, proxima_schema_id,
};

pub const MCP_CALL_FACT_SCHEMA: &str = proxima_schema_id!("mcp-call-logged-v1");
pub const MCP_CALL_IO_SCHEMA: &str = proxima_schema_id!("mcp-call-io-v1");
pub const MCP_CALL_CITATION_SCHEMA: &str = proxima_schema_id!("mcp-call-io-citation-v1");
pub const MCP_CALL_SOURCE_ID: &str = "proxima-core/mcp-call";

#[derive(Debug, Clone)]
pub struct McpCallLogInput {
    pub owner: Owner,
    pub actor_oid: String,
    pub actor_upn: String,
    pub tool_name: String,
    pub ok: bool,
    pub error: Option<String>,
    pub latency_ms: u32,
    pub io_body: Vec<u8>,
    pub io_byte_len_original: u64,
    pub io_truncated: bool,
    pub observed_at: time::OffsetDateTime,
    pub occurred_at: time::OffsetDateTime,
}

impl McpCallLogInput {
    #[must_use]
    pub fn io_content_hash(&self) -> [u8; 32] {
        *blake3::hash(&self.io_body).as_bytes()
    }

    #[must_use]
    pub fn payload(&self) -> McpCallLoggedV1 {
        McpCallLoggedV1 {
            tool_name: self.tool_name.clone(),
            actor_oid: self.actor_oid.clone(),
            actor_upn: self.actor_upn.clone(),
            ok: self.ok,
            error: self.error.clone(),
            latency_ms: self.latency_ms,
            io_byte_len: self.io_byte_len_original,
            io_truncated: self.io_truncated,
            io_content_hash: self.io_content_hash(),
        }
    }

    /// Whole-verb replay key. Identical call occurrences under the same
    /// Owner replay; repeated calls with identical I/O at different
    /// timestamps remain distinct Facts while sharing the cited I/O object
    /// through `content_hash`.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the typed Fact payload cannot
    /// be encoded for the event hash.
    pub fn event_id(&self) -> Result<EventId, serde_json::Error> {
        let payload = serde_json::to_value(self.payload())?;
        let payload = canonical_json_bytes(&payload);

        let mut hasher = blake3::Hasher::new();
        hasher.update(SourceId::new(MCP_CALL_SOURCE_ID).as_str().as_bytes());
        hasher.update(b"\0");
        let (kind, id, org_id) = self.owner.columns();
        hasher.update(kind.as_str().as_bytes());
        hasher.update(b"\0");
        hasher.update(id.as_bytes());
        hasher.update(b"\0");
        hasher.update(org_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(&payload);
        hasher.update(b"\0");
        hasher.update(&self.observed_at.unix_timestamp_nanos().to_le_bytes());
        hasher.update(b"\0");
        hasher.update(&self.occurred_at.unix_timestamp_nanos().to_le_bytes());
        Ok(EventId::new(*hasher.finalize().as_bytes()))
    }

    #[must_use]
    pub fn fact_schema_version(&self) -> SchemaVersion {
        SchemaVersion::new(1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct McpCallLoggedV1 {
    pub tool_name: String,
    pub actor_oid: String,
    pub actor_upn: String,
    pub ok: bool,
    pub error: Option<String>,
    pub latency_ms: u32,
    pub io_byte_len: u64,
    pub io_truncated: bool,
    pub io_content_hash: [u8; 32],
}

impl FactPayload for McpCallLoggedV1 {
    const SCHEMA_ID: &'static str = MCP_CALL_FACT_SCHEMA;
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        let status = if self.ok { "ok" } else { "error" };
        format!(
            "MCP call {} {status} ({} ms)",
            self.tool_name, self.latency_ms
        )
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_core.mcp_call_logged_v1")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct McpCallIoV1 {
    pub content_hash: [u8; 32],
    pub byte_len: u64,
    pub truncated: bool,
}

impl CitedObjectPayload for McpCallIoV1 {
    const SCHEMA_ID: &'static str = MCP_CALL_IO_SCHEMA;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.cited_mcp_call_io_v1"
    }

    fn idempotency_key(&self) -> [u8; 32] {
        self.content_hash
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct McpCallIoCitationV1;

impl CitationMappingPayload for McpCallIoCitationV1 {
    const SCHEMA_ID: &'static str = MCP_CALL_CITATION_SCHEMA;
    const SCHEMA_VERSION: u32 = 1;

    // Pure link — no sidecar table (uses the trait default `None`). The
    // citation_mappings row carries the whole mapping.

    fn cited_object_schema() -> SchemaId {
        SchemaId::new(MCP_CALL_IO_SCHEMA.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpCallLogOutcome {
    pub event_id: EventId,
    pub fact_memory_id: MemoryId,
    pub cited_object_id: Uuid,
    pub citation_mapping_id: Uuid,
    pub change_event_seq: Uuid,
    pub idempotent_replay: bool,
}
