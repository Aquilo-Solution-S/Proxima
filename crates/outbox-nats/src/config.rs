//! The `PROXIMA_NATS_*` block: one parser, owned here (issue #305).
//!
//! The facade delegates to [`NatsPublisherConfig::from_lookup`] and
//! [`NatsConsumerConfig::from_lookup`] rather than re-reading the block, so
//! a host injecting its own environment cannot get a different answer than
//! one reading the process environment.
//!
//! `PROXIMA_NATS_URL` is the presence key. Unset means the publisher is
//! OFF, not that the deployment is misconfigured: capture keeps running and
//! records stay `pending`, which is this feature's rollback path
//! (docs/18 §Rollback).

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use proxima_core::publication::PublicationLimits;
use proxima_core::storage_ports::publication::{PublisherId, PublisherIdError};

/// Presence key for the whole block.
pub const ENV_URL: &str = "PROXIMA_NATS_URL";
pub const ENV_PROFILE: &str = "PROXIMA_NATS_PROFILE";
pub const ENV_STREAM: &str = "PROXIMA_NATS_STREAM";
pub const ENV_SUBJECT_PREFIX: &str = "PROXIMA_NATS_SUBJECT_PREFIX";
pub const ENV_CREDS_FILE: &str = "PROXIMA_NATS_CREDS_FILE";
pub const ENV_USER: &str = "PROXIMA_NATS_USER";
pub const ENV_PASSWORD: &str = "PROXIMA_NATS_PASSWORD";
pub const ENV_TOKEN: &str = "PROXIMA_NATS_TOKEN";
pub const ENV_BATCH: &str = "PROXIMA_NATS_BATCH";
pub const ENV_LEASE_SECS: &str = "PROXIMA_NATS_LEASE_SECS";
pub const ENV_POLL_MS: &str = "PROXIMA_NATS_POLL_MS";
pub const ENV_PUBLISH_TIMEOUT_MS: &str = "PROXIMA_NATS_PUBLISH_TIMEOUT_MS";
pub const ENV_PUBLISHER_ID: &str = "PROXIMA_NATS_PUBLISHER_ID";
pub const ENV_MAX_STREAM_BYTES: &str = "PROXIMA_NATS_MAX_STREAM_BYTES";
pub const ENV_MAX_MESSAGE_BYTES: &str = "PROXIMA_NATS_MAX_MESSAGE_BYTES";
pub const ENV_DUPLICATE_WINDOW_SECS: &str = "PROXIMA_NATS_DUPLICATE_WINDOW_SECS";
pub const ENV_CONSUMER_NAME: &str = "PROXIMA_NATS_CONSUMER_NAME";
pub const ENV_ACK_WAIT_SECS: &str = "PROXIMA_NATS_ACK_WAIT_SECS";
pub const ENV_MAX_ACK_PENDING: &str = "PROXIMA_NATS_MAX_ACK_PENDING";

/// The only shipped profile's name on the wire.
pub const PROFILE_LOCAL_FILE: &str = "local-file";

pub const DEFAULT_STREAM: &str = "PROXIMA_FACTS";
pub const DEFAULT_SUBJECT_PREFIX: &str = "proxima.fact";
pub const DEFAULT_CONSUMER_NAME: &str = "proxima-reference";
pub const DEFAULT_BATCH: u32 = 64;
pub const DEFAULT_LEASE: Duration = Duration::from_secs(30);
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);
pub const DEFAULT_PUBLISH_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_ACK_WAIT: Duration = Duration::from_secs(30);
pub const DEFAULT_DUPLICATE_WINDOW: Duration = Duration::from_mins(2);
pub const DEFAULT_MAX_ACK_PENDING: i64 = 1000;
/// 1 GiB of stream capacity by default. Explicit, never `-1`: an unlimited
/// stream turns broker backpressure into disk exhaustion on the host.
pub const DEFAULT_MAX_STREAM_BYTES: i64 = 1024 * 1024 * 1024;
/// Headroom above the capture cap for the `CloudEvents` envelope and the
/// three headers, so a payload the outbox accepted is never one the stream
/// refuses.
pub const MESSAGE_SIZE_HEADROOM_BYTES: usize = 16 * 1024;

