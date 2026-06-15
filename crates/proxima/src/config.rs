use proxima_blob_s3::S3RuntimeConfig;

use crate::EmbedError;

const DEFAULT_UPLOAD_TTL_SECONDS: u64 = 900;
const DEFAULT_READ_TTL_SECONDS: u64 = 300;

/// Low-level configuration for an embedded Proxima engine.
///
/// Plain data consumed by [`crate::ProximaBuilder::new`]. Environment
/// resolution (`DATABASE_URL`, the `PROXIMA_S3_*` block) lives in
/// [`crate::RuntimeBuilder`]; hosts driving the facade through it never
/// construct this directly.
#[derive(Debug, Clone)]
pub struct EmbedConfig {
    pub database_url: String,
    pub s3: Option<S3RuntimeConfig>,
}

pub(crate) fn s3_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<S3RuntimeConfig>, EmbedError> {
    if lookup("PROXIMA_S3_BUCKET").is_none() {
        return Ok(None);
    }

    Ok(Some(S3RuntimeConfig {
        bucket: lookup("PROXIMA_S3_BUCKET")
            .ok_or_else(|| EmbedError::Config("PROXIMA_S3_BUCKET is required".into()))?,
        region: lookup("PROXIMA_S3_REGION").ok_or_else(|| {
            EmbedError::Config("PROXIMA_S3_REGION is required with PROXIMA_S3_BUCKET".into())
        })?,
        endpoint_url: lookup("PROXIMA_S3_ENDPOINT_URL"),
        force_path_style: parse_bool(lookup, "PROXIMA_S3_FORCE_PATH_STYLE")?,
        upload_ttl_seconds: parse_ttl(
            lookup,
            "PROXIMA_S3_UPLOAD_TTL_SECONDS",
            DEFAULT_UPLOAD_TTL_SECONDS,
        )?,
        read_ttl_seconds: parse_ttl(
            lookup,
            "PROXIMA_S3_READ_TTL_SECONDS",
            DEFAULT_READ_TTL_SECONDS,
        )?,
    }))
}

pub(crate) fn parse_bool(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &str,
) -> Result<bool, EmbedError> {
    let Some(raw) = lookup(key) else {
        return Ok(false);
    };
    parse_bool_value(key, &raw)
}

pub(crate) fn parse_bool_value(key: &str, raw: &str) -> Result<bool, EmbedError> {
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
    fn s3_absent_when_bucket_unset() {
        let s3 = s3_from_lookup(&env(&[])).unwrap();
        assert!(s3.is_none());
    }

    #[test]
    fn s3_present_when_bucket_set() {
        let s3 = s3_from_lookup(&env(&[
            ("PROXIMA_S3_BUCKET", "proxima"),
            ("PROXIMA_S3_REGION", "us-east-1"),
        ]))
        .unwrap();
        assert_eq!(s3.as_ref().map(|s| s.bucket.as_str()), Some("proxima"));
    }
}
