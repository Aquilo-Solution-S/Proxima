use std::time::Duration;

use proxima_blob_s3::S3RuntimeConfig;
use proxima_core::publication::{PublicationConfig, PublicationLimits, PublicationSource};
use proxima_storage_pg::{PgPoolConfig, PgTuning};

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
/// One parser for the block; the facade must not re-read it.
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

/// Read the `PROXIMA_PG_*` pool block through the storage crate's parser.
///
/// Pool construction consumes this resolved value. The canonical runtime path
/// therefore never falls back to a second process-environment read.
pub(crate) fn pg_pool_config_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<PgPoolConfig>, EmbedError> {
    PgPoolConfig::from_lookup(lookup).map_err(|error| EmbedError::Config(error.to_string()))
}

/// Environment key naming this deployment's `CloudEvents` producer identity.
pub(crate) const ENV_PUBLICATION_SOURCE: &str = "PROXIMA_PUBLICATION_SOURCE";
/// Environment key bounding the un-published outbox backlog.
pub(crate) const ENV_OUTBOX_MAX_PENDING: &str = "PROXIMA_OUTBOX_MAX_PENDING";
/// Environment key bounding one sealed envelope.
pub(crate) const ENV_OUTBOX_MAX_PAYLOAD_BYTES: &str = "PROXIMA_OUTBOX_MAX_PAYLOAD_BYTES";
/// Environment key naming the stream's per-message ceiling.
///
/// Parsed by the adapter (`crates/outbox-nats/src/config.rs`); named here
/// only so [`crate::RuntimeBuilder::resolve`] can tell an operator's chosen
/// value from the adapter's default before deriving one from the capture
/// ceiling.
#[cfg(feature = "outbox-nats")]
pub(crate) const ENV_NATS_MAX_MESSAGE_BYTES: &str = "PROXIMA_NATS_MAX_MESSAGE_BYTES";

/// Environment key retiring records that were already DELIVERED.
pub(crate) const ENV_OUTBOX_PUBLISHED_RETENTION_SECS: &str =
    "PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS";

/// Shortest retention horizon an operator may configure.
///
/// A horizon under a minute is indistinguishable from "delete on delivery",
/// and delivery is not the same event as consumption: a consumer that is
/// behind, or an operator reconciling what was sent, has nothing left to
/// read from. One minute is not a safe replay window either — it is the
/// point below which the setting is certainly a mistake, so it is refused
/// rather than honoured.
pub(crate) const MIN_PUBLISHED_RETENTION: Duration = Duration::from_mins(1);

/// Read how long a DELIVERED record is kept before the publisher task
/// reclaims it (docs/18 §Retention).
///
/// `None` — the default, and what `0` spells explicitly — keeps published
/// records forever, which is the behaviour every deployment had before this
/// knob existed. Only `published` rows are ever in scope; see
/// [`proxima_core::storage_ports::publication::PublicationRetentionPort`].
///
/// # Errors
///
/// [`EmbedError::Config`] for a non-numeric value or a non-zero horizon
/// under [`MIN_PUBLISHED_RETENTION`].
pub(crate) fn published_retention_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<Duration>, EmbedError> {
    let Some(raw) = lookup(ENV_OUTBOX_PUBLISHED_RETENTION_SECS) else {
        return Ok(None);
    };
    let seconds: u64 = raw.parse().map_err(|_| {
        EmbedError::Config(format!(
            "{ENV_OUTBOX_PUBLISHED_RETENTION_SECS} must be a whole number of seconds, \
             got {raw:?}"
        ))
    })?;
    if seconds == 0 {
        return Ok(None);
    }
    let horizon = Duration::from_secs(seconds);
    if horizon < MIN_PUBLISHED_RETENTION {
        return Err(EmbedError::Config(format!(
            "{ENV_OUTBOX_PUBLISHED_RETENTION_SECS} is {seconds}s, under the \
             {}s floor; set 0 to keep published records forever",
            MIN_PUBLISHED_RETENTION.as_secs()
        )));
    }
    Ok(Some(horizon))
}

/// Read the publication block (docs/18 §Configuration) into the ONE value
/// the engine is built with.
///
/// The result is not optional. Every deployment has a publication
/// configuration — a source (or none) and a pair of bounds — and the engine
/// is always constructed through
/// [`proxima_core::engine::EngineBuilder::try_with_publication_config`], so
/// a host that freezes a listenable schema without naming a source fails at
/// boot rather than on its first admission.
///
/// The limits parsed here are the limits ENFORCED at capture: they travel
/// with the draft on the authorization witness. There is no storage-side
/// copy to keep in step.
///
/// Reads none of these keys when the caller already supplied a
/// configuration programmatically — same single-parser rule as the S3 and
/// Postgres blocks.
///
/// # Errors
///
/// [`EmbedError::Config`] for a source that is not an absolute URI/URN, a
/// non-numeric or zero bound.
pub(crate) fn publication_config_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<PublicationConfig, EmbedError> {
    let source = lookup(ENV_PUBLICATION_SOURCE)
        .map(|raw| {
            PublicationSource::new(raw)
                .map_err(|error| EmbedError::Config(format!("{ENV_PUBLICATION_SOURCE}: {error}")))
        })
        .transpose()?;
    let defaults = PublicationLimits::default();
    let limits = PublicationLimits {
        max_pending: parse_positive(lookup, ENV_OUTBOX_MAX_PENDING)?
            .unwrap_or(defaults.max_pending),
        max_payload_bytes: parse_positive(lookup, ENV_OUTBOX_MAX_PAYLOAD_BYTES)?
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(defaults.max_payload_bytes),
    };
    Ok(PublicationConfig { source, limits })
}