/// How the publisher authenticates to the broker.
///
/// The four forms are mutually exclusive: a configuration naming two is a
/// [`ConfigError::ConflictingAuth`] rather than a silent precedence rule,
/// because "which credential did production actually use" is the question
/// an operator cannot afford to guess.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NatsAuth {
    #[default]
    None,
    CredsFile(PathBuf),
    UserPassword {
        user: String,
        password: String,
    },
    Token(String),
}

/// The shipped `JetStream` profile.
///
/// One variant on purpose. A cluster profile is a different durability
/// claim (replicas, placement, leader election) and shipping it as a
/// string a deployment can set without the code that honours it would be a
/// silent downgrade; any other value is [`ConfigError::UnsupportedProfile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeliveryProfile {
    /// File storage, `replicas: 1`, retention `Limits`, discard `New`,
    /// `max_age: 0`, subjects `<prefix>.>`.
    #[default]
    LocalFile,
}

impl DeliveryProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalFile => PROFILE_LOCAL_FILE,
        }
    }

    /// Parse a configured profile name.
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnsupportedProfile`] for any value other than
    /// `local-file`.
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        if raw == PROFILE_LOCAL_FILE {
            return Ok(Self::LocalFile);
        }
        Err(ConfigError::UnsupportedProfile {
            value: raw.to_owned(),
        })
    }
}

impl std::fmt::Display for DeliveryProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Everything the publisher needs to reach one broker and hold one stream.
#[derive(Debug, Clone)]
pub struct NatsPublisherConfig {
    /// `nats://…`; a comma-separated list names several servers of one
    /// cluster.
    pub url: String,
    pub auth: NatsAuth,
    pub profile: DeliveryProfile,
    pub stream: String,
    /// Validated: dot-separated tokens of `[A-Za-z0-9_-]`, no wildcards. A
    /// prefix carrying `*` or `>` would make the stream subscribe to
    /// subjects it does not own.
    pub subject_prefix: String,
    pub publisher_id: PublisherId,
    pub batch: NonZeroU32,
    pub lease: Duration,
    pub poll_interval: Duration,
    pub publish_timeout: Duration,
    /// Stream `max_bytes`. `-1` (unlimited) is refused: capacity has to be
    /// a number someone chose.
    pub max_stream_bytes: i64,
    /// Stream `max_msg_size`.
    pub max_message_bytes: i32,
    /// Broker-side dedup window, keyed on `Nats-Msg-Id`.
    pub duplicate_window: Duration,
}

impl NatsPublisherConfig {
    /// Build the default configuration for one broker URL.
    ///
    /// # Errors
    ///
    /// [`ConfigError::PublisherId`] when the derived publisher id is not a
    /// valid [`PublisherId`], which needs a hostile `HOSTNAME`.
    ///
    /// # Panics
    ///
    /// Never: the only unwrap is [`DEFAULT_BATCH`], a non-zero literal.
    pub fn new(url: impl Into<String>) -> Result<Self, ConfigError> {
        Ok(Self {
            url: url.into(),
            auth: NatsAuth::None,
            profile: DeliveryProfile::LocalFile,
            stream: DEFAULT_STREAM.to_owned(),
            subject_prefix: DEFAULT_SUBJECT_PREFIX.to_owned(),
            publisher_id: default_publisher_id()?,
            batch: NonZeroU32::new(DEFAULT_BATCH).expect("64 is not zero"),
            lease: DEFAULT_LEASE,
            poll_interval: DEFAULT_POLL_INTERVAL,
            publish_timeout: DEFAULT_PUBLISH_TIMEOUT,
            max_stream_bytes: DEFAULT_MAX_STREAM_BYTES,
            max_message_bytes: default_max_message_bytes(&PublicationLimits::default()),
            duplicate_window: DEFAULT_DUPLICATE_WINDOW,
        })
    }

