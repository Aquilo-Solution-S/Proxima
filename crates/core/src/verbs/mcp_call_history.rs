//! `read_mcp_call_history` verb — bounded newest-first read of one Owner's
//! MCP-call activity log, optionally scoped to a single actor. Read-side
//! counterpart to `persist_mcp_call`. See docs/14 §protocol surface.

use crate::OwnerRef;

pub const MAX_MCP_CALL_HISTORY_LIMIT: u32 = 200;

#[derive(Debug, Clone)]
pub struct McpCallHistoryRequest {
    pub principal: OwnerRef,
    /// `Some` => scope to one actor (per-user privacy view); `None` => all
    /// actors under the Owner.
    pub actor_oid: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone)]
pub struct McpCallRecord {
    pub at: time::OffsetDateTime,
    pub tool_name: String,
    pub ok: bool,
    pub error: Option<String>,
    pub io_body: Option<Vec<u8>>,
    pub io_truncated: bool,
}

#[derive(Debug, Clone)]
pub struct McpCallHistoryResponse {
    pub calls: Vec<McpCallRecord>,
}
