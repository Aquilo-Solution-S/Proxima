//! NATS `JetStream` publisher for Proxima's transactional Fact outbox
//! (issue #305, docs/18).
//!
//! Optional adapter. `proxima-core` knows nothing about NATS: it captures
//! a `CloudEvents` envelope inside the Fact's own transaction and exposes
//! a host-only [`PublicationOutboxPort`]. This crate is one implementation
//! of the other half — claim the captured bytes, put them on a broker,
//! record the receipt — and a reference consumer showing what a durable
//! sink owes the stream in return.
//!
//! ```text
//! publication_outbox (Postgres)          NATS JetStream           sink
//!   claim(lease) ──► publish(bytes) ──► PubAck ──► mark_published
//!                          │                            (fenced on the claim)
//!                          └──► durable pull consumer ──► DurableIntake
//!                                                          └─ ACK only after
//!                                                             a durable outcome
//! ```
//!
//! Three properties hold that nothing here has to be careful about:
//!
//! - **The bytes are never rebuilt.** The publisher ships
//!   [`ClaimedPublication::envelope`] verbatim, so an upgraded flavor
//!   cannot reinterpret an event captured by an older one.
//! - **Nothing deletes.** Claims lease, leases expire, releases return.
//!   Compliance erasure is the only path a record leaves the outbox by.
//! - **Both sides are at-least-once.** `Nats-Msg-Id` deduplicates inside
//!   the broker's window; the sink deduplicates on the `CloudEvents` id
//!   beyond it.
//!
//! [`PublicationOutboxPort`]: proxima_core::storage_ports::publication::PublicationOutboxPort
//! [`ClaimedPublication::envelope`]: proxima_core::storage_ports::publication::ClaimedPublication::envelope

/// Build an [`InboxPrefix`] from a literal, proving the token rule at build
/// time.
///
/// The assertion and the construction are one expansion, so the prefix a
/// binary ships cannot be invalid: an unparseable literal is a build error,
/// not a startup panic. Mirrors what `proxima_flavor!` does for schema id
/// prefixes.
#[macro_export]
macro_rules! const_inbox_prefix {
    ($value:expr $(,)?) => {{
        const _: () = ::std::assert!(
            $crate::config::inbox_prefix_is_valid($value),
            ::std::concat!(
                "inbox prefix ",
                ::std::stringify!($value),
                " is not a dot-separated [A-Za-z0-9_-] token",
            ),
        );
        $crate::config::InboxPrefix::from_proved_const($value)
    }};
}

pub mod cleaner;
pub mod config;
pub mod consumer;
pub mod publisher;

use std::time::Duration;

pub use cleaner::{
    COPY_CLEANER_INBOX_PREFIX, COPY_CLEANER_STREAM, COPY_CLEANER_SUBJECT_PREFIX,
    CleanerConfigError, CopyCleanerConnectError, CopyCleanerConnectionState, CopyCleanerFailure,
    CopyCleanerHealth, CopyCleanerHealthReader, CopyCleanerScanState, CopyCleanerSliceReport,
    CopyCleanerTaskState, JetStreamCopyCleaner, JetStreamCopyCleanerConfig,
    spawn_supervised_copy_cleaner,
};
pub use config::{
    ConfigError, InboxPrefix, NatsAuth, NatsConsumerConfig, NatsPublisherConfig, ParsedSubject,
    SubjectParseError, inbox_prefix_is_valid, parse_subject, subject_for, type_token,
};
pub use consumer::{
    AckAction, AckAlwaysHook, AckHook, CloudEventEnvelope, ConsumeReport, ConsumerConnectionState,
    ConsumerError, ConsumerHealth, ConsumerHealthReader, ConsumerPassState, ConsumerTaskState,
    DurableIntake, Intake, IntakeError, ReceivedEvent, ReferenceConsumer,
};
pub use publisher::{
    CONTENT_TYPE_CLOUDEVENTS, ContinueHook, DrainReport, DrainSummary, HEADER_CONTENT_TYPE,
    HEADER_MSG_ID, HEADER_ORIGIN_SCOPE, HEADER_SCHEMA, HookAction, JetStreamPublisher, PublishHook,
    PublisherConnectionState, PublisherDrainState, PublisherError, PublisherHealth,
    PublisherHealthReader, PublisherTaskState, SupervisedPublisher, spawn_supervised,
};

/// Open one client connection under the configured credentials.
///
/// `retry_on_initial_connect` is deliberately NOT set: a host that names an
/// unreachable broker learns so from `connect`, and the retry policy lives
/// in the caller's boot loop where it can be logged and given up on. A
/// client that retries forever inside `connect` turns a typo into a hang.
async fn connect_client(
    url: &str,
    auth: &NatsAuth,
    inbox_prefix: Option<&InboxPrefix>,
    timeout: Duration,
) -> Result<async_nats::Client, async_nats::ConnectError> {
    let servers: Vec<String> = url
        .split(',')
        .map(str::trim)
        .filter(|server| !server.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let options = async_nats::ConnectOptions::new()
        .name("proxima-outbox")
        .connection_timeout(timeout)
        .request_timeout(Some(timeout));
    let options = match auth {
        NatsAuth::None => options,
        NatsAuth::CredsFile(path) => options.credentials_file(path).await.map_err(|error| {
            async_nats::ConnectError::with_source(
                async_nats::ConnectErrorKind::Authentication,
                error,
            )
        })?,
        NatsAuth::UserPassword { user, password } => {
            options.user_and_password(user.clone(), password.clone())
        }
        NatsAuth::Token(token) => options.token(token.clone()),
    };
    let options = match inbox_prefix {
        Some(prefix) => options.custom_inbox_prefix(prefix.as_str()),
        None => options,
    };
    options.connect(servers).await
}
