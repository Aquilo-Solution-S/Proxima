//! S3-backed cited-blob service: presigned upload -> confirm -> read.
//!
//! Usable by any composite binary (embedded hosts included) so it can
//! ingest cited blobs directly, independent of any frontend.
//! Tables: `proxima_core.cited_object_uploads` / `cited_objects` /
//! `cited_uploaded_blob_v1` (see storage-pg baseline migration).

mod config;
mod error;
mod store;

pub use config::S3RuntimeConfig;
pub use error::BlobError;
pub use store::{
    cold_object_key, cold_owner_prefix, owner_hash_hex_public, CitedBlobReadUrlOutcomeTs,
    S3ColdStore,
    CitedBlobReadUrlTs, CitedBlobStore, CitedBlobUploadAbortOutcomeTs,
    CitedBlobUploadAbortTs, CitedBlobUploadCompleteTs, CitedBlobUploadPrepareOutcomeTs,
    CitedBlobUploadPrepareTs, PresignedHeaderTs,
};
