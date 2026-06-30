use proxima_core::MemoryId;

use crate::payloads::FileState;

/// One head row from a NK-scoped sidecar query.
#[derive(Debug, Clone)]
pub struct FileRevisionHead {
    pub memory_id: MemoryId,
    pub file_path: String,
    pub content_sha256: [u8; 32],
    pub state: FileState,
}
