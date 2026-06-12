use proxima_blob_s3::S3RuntimeConfig;

use crate::EmbedError;

const DEFAULT_UPLOAD_TTL_SECONDS: u64 = 900;
const DEFAULT_READ_TTL_SECONDS: u64 = 300;

/// Env-driven configuration for an embedded Proxima engine.
///
/// | Var | Required | Meaning |
/// |---|---|---|
/// | `DATABASE_URL` | yes | Postgres for engine tables (`proxima_core` schema) |
/// | `PROXIMA_S3_BUCKET` | no | enables the cited-blob store when set |
/// | `PROXIMA_S3_REGION` etc. | with bucket | see `proxima_blob_s3::S3RuntimeConfig` |
#[derive(Debug, Clone)]
pub struct EmbedConfig {
    pub database_url: String,
    pub s3: Option<S3RuntimeConfig>,
}

impl EmbedConfig {
    /// Read configuration from process env.
    ///
    /// # Errors
    ///
    /// Returns `EmbedError::Config` when `DATABASE_URL` is missing or
    /// the S3 block is partially configured.
    pub fn from_env() -> Result<Self, EmbedError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Same as [`Self::from_env`] over an injected lookup.
    ///
    /// # Errors
    ///
    /// See [`Self::from_env`].
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, EmbedError> {
        let database_url = lookup("DATABASE_URL")
            .ok_or_else(|| EmbedError::Config("DATABASE_URL is required".into()))?;
        let s3 = if lookup("PROXIMA_S3_BUCKET").is_some() {
            Some(S3RuntimeConfig {
                bucket: lookup("PROXIMA_S3_BUCKET")
                    .ok_or_else(|| EmbedError::Config("PROXIMA_S3_BUCKET is required".into()))?,
                region: lookup("PROXIMA_S3_REGION").ok_or_else(|| {
                    EmbedError::Config(
                        "PROXIMA_S3_REGION is required with PROXIMA_S3_BUCKET".into(),
                    )
                })?,
                endpoint_url: lookup("PROXIMA_S3_ENDPOINT_URL"),
                force_path_style: parse_bool(&lookup, "PROXIMA_S3_FORCE_PATH_STYLE")?,
                upload_ttl_seconds: parse_ttl(
                    &lookup,
                    "PROXIMA_S3_UPLOAD_TTL_SECONDS",
                    DEFAULT_UPLOAD_TTL_SECONDS,
                )?,
                read_ttl_seconds: parse_ttl(
                    &lookup,
                    "PROXIMA_S3_READ_TTL_SECONDS",
                    DEFAULT_READ_TTL_SECONDS,
                )?,
            })
        } else {
            None
        };
        Ok(Self { database_url, s3 })
    }
}

fn parse_bool(lookup: &impl Fn(&str) -> Option<String>, key: &str) -> Result<bool, EmbedError> {
    let Some(raw) = lookup(key) else {
        return Ok(false);
    };
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(EmbedError::Config(format!(
            "{key} must be a boolean, got {raw:?}"
        ))),
    }
}

fn parse_ttl(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &str,
    default: u64,
) -> Result<u64, EmbedError> {
    match lookup(key) {
        None => Ok(default),
        Some(raw) => raw
            .parse()
            .map_err(|_| EmbedError::Config(format!("{key} must be a u64, got {raw:?}"))),
    }
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

    #[test]
    fn requires_database_url() {
        let err = EmbedConfig::from_lookup(env(&[])).unwrap_err();
        assert!(err.to_string().contains("DATABASE_URL"));
    }

    #[test]
    fn s3_absent_when_bucket_unset() {
        let cfg = EmbedConfig::from_lookup(env(&[("DATABASE_URL", "postgres://x/y")])).unwrap();
        assert!(cfg.s3.is_none());
    }

    #[test]
    fn s3_present_when_bucket_set() {
        let cfg = EmbedConfig::from_lookup(env(&[
            ("DATABASE_URL", "postgres://x/y"),
            ("PROXIMA_S3_BUCKET", "proxima"),
            ("PROXIMA_S3_REGION", "us-east-1"),
        ]))
        .unwrap();
        assert_eq!(cfg.s3.as_ref().map(|s| s.bucket.as_str()), Some("proxima"));
    }
}