    /// Read the block from an injected lookup.
    ///
    /// `Ok(None)` when [`ENV_URL`] is unset — an optional lane a host did
    /// not configure is not a misconfigured host.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] for an unsupported profile, an invalid subject
    /// prefix, conflicting or incomplete auth, or a malformed number.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, ConfigError> {
        let lookup = |key: &str| proxima_core::env_value(&lookup, key);
        let Some(url) = lookup(ENV_URL) else {
            return Ok(None);
        };
        let mut config = Self::new(url)?;
        if let Some(raw) = lookup(ENV_PROFILE) {
            config.profile = DeliveryProfile::parse(&raw)?;
        }
        if let Some(raw) = lookup(ENV_STREAM) {
            config.stream = validated_stream_name(&raw)?;
        }
        if let Some(raw) = lookup(ENV_SUBJECT_PREFIX) {
            config.subject_prefix = validated_subject_prefix(&raw)?;
        }
        if let Some(raw) = lookup(ENV_PUBLISHER_ID) {
            config.publisher_id =
                PublisherId::new(raw).map_err(|error| ConfigError::PublisherId { error })?;
        }
        config.auth = auth_from_lookup(&lookup)?;
        if let Some(raw) = lookup(ENV_BATCH) {
            config.batch = parse_non_zero_u32(ENV_BATCH, &raw)?;
        }
        if let Some(raw) = lookup(ENV_LEASE_SECS) {
            config.lease = Duration::from_secs(parse_positive_u64(ENV_LEASE_SECS, &raw)?);
        }
        if let Some(raw) = lookup(ENV_POLL_MS) {
            config.poll_interval = Duration::from_millis(parse_positive_u64(ENV_POLL_MS, &raw)?);
        }
        if let Some(raw) = lookup(ENV_PUBLISH_TIMEOUT_MS) {
            config.publish_timeout =
                Duration::from_millis(parse_positive_u64(ENV_PUBLISH_TIMEOUT_MS, &raw)?);
        }
        if let Some(raw) = lookup(ENV_MAX_STREAM_BYTES) {
            config.max_stream_bytes = parse_stream_bytes(&raw)?;
        }
        if let Some(raw) = lookup(ENV_MAX_MESSAGE_BYTES) {
            let bytes = parse_positive_u64(ENV_MAX_MESSAGE_BYTES, &raw)?;
            config.max_message_bytes = i32::try_from(bytes).map_err(|_| ConfigError::Number {
                key: ENV_MAX_MESSAGE_BYTES,
                value: raw.clone(),
            })?;
        }
        if let Some(raw) = lookup(ENV_DUPLICATE_WINDOW_SECS) {
            config.duplicate_window =
                Duration::from_secs(parse_positive_u64(ENV_DUPLICATE_WINDOW_SECS, &raw)?);
        }
        config.validate()?;
        Ok(Some(config))
    }

    /// Read the block from process environment.
    ///
    /// # Errors
    ///
    /// As [`Self::from_lookup`].
    pub fn from_env() -> Result<Option<Self>, ConfigError> {
        Self::from_lookup(proxima_core::process_env)
    }

    /// Refuse a configuration whose timings cannot work.
    ///
    /// A sub-second lease is shorter than one round trip to the broker, so
    /// every record would be re-claimed by a second publisher while the
    /// first is still waiting for its `PubAck` — turning at-least-once
    /// into always-twice. A zero publish timeout never waits for a
    /// `PubAck` at all, so no record would ever be marked published.
    ///
    /// # Errors
    ///
    /// [`ConfigError::LeaseTooShort`] for a lease under one second and
    /// [`ConfigError::ZeroTimeout`] for a zero publish timeout or poll
    /// interval.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.lease < Duration::from_secs(1) {
            return Err(ConfigError::LeaseTooShort {
                millis: u64::try_from(self.lease.as_millis()).unwrap_or(u64::MAX),
            });
        }
        if self.publish_timeout.is_zero() {
            return Err(ConfigError::ZeroTimeout {
                key: ENV_PUBLISH_TIMEOUT_MS,
            });
        }
        if self.poll_interval.is_zero() {
            return Err(ConfigError::ZeroTimeout { key: ENV_POLL_MS });
        }
        Ok(())
    }

    /// Raise the stream's `max_msg_size` to the deployment's capture cap
    /// plus [`MESSAGE_SIZE_HEADROOM_BYTES`].
    ///
    /// The two ceilings have to agree or a Fact the outbox accepted is one
    /// the stream refuses forever; the host calls this with the same
    /// [`PublicationLimits`] the engine enforces, so there is one number.
    #[must_use]
    pub fn with_capture_limits(mut self, limits: &PublicationLimits) -> Self {
        self.max_message_bytes = default_max_message_bytes(limits);
        self
    }

    /// Every subject this stream owns.
    #[must_use]
    pub fn subject_filter(&self) -> String {
        format!("{}.>", self.subject_prefix)
    }
}

