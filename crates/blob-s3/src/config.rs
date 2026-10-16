use aws_sdk_s3::Client;
use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use proxima_core::{
    EndpointUrlError, EndpointUrlPolicy, PlaintextHosts, env_value, process_env,
    validate_endpoint_url_for_hosts,
};

use crate::BlobError;

pub(crate) const DEFAULT_UPLOAD_TTL_SECONDS: u64 = 900;
pub(crate) const DEFAULT_MAX_BLOB_BYTES: u64 = 100 * 1024 * 1024;
pub(crate) const DEFAULT_READ_TTL_SECONDS: u64 = 300;

/// `SigV4` ceiling on a presigned URL (7 days). Bound here: `read_url` /
/// `prepare_upload` compute `now + Duration::seconds(ttl)` before
/// `PresigningConfig::expires_in`, and that add panics on overflow.
pub(crate) const MAX_PRESIGN_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Reject a TTL no presigned URL could carry, naming the variable.
pub(crate) fn validate_presign_ttl(key: &str, seconds: u64) -> Result<(), BlobError> {
    if seconds == 0 {
        return Err(BlobError::Config(format!(
            "{key} must be greater than zero"
        )));
    }
    if seconds > MAX_PRESIGN_TTL_SECONDS {
        return Err(BlobError::Config(format!(
            "{key}={seconds} exceeds the maximum presigned-URL lifetime of \
             {MAX_PRESIGN_TTL_SECONDS} seconds (7 days)"
        )));
    }
    Ok(())
}

/// Static S3 key pair; the secret is private and redacted by `Debug`.
///
/// ```
/// proxima_blob_s3::S3KeyPair::new(String::new(), String::new());
/// ```
///
/// ```compile_fail
/// proxima_blob_s3::S3KeyPair::new(String::new(), String::new()).secret_access_key;
/// ```
#[derive(Clone)]
pub struct S3KeyPair {
    access_key_id: String,
    secret_access_key: String,
}

impl S3KeyPair {
    /// Construct a static key pair supplied by a host.
    #[must_use]
    pub fn new(access_key_id: String, secret_access_key: String) -> Self {
        Self {
            access_key_id,
            secret_access_key,
        }
    }
}

impl std::fmt::Debug for S3KeyPair {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("S3KeyPair")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[REDACTED]")
            .finish()
    }
}

/// Runtime S3 target and TTL settings for cited-blob uploads.
#[derive(Debug, Clone)]
pub struct S3RuntimeConfig {
    pub bucket: String,
    pub region: String,
    pub key_pair: S3KeyPair,
    /// Endpoint for internal object I/O; `None` uses AWS endpoint resolution.
    pub endpoint_url: Option<String>,
    /// Holder-facing endpoint used before signing upload and read URLs.
    /// `None` uses `endpoint_url` or AWS endpoint resolution. Internal object
    /// I/O always uses `endpoint_url`.
    pub public_endpoint_url: Option<String>,
    /// Hosts, besides loopback, that may be named in either endpoint over
    /// plaintext `http://`. Empty by default ([`PlaintextHosts::none`]).
    /// `from_lookup` reads it from `PROXIMA_PLAINTEXT_HOSTS`.
    pub plaintext_hosts: PlaintextHosts,
    pub force_path_style: bool,
    pub upload_ttl_seconds: u64,
    pub read_ttl_seconds: u64,
    /// Hard cap on a single blob's byte length. `None` selects the 100 MiB
    /// default when the store is constructed. Enforced at prepare time (declared
    /// `byte_len`) and again while streaming the uploaded object at completion,
    /// so an under-declared object cannot bypass the cap.
    pub max_blob_bytes: Option<u64>,
}

impl S3RuntimeConfig {
    /// Read S3 runtime configuration from the process environment.
    ///
    /// Thin adapter over [`Self::from_lookup`]: this crate holds the single
    /// `PROXIMA_S3_*` parser, so an embedding host that injects its own
    /// environment cannot end up with different answers than a host that uses
    /// the process environment.
    ///
    /// # Errors
    /// Returns `BlobError::Config` when a required variable is missing
    /// or a typed setting cannot be parsed.
    pub fn from_env() -> Result<Self, BlobError> {
        Self::from_lookup(&process_env)?
            .ok_or_else(|| BlobError::Config("missing PROXIMA_S3_BUCKET".to_string()))
    }

