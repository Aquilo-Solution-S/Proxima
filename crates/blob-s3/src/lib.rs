//! S3-backed cited-blob service: presigned upload -> confirm -> read.
//!
//! Usable by any composite binary (embedded hosts included) so it can
//! ingest cited blobs directly, independent of any frontend.
//! Tables: `proxima_core.blob` / `proxima_core.blob_uploads`.

mod config;
mod error;
mod store;

pub use config::S3RuntimeConfig;
pub use error::BlobError;
pub use store::{
    CitedBlobReadUrlOutcomeTs, CitedBlobReadUrlTs, CitedBlobStore, CitedBlobUploadAbortOutcomeTs,
    CitedBlobUploadAbortTs, CitedBlobUploadCompleteTs, CitedBlobUploadPrepareOutcomeTs,
    CitedBlobUploadPrepareTs, PresignedHeaderTs, S3ColdStore, cold_object_key,
};