/// Everything the reference consumer needs to bind one durable pull
/// consumer.
#[derive(Debug, Clone)]
pub struct NatsConsumerConfig {
    pub url: String,
    pub auth: NatsAuth,
    pub stream: String,
    pub subject_prefix: String,
    /// Durable name. Two processes sharing it share the work; two
    /// deployments sharing it by accident share the acknowledgements.
    pub durable_name: String,
    pub ack_wait: Duration,
    pub max_ack_pending: i64,
}

impl NatsConsumerConfig {
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            auth: NatsAuth::None,
            stream: DEFAULT_STREAM.to_owned(),
            subject_prefix: DEFAULT_SUBJECT_PREFIX.to_owned(),
            durable_name: DEFAULT_CONSUMER_NAME.to_owned(),
            ack_wait: DEFAULT_ACK_WAIT,
            max_ack_pending: DEFAULT_MAX_ACK_PENDING,
        }
    }

    /// Read the consumer half of the block from an injected lookup.
    ///
    /// `Ok(None)` when [`ENV_URL`] is unset, matching the publisher.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] for an invalid subject prefix, conflicting auth, or
    /// a malformed number.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, ConfigError> {
        let lookup = |key: &str| proxima_core::env_value(&lookup, key);
        let Some(url) = lookup(ENV_URL) else {
            return Ok(None);
        };
        let mut config = Self::new(url);
        if let Some(raw) = lookup(ENV_STREAM) {
            config.stream = validated_stream_name(&raw)?;
        }
        if let Some(raw) = lookup(ENV_SUBJECT_PREFIX) {
            config.subject_prefix = validated_subject_prefix(&raw)?;
        }
        if let Some(raw) = lookup(ENV_CONSUMER_NAME) {
            config.durable_name = validated_stream_name(&raw)?;
        }
        config.auth = auth_from_lookup(&lookup)?;
        if let Some(raw) = lookup(ENV_ACK_WAIT_SECS) {
            config.ack_wait = Duration::from_secs(parse_positive_u64(ENV_ACK_WAIT_SECS, &raw)?);
        }
        if let Some(raw) = lookup(ENV_MAX_ACK_PENDING) {
            let pending = parse_positive_u64(ENV_MAX_ACK_PENDING, &raw)?;
            config.max_ack_pending = i64::try_from(pending).map_err(|_| ConfigError::Number {
                key: ENV_MAX_ACK_PENDING,
                value: raw.clone(),
            })?;
        }
        Ok(Some(config))
    }

    /// Read the consumer half from process environment.
    ///
    /// # Errors
    ///
    /// As [`Self::from_lookup`].
    pub fn from_env() -> Result<Option<Self>, ConfigError> {
        Self::from_lookup(proxima_core::process_env)
    }

    /// Every subject this consumer filters on.
    #[must_use]
    pub fn subject_filter(&self) -> String {
        format!("{}.>", self.subject_prefix)
    }
}

/// Why a `PROXIMA_NATS_*` block was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error(
        "PROXIMA_NATS_PROFILE must be `local-file`, the only shipped delivery profile, \
         got {value:?}"
    )]
    UnsupportedProfile { value: String },
    #[error(
        "PROXIMA_NATS_SUBJECT_PREFIX must be dot-separated tokens of [A-Za-z0-9_-] \
         and must not carry the `*` or `>` wildcards, got {value:?}"
    )]
    InvalidSubjectPrefix { value: String },
    #[error("{key} must not contain whitespace, `.`, `*` or `>`, got {value:?}")]
    InvalidName { key: &'static str, value: String },
    #[error("{key} must be a positive integer, got {value:?}")]
    Number { key: &'static str, value: String },
    #[error(
        "PROXIMA_NATS_MAX_STREAM_BYTES must be a positive byte count; unlimited (-1) is \
         refused because stream capacity has to be an explicit choice, got {value:?}"
    )]
    UnboundedStream { value: String },
    #[error("the NATS auth forms are mutually exclusive: {first} and {second} are both set")]
    ConflictingAuth {
        first: &'static str,
        second: &'static str,
    },
    #[error(
        "PROXIMA_NATS_USER and PROXIMA_NATS_PASSWORD must be set together; {missing} is missing"
    )]
    IncompleteAuth { missing: &'static str },
    #[error(
        "PROXIMA_NATS_LEASE_SECS must be at least one second, got {millis}ms; a lease \
         shorter than one broker round trip re-claims every record mid-flight"
    )]
    LeaseTooShort { millis: u64 },
    #[error("{key} must be greater than zero")]
    ZeroTimeout { key: &'static str },
    #[error("publisher id: {error}")]
    PublisherId { error: PublisherIdError },
}

