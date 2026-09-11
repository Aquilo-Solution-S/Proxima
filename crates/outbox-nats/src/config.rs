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

use proxima_core::storage_ports::publication::{PublisherId, PublisherIdError};

/// Presence key for the whole block.
pub const ENV_URL: &str = "PROXIMA_NATS_URL";
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
pub const ENV_CONSUMER_STREAM: &str = "PROXIMA_NATS_CONSUMER_STREAM";
pub const ENV_CONSUMER_NAME: &str = "PROXIMA_NATS_CONSUMER_NAME";

pub const DEFAULT_SUBJECT_PREFIX: &str = "proxima.fact";
pub const DEFAULT_CONSUMER_STREAM: &str = "PROXIMA_FACTS";
pub const DEFAULT_CONSUMER_NAME: &str = "proxima-reference";
pub const DEFAULT_BATCH: u32 = 64;
pub const DEFAULT_LEASE: Duration = Duration::from_secs(30);
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);
pub const DEFAULT_PUBLISH_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the consumer waits for a connection and for one `JetStream`
/// API round trip.
///
/// Deliberately NOT `ack_wait`. `ack_wait` is how long a SINK may take to
/// make an outcome durable — minutes, in a deployment that wants patient
/// redelivery — and reusing it as the connect timeout would make an
/// unreachable broker hang for that long before anyone is told.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// How the publisher authenticates to the broker.
///
/// The four forms are mutually exclusive: a configuration naming two is a
/// [`ConfigError::ConflictingAuth`] rather than a silent precedence rule,
/// because "which credential did production actually use" is the question
/// an operator cannot afford to guess.
#[derive(Clone, PartialEq, Eq, Default)]
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

/// Hand-written so a `tracing::info!(?config)` cannot write the broker
/// credential to a log file. The user name survives because it names an
/// account rather than proving one; the creds-file PATH does not, because
/// it is the credential's address and printing it tells a reader of the
/// log where to go looking.
impl std::fmt::Debug for NatsAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::CredsFile(_) => f.write_str("CredsFile(<redacted>)"),
            Self::UserPassword { user, .. } => f
                .debug_struct("UserPassword")
                .field("user", user)
                .field("password", &REDACTED)
                .finish(),
            Self::Token(_) => f.write_str("Token(<redacted>)"),
        }
    }
}

/// What every redacted field prints instead of its value.
const REDACTED: &str = "<redacted>";

/// A broker URL with any `user:password@` userinfo removed.
///
/// `nats://user:secret@host:4222` is a documented NATS form, so the URL is
/// as much a credential as [`NatsAuth`] is.
fn redacted_url(url: &str) -> String {
    url.split(',')
        .map(|server| match (server.find("//"), server.rfind('@')) {
            (Some(scheme), Some(at)) if at > scheme => {
                format!("{}//{REDACTED}@{}", &server[..scheme], &server[at + 1..])
            }
            _ => server.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Everything the publisher needs to reach one broker and publish one source
/// subject. Stream topology is provisioned and owned by the deployment.
#[derive(Clone)]
pub struct NatsPublisherConfig {
    /// `nats://…`; a comma-separated list names several servers of one
    /// cluster.
    pub url: String,
    pub auth: NatsAuth,
    /// Validated: dot-separated tokens of `[A-Za-z0-9_-]`, no wildcards. This
    /// is the source subject prefix; stream transforms and partitions are
    /// deployment-owned and may rewrite it after publication.
    pub subject_prefix: String,
    pub publisher_id: PublisherId,
    pub batch: NonZeroU32,
    pub lease: Duration,
    pub poll_interval: Duration,
    pub publish_timeout: Duration,
}

/// Hand-written, and NOT `finish_non_exhaustive`: every field is printed
/// except the two that carry credentials. The derive would have printed
/// the URL's userinfo verbatim, and this type is re-exported from the
/// facade for hosts to log.
impl std::fmt::Debug for NatsPublisherConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NatsPublisherConfig")
            .field("url", &redacted_url(&self.url))
            .field("auth", &self.auth)
            .field("subject_prefix", &self.subject_prefix)
            .field("publisher_id", &self.publisher_id)
            .field("batch", &self.batch)
            .field("lease", &self.lease)
            .field("poll_interval", &self.poll_interval)
            .field("publish_timeout", &self.publish_timeout)
            .finish()
    }
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
            subject_prefix: DEFAULT_SUBJECT_PREFIX.to_owned(),
            publisher_id: default_publisher_id(&proxima_core::process_env)?,
            batch: NonZeroU32::new(DEFAULT_BATCH).expect("64 is not zero"),
            lease: DEFAULT_LEASE,
            poll_interval: DEFAULT_POLL_INTERVAL,
            publish_timeout: DEFAULT_PUBLISH_TIMEOUT,
        })
    }

    /// Read the block from an injected lookup.
    ///
    /// `Ok(None)` when [`ENV_URL`] is unset — an optional lane a host did
    /// not configure is not a misconfigured host.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] for an invalid subject prefix, conflicting or
    /// incomplete auth, or a malformed number.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, ConfigError> {
        let lookup = |key: &str| proxima_core::env_value(&lookup, key);
        let Some(url) = lookup(ENV_URL) else {
            return Ok(None);
        };
        let mut config = Self::new(url)?;
        if let Some(raw) = lookup(ENV_SUBJECT_PREFIX) {
            config.subject_prefix = validated_subject_prefix(&raw)?;
        }
        // Both the explicit key and the default's `HOSTNAME` come out of the
        // INJECTED lookup: a host that hands us an environment must not get
        // a publisher label assembled from the process's own.
        config.publisher_id = match lookup(ENV_PUBLISHER_ID) {
            Some(raw) => {
                PublisherId::new(raw).map_err(|error| ConfigError::PublisherId { error })?
            }
            None => default_publisher_id(&lookup)?,
        };
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
}