    /// Read S3 runtime configuration from an injected environment.
    ///
    /// Returns `Ok(None)` when `PROXIMA_S3_BUCKET` is unset — the S3 lane is
    /// optional, and a host that configures no bucket is not misconfigured.
    /// [`Self::from_env`] turns that into an error because its caller has
    /// already decided S3 is required.
    ///
    /// Every value is read through [`proxima_core::env_value`], so a variable
    /// set to the empty string (or to whitespace) reads as unset rather than
    /// as a value that fails to parse.
    ///
    /// # Errors
    /// Returns `BlobError::Config` when a variable required alongside the
    /// bucket is missing, or a typed setting cannot be parsed.
    pub fn from_lookup(
        lookup: &impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, BlobError> {
        // Parsed before the bucket check so a malformed setting fails boot
        // whether or not this lane is configured.
        let plaintext_hosts = PlaintextHosts::from_lookup(lookup)
            .map_err(|error| BlobError::Config(error.to_string()))?;
        let Some(bucket) = env_value(lookup, "PROXIMA_S3_BUCKET") else {
            return Ok(None);
        };
        let region = env_value(lookup, "PROXIMA_S3_REGION").ok_or_else(|| {
            BlobError::Config(
                "missing PROXIMA_S3_REGION (required with PROXIMA_S3_BUCKET)".to_string(),
            )
        })?;
        let key_pair = S3KeyPair::new(
            required_key(lookup, "PROXIMA_S3_ACCESS_KEY_ID")?,
            required_key(lookup, "PROXIMA_S3_SECRET_ACCESS_KEY")?,
        );
        let endpoint_url = env_value(lookup, "PROXIMA_S3_ENDPOINT_URL");
        if let Some(endpoint_url) = &endpoint_url {
            validate_endpoint_url("PROXIMA_S3_ENDPOINT_URL", endpoint_url, &plaintext_hosts)?;
        }
        let public_endpoint_url = env_value(lookup, "PROXIMA_S3_PUBLIC_ENDPOINT_URL");
        if let Some(endpoint_url) = &public_endpoint_url {
            validate_endpoint_url(
                "PROXIMA_S3_PUBLIC_ENDPOINT_URL",
                endpoint_url,
                &plaintext_hosts,
            )?;
        }
        Ok(Some(Self {
            bucket,
            region,
            key_pair,
            endpoint_url,
            public_endpoint_url,
            plaintext_hosts,
            force_path_style: parse_bool_env(lookup, "PROXIMA_S3_FORCE_PATH_STYLE")?,
            upload_ttl_seconds: parse_presign_ttl_env(
                lookup,
                "PROXIMA_S3_UPLOAD_TTL_SECONDS",
                DEFAULT_UPLOAD_TTL_SECONDS,
            )?,
            read_ttl_seconds: parse_presign_ttl_env(
                lookup,
                "PROXIMA_S3_READ_TTL_SECONDS",
                DEFAULT_READ_TTL_SECONDS,
            )?,
            // Left `None` when unset rather than defaulted here: the field's
            // own contract is that `None` selects the default at store
            // construction, and `CitedBlobStore::new` applies it for direct
            // constructors too. Defaulting in the parser as well was the
            // second of two application points for one value.
            max_blob_bytes: parse_optional_u64_env(lookup, "PROXIMA_S3_MAX_BLOB_BYTES")?,
        }))
    }

    /// True when `PROXIMA_S3_BUCKET` is configured.
    ///
    /// Uses the same empty-is-unset rule as the parser, so this cannot report
    /// "configured" for a bucket [`Self::from_env`] then rejects as missing.
    #[must_use]
    pub fn present_in_env() -> bool {
        env_value(&process_env, "PROXIMA_S3_BUCKET").is_some()
    }

    /// Build the internal object-I/O client solely from configured values.
    #[must_use]
    pub fn client(&self) -> Client {
        self.client_for_endpoint(self.endpoint_url.as_deref())
    }

    pub(crate) fn presigning_client(&self) -> Client {
        self.client_for_endpoint(
            self.public_endpoint_url
                .as_deref()
                .or(self.endpoint_url.as_deref()),
        )
    }

    fn client_for_endpoint(&self, endpoint_url: Option<&str>) -> Client {
        let mut builder = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(self.region.clone()))
            .force_path_style(self.force_path_style)
            .credentials_provider(Credentials::new(
                self.key_pair.access_key_id.clone(),
                self.key_pair.secret_access_key.clone(),
                None,
                None,
                "proxima-s3-config",
            ));
        if let Some(endpoint_url) = endpoint_url {
            builder = builder.endpoint_url(endpoint_url);
        }
        Client::from_conf(builder.build())
    }
}