/// The default publisher label: `<hostname>:<pid>`.
///
/// `HOSTNAME` rather than a syscall: reading it needs no new dependency and
/// no `unsafe`, and this value is an operator label rather than an
/// identity — the fencing token is the claim token. A host that wants a
/// stable label sets [`ENV_PUBLISHER_ID`].
fn default_publisher_id() -> Result<PublisherId, ConfigError> {
    let host = proxima_core::env_value(&proxima_core::process_env, "HOSTNAME")
        .unwrap_or_else(|| "proxima".to_owned());
    let host: String = host
        .chars()
        .filter(|c| !c.is_control())
        .take(96)
        .collect::<String>();
    let host = if host.trim().is_empty() {
        "proxima".to_owned()
    } else {
        host
    };
    PublisherId::new(format!("{host}:{}", std::process::id()))
        .map_err(|error| ConfigError::PublisherId { error })
}

/// The stream's `max_msg_size` for a deployment enforcing `limits`.
fn default_max_message_bytes(limits: &PublicationLimits) -> i32 {
    let bytes = limits
        .max_payload_bytes
        .saturating_add(MESSAGE_SIZE_HEADROOM_BYTES);
    i32::try_from(bytes).unwrap_or(i32::MAX)
}

fn auth_from_lookup(lookup: &impl Fn(&str) -> Option<String>) -> Result<NatsAuth, ConfigError> {
    let creds = lookup(ENV_CREDS_FILE);
    let user = lookup(ENV_USER);
    let password = lookup(ENV_PASSWORD);
    let token = lookup(ENV_TOKEN);
    if creds.is_some() && (user.is_some() || password.is_some()) {
        return Err(ConfigError::ConflictingAuth {
            first: ENV_CREDS_FILE,
            second: ENV_USER,
        });
    }
    if creds.is_some() && token.is_some() {
        return Err(ConfigError::ConflictingAuth {
            first: ENV_CREDS_FILE,
            second: ENV_TOKEN,
        });
    }
    if token.is_some() && (user.is_some() || password.is_some()) {
        return Err(ConfigError::ConflictingAuth {
            first: ENV_TOKEN,
            second: ENV_USER,
        });
    }
    if let Some(creds) = creds {
        return Ok(NatsAuth::CredsFile(PathBuf::from(creds)));
    }
    if let Some(token) = token {
        return Ok(NatsAuth::Token(token));
    }
    match (user, password) {
        (Some(user), Some(password)) => Ok(NatsAuth::UserPassword { user, password }),
        (Some(_), None) => Err(ConfigError::IncompleteAuth {
            missing: ENV_PASSWORD,
        }),
        (None, Some(_)) => Err(ConfigError::IncompleteAuth { missing: ENV_USER }),
        (None, None) => Ok(NatsAuth::None),
    }
}

/// A stream or durable-consumer name: no whitespace and none of the three
/// subject metacharacters, which is exactly what the broker enforces.
fn validated_stream_name(raw: &str) -> Result<String, ConfigError> {
    if raw.is_empty()
        || raw
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '.' | '*' | '>'))
    {
        return Err(ConfigError::InvalidName {
            key: ENV_STREAM,
            value: raw.to_owned(),
        });
    }
    Ok(raw.to_owned())
}

/// A subject prefix: dot-separated tokens of `[A-Za-z0-9_-]`.
fn validated_subject_prefix(raw: &str) -> Result<String, ConfigError> {
    let invalid = || ConfigError::InvalidSubjectPrefix {
        value: raw.to_owned(),
    };
    if raw.is_empty() || raw.starts_with('.') || raw.ends_with('.') {
        return Err(invalid());
    }
    for token in raw.split('.') {
        if token.is_empty()
            || !token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        {
            return Err(invalid());
        }
    }
    Ok(raw.to_owned())
}

fn parse_non_zero_u32(key: &'static str, raw: &str) -> Result<NonZeroU32, ConfigError> {
    raw.parse::<NonZeroU32>().map_err(|_| ConfigError::Number {
        key,
        value: raw.to_owned(),
    })
}

