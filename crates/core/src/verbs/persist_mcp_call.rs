//! `persist_mcp_call` verb typed surface.
//!
//! Engine-internal materialization of one MCP tool call as a Fact with
//! a content-addressed inline I/O citation object.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    CitationMappingPayload, CitedObjectPayload, FactPayload, FactReceiptId, MemoryId, Owner,
    PayloadKeyBuilder, SchemaId, SchemaVersion, SourceId,
};

pub const MCP_CALL_FACT_SCHEMA: &str = "core/mcp-call-logged-v1";
pub const MCP_CALL_IO_SCHEMA: &str = "core/mcp-call-io-v1";
pub const MCP_CALL_CITATION_SCHEMA: &str = "core/mcp-call-io-citation-v1";
pub const MCP_CALL_SOURCE_ID: &str = "core/mcp-call";

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
    #[must_use]
    pub fn receipt_id(&self) -> FactReceiptId {
        let payload_key = self.payload().receipt_key();
        let mut hasher = blake3::Hasher::new();
        hasher.update(SourceId::new(MCP_CALL_SOURCE_ID).as_str().as_bytes());
        hasher.update(b"\0");
        let kind = crate::OwnerRefKind::of(&self.owner);
        let id = self.owner.stable_key_uuid();
        hasher.update(kind.as_str().as_bytes());
        hasher.update(b"\0");
        hasher.update(id.as_bytes());
        hasher.update(b"\0");
        hasher.update(&payload_key);
        hasher.update(b"\0");
        hasher.update(&self.observed_at.unix_timestamp_nanos().to_le_bytes());
        hasher.update(b"\0");
        hasher.update(&self.occurred_at.unix_timestamp_nanos().to_le_bytes());
        FactReceiptId::new(*hasher.finalize().as_bytes())
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

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("tool_name", &self.tool_name);
        key.field_str("actor_oid", &self.actor_oid);
        key.field_str("actor_upn", &self.actor_upn);
        key.field_bool("ok", self.ok);
        key.field_option_str("error", self.error.as_deref());
        key.field_u32("latency_ms", self.latency_ms);
        key.field_u64("io_byte_len", self.io_byte_len);
        key.field_bool("io_truncated", self.io_truncated);
        key.field_bytes("io_content_hash", &self.io_content_hash);
        key.finish()
    }

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
    pub body: Vec<u8>,
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
    pub receipt_id: FactReceiptId,
    pub fact_memory_id: MemoryId,
    pub cited_object_id: Uuid,
    pub citation_mapping_id: Uuid,
    pub change_event_seq: Uuid,
    pub idempotent_replay: bool,
}

#[cfg(test)]
mod tests {
    use super::McpCallLogInput;
    use crate::{OwnerRef, UserId};
    use uuid::Uuid;

    /// Pins the org-free MCP-call replay key against drift. Track B / S0:
    /// the BLAKE3 folds source ‖ principal kind/id ‖ payload key ‖
    /// timestamps — no org. A fixed input must reproduce exactly this hex.
    #[test]
    fn mcp_call_receipt_id_golden_is_org_free() {
        let owner = OwnerRef::Personal(UserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid literal"),
        ));
        let input = McpCallLogInput {
            owner,
            actor_oid: "actor-oid".to_string(),
            actor_upn: "actor@example.com".to_string(),
            tool_name: "golden/tool".to_string(),
            ok: true,
            error: None,
            latency_ms: 42,
            io_body: b"golden-io".to_vec(),
            io_byte_len_original: 9,
            io_truncated: false,
            observed_at: time::OffsetDateTime::UNIX_EPOCH,
            occurred_at: time::OffsetDateTime::UNIX_EPOCH,
        };
        assert_eq!(
            hex::encode(input.receipt_id().into_inner()),
            "6c9590b12d7baac76048bea402909193a398018010b16b00dcd437e4dfe2d469"
        );
    }
}