/// Everything the reference consumer needs to bind one durable pull
/// consumer.
#[derive(Clone)]
pub struct NatsConsumerConfig {
    pub url: String,
    pub auth: NatsAuth,
    pub stream: String,
    /// Durable name. Two processes sharing it share the work; two
    /// deployments sharing it by accident share the acknowledgements.
    pub durable_name: String,
    /// How long THIS process waits for the broker: connect, and one
    /// `JetStream` API round trip. This is bounded independently of the
    /// broker's consumer ACK policy and is not read from the environment —
    /// an unreachable broker must be reported in seconds however patient the
    /// sink is.
    pub request_timeout: Duration,
}

/// Hand-written for the same reason as [`NatsPublisherConfig`]'s.
impl std::fmt::Debug for NatsConsumerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NatsConsumerConfig")
            .field("url", &redacted_url(&self.url))
            .field("auth", &self.auth)
            .field("stream", &self.stream)
            .field("durable_name", &self.durable_name)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

impl NatsConsumerConfig {
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            auth: NatsAuth::None,
            stream: DEFAULT_CONSUMER_STREAM.to_owned(),
            durable_name: DEFAULT_CONSUMER_NAME.to_owned(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    /// Read the consumer half of the block from an injected lookup.
    ///
    /// `Ok(None)` when [`ENV_URL`] is unset, matching the publisher.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] for an invalid binding name or conflicting auth.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, ConfigError> {
        let lookup = |key: &str| proxima_core::env_value(&lookup, key);
        let Some(url) = lookup(ENV_URL) else {
            return Ok(None);
        };
        let mut config = Self::new(url);
        if let Some(raw) = lookup(ENV_CONSUMER_STREAM) {
            config.stream = validated_name(ENV_CONSUMER_STREAM, &raw)?;
        }
        if let Some(raw) = lookup(ENV_CONSUMER_NAME) {
            config.durable_name = validated_name(ENV_CONSUMER_NAME, &raw)?;
        }
        config.auth = auth_from_lookup(&lookup)?;
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
}

/// Why a `PROXIMA_NATS_*` block was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error(
        "PROXIMA_NATS_SUBJECT_PREFIX must be dot-separated tokens of [A-Za-z0-9_-] \
         and must not carry the `*` or `>` wildcards, got {value:?}"
    )]
    InvalidSubjectPrefix { value: String },
    #[error("{key} must not contain whitespace, `.`, `*` or `>`, got {value:?}")]
    InvalidName { key: &'static str, value: String },
    #[error("{key} must be a positive integer, got {value:?}")]
    Number { key: &'static str, value: String },
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

/// The default publisher label: `<hostname>:<pid>`, read through `lookup`.
///
/// `HOSTNAME` rather than a syscall: reading it needs no new dependency and
/// no `unsafe`, and this value is an operator label rather than an
/// identity — the fencing token is the claim token. A host that wants a
/// stable label sets [`ENV_PUBLISHER_ID`].
///
/// It takes the lookup rather than reading the process environment so that
/// an injected environment produces the same answer everywhere in this
/// module. Sanitisation is by BYTES, not characters, so that no value of
/// `HOSTNAME` can push the label past [`PublisherId`]'s 128-byte bound and
/// turn a hostile environment variable into a boot failure.
fn default_publisher_id(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<PublisherId, ConfigError> {
    let host = proxima_core::env_value(lookup, "HOSTNAME").unwrap_or_default();
    let mut label = String::with_capacity(96);
    for c in host.chars().filter(|c| !c.is_control()) {
        if label.len() + c.len_utf8() > 96 {
            break;
        }
        label.push(c);
    }
    if label.trim().is_empty() {
        label.clear();
        label.push_str("proxima");
    }
    PublisherId::new(format!("{label}:{}", std::process::id()))
        .map_err(|error| ConfigError::PublisherId { error })
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
fn validated_name(key: &'static str, raw: &str) -> Result<String, ConfigError> {
    if raw.is_empty()
        || raw
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '.' | '*' | '>'))
    {
        return Err(ConfigError::InvalidName {
            key,
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

/// The subject one captured event is published on.
///
/// `<prefix>.<owner_kind>.<owner_uuid>.<type_token>`. Owner routing lives
/// in the subject so a NATS account can restrict a consumer to one owner's
/// events with a subject permission, without the broker parsing the
/// payload.
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

/// The subject token for one event type: `_xx` percent-style escaping.
///
/// Every byte outside `[A-Za-z0-9-]` — including `_` itself — becomes `_`
/// followed by two lowercase hex digits, so `probe/listenable-v1` is
/// `probe_2flistenable-v1` and `probe_listenable-v1` is
/// `probe_5flistenable-v1`.
///
/// The escape of `_` is what makes the mapping INJECTIVE, and injective is
/// a security property here rather than a nicety: docs/18 documents subject
/// permissions as the way a NATS account is restricted to a set of event
/// types, and two schema ids sharing a token would silently widen such a
/// grant to a type the operator never named.
#[must_use]
pub fn type_token(event_type: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut token = String::with_capacity(event_type.len());
    for byte in event_type.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' {
            token.push(char::from(byte));
        } else {
            token.push('_');
            token.push(char::from(HEX[usize::from(byte >> 4)]));
            token.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    if token.is_empty() {
        // Unreachable through a registered schema id, and still not `""`:
        // an empty NATS subject token is not a subject. A bare `_` is
        // never the image of a non-empty input, since every escape carries
        // its two hex digits.
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
            NatsPublisherConfig::from_lookup(env(&[(ENV_CONSUMER_STREAM, "OTHER")]))
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
    fn the_numbers_must_be_positive() {
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
    }

    #[test]
    fn the_defaults_are_the_documented_ones() {
        let config = NatsPublisherConfig::from_lookup(env(&[(ENV_URL, "nats://127.0.0.1:4222")]))
            .expect("parses")
            .expect("url set");
        assert_eq!(config.subject_prefix, DEFAULT_SUBJECT_PREFIX);
        assert_eq!(config.batch.get(), DEFAULT_BATCH);
        assert_eq!(config.lease, DEFAULT_LEASE);
        assert_eq!(config.poll_interval, DEFAULT_POLL_INTERVAL);
        assert_eq!(config.publish_timeout, DEFAULT_PUBLISH_TIMEOUT);
        assert_eq!(config.auth, NatsAuth::None);

        let consumer = NatsConsumerConfig::from_lookup(env(&[(ENV_URL, "nats://127.0.0.1:4222")]))
            .expect("parses")
            .expect("url set");
        assert_eq!(consumer.durable_name, DEFAULT_CONSUMER_NAME);
        assert_eq!(consumer.stream, DEFAULT_CONSUMER_STREAM);
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
    fn the_subject_carries_the_owner_and_an_escaped_type() {
        let owner = uuid::Uuid::nil();
        assert_eq!(
            subject_for("proxima.fact", "personal", owner, "acme/build.finished-v1"),
            format!("proxima.fact.personal.{owner}.acme_2fbuild_2efinished-v1")
        );
        assert_eq!(type_token(""), "_");
        assert_eq!(type_token("a.b*c>d e"), "a_2eb_2ac_3ed_20e");
        // No wildcard and no separator survives into the subject.
        for token in [
            type_token("a.b*c>d e"),
            type_token("acme/build.finished-v1"),
        ] {
            assert!(
                !token.contains(['.', '*', '>', ' ']),
                "{token} would not be one subject token"
            );
        }
    }

    #[test]
    fn two_distinct_schema_ids_never_share_a_subject_token() {
        // The pair the old collapse-to-underscore mapping conflated, plus
        // every neighbouring shape a flavor could register.
        let ids = [
            "probe/listenable-v1",
            "probe_listenable-v1",
            "probe.listenable-v1",
            "probe listenable-v1",
            "probe/listenable_v1",
            "probe//listenable-v1",
            "probe_2flistenable-v1",
            "PROBE/listenable-v1",
            "",
            "_",
        ];
        let mut tokens: Vec<String> = ids.iter().map(|id| type_token(id)).collect();
        tokens.sort();
        let before = tokens.len();
        tokens.dedup();
        assert_eq!(
            tokens.len(),
            before,
            "the type token must be injective: a collision widens a subject-scoped \
             NATS permission to a schema the operator never granted"
        );
    }

    #[test]
    fn the_debug_of_a_config_carries_no_credential() {
        let mut config = NatsPublisherConfig::from_lookup(env(&[
            (ENV_URL, "nats://someone:hunter2@broker.internal:4222"),
            (ENV_TOKEN, "s3cr3t-token"),
        ]))
        .expect("parses")
        .expect("url set");
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("s3cr3t-token"), "{rendered}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(
            rendered.contains("broker.internal:4222"),
            "the host is not the secret: {rendered}"
        );

        config.auth = NatsAuth::UserPassword {
            user: "someone".to_owned(),
            password: "hunter2".to_owned(),
        };
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("someone"), "the user name is a label");

        config.auth = NatsAuth::CredsFile(PathBuf::from("/run/secrets/nats.creds"));
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("nats.creds"), "{rendered}");

        let consumer = NatsConsumerConfig::from_lookup(env(&[
            (ENV_URL, "nats://someone:hunter2@broker.internal:4222"),
            (ENV_PASSWORD, "hunter2"),
            (ENV_USER, "someone"),
        ]))
        .expect("parses")
        .expect("url set");
        let rendered = format!("{consumer:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert_eq!(consumer.request_timeout, DEFAULT_REQUEST_TIMEOUT);
    }

    #[test]
    fn the_default_publisher_id_comes_from_the_injected_environment() {
        let config = NatsPublisherConfig::from_lookup(env(&[
            (ENV_URL, "nats://x:4222"),
            ("HOSTNAME", "injected-host"),
        ]))
        .expect("parses")
        .expect("url set");
        assert!(
            config.publisher_id.as_str().starts_with("injected-host:"),
            "the injected environment must be the one that names the publisher, got {}",
            config.publisher_id
        );

        // An explicit id still wins, and an environment naming no host at
        // all is not a boot failure.
        let config = NatsPublisherConfig::from_lookup(env(&[
            (ENV_URL, "nats://x:4222"),
            ("HOSTNAME", "injected-host"),
            (ENV_PUBLISHER_ID, "chosen"),
        ]))
        .expect("parses")
        .expect("url set");
        assert_eq!(config.publisher_id.as_str(), "chosen");

        let config = NatsPublisherConfig::from_lookup(env(&[(ENV_URL, "nats://x:4222")]))
            .expect("parses")
            .expect("url set");
        assert!(config.publisher_id.as_str().starts_with("proxima:"));
    }

    #[test]
    fn a_hostile_hostname_cannot_break_the_publisher_label() {
        let long = "h".repeat(300);
        let id = default_publisher_id(&env(&[("HOSTNAME", long.as_str())])).expect("bounded");
        assert!(id.as_str().len() <= 128, "{}", id.as_str());
        let wide = "ü".repeat(200);
        let id = default_publisher_id(&env(&[("HOSTNAME", wide.as_str())])).expect("bounded");
        assert!(id.as_str().len() <= 128, "{}", id.as_str());
    }
}