/// A bound must be a positive integer. Zero is refused rather than treated
/// as "unlimited": both limits are refusals, and a zero one would refuse
/// every listenable write instead of none.
fn parse_positive(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &str,
) -> Result<Option<u64>, EmbedError> {
    let Some(raw) = lookup(key) else {
        return Ok(None);
    };
    let value: u64 = raw.parse().map_err(|_| {
        EmbedError::Config(format!("{key} must be a positive integer, got {raw:?}"))
    })?;
    if value == 0 {
        return Err(EmbedError::Config(format!(
            "{key} must be greater than zero; a zero bound refuses every listenable write"
        )));
    }
    Ok(Some(value))
}

/// Read the `PROXIMA_NATS_*` block through the adapter crate's parser.
///
/// Same single-parser rule as the S3 and Postgres blocks: the adapter owns
/// the keys, their defaults and their validation. `Ok(None)` when
/// `PROXIMA_NATS_URL` is unset — a deployment that names no broker gets no
/// publisher, and that is not an error.
///
/// # Errors
///
/// [`EmbedError::Config`] for any malformed key in the block.
#[cfg(feature = "outbox-nats")]
pub(crate) fn nats_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<proxima_outbox_nats::NatsPublisherConfig>, EmbedError> {
    proxima_outbox_nats::NatsPublisherConfig::from_lookup(lookup)
        .map_err(|error| EmbedError::Config(error.to_string()))
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

    /// The retention knob is off by default, spells "off" as `0`, and
    /// refuses a horizon so short that delivery and deletion would be the
    /// same event.
    #[test]
    fn the_published_retention_horizon_is_off_by_default_and_floored() {
        assert_eq!(published_retention_from_lookup(&env(&[])).unwrap(), None);
        assert_eq!(
            published_retention_from_lookup(&env(&[(ENV_OUTBOX_PUBLISHED_RETENTION_SECS, "0")]))
                .unwrap(),
            None,
            "0 is the explicit spelling of keep-forever, not a zero-second horizon"
        );
        assert_eq!(
            published_retention_from_lookup(&env(&[(
                ENV_OUTBOX_PUBLISHED_RETENTION_SECS,
                "604800"
            )]))
            .unwrap(),
            Some(Duration::from_hours(24 * 7))
        );
        let too_short =
            published_retention_from_lookup(&env(&[(ENV_OUTBOX_PUBLISHED_RETENTION_SECS, "59")]))
                .expect_err("under the floor");
        assert!(
            too_short
                .to_string()
                .contains(ENV_OUTBOX_PUBLISHED_RETENTION_SECS),
            "{too_short}"
        );
        let malformed = published_retention_from_lookup(&env(&[(
            ENV_OUTBOX_PUBLISHED_RETENTION_SECS,
            "a week",
        )]))
        .expect_err("not a number");
        assert!(malformed.to_string().contains("a week"), "{malformed}");
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
    fn pg_pool_config_uses_the_injected_lookup() {
        let config = pg_pool_config_from_lookup(&env(&[("PROXIMA_PG_MAX_CONNECTIONS", "4")]))
            .unwrap()
            .expect("non-default pool config");
        assert_eq!(config.max_connections, 4);
    }

    #[test]
    fn publication_defaults_when_the_block_is_unset() {
        let config = publication_config_from_lookup(&env(&[])).unwrap();
        assert!(config.source.is_none());
        assert_eq!(config.limits, PublicationLimits::default());
    }

    #[test]
    fn publication_reads_the_source_and_both_bounds() {
        let config = publication_config_from_lookup(&env(&[
            (ENV_PUBLICATION_SOURCE, "urn:proxima:test"),
            (ENV_OUTBOX_MAX_PENDING, "7"),
            (ENV_OUTBOX_MAX_PAYLOAD_BYTES, "4096"),
        ]))
        .unwrap();
        assert_eq!(
            config.source.as_ref().map(PublicationSource::as_str),
            Some("urn:proxima:test")
        );
        assert_eq!(config.limits.max_pending, 7);
        assert_eq!(config.limits.max_payload_bytes, 4096);
    }

    #[test]
    fn a_malformed_publication_block_is_a_boot_error() {
        for pairs in [
            vec![(ENV_PUBLICATION_SOURCE, "not a uri")],
            vec![(ENV_OUTBOX_MAX_PENDING, "lots")],
            vec![(ENV_OUTBOX_MAX_PENDING, "0")],
            vec![(ENV_OUTBOX_MAX_PAYLOAD_BYTES, "-1")],
            vec![(ENV_OUTBOX_MAX_PAYLOAD_BYTES, "0")],
        ] {
            let err = publication_config_from_lookup(&env(&pairs))
                .expect_err("a malformed publication block must refuse the boot");
            assert!(matches!(err, EmbedError::Config(_)), "{err} for {pairs:?}");
        }
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