fn required_key(lookup: &impl Fn(&str) -> Option<String>, key: &str) -> Result<String, BlobError> {
    env_value(lookup, key).ok_or_else(|| {
        BlobError::Config(format!("missing {key} (required with PROXIMA_S3_BUCKET)"))
    })
}

fn parse_bool_env(lookup: &impl Fn(&str) -> Option<String>, key: &str) -> Result<bool, BlobError> {
    let Some(value) = env_value(lookup, key) else {
        return Ok(false);
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(BlobError::Config(format!("invalid boolean {key}={value}"))),
    }
}

fn parse_u64_env(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &str,
    default: u64,
) -> Result<u64, BlobError> {
    let Some(value) = env_value(lookup, key) else {
        return Ok(default);
    };
    value
        .parse::<u64>()
        .map_err(|_| BlobError::Config(format!("invalid integer {key}={value}")))
}

fn parse_presign_ttl_env(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &str,
    default: u64,
) -> Result<u64, BlobError> {
    let seconds = parse_u64_env(lookup, key, default)?;
    validate_presign_ttl(key, seconds)?;
    Ok(seconds)
}

fn parse_optional_u64_env(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &str,
) -> Result<Option<u64>, BlobError> {
    let Some(value) = env_value(lookup, key) else {
        return Ok(None);
    };
    value
        .parse::<u64>()
        .map(Some)
        .map_err(|_| BlobError::Config(format!("invalid integer {key}={value}")))
}

