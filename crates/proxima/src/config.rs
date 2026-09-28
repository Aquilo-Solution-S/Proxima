use std::time::Duration;

use proxima_blob_s3::S3RuntimeConfig;
use proxima_core::publication::{PublicationConfig, PublicationLimits, PublicationSource};
use proxima_storage_pg::{PgPoolConfig, PgTuning};

use crate::ProximaError;

/// Read the `PROXIMA_S3_*` block through the blob crate's parser.
/// One parser for the block; the facade must not re-read it.
pub(crate) fn s3_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<S3RuntimeConfig>, ProximaError> {
    S3RuntimeConfig::from_lookup(lookup).map_err(|error| ProximaError::Config(error.to_string()))
}

/// Read the `PROXIMA_PG_*` tuning block through the storage crate's parser.
///
/// Same single-parser rule as the S3 block above: the storage crate is where
/// these knobs are consumed and where their defaults are defined, so a host
/// injecting its own environment cannot get a different answer than one
/// reading the process environment.
pub(crate) fn pg_tuning_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<PgTuning>, ProximaError> {
    PgTuning::from_lookup(lookup).map_err(|error| ProximaError::Config(error.to_string()))
}

/// Read the `PROXIMA_PG_*` pool block through the storage crate's parser.
///
/// Pool construction consumes this resolved value. The canonical runtime path
/// therefore never falls back to a second process-environment read.
pub(crate) fn pg_pool_config_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<PgPoolConfig>, ProximaError> {
    PgPoolConfig::from_lookup(lookup).map_err(|error| ProximaError::Config(error.to_string()))
}

/// Environment key naming this deployment's `CloudEvents` producer identity.
pub(crate) const ENV_PUBLICATION_SOURCE: &str = "PROXIMA_PUBLICATION_SOURCE";
/// Environment key bounding the un-published outbox backlog.
pub(crate) const ENV_OUTBOX_MAX_PENDING: &str = "PROXIMA_OUTBOX_MAX_PENDING";
/// Environment key bounding one sealed envelope.
pub(crate) const ENV_OUTBOX_MAX_PAYLOAD_BYTES: &str = "PROXIMA_OUTBOX_MAX_PAYLOAD_BYTES";
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
/// [`ProximaError::Config`] for a non-numeric value or a non-zero horizon
/// under [`MIN_PUBLISHED_RETENTION`].
pub(crate) fn published_retention_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<Duration>, ProximaError> {
    let Some(raw) = lookup(ENV_OUTBOX_PUBLISHED_RETENTION_SECS) else {
        return Ok(None);
    };
    let seconds: u64 = raw.parse().map_err(|_| {
        ProximaError::Config(format!(
            "{ENV_OUTBOX_PUBLISHED_RETENTION_SECS} must be a whole number of seconds, \
             got {raw:?}"
        ))
    })?;
    if seconds == 0 {
        return Ok(None);
    }
    let horizon = Duration::from_secs(seconds);
    if horizon < MIN_PUBLISHED_RETENTION {
        return Err(ProximaError::Config(format!(
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
/// [`ProximaError::Config`] for a source that is not an absolute URI/URN, a
/// non-numeric or zero bound.
pub(crate) fn publication_config_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<PublicationConfig, ProximaError> {
    let source = lookup(ENV_PUBLICATION_SOURCE)
        .map(|raw| {
            PublicationSource::new(raw)
                .map_err(|error| ProximaError::Config(format!("{ENV_PUBLICATION_SOURCE}: {error}")))
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
) -> Result<Option<u64>, ProximaError> {
    let Some(raw) = lookup(key) else {
        return Ok(None);
    };
    let value: u64 = raw.parse().map_err(|_| {
        ProximaError::Config(format!("{key} must be a positive integer, got {raw:?}"))
    })?;
    if value == 0 {
        return Err(ProximaError::Config(format!(
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
/// [`ProximaError::Config`] for any malformed key in the block.
#[cfg(feature = "outbox-nats")]
pub(crate) fn nats_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<proxima_outbox_nats::NatsPublisherConfig>, ProximaError> {
    proxima_outbox_nats::NatsPublisherConfig::from_lookup(lookup)
        .map_err(|error| ProximaError::Config(error.to_string()))
}

/// Read the `PROXIMA_COPY_CLEANER_*` block through the adapter crate's
/// parser. `Ok(None)` when none of its keys is set; a key set without
/// `PROXIMA_COPY_CLEANER_URL` is a half-configured section and refused.
///
/// # Errors
///
/// [`ProximaError::Config`] for a broker-less section or any malformed key.
#[cfg(feature = "outbox-nats")]
pub(crate) fn copy_cleaner_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<proxima_outbox_nats::JetStreamCopyCleanerConfig>, ProximaError> {
    proxima_outbox_nats::JetStreamCopyCleanerConfig::from_lookup(lookup)
        .map_err(|error| ProximaError::Config(error.to_string()))
}

/// The broker URLs a build without `outbox-nats` cannot honour.
#[cfg(not(feature = "outbox-nats"))]
const UNCOMPILED_BROKER_URLS: [&str; 2] = ["PROXIMA_NATS_URL", "PROXIMA_COPY_CLEANER_URL"];

/// Without `outbox-nats` there is no publisher or cleaner to start: a broker
/// the operator named is a request this binary cannot honour, refused rather
/// than ignored (as `PROXIMA_OIDC_ISSUER` without `auth-oidc` is).
///
/// # Errors
///
/// [`ProximaError::Config`] naming the first broker URL that is set.
#[cfg(not(feature = "outbox-nats"))]
pub(crate) fn refuse_uncompiled_broker(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<(), ProximaError> {
    match UNCOMPILED_BROKER_URLS
        .iter()
        .find(|key| lookup(key).is_some())
    {
        Some(key) => Err(ProximaError::Config(format!(
            "{key} is set but this binary was built without the `outbox-nats` cargo \
             feature; enable it or unset {key}"
        ))),
        None => Ok(()),
    }
}

pub(crate) fn parse_bool_value(key: &str, raw: &str) -> Result<bool, ProximaError> {
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ProximaError::Config(format!(
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
    fn pg_pool_config_uses_the_injected_lookup() {
        let config = pg_pool_config_from_lookup(&env(&[("PROXIMA_PG_MAX_CONNECTIONS", "4")]))
            .unwrap()
            .expect("non-default pool config");
        assert_eq!(config.max_connections, 4);
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
            assert!(
                matches!(err, ProximaError::Config(_)),
                "{err} for {pairs:?}"
            );
        }
    }

    #[cfg(not(feature = "outbox-nats"))]
    #[test]
    fn a_broker_url_without_the_outbox_feature_refuses() {
        assert!(refuse_uncompiled_broker(&env(&[])).is_ok());
        for key in UNCOMPILED_BROKER_URLS {
            let err = refuse_uncompiled_broker(&env(&[(key, "nats://127.0.0.1:4222")]))
                .expect_err("a broker this build cannot reach");
            assert!(err.to_string().contains(key), "{err}");
            assert!(err.to_string().contains("outbox-nats"), "{err}");
        }
    }
}
