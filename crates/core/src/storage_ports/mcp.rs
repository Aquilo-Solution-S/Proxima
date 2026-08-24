use crate::storage::StorageError;
use crate::verbs::mcp_call_history::{McpCallHistoryRequest, McpCallHistoryResponse};

// There is no MCP-call WRITE port beside this read one, and adding one back
// would be the defect it replaced: `Engine::persist_mcp_call` writes through
// the governed typed-Fact path, so the admission row it lands declares
// `proxima_core.mcp_call_logged_v1` and the read below finds it. A private
// write verb landed the memory row with an empty `sidecar_tables` stamp and
// no typed row at all, which left this read answering nothing.

#[async_trait::async_trait]
pub trait McpCallReadPort: Send + Sync {
    async fn read_mcp_call_history(
        &self,
        req: &McpCallHistoryRequest,
    ) -> Result<McpCallHistoryResponse, StorageError>;
}
