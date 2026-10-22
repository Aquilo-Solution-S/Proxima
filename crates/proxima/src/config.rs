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

/// Environment key for how often the runtime sweeps expired uploads and
/// drains object purge debt; `0` turns the in-process drain off.
pub(crate) const ENV_OBJECT_PURGE_INTERVAL_SECS: &str = "PROXIMA_OBJECT_PURGE_INTERVAL_SECS";

/// How often the object-purge feature runs when nothing says otherwise.
pub(crate) const DEFAULT_OBJECT_PURGE_INTERVAL: Duration = Duration::from_mins(1);

/// Read the object-purge interval. `Some(Duration::ZERO)` is the explicit
/// opt-out; `None` leaves the default.
///
/// # Errors
///
/// [`ProximaError::Config`] for a non-numeric value.
pub(crate) fn object_purge_interval_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<Duration>, ProximaError> {
    let Some(raw) = lookup(ENV_OBJECT_PURGE_INTERVAL_SECS) else {
        return Ok(None);
    };
    let seconds: u64 = raw.parse().map_err(|_| {
        ProximaError::Config(format!(
            "{ENV_OBJECT_PURGE_INTERVAL_SECS} must be a whole number of seconds, got {raw:?}"
        ))
    })?;
    Ok(Some(Duration::from_secs(seconds)))
}

/// Shortest retention horizon an operator may configure.
///
/// A horizon under a minute is indistinguishable from "delete on delivery",
/// and delivery is not the same event as consumption: a consumer that is
/// behind, or an operator reconciling what was sent, has nothing left to
/// read from. One minute is not a safe replay window either — it is the
/// point below which the setting is certainly a mistake, so it is refused
/// rather than honoured.
pub(crate) const MIN_PUBLISHED_RETENTION: Duration = Duration::from_mins(1);

/// How long a DELIVERED record is kept when nothing says otherwise.
pub(crate) const DEFAULT_PUBLISHED_RETENTION: Duration = Duration::from_hours(24 * 7);

/// Read how long a DELIVERED record is kept before the publisher task
/// reclaims it (docs/host-integration.md §Retention). `Some(Duration::ZERO)`
/// is the explicit opt-out that keeps published records forever; `None`
/// leaves the default, [`DEFAULT_PUBLISHED_RETENTION`]. Only `published`
/// rows are ever in scope; see
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
        return Ok(Some(Duration::ZERO));
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

/// Read the publication block (docs/host-integration.md §Configuration) into the ONE value
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

pub(crate) fn parse_bool_value(key: &str, raw: &str) -> Result<bool, ProximaError> {
    proxima_core::parse_bool_word(raw)
        .ok_or_else(|| ProximaError::Config(format!("{key} must be a boolean, got {raw:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::env_from_pairs as env;

    /// The retention knob leaves the default when unset, spells "off" as `0`,
    /// and refuses a horizon so short that delivery and deletion would be the
    /// same event.
    #[test]
    fn the_published_retention_horizon_is_floored_and_zero_is_off() {
        assert_eq!(published_retention_from_lookup(&env(&[])).unwrap(), None);
        assert_eq!(
            published_retention_from_lookup(&env(&[(ENV_OUTBOX_PUBLISHED_RETENTION_SECS, "0")]))
                .unwrap(),
            Some(Duration::ZERO),
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
}
