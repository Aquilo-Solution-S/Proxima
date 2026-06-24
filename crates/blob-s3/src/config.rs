use aws_config::BehaviorVersion;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::Region;

use crate::BlobError;

pub(crate) const DEFAULT_UPLOAD_TTL_SECONDS: u64 = 900;
pub(crate) const DEFAULT_READ_TTL_SECONDS: u64 = 300;

/// Runtime S3 target and TTL settings for cited-blob uploads.
#[derive(Debug, Clone)]
pub struct S3RuntimeConfig {
    pub bucket: String,
    pub region: String,
    pub endpoint_url: Option<String>,
    pub force_path_style: bool,
    pub upload_ttl_seconds: u64,
    pub read_ttl_seconds: u64,
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
        Ok(Self {
            bucket,
            region,
            endpoint_url: optional_env("PROXIMA_S3_ENDPOINT_URL"),
            force_path_style: parse_bool_env("PROXIMA_S3_FORCE_PATH_STYLE")?,
            upload_ttl_seconds: parse_u64_env(
                "PROXIMA_S3_UPLOAD_TTL_SECONDS",
                DEFAULT_UPLOAD_TTL_SECONDS,
            )?,
            read_ttl_seconds: parse_u64_env(
                "PROXIMA_S3_READ_TTL_SECONDS",
                DEFAULT_READ_TTL_SECONDS,
            )?,
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
}
