//! The wire shapes the lane speaks, and nothing else.
//!
//! These are re-exported from the crate root, so they are the only types
//! under `store/` a caller can name. Keeping them free of logic means the
//! public surface can be read in one file.

use proxima_core::OwnerId;

/// Tauri/TS-compatible cited-blob upload request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CitedBlobUploadPrepareTs {
    pub owner: OwnerId,
    pub filename: String,
    pub mime: String,
    pub byte_len: u64,
}

/// Tauri/TS-compatible cited-blob upload preparation response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CitedBlobUploadPrepareOutcomeTs {
    pub upload_id: String,
    pub upload_url: String,
    pub expires_at: String,
    pub headers: Vec<PresignedHeaderTs>,
}

/// Header required by a presigned upload.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PresignedHeaderTs {
    pub name: String,
    pub value: String,
}

/// Tauri/TS-compatible cited-blob completion request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CitedBlobUploadCompleteTs {
    pub owner: OwnerId,
    pub upload_id: String,
}

/// Tauri/TS-compatible cited-blob abort request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CitedBlobUploadAbortTs {
    pub owner: OwnerId,
    pub upload_id: String,
}

/// Tauri/TS-compatible cited-blob abort response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CitedBlobUploadAbortOutcomeTs {
    pub aborted: bool,
}

/// Tauri/TS-compatible cited-blob read URL request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CitedBlobReadUrlTs {
    pub owner: OwnerId,
    pub cited_object_id: String,
}

/// Tauri/TS-compatible cited-blob read URL response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CitedBlobReadUrlOutcomeTs {
    pub read_url: String,
    pub expires_at: String,
}

impl CitedBlobUploadPrepareTs {
    /// The storage `Owner` for this request.
    #[must_use]
    pub fn owner(&self) -> OwnerId {
        self.owner
    }
}

impl CitedBlobUploadCompleteTs {
    /// The storage `Owner` for this request.
    #[must_use]
    pub fn owner(&self) -> OwnerId {
        self.owner
    }
}

impl CitedBlobUploadAbortTs {
    /// The storage `Owner` for this request.
    #[must_use]
    pub fn owner(&self) -> OwnerId {
        self.owner
    }
}

impl CitedBlobReadUrlTs {
    /// The storage `Owner` for this request.
    #[must_use]
    pub fn owner(&self) -> OwnerId {
        self.owner
    }
}
