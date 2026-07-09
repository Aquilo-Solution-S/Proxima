//! `read_mcp_call_history` verb — bounded newest-first read of one Owner's
//! MCP-call activity log, optionally scoped to a single actor. Read-side
//! counterpart to `persist_mcp_call`. See docs/14 §protocol surface.

use crate::{MemoryId, OwnerRef};

pub const MAX_MCP_CALL_HISTORY_LIMIT: u32 = 200;

#[derive(Debug, Clone)]
pub struct McpCallHistoryRequest {
    pub owner: OwnerRef,
    /// `Some` => scope to one actor (per-user privacy view); `None` => all
    /// actors under the Owner.
    pub actor_oid: Option<String>,
    pub limit: u32,
    /// Hydrate the inline I/O `body` (and its citation joins) — the biggest
    /// payload cost. `false` (default) omits it; each returned `io_body` is
    /// then `None`. NOTE: this flips the historical always-load-body
    /// behavior; callers that render bodies must opt in.
    pub include_body: bool,
    /// Keyset pagination cursor: return only rows strictly older than
    /// `(created_at, memory_id)`. `None` starts from the newest row. Mirrors
    /// `query_memories`' `(created_at, memory_id)` tiebreak so paging past the
    /// `MAX_MCP_CALL_HISTORY_LIMIT` cap is stable. Build the next cursor from
    /// the last returned record's `(at, memory_id)`.
    pub before: Option<(time::OffsetDateTime, uuid::Uuid)>,
}

#[derive(Debug, Clone)]
pub struct McpCallRecord {
    pub at: time::OffsetDateTime,
    /// Logged-call Fact memory id; forms the keyset-cursor tiebreak with `at`.
    pub memory_id: MemoryId,
    pub tool_name: String,
    pub ok: bool,
    pub error: Option<String>,
    /// `None` when the request did not set `include_body`, or the call has no
    /// inline I/O body.
    pub io_body: Option<Vec<u8>>,
    pub io_truncated: bool,
}

#[derive(Debug, Clone)]
pub struct McpCallHistoryResponse {
    pub calls: Vec<McpCallRecord>,
}
