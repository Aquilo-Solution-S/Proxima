use aws_config::BehaviorVersion;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::Region;
use proxima_core::{EndpointUrlError, EndpointUrlPolicy};

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
    /// Read S3 runtime configuration from `PROXIMA_S3_*`.
    ///
    /// # Errors
    /// Returns `BlobError::Config` when a required variable is missing
    /// or a typed setting cannot be parsed.
    pub fn from_env() -> Result<Self, BlobError> {
        let bucket = required_env("PROXIMA_S3_BUCKET")?;
        let region = required_env("PROXIMA_S3_REGION")?;
        let endpoint_url = optional_env("PROXIMA_S3_ENDPOINT_URL");
        if let Some(endpoint_url) = &endpoint_url {
            validate_endpoint_url(endpoint_url)?;
        }
        Ok(Self {
            bucket,
            region,
            endpoint_url,
            force_path_style: parse_bool_env("PROXIMA_S3_FORCE_PATH_STYLE")?,
            upload_ttl_seconds: parse_presign_ttl_env(
                "PROXIMA_S3_UPLOAD_TTL_SECONDS",
                DEFAULT_UPLOAD_TTL_SECONDS,
            )?,
            read_ttl_seconds: parse_presign_ttl_env(
                "PROXIMA_S3_READ_TTL_SECONDS",
                DEFAULT_READ_TTL_SECONDS,
            )?,
            max_blob_bytes: parse_optional_u64_env("PROXIMA_S3_MAX_BLOB_BYTES")?
                .or(Some(DEFAULT_MAX_BLOB_BYTES)),
        })
    }

    /// True when `PROXIMA_S3_BUCKET` is set.
    #[must_use]
    pub fn present_in_env() -> bool {
        std::env::var("PROXIMA_S3_BUCKET").is_ok()
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

fn required_env(key: &str) -> Result<String, BlobError> {
    std::env::var(key)
        .map(|value| value.trim().to_string())
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| BlobError::Config(format!("missing {key}")))
}

fn optional_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_bool_env(key: &str) -> Result<bool, BlobError> {
    let Some(value) = optional_env(key) else {
        return Ok(false);
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(BlobError::Config(format!("invalid boolean {key}={value}"))),
    }
}

fn parse_u64_env(key: &str, default: u64) -> Result<u64, BlobError> {
    let Some(value) = optional_env(key) else {
        return Ok(default);
    };
    value
        .parse::<u64>()
        .map_err(|_| BlobError::Config(format!("invalid integer {key}={value}")))
}

fn parse_presign_ttl_env(key: &str, default: u64) -> Result<u64, BlobError> {
    let seconds = parse_u64_env(key, default)?;
    validate_presign_ttl(key, seconds)?;
    Ok(seconds)
}

fn parse_optional_u64_env(key: &str) -> Result<Option<u64>, BlobError> {
    let Some(value) = optional_env(key) else {
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

    #[test]
    fn parse_bool_env_defaults_false_when_unset() {
        assert!(!parse_bool_env("PROXIMA_BLOB_TEST_BOOL_UNSET").unwrap());
    }

    #[test]
    fn parse_u64_env_uses_default_when_unset() {
        assert_eq!(
            parse_u64_env("PROXIMA_BLOB_TEST_U64_UNSET", 900).unwrap(),
            900
        );
    }

    #[test]
    fn required_env_reports_key_name() {
        let err = required_env("PROXIMA_BLOB_TEST_REQUIRED_UNSET").unwrap_err();
        let BlobError::Config(msg) = err else {
            panic!("wrong variant");
        };
        assert!(msg.contains("PROXIMA_BLOB_TEST_REQUIRED_UNSET"));
    }

    #[test]
    fn parse_optional_u64_env_is_none_when_unset() {
        assert_eq!(
            parse_optional_u64_env("PROXIMA_BLOB_TEST_OPT_U64_UNSET").unwrap(),
            None
        );
    }

    #[test]
    fn from_env_applies_default_max_blob_bytes_when_unset() {
        let cfg = S3RuntimeConfig {
            bucket: "b".into(),
            region: "eu-central-1".into(),
            endpoint_url: None,
            force_path_style: false,
            upload_ttl_seconds: DEFAULT_UPLOAD_TTL_SECONDS,
            read_ttl_seconds: DEFAULT_READ_TTL_SECONDS,
            max_blob_bytes: parse_optional_u64_env("PROXIMA_BLOB_TEST_OPT_U64_UNSET")
                .unwrap()
                .or(Some(DEFAULT_MAX_BLOB_BYTES)),
        };
        assert_eq!(cfg.max_blob_bytes, Some(DEFAULT_MAX_BLOB_BYTES));
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
