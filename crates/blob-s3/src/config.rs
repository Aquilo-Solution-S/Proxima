use aws_config::BehaviorVersion;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::Region;
use proxima_core::{EndpointUrlError, EndpointUrlPolicy, env_value, process_env};

use crate::BlobError;

pub(crate) const DEFAULT_UPLOAD_TTL_SECONDS: u64 = 900;
pub(crate) const DEFAULT_MAX_BLOB_BYTES: u64 = 100 * 1024 * 1024;
pub(crate) const DEFAULT_READ_TTL_SECONDS: u64 = 300;

/// `SigV4`'s own ceiling on a presigned URL's lifetime (7 days). Bounding
/// here rather than at the presign call is what keeps an out-of-range value
/// a startup error instead of a request-path fault: `read_url` and
/// `prepare_upload` compute `now + Duration::seconds(ttl)` BEFORE reaching
/// `PresigningConfig::expires_in`, and that addition panics on overflow, so
/// the only validator used to sit downstream of the thing it had to guard.
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

/// Runtime S3 target and TTL settings for cited-blob uploads.
#[derive(Debug, Clone)]
pub struct S3RuntimeConfig {
    pub bucket: String,
    pub region: String,
    pub endpoint_url: Option<String>,
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
        let Some(bucket) = env_value(lookup, "PROXIMA_S3_BUCKET") else {
            return Ok(None);
        };
        let region = env_value(lookup, "PROXIMA_S3_REGION").ok_or_else(|| {
            BlobError::Config(
                "missing PROXIMA_S3_REGION (required with PROXIMA_S3_BUCKET)".to_string(),
            )
        })?;
        let endpoint_url = env_value(lookup, "PROXIMA_S3_ENDPOINT_URL");
        if let Some(endpoint_url) = &endpoint_url {
            validate_endpoint_url(endpoint_url)?;
        }
        Ok(Some(Self {
            bucket,
            region,
            endpoint_url,
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

    pub(crate) async fn client(&self) -> Result<Client, BlobError> {
        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(self.region.clone()));
        if let Some(endpoint_url) = &self.endpoint_url {
            loader = loader.endpoint_url(endpoint_url);
        }
        let shared = loader.load().await;
        let mut builder = aws_sdk_s3::config::Builder::from(&shared);
        if self.force_path_style {
            builder = builder.force_path_style(true);
        }
        Ok(Client::from_conf(builder.build()))
    }
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
/// HTTP is accepted only for a loopback host (local object store/dev), unlike
/// production S3 endpoints which must be TLS.
pub(crate) fn validate_endpoint_url(raw: &str) -> Result<(), BlobError> {
    proxima_core::validate_endpoint_url(raw, EndpointUrlPolicy::AllowLoopbackHttp).map_err(
        |error| match error {
            EndpointUrlError::InvalidUrl(reason) => BlobError::Config(format!(
                "PROXIMA_S3_ENDPOINT_URL is not a valid URL: {reason}"
            )),
            EndpointUrlError::InsecureTransport => BlobError::Config(format!(
                "PROXIMA_S3_ENDPOINT_URL must use https (plaintext http allowed only for loopback): {raw}"
            )),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    fn bucketed<'a>(extra: &'a [(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
        let mut pairs = vec![
            ("PROXIMA_S3_BUCKET", "b"),
            ("PROXIMA_S3_REGION", "eu-central-1"),
        ];
        pairs.extend_from_slice(extra);
        pairs
    }

    #[test]
    fn parse_bool_env_defaults_false_when_unset() {
        assert!(!parse_bool_env(&env(&[]), "PROXIMA_S3_FORCE_PATH_STYLE").unwrap());
    }

    #[test]
    fn parse_u64_env_uses_default_when_unset() {
        assert_eq!(
            parse_u64_env(&env(&[]), "PROXIMA_S3_UPLOAD_TTL_SECONDS", 900).unwrap(),
            900
        );
    }

    #[test]
    fn parse_optional_u64_env_is_none_when_unset() {
        assert_eq!(
            parse_optional_u64_env(&env(&[]), "PROXIMA_S3_MAX_BLOB_BYTES").unwrap(),
            None
        );
    }

    /// `max_blob_bytes` stays `None` here on purpose — the cap is applied
    /// once, by `CitedBlobStore::new`, which is also the path a host taking
    /// the direct constructor uses. The TTLs do default in the parser,
    /// because nothing downstream can distinguish "unset" from "900".
    #[test]
    fn from_lookup_leaves_the_blob_cap_to_the_store() {
        let cfg = S3RuntimeConfig::from_lookup(&env(&bucketed(&[])))
            .unwrap()
            .expect("bucket is set");
        assert_eq!(cfg.max_blob_bytes, None);
        assert_eq!(cfg.upload_ttl_seconds, DEFAULT_UPLOAD_TTL_SECONDS);
        assert_eq!(cfg.read_ttl_seconds, DEFAULT_READ_TTL_SECONDS);
    }

    #[test]
    fn from_lookup_is_none_without_a_bucket() {
        assert!(S3RuntimeConfig::from_lookup(&env(&[])).unwrap().is_none());
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

    /// Both halves of the old divergence, pinned. The facade used to read
    /// these raw: a whitespace-only bucket was a real bucket name, and a
    /// TTL with a trailing newline failed to parse.
    #[test]
    fn blank_bucket_reads_as_no_bucket() {
        assert!(
            S3RuntimeConfig::from_lookup(&env(&[("PROXIMA_S3_BUCKET", "   ")]))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_from_every_field() {
        let cfg = S3RuntimeConfig::from_lookup(&env(&[
            ("PROXIMA_S3_BUCKET", " b\n"),
            ("PROXIMA_S3_REGION", " eu-central-1\n"),
            ("PROXIMA_S3_UPLOAD_TTL_SECONDS", "900\n"),
            ("PROXIMA_S3_FORCE_PATH_STYLE", " true "),
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
    fn https_endpoint_is_accepted() {
        validate_endpoint_url("https://s3.eu-central-1.amazonaws.com").expect("https accepted");
    }

    #[test]
    fn loopback_http_endpoint_is_accepted() {
        validate_endpoint_url("http://localhost:9000").expect("localhost http accepted");
        validate_endpoint_url("http://127.0.0.1:9000").expect("ipv4 loopback http accepted");
        validate_endpoint_url("http://[::1]:9000").expect("ipv6 loopback http accepted");
    }

    #[test]
    fn non_loopback_http_endpoint_is_rejected() {
        let err = validate_endpoint_url("http://s3.example.com").unwrap_err();
        let BlobError::Config(msg) = err else {
            panic!("wrong variant");
        };
        assert!(msg.contains("must use https"));
    }

    #[test]
    fn schemeless_endpoint_is_rejected() {
        let err = validate_endpoint_url("s3.example.com").unwrap_err();
        assert!(matches!(err, BlobError::Config(_)));
    }
}