fn parse_positive_u64(key: &'static str, raw: &str) -> Result<u64, ConfigError> {
    match raw.parse::<u64>() {
        Ok(value) if value > 0 => Ok(value),
        _ => Err(ConfigError::Number {
            key,
            value: raw.to_owned(),
        }),
    }
}

fn parse_stream_bytes(raw: &str) -> Result<i64, ConfigError> {
    match raw.parse::<i64>() {
        Ok(value) if value > 0 => Ok(value),
        _ => Err(ConfigError::UnboundedStream {
            value: raw.to_owned(),
        }),
    }
}

/// The subject one captured event is published on.
///
/// `<prefix>.<owner_kind>.<owner_uuid>.<type_token>`, where `type_token`
/// replaces every character outside `[A-Za-z0-9_-]` with `_`. Owner routing
/// lives in the subject so a NATS account can restrict a consumer to one
/// owner's events with a subject permission, without the broker parsing
/// the payload.
#[must_use]
pub fn subject_for(
    prefix: &str,
    owner_kind: &str,
    owner_id: uuid::Uuid,
    event_type: &str,
) -> String {
    format!(
        "{prefix}.{owner_kind}.{owner_id}.{}",
        type_token(event_type)
    )
}

/// The subject token for one event type.
#[must_use]
pub fn type_token(event_type: &str) -> String {
    let token: String = event_type
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if token.is_empty() {
        "_".to_owned()
    } else {
        token
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
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn an_unset_url_is_a_publisher_that_is_off_not_a_broken_host() {
        assert!(
            NatsPublisherConfig::from_lookup(env(&[("PROXIMA_NATS_STREAM", "OTHER")]))
                .expect("parses")
                .is_none()
        );
        assert!(
            NatsConsumerConfig::from_lookup(env(&[]))
                .expect("parses")
                .is_none()
        );
    }

    #[test]
    fn any_profile_but_local_file_is_refused_by_name() {
        let err = NatsPublisherConfig::from_lookup(env(&[
            (ENV_URL, "nats://127.0.0.1:4222"),
            (ENV_PROFILE, "cluster"),
        ]))
        .expect_err("cluster is not shipped");
        assert_eq!(
            err,
            ConfigError::UnsupportedProfile {
                value: "cluster".to_owned()
            }
        );
        assert_eq!(
            DeliveryProfile::parse(PROFILE_LOCAL_FILE).expect("shipped"),
            DeliveryProfile::LocalFile
        );
    }

    #[test]
    fn a_wildcard_prefix_would_subscribe_the_stream_to_foreign_subjects() {
        for bad in [
            "a.*",
            "proxima.>",
            "",
            ".leading",
            "trailing.",
            "a..b",
            "a b",
        ] {
            assert!(
                validated_subject_prefix(bad).is_err(),
                "{bad} must be refused"
            );
        }
        assert_eq!(
            validated_subject_prefix("proxima.fact").expect("valid"),
            "proxima.fact"
        );
        assert_eq!(validated_subject_prefix("a_b-c").expect("valid"), "a_b-c");
    }

    #[test]
    fn the_auth_forms_are_mutually_exclusive() {
        let err = NatsPublisherConfig::from_lookup(env(&[
            (ENV_URL, "nats://127.0.0.1:4222"),
            (ENV_CREDS_FILE, "/tmp/x.creds"),
            (ENV_TOKEN, "t"),
        ]))
        .expect_err("two forms");
        assert!(matches!(err, ConfigError::ConflictingAuth { .. }));

        let err = NatsPublisherConfig::from_lookup(env(&[
            (ENV_URL, "nats://127.0.0.1:4222"),
            (ENV_USER, "u"),
        ]))
        .expect_err("half a form");
        assert!(matches!(err, ConfigError::IncompleteAuth { .. }));

        let config = NatsPublisherConfig::from_lookup(env(&[
            (ENV_URL, "nats://127.0.0.1:4222"),
            (ENV_USER, "u"),
            (ENV_PASSWORD, "p"),
        ]))
        .expect("both halves")
        .expect("url set");
        assert_eq!(
            config.auth,
            NatsAuth::UserPassword {
                user: "u".to_owned(),
                password: "p".to_owned()
            }
        );
    }

    #[test]
    fn an_unlimited_stream_is_refused_and_the_numbers_must_be_positive() {
        for (key, value) in [
            (ENV_BATCH, "0"),
            (ENV_LEASE_SECS, "0"),
            (ENV_POLL_MS, "abc"),
            (ENV_PUBLISH_TIMEOUT_MS, "-1"),
        ] {
            let err =
                NatsPublisherConfig::from_lookup(env(&[(ENV_URL, "nats://x:4222"), (key, value)]))
                    .expect_err("{key} must refuse {value}");
            assert!(matches!(err, ConfigError::Number { .. }), "{err}");
        }
        let err = NatsPublisherConfig::from_lookup(env(&[
            (ENV_URL, "nats://x:4222"),
            (ENV_MAX_STREAM_BYTES, "-1"),
        ]))
        .expect_err("unlimited is refused");
        assert!(matches!(err, ConfigError::UnboundedStream { .. }));
    }

    #[test]
    fn the_defaults_are_the_documented_ones() {
        let config = NatsPublisherConfig::from_lookup(env(&[(ENV_URL, "nats://127.0.0.1:4222")]))
            .expect("parses")
            .expect("url set");
        assert_eq!(config.stream, DEFAULT_STREAM);
        assert_eq!(config.subject_prefix, DEFAULT_SUBJECT_PREFIX);
        assert_eq!(config.batch.get(), DEFAULT_BATCH);
        assert_eq!(config.lease, DEFAULT_LEASE);
        assert_eq!(config.poll_interval, DEFAULT_POLL_INTERVAL);
        assert_eq!(config.publish_timeout, DEFAULT_PUBLISH_TIMEOUT);
        assert_eq!(config.duplicate_window, DEFAULT_DUPLICATE_WINDOW);
        assert_eq!(config.max_stream_bytes, DEFAULT_MAX_STREAM_BYTES);
        assert_eq!(config.profile, DeliveryProfile::LocalFile);
        assert_eq!(config.auth, NatsAuth::None);
        assert_eq!(config.subject_filter(), "proxima.fact.>");

        let consumer = NatsConsumerConfig::from_lookup(env(&[(ENV_URL, "nats://127.0.0.1:4222")]))
            .expect("parses")
            .expect("url set");
        assert_eq!(consumer.durable_name, DEFAULT_CONSUMER_NAME);
        assert_eq!(consumer.ack_wait, DEFAULT_ACK_WAIT);
        assert_eq!(consumer.max_ack_pending, DEFAULT_MAX_ACK_PENDING);
    }

    #[test]
    fn a_lease_shorter_than_a_round_trip_and_a_zero_timeout_are_refused() {
        let mut config = NatsPublisherConfig::new("nats://x:4222").expect("publisher id");
        config.lease = Duration::from_millis(300);
        assert!(matches!(
            config.validate(),
            Err(ConfigError::LeaseTooShort { millis: 300 })
        ));

        let mut config = NatsPublisherConfig::new("nats://x:4222").expect("publisher id");
        config.publish_timeout = Duration::ZERO;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::ZeroTimeout {
                key: ENV_PUBLISH_TIMEOUT_MS
            })
        ));

        let mut config = NatsPublisherConfig::new("nats://x:4222").expect("publisher id");
        config.poll_interval = Duration::ZERO;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::ZeroTimeout { key: ENV_POLL_MS })
        ));

        assert!(
            NatsPublisherConfig::new("nats://x:4222")
                .expect("publisher id")
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn the_message_ceiling_follows_the_capture_ceiling() {
        let limits = PublicationLimits {
            max_pending: 10,
            max_payload_bytes: 1_000,
        };
        let config = NatsPublisherConfig::new("nats://x:4222")
            .expect("publisher id")
            .with_capture_limits(&limits);
        assert_eq!(
            config.max_message_bytes,
            1_000 + i32::try_from(MESSAGE_SIZE_HEADROOM_BYTES).expect("16 KiB fits")
        );
    }

    #[test]
    fn the_subject_carries_the_owner_and_a_sanitized_type() {
        let owner = uuid::Uuid::nil();
        assert_eq!(
            subject_for("proxima.fact", "personal", owner, "acme/build.finished-v1"),
            format!("proxima.fact.personal.{owner}.acme_build_finished-v1")
        );
        assert_eq!(type_token(""), "_");
        assert_eq!(type_token("a.b*c>d e"), "a_b_c_d_e");
    }
}
