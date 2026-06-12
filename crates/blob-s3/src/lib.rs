//! S3-backed cited-blob service: presigned upload -> confirm -> read.
//!
//! Extracted from the shell's Tauri commands so any composite binary
//! (embedded hosts included) can ingest cited blobs without the shell.
//! Tables: `proxima_core.cited_object_uploads` / `cited_objects` /
//! `cited_uploaded_blob_v1` (see storage-pg baseline migration).

mod config;
mod error;
mod store;

pub use config::S3RuntimeConfig;
pub use error::BlobError;
pub use store::{
    CitedBlobReadUrlOutcomeTs, CitedBlobReadUrlTs, CitedBlobStore, CitedBlobUploadAbortOutcomeTs,
    CitedBlobUploadAbortTs, CitedBlobUploadCompleteOutcomeTs, CitedBlobUploadCompleteTs,
    CitedBlobUploadPrepareOutcomeTs, CitedBlobUploadPrepareTs, PresignedHeaderTs,
};
