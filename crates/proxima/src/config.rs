use proxima_blob_s3::S3RuntimeConfig;
use proxima_storage_pg::PgTuning;

use crate::EmbedError;

/// Low-level configuration for an embedded Proxima engine.
///
/// Plain data consumed by [`crate::ProximaBuilder::new`]. Environment
/// resolution (`DATABASE_URL`, the `PROXIMA_S3_*` block) lives in
/// [`crate::RuntimeBuilder`]; hosts driving the facade through it never
/// construct this directly.
#[derive(Clone)]
pub struct EmbedConfig {
    pub database_url: String,
    pub s3: Option<S3RuntimeConfig>,
}

impl std::fmt::Debug for EmbedConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbedConfig")
            .field("database_url", &"<redacted>")
            .field("s3", &self.s3)
            .finish()
    }
}

/// Read the `PROXIMA_S3_*` block through the blob crate's parser.
///
/// The facade used to re-read all six variables itself. Two parsers over one
/// variable block meant one binary answered two ways: `maintain-blobs` reached
/// `S3RuntimeConfig::from_env` while `serve` reached this function, so
/// `PROXIMA_S3_UPLOAD_TTL_SECONDS="900\n"` was accepted by one subcommand and
/// refused by the other, and a whitespace-only bucket was "missing" to one and
/// a real bucket name to the other.
pub(crate) fn s3_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<S3RuntimeConfig>, EmbedError> {
    S3RuntimeConfig::from_lookup(lookup).map_err(|error| EmbedError::Config(error.to_string()))
}

/// Read the `PROXIMA_PG_*` tuning block through the storage crate's parser.
///
/// Same single-parser rule as the S3 block above: the storage crate is where
/// these knobs are consumed and where their defaults are defined, so a host
/// injecting its own environment cannot get a different answer than one
/// reading the process environment.
pub(crate) fn pg_tuning_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<PgTuning>, EmbedError> {
    PgTuning::from_lookup(lookup).map_err(|error| EmbedError::Config(error.to_string()))
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

    #[test]
    fn embed_config_debug_redacts_database_url() {
        let config = EmbedConfig {
            database_url: "postgres://user:secret@localhost/proxima".to_string(),
            s3: None,
        };
        let debug = format!("{config:?}");

        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("postgres://user"));
    }
}