/// Reject an S3 endpoint that would carry presigned URLs (and the credentials
/// signed into them) over plaintext HTTP. HTTPS is always accepted; plaintext
/// HTTP is accepted only for a loopback host or a host named in
/// `plaintext_hosts` (local object store/dev, or a sidecar on a network with
/// no route out), unlike production S3 endpoints which must be TLS.
pub(crate) fn validate_endpoint_url(
    key: &str,
    raw: &str,
    plaintext_hosts: &PlaintextHosts,
) -> Result<(), BlobError> {
    validate_endpoint_url_for_hosts(raw, EndpointUrlPolicy::AllowLoopbackHttp, plaintext_hosts)
        .map_err(|error| match error {
            EndpointUrlError::InvalidUrl(reason) => BlobError::Config(format!(
                "{key} is not a valid URL: {reason}"
            )),
            // With no host named the message is the one this lane always gave.
            EndpointUrlError::InsecureTransport if plaintext_hosts.is_empty() => {
                BlobError::Config(format!(
                    "{key} must use https (plaintext http allowed only for loopback): {raw}"
                ))
            }
            EndpointUrlError::InsecureTransport => BlobError::Config(format!(
                "{key} must use https (plaintext http allowed only for loopback or a host named in PROXIMA_PLAINTEXT_HOSTS): {raw}"
            )),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A later pair overrides an earlier one with the same key.
    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .rev()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    fn bucketed<'a>(extra: &'a [(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
        let mut pairs = vec![
            ("PROXIMA_S3_BUCKET", "b"),
            ("PROXIMA_S3_REGION", "eu-central-1"),
            ("PROXIMA_S3_ACCESS_KEY_ID", "test-access-key"),
            ("PROXIMA_S3_SECRET_ACCESS_KEY", "test-secret-key"),
        ];
        pairs.extend_from_slice(extra);
        pairs
    }

    /// The S3 lane is optional, so an unset bucket is `Ok(None)` — but a
    /// bucket without a region is a half-configured lane, not an absent one.
    #[test]
    fn from_lookup_requires_a_region_alongside_the_bucket() {
        let err = S3RuntimeConfig::from_lookup(&env(&[("PROXIMA_S3_BUCKET", "b")])).unwrap_err();
        let BlobError::Config(msg) = err else {
            panic!("wrong variant");
        };
        assert!(msg.contains("PROXIMA_S3_REGION"), "{msg}");
    }

    #[test]
    fn from_lookup_rejects_an_invalid_public_endpoint() {
        for endpoint in [
            "http://s3.example.com",
            "s3.example.com",
            "ftp://s3.example.com",
            "https://",
        ] {
            let extra = [("PROXIMA_S3_PUBLIC_ENDPOINT_URL", endpoint)];
            let pairs = bucketed(&extra);
            let error = S3RuntimeConfig::from_lookup(&env(&pairs))
                .expect_err("invalid public endpoint must fail configuration");
            let BlobError::Config(message) = error else {
                panic!("wrong error variant");
            };
            assert!(
                message.contains("PROXIMA_S3_PUBLIC_ENDPOINT_URL"),
                "public endpoint error must name its setting"
            );
        }
    }

    #[test]
    fn public_endpoint_is_optional_and_an_unconfigured_lane_stays_absent() {
        for value in [None, Some(""), Some("   ")] {
            let extra: Vec<_> = value
                .into_iter()
                .map(|value| ("PROXIMA_S3_PUBLIC_ENDPOINT_URL", value))
                .collect();
            let pairs = bucketed(&extra);
            let config = S3RuntimeConfig::from_lookup(&env(&pairs))
                .unwrap()
                .expect("bucket is set");
            assert!(config.public_endpoint_url.is_none());
        }
        let no_bucket = [("PROXIMA_S3_PUBLIC_ENDPOINT_URL", "not an endpoint")];
        assert!(
            S3RuntimeConfig::from_lookup(&env(&no_bucket))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn public_endpoint_uses_the_existing_endpoint_transport_policy() {
        for (endpoint, hosts) in [
            ("https://files.example/objects/", None),
            ("http://localhost:9001/objects/", None),
            ("http://127.0.0.1:9001/objects/", None),
            ("http://[::1]:9001/objects/", None),
            (
                "http://public-gateway:9090/objects/",
                Some("public-gateway"),
            ),
        ] {
            let mut extra = vec![("PROXIMA_S3_PUBLIC_ENDPOINT_URL", endpoint)];
            if let Some(hosts) = hosts {
                extra.push(("PROXIMA_PLAINTEXT_HOSTS", hosts));
            }
            let pairs = bucketed(&extra);
            let config = S3RuntimeConfig::from_lookup(&env(&pairs))
                .unwrap()
                .expect("bucket is set");
            assert_eq!(config.public_endpoint_url.as_deref(), Some(endpoint));
            assert!(config.endpoint_url.is_none());
        }
        let pairs = bucketed(&[(
            "PROXIMA_S3_PUBLIC_ENDPOINT_URL",
            "  https://files.example/objects/  ",
        )]);
        assert_eq!(
            S3RuntimeConfig::from_lookup(&env(&pairs))
                .unwrap()
                .unwrap()
                .public_endpoint_url
                .as_deref(),
            Some("https://files.example/objects/")
        );
        let pairs = bucketed(&[
            (
                "PROXIMA_S3_PUBLIC_ENDPOINT_URL",
                "http://other-gateway:9090",
            ),
            ("PROXIMA_PLAINTEXT_HOSTS", "public-gateway"),
        ]);
        let error = S3RuntimeConfig::from_lookup(&env(&pairs)).unwrap_err();
        assert!(
            matches!(error, BlobError::Config(message) if message.contains("PROXIMA_S3_PUBLIC_ENDPOINT_URL") && message.contains("PROXIMA_PLAINTEXT_HOSTS"))
        );
    }

    fn assert_missing_key_is_refused(missing: &str) {
        let pairs = bucketed(&[]);
        let lookup = |key: &str| {
            if key == missing {
                None
            } else {
                env(&pairs)(key)
            }
        };
        let err = S3RuntimeConfig::from_lookup(&lookup).unwrap_err();
        let BlobError::Config(msg) = err else {
            panic!("wrong variant");
        };
        assert!(msg.contains(missing), "{msg}");
    }

    #[test]
    fn from_lookup_requires_access_key_id_alongside_the_bucket() {
        assert_missing_key_is_refused("PROXIMA_S3_ACCESS_KEY_ID");
    }

    #[test]
    fn from_lookup_requires_secret_access_key_alongside_the_bucket() {
        assert_missing_key_is_refused("PROXIMA_S3_SECRET_ACCESS_KEY");
    }

    // The SDK's deprecated Config::credentials_provider() always returns None.
    // Observe the configured resolver through its supported identity-cache seam.
    #[derive(Debug)]
    struct CredentialsObserver(std::sync::Arc<std::sync::Mutex<Option<Credentials>>>);

    impl aws_sdk_s3::config::ResolveCachedIdentity for CredentialsObserver {
        fn resolve_cached_identity<'a>(
            &'a self,
            resolver: aws_smithy_runtime_api::client::identity::SharedIdentityResolver,
            runtime_components: &'a aws_smithy_runtime_api::client::runtime_components::RuntimeComponents,
            config_bag: &'a aws_sdk_s3::config::ConfigBag,
        ) -> aws_smithy_runtime_api::client::identity::IdentityFuture<'a> {
            use aws_smithy_runtime_api::client::identity::{IdentityFuture, ResolveIdentity};

            IdentityFuture::new(async move {
                let identity = resolver
                    .resolve_identity(runtime_components, config_bag)
                    .await?;
                *self.0.lock().unwrap() = identity.data::<Credentials>().cloned();
                Ok(identity)
            })
        }
    }

    async fn presign_config_probe(config: &S3RuntimeConfig, client: &Client) -> reqwest::Url {
        use aws_sdk_s3::presigning::PresigningConfig;
        use std::time::{Duration, SystemTime};

        let observed = std::sync::Arc::new(std::sync::Mutex::new(None));
        let client = Client::from_conf(
            client
                .config()
                .to_builder()
                .identity_cache(CredentialsObserver(observed.clone()))
                .build(),
        );
        assert_eq!(client.config().region().unwrap().as_ref(), config.region);
        let request = client
            .get_object()
            .bucket(&config.bucket)
            .key("config-probe")
            .presigned(
                PresigningConfig::builder()
                    .start_time(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000))
                    .expires_in(Duration::from_mins(1))
                    .build()
                    .unwrap(),
            )
            .await
            .expect("configuration-only presigning succeeds");
        let credentials = observed
            .lock()
            .unwrap()
            .take()
            .expect("configured resolver ran");
        assert_eq!(credentials.access_key_id(), config.key_pair.access_key_id);
        assert!(credentials.secret_access_key() == config.key_pair.secret_access_key);
        assert!(credentials.session_token().is_none());
        assert!(credentials.expiry().is_none());
        reqwest::Url::parse(request.uri()).expect("the SDK emits a valid request URI")
    }

    fn signed_query_value(url: &reqwest::Url, key: &str) -> String {
        url.query_pairs()
            .find(|(name, _)| name == key)
            .expect("the SDK includes the signing field")
            .1
            .into_owned()
    }

    #[tokio::test]
    async fn client_uses_only_configured_credentials_region_and_endpoint() {
        let config = S3RuntimeConfig::from_lookup(&env(&bucketed(&[
            (
                "PROXIMA_S3_ENDPOINT_URL",
                "https://objects.internal.example/internal/",
            ),
            (
                "PROXIMA_S3_PUBLIC_ENDPOINT_URL",
                "https://files.example/objects/",
            ),
            ("PROXIMA_S3_FORCE_PATH_STYLE", "true"),
        ])))
        .unwrap()
        .expect("bucket is set");
        for (client, host, path) in [
            (
                config.client(),
                "objects.internal.example",
                "/internal/b/config-probe",
            ),
            (
                config.presigning_client(),
                "files.example",
                "/objects/b/config-probe",
            ),
        ] {
            let url = presign_config_probe(&config, &client).await;
            assert_eq!(url.host_str(), Some(host));
            assert_eq!(url.path(), path);
            assert!(
                signed_query_value(&url, "X-Amz-Credential")
                    == format!(
                        "{}/20231114/{}/s3/aws4_request",
                        config.key_pair.access_key_id, config.region
                    )
            );
            assert!(
                signed_query_value(&url, "X-Amz-SignedHeaders")
                    .split(';')
                    .any(|header| header == "host")
            );
        }
    }

    #[tokio::test]
    async fn an_unset_public_endpoint_presigns_the_internal_endpoint() {
        let config = S3RuntimeConfig::from_lookup(&env(&bucketed(&[
            ("PROXIMA_S3_ENDPOINT_URL", "http://127.0.0.1:9000"),
            ("PROXIMA_S3_FORCE_PATH_STYLE", "true"),
        ])))
        .unwrap()
        .expect("bucket is set");
        assert!(config.public_endpoint_url.is_none());
        let internal = presign_config_probe(&config, &config.client()).await;
        let presigning = presign_config_probe(&config, &config.presigning_client()).await;
        for url in [&internal, &presigning] {
            assert_eq!(url.scheme(), "http");
            assert_eq!(url.host_str(), Some("127.0.0.1"));
            assert_eq!(url.port(), Some(9000));
            assert_eq!(url.path(), "/b/config-probe");
        }
    }

    #[tokio::test]
    async fn the_public_endpoint_prefix_is_part_of_the_signed_path() {
        let config = S3RuntimeConfig::from_lookup(&env(&bucketed(&[
            (
                "PROXIMA_S3_PUBLIC_ENDPOINT_URL",
                "https://files.example/objects/",
            ),
            ("PROXIMA_S3_FORCE_PATH_STYLE", "true"),
        ])))
        .unwrap()
        .expect("bucket is set");
        assert!(config.endpoint_url.is_none());
        let internal = presign_config_probe(&config, &config.client()).await;
        assert!(
            internal.host_str() != Some("files.example"),
            "public-only configuration leaves internal endpoint resolution unchanged"
        );
        let prefixed = presign_config_probe(&config, &config.presigning_client()).await;
        let mut root_endpoint = config.clone();
        root_endpoint.public_endpoint_url = Some("https://files.example".into());
        let unprefixed =
            presign_config_probe(&root_endpoint, &root_endpoint.presigning_client()).await;
        assert_eq!(prefixed.path(), "/objects/b/config-probe");
        assert_eq!(unprefixed.path(), "/b/config-probe");
        assert!(
            signed_query_value(&prefixed, "X-Amz-Date")
                == signed_query_value(&unprefixed, "X-Amz-Date")
        );
        assert!(
            signed_query_value(&prefixed, "X-Amz-Credential")
                == signed_query_value(&unprefixed, "X-Amz-Credential")
        );
        assert!(
            signed_query_value(&prefixed, "X-Amz-Signature")
                != signed_query_value(&unprefixed, "X-Amz-Signature"),
            "the SDK signs the endpoint prefix before returning the URL"
        );
    }

    #[test]
    fn from_lookup_rejects_blank_key_variables() {
        for key in ["PROXIMA_S3_ACCESS_KEY_ID", "PROXIMA_S3_SECRET_ACCESS_KEY"] {
            let extra = [(key, " \n")];
            let pairs = bucketed(&extra);
            let error = S3RuntimeConfig::from_lookup(&env(&pairs)).unwrap_err();
            assert!(matches!(error, BlobError::Config(message) if message.contains(key)));
        }
    }

    #[test]
    fn config_debug_redacts_the_secret_and_shows_the_access_key_id() {
        let config = S3RuntimeConfig::from_lookup(&env(&bucketed(&[])))
            .unwrap()
            .expect("bucket is set");
        let debug = format!("{config:?}");
        assert!(!config.key_pair.secret_access_key.is_empty());
        assert!(!debug.contains(&config.key_pair.secret_access_key));
        assert!(debug.contains(&config.key_pair.access_key_id));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_from_every_field() {
        let cfg = S3RuntimeConfig::from_lookup(&env(&[
            ("PROXIMA_S3_BUCKET", " b\n"),
            ("PROXIMA_S3_REGION", " eu-central-1\n"),
            ("PROXIMA_S3_UPLOAD_TTL_SECONDS", "900\n"),
            ("PROXIMA_S3_FORCE_PATH_STYLE", " true "),
            ("PROXIMA_S3_ACCESS_KEY_ID", "test-access-key"),
            ("PROXIMA_S3_SECRET_ACCESS_KEY", "test-secret-key"),
        ]))
        .unwrap()
        .expect("bucket is set");
        assert_eq!(cfg.bucket, "b");
        assert_eq!(cfg.region, "eu-central-1");
        assert_eq!(cfg.upload_ttl_seconds, 900);
        assert!(cfg.force_path_style);
    }

    /// An out-of-range TTL is a startup error on every path into the config,
    /// not only the process-environment one.
    #[test]
    fn from_lookup_rejects_a_ttl_no_presigned_url_could_carry() {
        let err = S3RuntimeConfig::from_lookup(&env(&bucketed(&[(
            "PROXIMA_S3_UPLOAD_TTL_SECONDS",
            "0",
        )])))
        .unwrap_err();
        let BlobError::Config(msg) = err else {
            panic!("wrong variant");
        };
        assert!(msg.contains("greater than zero"), "{msg}");
    }

    #[test]
    fn non_loopback_http_endpoint_is_rejected() {
        let err = validate_endpoint_url(
            "PROXIMA_S3_ENDPOINT_URL",
            "http://s3.example.com",
            &PlaintextHosts::none(),
        )
        .unwrap_err();
        let BlobError::Config(msg) = err else {
            panic!("wrong variant");
        };
        assert!(msg.contains("must use https"));
    }

    #[test]
    fn schemeless_endpoint_is_rejected() {
        let err = validate_endpoint_url(
            "PROXIMA_S3_ENDPOINT_URL",
            "s3.example.com",
            &PlaintextHosts::none(),
        )
        .unwrap_err();
        assert!(matches!(err, BlobError::Config(_)));
    }

    fn with_endpoint<'a>(
        endpoint: &'a str,
        hosts: Option<&'a str>,
    ) -> Result<Option<S3RuntimeConfig>, BlobError> {
        let mut extra = vec![("PROXIMA_S3_ENDPOINT_URL", endpoint)];
        if let Some(hosts) = hosts {
            extra.push(("PROXIMA_PLAINTEXT_HOSTS", hosts));
        }
        let pairs = bucketed(&extra);
        S3RuntimeConfig::from_lookup(&env(&pairs))
    }

    #[test]
    fn a_named_host_may_be_the_plaintext_s3_endpoint() {
        let config = with_endpoint("http://gateway:9090", Some("gateway"))
            .unwrap()
            .expect("bucket is set");
        assert_eq!(config.endpoint_url.as_deref(), Some("http://gateway:9090"));
        assert!(config.plaintext_hosts.contains("gateway"));
    }

    #[test]
    fn an_unnamed_host_stays_refused_as_the_s3_endpoint() {
        for (endpoint, hosts) in [
            ("http://gateway:9090", None),
            ("http://gateway:9090", Some("   ")),
            ("http://other:9090", Some("gateway")),
        ] {
            let error = with_endpoint(endpoint, hosts).unwrap_err();
            assert!(
                matches!(&error, BlobError::Config(message) if message.contains("must use https")),
                "{endpoint} with {hosts:?}: {error}",
            );
        }
    }

    #[test]
    fn a_wildcard_or_range_in_the_setting_fails_boot_even_without_a_bucket() {
        for hosts in ["*", "*.internal", "10.0.0.0/8", "10.0.0.1"] {
            let error = with_endpoint("https://s3.example.com", Some(hosts)).unwrap_err();
            assert!(
                matches!(&error, BlobError::Config(message) if message.contains("PROXIMA_PLAINTEXT_HOSTS")),
                "{hosts}: {error}",
            );
            let no_bucket = [("PROXIMA_PLAINTEXT_HOSTS", hosts)];
            assert!(
                S3RuntimeConfig::from_lookup(&env(&no_bucket)).is_err(),
                "{hosts}"
            );
        }
    }

    #[test]
    fn unset_setting_leaves_s3_config_as_before() {
        let config = S3RuntimeConfig::from_lookup(&env(&bucketed(&[])))
            .unwrap()
            .expect("bucket is set");
        assert!(config.plaintext_hosts.is_empty());
        assert!(S3RuntimeConfig::from_lookup(&env(&[])).unwrap().is_none());
        assert!(with_endpoint("http://127.0.0.1:9000", None).is_ok());
        let BlobError::Config(message) = with_endpoint("http://s3.example.com", None).unwrap_err()
        else {
            panic!("wrong variant");
        };
        assert_eq!(
            message,
            "PROXIMA_S3_ENDPOINT_URL must use https (plaintext http allowed only for loopback): http://s3.example.com"
        );
    }
}
