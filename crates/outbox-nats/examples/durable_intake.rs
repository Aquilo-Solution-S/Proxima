//! A small PostgreSQL-backed durable sink for Proxima's Fact stream.
//!
//! The schema in `durable_intake.sql` belongs to this reference example. A
//! deployment provisions it once, then supplies `PROXIMA_INTAKE_DATABASE_URL`.
//! The application only binds an existing NATS stream and durable consumer;
//! it never creates broker topology.
//!
//! The sink commits the exact delivered bytes, digest, original id, delivery
//! facts, decision timestamp, and terminal outcome before returning `Ok`.
//! `PostgreSQL` constraints own deduplication: a unique partial index preserves
//! the first decision for each event id, while a later payload is a durable
//! rejected conflict.

use std::env;
use std::fmt;
use std::sync::Arc;

use proxima_outbox_nats::{
    DurableIntake, Intake, IntakeError, NatsConsumerConfig, ReceivedEvent, ReferenceConsumer,
};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;

const ENV_DATABASE_URL: &str = "PROXIMA_INTAKE_DATABASE_URL";

const INSERT_PRIMARY: &str = "
    INSERT INTO proxima_durable_intake.decisions (
        event_id, payload_digest, raw_payload, subject, stream_sequence,
        delivered_count, decision_at, outcome, reason, is_primary,
        original_payload_digest
    ) VALUES ($1, $2, $3, $4, $5, $6, now(), $7, $8, true, NULL)
    ON CONFLICT DO NOTHING
    RETURNING outcome, reason
";

const SELECT_PRIMARY: &str = "
    SELECT payload_digest, outcome, reason
    FROM proxima_durable_intake.decisions
    WHERE event_id = $1 AND is_primary
";

const INSERT_CONFLICT: &str = "
    INSERT INTO proxima_durable_intake.decisions (
        event_id, payload_digest, raw_payload, subject, stream_sequence,
        delivered_count, decision_at, outcome, reason, is_primary,
        original_payload_digest
    ) VALUES ($1, $2, $3, $4, $5, $6, now(), 'rejected', $7, false, $8)
    ON CONFLICT DO NOTHING
    RETURNING outcome, reason
";

const SELECT_DECISION: &str = "
    SELECT outcome, reason
    FROM proxima_durable_intake.decisions
    WHERE event_id = $1 AND payload_digest = $2
";

#[derive(Debug, Clone, sqlx::FromRow)]
struct StoredDecision {
    outcome: String,
    reason: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct StoredPrimary {
    payload_digest: String,
    outcome: String,
    reason: Option<String>,
}

/// The reference sink. Independent instances share only `PostgreSQL` state.
#[derive(Clone)]
pub struct PostgresIntake {
    pool: PgPool,
}

impl fmt::Debug for PostgresIntake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresIntake").finish_non_exhaustive()
    }
}

