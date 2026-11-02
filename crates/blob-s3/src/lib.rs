//! S3-backed cited-blob service: presigned upload -> confirm -> read.
//!
//! Usable by any composite binary (embedded hosts included) so it can
//! ingest cited blobs directly, independent of any frontend.
//! Tables: `proxiima_core.blob` / `proxiima_core.blob_uploads`.

mod config;
mod error;
mod store;

pub use config::{S3KeyPair, S3RuntimeConfig};
pub use error::BlobError;
pub use store::{
    CitedBlobReadUrlOutcomeTs, CitedBlobReadUrlTs, CitedBlobStore, CitedBlobUploadAbortOutcomeTs,
    CitedBlobUploadAbortTs, CitedBlobUploadCompleteTs, CitedBlobUploadPrepareOutcomeTs,
    CitedBlobUploadPrepareTs, PresignedHeaderTs, S3ColdStore, UploadExpirySweepOutcome,
};
