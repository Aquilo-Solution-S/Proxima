//! The cited-blob store, split by what each piece is responsible for.
//!
//! This file holds only the service handle: its fields, its construction
//! (where the runtime config is validated), and its memoized S3 client.
//! The behaviour lives next door:
//!
//! - [`dto`] — the wire types, the whole public surface
//! - [`upload`] / [`read`] — the presigned request-path verbs
//! - [`verified_read`] — bounded in-process bytes, verified before release
//! - [`port`] — the [`CitedBlobPort`](proxima_core::storage_ports::CitedBlobPort)
//!   adapter, translation only
//! - [`erase`] — Art. 17 purge of an owner's bytes
//! - [`rows`] / [`digest`] / [`keys`] / [`guards`] / [`transitions`] — the
//!   support the verbs are assembled from
//!
//! The split is by responsibility, not by line count: each of those files
//! can be read on its own, and the verbs that share an ordering contract
//! stay together.

mod cold;
mod digest;
mod dto;
mod erase;
mod guards;
mod keys;
mod port;
mod read;
mod reconcile;
mod rows;
mod transitions;
mod upload;
mod verified_read;

#[cfg(test)]
mod testkit;

pub use cold::S3ColdStore;
pub use dto::{
    CitedBlobReadUrlOutcomeTs, CitedBlobReadUrlTs, CitedBlobUploadAbortOutcomeTs,
    CitedBlobUploadAbortTs, CitedBlobUploadCompleteTs, CitedBlobUploadPrepareOutcomeTs,
    CitedBlobUploadPrepareTs, PresignedHeaderTs,
};
pub use keys::cold_object_key;

use crate::config::{
    DEFAULT_MAX_BLOB_BYTES, S3RuntimeConfig, validate_endpoint_url, validate_presign_ttl,
};
use crate::error::BlobError;
use proxima_core::authz::{SystemAuthority, SystemAuthorityBinding};

/// Cited-blob transfer, verified-read, and reconcile backend over one
/// Postgres pool and one S3 target. Construct once at boot; capabilities are
/// exposed through separate ports over the same shared instance.
#[derive(Debug, Clone)]
pub struct CitedBlobStore {
    pool: sqlx::PgPool,
    config: S3RuntimeConfig,
    /// Lazily-built S3 client, memoized so the full credential chain is
    /// resolved once per store rather than on every request.
    client: tokio::sync::OnceCell<aws_sdk_s3::Client>,
    /// Set once by normal Proxima boot and shared by every store clone.
    /// A witness minted by an unrelated disposable Engine cannot authorize
    /// this store's global operations.
    system_authority: std::sync::Arc<std::sync::OnceLock<SystemAuthorityBinding>>,
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
            system_authority: std::sync::Arc::new(std::sync::OnceLock::new()),
        })
    }

    /// Internal facade boot seam. Bind host-wide operations to the same boot
    /// instance as the Engine. Binding is shared across clones and cannot be
    /// replaced.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::State`] when a different authority already bound
    /// this store.
    #[doc(hidden)]
    pub fn bind_system_authority(&self, authority: &SystemAuthority) -> Result<(), BlobError> {
        let binding = authority.binding();
        if let Some(existing) = self.system_authority.get() {
            return if existing == &binding {
                Ok(())
            } else {
                Err(BlobError::State(
                    "cited-blob store is already bound to another system authority".into(),
                ))
            };
        }
        match self.system_authority.set(binding) {
            Ok(()) => Ok(()),
            Err(attempted) if self.system_authority.get() == Some(&attempted) => Ok(()),
            Err(_) => Err(BlobError::State(
                "cited-blob store authority binding raced another boot".into(),
            )),
        }
    }

    /// The memoized S3 client, built on first use from the runtime config.
    ///
    /// # Errors
    /// Returns `BlobError` when the AWS client cannot be constructed.
    pub(super) async fn client(&self) -> Result<&aws_sdk_s3::Client, BlobError> {
        self.client.get_or_try_init(|| self.config.client()).await
    }

    pub(super) fn bucket(&self) -> &str {
        &self.config.bucket
    }
}

#[cfg(test)]
mod tests {
    use proxima_core::{Engine, FlavorRegistry};

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

    #[tokio::test]
    async fn store_authority_binding_is_same_boot_idempotent_and_foreign_boot_rejected() {
        let store = CitedBlobStore::new(lazy_test_pool(), store_config(None, None))
            .expect("test store builds");
        let (_, authority) =
            Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests()).into_system_authority();
        let (_, foreign) =
            Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests()).into_system_authority();

        store.bind_system_authority(&authority).expect("first bind");
        store
            .bind_system_authority(&authority)
            .expect("same-boot bind is idempotent");
        let error = store
            .bind_system_authority(&foreign)
            .expect_err("a different boot cannot replace the binding");

        assert!(matches!(error, BlobError::State(_)));
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