impl PostgresIntake {
    async fn connect(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    async fn decide_and_record(&self, event: &ReceivedEvent) -> Result<Intake, sqlx::Error> {
        let digest_text = blake3::hash(&event.raw).to_hex().to_string();
        let stream_sequence = i64::try_from(event.stream_sequence).map_err(|_| {
            sqlx::Error::Protocol("stream sequence does not fit PostgreSQL BIGINT".to_owned())
        })?;
        let delivered_count = i64::try_from(event.delivered_count).map_err(|_| {
            sqlx::Error::Protocol("delivery count does not fit PostgreSQL BIGINT".to_owned())
        })?;
        let mut transaction = self.pool.begin().await?;

        // Read before validation: a terminal historical decision is never
        // reinterpreted by a later release.
        if let Some(saved) = sqlx::query_as::<_, StoredDecision>(SELECT_DECISION)
            .bind(&event.id)
            .bind(&digest_text)
            .fetch_optional(&mut *transaction)
            .await?
        {
            let outcome = saved.into_intake()?;
            transaction.commit().await?;
            return Ok(outcome);
        }

        let outcome = validation_outcome(event);
        let (outcome_name, reason) = match &outcome {
            Intake::Accepted => ("accepted", None),
            Intake::Rejected { reason } => ("rejected", Some(reason.as_str())),
        };
        if let Some(inserted) = sqlx::query_as::<_, StoredDecision>(INSERT_PRIMARY)
            .bind(&event.id)
            .bind(&digest_text)
            .bind(event.raw.as_ref())
            .bind(&event.subject)
            .bind(stream_sequence)
            .bind(delivered_count)
            .bind(outcome_name)
            .bind(reason)
            .fetch_optional(&mut *transaction)
            .await?
        {
            let outcome = inserted.into_intake()?;
            transaction.commit().await?;
            return Ok(outcome);
        }

        // Another sink won the primary insert. The unique event-id index has
        // made that winner visible before this statement runs.
        let primary = sqlx::query_as::<_, StoredPrimary>(SELECT_PRIMARY)
            .bind(&event.id)
            .fetch_one(&mut *transaction)
            .await?;
        if primary.payload_digest == digest_text {
            let outcome = StoredDecision {
                outcome: primary.outcome,
                reason: primary.reason,
            }
            .into_intake()?;
            transaction.commit().await?;
            return Ok(outcome);
        }

        let conflict_reason = format!(
            "conflicting payload for event id {}: recorded digest {}, received {}",
            event.id, primary.payload_digest, digest_text
        );
        let saved = sqlx::query_as::<_, StoredDecision>(INSERT_CONFLICT)
            .bind(&event.id)
            .bind(&digest_text)
            .bind(event.raw.as_ref())
            .bind(&event.subject)
            .bind(stream_sequence)
            .bind(delivered_count)
            .bind(&conflict_reason)
            .bind(&primary.payload_digest)
            .fetch_optional(&mut *transaction)
            .await?;
        let outcome = match saved {
            Some(saved) => saved.into_intake()?,
            None => sqlx::query_as::<_, StoredDecision>(SELECT_DECISION)
                .bind(&event.id)
                .bind(&digest_text)
                .fetch_one(&mut *transaction)
                .await?
                .into_intake()?,
        };
        transaction.commit().await?;
        Ok(outcome)
    }
}

fn validation_outcome(event: &ReceivedEvent) -> Intake {
    if event.envelope.specversion == "1.0" {
        Intake::Accepted
    } else {
        Intake::Rejected {
            reason: format!(
                "unsupported CloudEvents specversion {:?}",
                event.envelope.specversion
            ),
        }
    }
}

impl StoredDecision {
    fn into_intake(self) -> Result<Intake, sqlx::Error> {
        match (self.outcome.as_str(), self.reason) {
            ("accepted", None) => Ok(Intake::Accepted),
            ("rejected", Some(reason)) => Ok(Intake::Rejected { reason }),
            _ => Err(sqlx::Error::Protocol(
                "durable intake schema returned an invalid outcome".to_owned(),
            )),
        }
    }
}

#[async_trait::async_trait]
impl DurableIntake for PostgresIntake {
    async fn accept(&self, event: &ReceivedEvent) -> Result<Intake, IntakeError> {
        self.decide_and_record(event)
            .await
            .map_err(|error| IntakeError::new(format!("could not commit durable intake: {error}")))
    }
}

fn tracing_line(message: &str) {
    println!("durable_intake: {message}");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(config) = NatsConsumerConfig::from_env()? else {
        return Err("PROXIMA_NATS_URL is unset; nothing to consume".into());
    };
    let database_url = env::var(ENV_DATABASE_URL)
        .map_err(|_| format!("{ENV_DATABASE_URL} is unset; provision the example schema first"))?;
    if database_url.trim().is_empty() {
        return Err(format!("{ENV_DATABASE_URL} must not be empty").into());
    }
    let intake = Arc::new(PostgresIntake::connect(&database_url).await?);
    tracing_line(&format!(
        "consuming stream {} with durable {} into the provisioned PostgreSQL intake schema",
        config.stream, config.durable_name
    ));
    let consumer = ReferenceConsumer::connect(config, intake).await?;
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            stop.cancel();
        }
    });
    consumer.run(cancel).await;
    tracing_line("stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_nats::jetstream;
    use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy};
    use bytes::Bytes;
    use proxima_outbox_nats::{AckAction, AckHook, CloudEventEnvelope};
    use proxima_pg_testkit::{DbGuard, create_db, db_url};
    use sqlx::postgres::PgPoolOptions;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use uuid::Uuid;

    struct DatabaseFixture {
        intake: PostgresIntake,
        pool: PgPool,
        url: String,
        _guard: DbGuard,
    }

    impl DatabaseFixture {
        async fn new() -> Self {
            let name = format!("intake_{}", Uuid::now_v7().simple());
            create_db(&name)
                .await
                .expect("test PostgreSQL is available");
            let guard = DbGuard::adopt(name.clone());
            let url = db_url(&name);
            let pool = PgPoolOptions::new()
                .max_connections(8)
                .connect(&url)
                .await
                .expect("the fresh test database connects");
            sqlx::raw_sql(include_str!("durable_intake.sql"))
                .execute(&pool)
                .await
                .expect("the example schema provisions");
            let intake = PostgresIntake::connect(&url)
                .await
                .expect("the intake pool connects");
            Self {
                intake,
                pool,
                url,
                _guard: guard,
            }
        }

        async fn count(&self) -> i64 {
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_durable_intake.decisions")
                .fetch_one(&self.pool)
                .await
                .expect("the decision count answers")
        }
    }

    fn event(id: &str, raw: &[u8], specversion: &str) -> ReceivedEvent {
        ReceivedEvent {
            id: id.to_owned(),
            subject: "proxima.fact.test".to_owned(),
            stream_sequence: 7,
            delivered_count: 1,
            raw: Bytes::copy_from_slice(raw),
            envelope: CloudEventEnvelope {
                specversion: specversion.to_owned(),
                id: id.to_owned(),
                source: "urn:test".to_owned(),
                event_type: "test/event".to_owned(),
                datacontenttype: None,
                dataschema: None,
                time: None,
                proximaowner: None,
                proximamodel: None,
                data: serde_json::json!({}),
            },
        }
    }

    #[tokio::test]
    async fn rejection_persists_raw_reason_digest_and_decision_time() {
        let database = DatabaseFixture::new().await;
        let before: time::OffsetDateTime = sqlx::query_scalar("SELECT now()")
            .fetch_one(&database.pool)
            .await
            .expect("the database clock answers");
        let raw = b"not-json-\0-with-exact-bytes";
        let outcome = database
            .intake
            .accept(&event("F:rejected", raw, "0.9"))
            .await
            .expect("a rejection is durable");
        let after: time::OffsetDateTime = sqlx::query_scalar("SELECT now()")
            .fetch_one(&database.pool)
            .await
            .expect("the database clock answers");
        assert_eq!(
            outcome,
            Intake::Rejected {
                reason: "unsupported CloudEvents specversion \"0.9\"".to_owned()
            }
        );
        let row: (
            Vec<u8>,
            String,
            time::OffsetDateTime,
            String,
            Option<String>,
            bool,
        ) = sqlx::query_as(
            "SELECT raw_payload, payload_digest, decision_at, outcome, reason, is_primary
             FROM proxima_durable_intake.decisions WHERE event_id = 'F:rejected'",
        )
        .fetch_one(&database.pool)
        .await
        .expect("the rejected row is present");
        assert_eq!(row.0, raw);
        assert_eq!(row.1, blake3::hash(raw).to_hex().to_string());
        assert!(row.2 >= before && row.2 <= after);
        assert_eq!(row.3, "rejected");
        assert_eq!(
            row.4.as_deref(),
            Some("unsupported CloudEvents specversion \"0.9\"")
        );
        assert!(row.5);
    }

    #[tokio::test]
    async fn accepted_and_rejected_redelivery_survive_reopen_without_new_rows() {
        let database = DatabaseFixture::new().await;
        let accepted = event("F:accepted", b"accepted-bytes", "1.0");
        let rejected = event("F:rejected-again", b"rejected-bytes", "0.9");
        let accepted_outcome = database.intake.accept(&accepted).await.expect("accepted");
        let rejected_outcome = database.intake.accept(&rejected).await.expect("rejected");
        assert_eq!(accepted_outcome, Intake::Accepted);
        assert!(matches!(rejected_outcome, Intake::Rejected { .. }));
        assert_eq!(database.count().await, 2);
        let mut accepted_replay = accepted.clone();
        accepted_replay.envelope.specversion = "0.9".to_owned();
        let mut rejected_replay = rejected.clone();
        rejected_replay.envelope.specversion = "1.0".to_owned();
        assert_eq!(
            database
                .intake
                .accept(&accepted_replay)
                .await
                .expect("saved accepted"),
            Intake::Accepted
        );
        assert_eq!(
            database
                .intake
                .accept(&rejected_replay)
                .await
                .expect("saved rejected"),
            rejected_outcome
        );
        database.intake.pool.close().await;
        let reopened = PostgresIntake::connect(&database.url)
            .await
            .expect("reopen");
        assert_eq!(
            reopened
                .accept(&accepted_replay)
                .await
                .expect("reopened accepted"),
            Intake::Accepted
        );
        assert_eq!(
            reopened
                .accept(&rejected_replay)
                .await
                .expect("reopened rejected"),
            rejected_outcome
        );
        assert_eq!(database.count().await, 2);
    }

    #[tokio::test]
    async fn conflicting_payload_keeps_original_and_deduplicates_conflict() {
        let database = DatabaseFixture::new().await;
        let original = event("F:conflict", b"original-bytes", "1.0");
        let conflicting = event("F:conflict", b"different-bytes", "1.0");
        assert_eq!(
            database.intake.accept(&original).await.expect("original"),
            Intake::Accepted
        );
        let rejection = database
            .intake
            .accept(&conflicting)
            .await
            .expect("conflict rejection");
        assert!(
            matches!(rejection, Intake::Rejected { ref reason } if reason.contains("conflicting payload"))
        );
        assert_eq!(
            database
                .intake
                .accept(&conflicting)
                .await
                .expect("saved conflict"),
            rejection
        );
        database.intake.pool.close().await;
        let reopened = PostgresIntake::connect(&database.url)
            .await
            .expect("reopen conflict sink");
        assert_eq!(
            reopened
                .accept(&conflicting)
                .await
                .expect("reopened conflict"),
            rejection
        );
        let rows: Vec<(bool, Vec<u8>, Option<String>)> = sqlx::query_as(
            "SELECT is_primary, raw_payload, original_payload_digest
             FROM proxima_durable_intake.decisions WHERE event_id = 'F:conflict'
             ORDER BY is_primary DESC",
        )
        .fetch_all(&database.pool)
        .await
        .expect("the rows answer");
        assert_eq!(rows.len(), 2);
        assert!(rows[0].0);
        assert_eq!(rows[0].1, b"original-bytes");
        assert!(!rows[1].0);
        assert_eq!(rows[1].1, b"different-bytes");
        assert_eq!(
            rows[1].2.as_deref(),
            Some(blake3::hash(b"original-bytes").to_hex().as_str())
        );
    }

    #[tokio::test]
    async fn independent_sinks_concurrently_commit_one_primary_decision() {
        let database = DatabaseFixture::new().await;
        let left = PostgresIntake::connect(&database.url)
            .await
            .expect("left sink");
        let right = PostgresIntake::connect(&database.url)
            .await
            .expect("right sink");
        let event = event("F:concurrent", b"same-bytes", "1.0");
        let (left_outcome, right_outcome) = tokio::join!(left.accept(&event), right.accept(&event));
        assert_eq!(left_outcome.expect("left commits"), Intake::Accepted);
        assert_eq!(right_outcome.expect("right replays"), Intake::Accepted);
        assert_eq!(database.count().await, 1);
    }

    #[tokio::test]
    async fn independent_sinks_concurrently_record_one_conflict_without_overwrite() {
        let database = DatabaseFixture::new().await;
        let left = PostgresIntake::connect(&database.url)
            .await
            .expect("left sink");
        let right = PostgresIntake::connect(&database.url)
            .await
            .expect("right sink");
        let left_event = event("F:concurrent-conflict", b"left-bytes", "1.0");
        let right_event = event("F:concurrent-conflict", b"right-bytes", "1.0");
        let (left_outcome, right_outcome) =
            tokio::join!(left.accept(&left_event), right.accept(&right_event));
        let left_outcome = left_outcome.expect("left commits");
        let right_outcome = right_outcome.expect("right commits conflict");
        assert!(
            matches!(
                (&left_outcome, &right_outcome),
                (Intake::Accepted, Intake::Rejected { .. })
                    | (Intake::Rejected { .. }, Intake::Accepted)
            ),
            "one payload must win and the other must be a durable conflict: {left_outcome:?} / {right_outcome:?}"
        );
        assert_eq!(database.count().await, 2);
    }

    #[tokio::test]
    async fn persistence_failure_returns_error_without_acceptance() {
        let database = DatabaseFixture::new().await;
        let verifier = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database.url)
            .await
            .expect("verifier connects");
        database.intake.pool.close().await;
        let result = database
            .intake
            .accept(&event("F:failed", b"not-committed", "1.0"))
            .await;
        assert!(result.is_err(), "closed storage must not report acceptance");
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_durable_intake.decisions WHERE event_id = 'F:failed'",
        ).fetch_one(&verifier).await.expect("verifier query answers");
        assert_eq!(count, 0);
        verifier.close().await;
    }

    #[derive(Debug)]
    struct DropFirstAck {
        remaining: AtomicBool,
    }

    #[async_trait::async_trait]
    impl AckHook for DropFirstAck {
        async fn before_ack(&self, _event: &ReceivedEvent, _outcome: &Intake) -> AckAction {
            if self.remaining.swap(false, Ordering::AcqRel) {
                AckAction::DropAck
            } else {
                AckAction::Continue
            }
        }
    }

    struct BrokerFixture {
        url: String,
        stream: String,
        durable: String,
        subject: String,
    }

    impl BrokerFixture {
        async fn new(test: &str) -> Option<Self> {
            let url = match env::var("PROXIMA_TEST_NATS_URL") {
                Ok(url) if !url.trim().is_empty() => url,
                _ => {
                    assert_ne!(
                        env::var("CI").as_deref(),
                        Ok("true"),
                        "PROXIMA_TEST_NATS_URL required under CI=true (test {test})"
                    );
                    eprintln!("skipping {test}: PROXIMA_TEST_NATS_URL is unset");
                    return None;
                }
            };
            let token = Uuid::now_v7().simple().to_string();
            let stream = format!("DI_{token}");
            let durable = format!("di_{token}");
            let subject = format!("durable.intake.{token}");
            let client = admin_client(&url).await.expect("test NATS connects");
            let context = jetstream::new(client);
            context
                .create_stream(jetstream::stream::Config {
                    name: stream.clone(),
                    subjects: vec![subject.clone()],
                    storage: jetstream::stream::StorageType::File,
                    retention: jetstream::stream::RetentionPolicy::Limits,
                    discard: jetstream::stream::DiscardPolicy::New,
                    max_age: Duration::ZERO,
                    max_bytes: 1024 * 1024,
                    max_message_size: -1,
                    duplicate_window: Duration::from_millis(100),
                    ..jetstream::stream::Config::default()
                })
                .await
                .expect("test stream provisions");
            let stream_handle = context.get_stream(&stream).await.expect("stream reads");
            stream_handle
                .create_consumer(jetstream::consumer::pull::Config {
                    durable_name: Some(durable.clone()),
                    ack_policy: AckPolicy::Explicit,
                    ack_wait: Duration::from_millis(200),
                    max_deliver: -1,
                    deliver_policy: DeliverPolicy::All,
                    filter_subject: subject.clone(),
                    ..jetstream::consumer::pull::Config::default()
                })
                .await
                .expect("test durable provisions");
            Some(Self {
                url,
                stream,
                durable,
                subject,
            })
        }

        async fn publish(&self, id: &str, raw: Vec<u8>) {
            let client = admin_client(&self.url).await.expect("test NATS reconnects");
            let context = jetstream::new(client);
            let mut headers = async_nats::HeaderMap::new();
            headers.insert("Nats-Msg-Id", id);
            context
                .publish_with_headers(self.subject.clone(), headers, Bytes::from(raw))
                .await
                .expect("publish request accepts")
                .await
                .expect("publish is acknowledged");
        }

        async fn close(&self) {
            if let Ok(client) = admin_client(&self.url).await {
                let _ = jetstream::new(client).delete_stream(&self.stream).await;
            }
        }

        fn config(&self) -> NatsConsumerConfig {
            let mut config = NatsConsumerConfig::new(self.url.clone());
            config.stream.clone_from(&self.stream);
            config.durable_name.clone_from(&self.durable);
            if let (Ok(user), Ok(password)) = (
                env::var("PROXIMA_TEST_NATS_ADMIN_USER"),
                env::var("PROXIMA_TEST_NATS_ADMIN_PASSWORD"),
            ) {
                config.auth = proxima_outbox_nats::NatsAuth::UserPassword { user, password };
            }
            config
        }
    }

    async fn admin_client(url: &str) -> Result<async_nats::Client, async_nats::ConnectError> {
        let options = async_nats::ConnectOptions::new();
        let options = match (
            env::var("PROXIMA_TEST_NATS_ADMIN_USER"),
            env::var("PROXIMA_TEST_NATS_ADMIN_PASSWORD"),
        ) {
            (Ok(user), Ok(password)) => options.user_and_password(user, password),
            (Err(_), Err(_)) => options,
            _ => panic!("test NATS credentials must be set together"),
        };
        options.connect(url).await
    }

    fn cloud_event(id: &str, specversion: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "specversion": specversion, "id": id, "source": "urn:proxima:test",
            "type": "test/event", "data": {"synthetic": true}
        }))
        .expect("synthetic event serializes")
    }

    #[tokio::test]
    async fn reference_consumer_recreation_recovers_after_lost_ack() {
        let Some(broker) =
            BrokerFixture::new("reference_consumer_recreation_recovers_after_lost_ack").await
        else {
            return;
        };
        let database = DatabaseFixture::new().await;
        broker
            .publish("F:lost-ack", cloud_event("F:lost-ack", "1.0"))
            .await;
        let first = ReferenceConsumer::connect_with_hook(
            broker.config(),
            Arc::new(database.intake.clone()),
            Arc::new(DropFirstAck {
                remaining: AtomicBool::new(true),
            }),
        )
        .await
        .expect("consumer binds");
        let report = first
            .process_once(
                std::num::NonZeroU32::new(1).unwrap(),
                Duration::from_secs(1),
            )
            .await
            .expect("first pass");
        assert_eq!(report.accepted, 1);
        assert_eq!(report.unacked, 1);
        drop(first);
        database.intake.pool.close().await;
        let reopened = PostgresIntake::connect(&database.url)
            .await
            .expect("recreated intake reconnects");
        let restarted = ReferenceConsumer::connect(broker.config(), Arc::new(reopened))
            .await
            .expect("recreated consumer binds");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut redelivered = false;
        while tokio::time::Instant::now() < deadline {
            let report = restarted
                .process_once(
                    std::num::NonZeroU32::new(1).unwrap(),
                    Duration::from_millis(300),
                )
                .await
                .expect("redelivery pass");
            if report.accepted == 1 {
                redelivered = true;
                break;
            }
        }
        assert!(
            redelivered,
            "the committed event must be redelivered after the dropped ACK"
        );
        assert_eq!(database.count().await, 1);
        broker.close().await;
    }

    #[tokio::test]
    async fn reference_consumer_naks_storage_failure_then_redelivers() {
        let Some(broker) =
            BrokerFixture::new("reference_consumer_naks_storage_failure_then_redelivers").await
        else {
            return;
        };
        let database = DatabaseFixture::new().await;
        broker.publish("F:nak", cloud_event("F:nak", "1.0")).await;
        database.intake.pool.close().await;
        let consumer =
            ReferenceConsumer::connect(broker.config(), Arc::new(database.intake.clone()))
                .await
                .expect("consumer binds");
        let first = consumer
            .process_once(
                std::num::NonZeroU32::new(1).unwrap(),
                Duration::from_secs(1),
            )
            .await
            .expect("failed intake is handled by NAK");
        assert_eq!(first.deferred, 1);
        assert_eq!(database.count().await, 0);
        drop(consumer);
        let recovered = PostgresIntake::connect(&database.url)
            .await
            .expect("storage reconnects");
        let consumer = ReferenceConsumer::connect(broker.config(), Arc::new(recovered))
            .await
            .expect("consumer binds recovered storage");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut accepted = false;
        while tokio::time::Instant::now() < deadline {
            let report = consumer
                .process_once(
                    std::num::NonZeroU32::new(1).unwrap(),
                    Duration::from_millis(300),
                )
                .await
                .expect("redelivery pass");
            if report.accepted == 1 {
                accepted = true;
                break;
            }
        }
        assert!(accepted, "the NAKed event must reach recovered storage");
        assert_eq!(database.count().await, 1);
        broker.close().await;
    }
}
