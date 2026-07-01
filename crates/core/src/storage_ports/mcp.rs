use crate::storage::StorageError;
use crate::verbs::mcp_call_history::{McpCallHistoryRequest, McpCallHistoryResponse};
use crate::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};

#[async_trait::async_trait]
pub trait McpCallWritePort: Send + Sync {
    async fn persist_mcp_call_atomic(
        &self,
        input: &McpCallLogInput,
    ) -> Result<McpCallLogOutcome, StorageError>;
}

#[async_trait::async_trait]
pub trait McpCallReadPort: Send + Sync {
    async fn read_mcp_call_history(
        &self,
        req: &McpCallHistoryRequest,
    ) -> Result<McpCallHistoryResponse, StorageError>;
}
