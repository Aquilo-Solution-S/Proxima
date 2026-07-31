//! The cited-blob store, split by what each piece is responsible for.
//!
//! This file holds only the service handle: its fields, its construction
//! (where the runtime config is validated), and its memoized S3 client.
//! The behaviour lives next door:
//!
//! - [`dto`] — the wire types, the whole public surface
//! - [`upload`] / [`read`] — the request-path verbs
//! - [`port`] — the [`CitedBlobPort`](proxima_core::storage_ports::CitedBlobPort)
//!   adapter, translation only
//! - [`erase`] — Art. 17 purge of an owner's bytes
//! - [`rows`] / [`digest`] / [`keys`] / [`guards`] / [`transitions`] — the
//!   support the verbs are assembled from
//!
//! The split is by responsibility, not by line count: each of those files
//! can be read on its own, and the verbs that share an ordering contract
//! stay together.

mod digest;
mod dto;
mod erase;
mod guards;
mod keys;
mod port;
mod read;
mod rows;
mod transitions;
mod upload;

#[cfg(test)]
mod testkit;

pub use dto::{
    CitedBlobReadUrlOutcomeTs, CitedBlobReadUrlTs, CitedBlobUploadAbortOutcomeTs,
    CitedBlobUploadAbortTs, CitedBlobUploadCompleteTs, CitedBlobUploadPrepareOutcomeTs,
    CitedBlobUploadPrepareTs, PresignedHeaderTs,
};

use crate::config::{
    DEFAULT_MAX_BLOB_BYTES, S3RuntimeConfig, validate_endpoint_url, validate_presign_ttl,
};
use crate::error::BlobError;

/// Cited-blob upload/read service over one Postgres pool and one
/// S3 target. Construct once at boot; methods are independently
/// callable per request.
#[derive(Debug, Clone)]
pub struct CitedBlobStore {
    pool: sqlx::PgPool,
    config: S3RuntimeConfig,
    /// Lazily-built S3 client, memoized so the full credential chain is
    /// resolved once per store rather than on every request.
    client: tokio::sync::OnceCell<aws_sdk_s3::Client>,
}

impl CitedBlobStore {
    /// Validate and normalize the S3 configuration at its consuming boundary.
    ///
    /// # Errors
    /// Returns [`BlobError::Config`] when the configured endpoint violates the
    /// HTTPS-or-loopback transport policy.
    pub fn new(pool: sqlx::PgPool, mut config: S3RuntimeConfig) -> Result<Self, BlobError> {
        if let Some(endpoint_url) = &config.endpoint_url {
            validate_endpoint_url(endpoint_url)?;
        }
        // Checked at the consuming boundary too, not only in `from_env`: a
        // host may build this struct directly, and an out-of-range TTL must
        // fail here rather than panic inside a request.
        validate_presign_ttl("PROXIMA_S3_UPLOAD_TTL_SECONDS", config.upload_ttl_seconds)?;
        validate_presign_ttl("PROXIMA_S3_READ_TTL_SECONDS", config.read_ttl_seconds)?;
        config.max_blob_bytes.get_or_insert(DEFAULT_MAX_BLOB_BYTES);

        Ok(Self {
            pool,
            config,
            client: tokio::sync::OnceCell::new(),
        })
    }

    /// The memoized S3 client, built on first use from the runtime config.
    ///
    /// # Errors
    /// Returns `BlobError` when the AWS client cannot be constructed.
    async fn client(&self) -> Result<&aws_sdk_s3::Client, BlobError> {
        self.client.get_or_try_init(|| self.config.client()).await
    }
}

#[cfg(test)]
mod tests {
    use super::guards::validate_prepare;
    use super::testkit::{lazy_test_pool, prepare_req, store_config};
    use super::*;

    #[tokio::test]
    async fn store_rejects_plaintext_non_loopback_endpoint() {
        let error = CitedBlobStore::new(
            lazy_test_pool(),
            store_config(Some("http://s3.internal:9000"), Some(1_024)),
        )
        .expect_err("plaintext remote S3 endpoint rejected");

        assert!(matches!(error, BlobError::Config(_)));
        assert!(error.to_string().contains("must use https"));
    }

    #[tokio::test]
    async fn store_applies_default_cap_when_config_omits_it() {
        let store = CitedBlobStore::new(lazy_test_pool(), store_config(None, None))
            .expect("default-capped store builds");

        assert_eq!(store.config.max_blob_bytes, Some(DEFAULT_MAX_BLOB_BYTES));
        let error = validate_prepare(
            &prepare_req(DEFAULT_MAX_BLOB_BYTES + 1),
            store.config.max_blob_bytes,
        )
        .expect_err("default cap rejects an oversized prepare request");
        assert!(error.to_string().contains("exceeds max blob size"));
    }

    /// An out-of-range TTL is a startup error, not a request-path panic.
    ///
    /// `read_url` and `prepare_upload` compute `now + Duration::seconds(ttl)`
    /// before ever reaching `PresigningConfig::expires_in`, and that
    /// addition panics on overflow — so the only validator sat downstream of
    /// the arithmetic it was supposed to guard, and a misconfigured host
    /// took down every read instead of refusing to boot.
    #[tokio::test]
    async fn store_rejects_a_ttl_no_presigned_url_could_carry() {
        let mut config = store_config(None, None);
        config.read_ttl_seconds = u64::MAX;

        let error = CitedBlobStore::new(lazy_test_pool(), config)
            .expect_err("an unusable read TTL is refused at construction");
        assert!(matches!(error, BlobError::Config(_)));
        assert!(
            error.to_string().contains("PROXIMA_S3_READ_TTL_SECONDS"),
            "the refusal names the variable to fix: {error}"
        );

        let mut config = store_config(None, None);
        config.upload_ttl_seconds = 0;
        let error = CitedBlobStore::new(lazy_test_pool(), config)
            .expect_err("a zero upload TTL is refused");
        assert!(
            error.to_string().contains("PROXIMA_S3_UPLOAD_TTL_SECONDS"),
            "the refusal names the variable to fix: {error}"
        );

        // The boundary itself is usable, so the bound is not merely small.
        let mut config = store_config(None, None);
        config.read_ttl_seconds = crate::config::MAX_PRESIGN_TTL_SECONDS;
        CitedBlobStore::new(lazy_test_pool(), config).expect("7 days is allowed");
    }
}
