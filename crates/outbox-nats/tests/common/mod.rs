//! Shared fixture for the `JetStream` end-to-end lane.
//!
//! Every test owns ONE stream and ONE database, both named after a fresh
//! UUID, and deletes both on the way out. Sharing either would make the
//! delivery assertions depend on test ordering, which is the one thing a
//! delivery test must not do.

use futures::FutureExt;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proxima_core::publication::{
    PublicationDraft, PublicationLimits, PublicationPlan, PublicationSource,
};
use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::storage_ports::publication::PublicationOutboxPort;
use proxima_core::test_fixtures::ListenableProbeV1;
use proxima_core::verbs::fact_ingest::{
    AuthorizedFactWrite, FactIngestOutcome, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::{
    AccessKind, FactIngestPort, FactPayload, Owner, OwnerRef, SchemaId, SchemaVersion, SourceId,
    StorageError, UserId,
};
use proxima_outbox_nats::{
    AckAction, AckHook, DurableIntake, Intake, IntakeError, NatsConsumerConfig,
    NatsPublisherConfig, ReceivedEvent,
};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::test_fixtures::fresh_pg;
use uuid::Uuid;

/// The broker every test in this lane talks to.
pub const ENV_NATS_URL: &str = "PROXIMA_TEST_NATS_URL";

/// Skip locally, fail under CI.
///
/// A skipped delivery test that silently passes in CI is worse than no
/// test: the lane exists precisely to prove the broker contract, so an
/// absent broker in CI is a failure, and an absent broker on a laptop is
/// not.
#[must_use]
pub fn nats_url_or_skip(test: &str) -> Option<String> {
    match std::env::var(ENV_NATS_URL) {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => {
            assert!(
                std::env::var("CI").as_deref() != Ok("true"),
                "{ENV_NATS_URL} required under CI=true (test {test})"
            );
            eprintln!("skipping {test}: {ENV_NATS_URL} is unset");
            None
        }
    }
}

/// One test's isolated world: a fresh database, a fresh stream name, and a
/// publisher configuration pointing at both.
pub struct Fixture {
    pub pg: PgStorage,
    pub db_name: String,
    pub owner: Owner,
    pub config: NatsPublisherConfig,
    pub url: String,
}

impl Fixture {
    /// Build the world. `prefix` names the database; the stream and the
    /// subject prefix are derived from a fresh UUID.
    pub async fn new(prefix: &str, url: String) -> Self {
        let (pg, db_name) = fresh_pg(prefix).await;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        register_owner(pg.pool_for_tests(), &owner).await;
        let token = Uuid::now_v7().simple().to_string();
        let mut config = NatsPublisherConfig::new(url.clone()).expect("a publisher id resolves");
        config.stream = format!("T_{token}");
        config.subject_prefix = format!("t_{token}");
        config.lease = Duration::from_secs(2);
        config.publish_timeout = Duration::from_secs(5);
        config.poll_interval = Duration::from_millis(100);
        Self {
            pg,
            db_name,
            owner,
            config,
            url,
        }
    }

    /// Adjust the publisher configuration before the world is used.
    #[must_use]
    pub fn tweak(mut self, edit: impl FnOnce(&mut NatsPublisherConfig)) -> Self {
        edit(&mut self.config);
        self
    }

    /// Run one test body against this world and ALWAYS tear it down.
    ///
    /// The catch/resume is not decoration: a panicking assertion that
    /// skipped `teardown` would leave a stream on a shared broker and a
    /// database on a shared server, and the next run would inherit them.
    /// The panic is re-raised afterwards, so a failing test still fails.
    pub async fn run(self, body: impl AsyncFnOnce(&Self)) {
        let outcome = std::panic::AssertUnwindSafe(body(&self))
            .catch_unwind()
            .await;
        self.teardown().await;
        if let Err(payload) = outcome {
            std::panic::resume_unwind(payload);
        }
    }

    /// The consumer half, bound to the same stream and subject prefix.
    #[must_use]
    pub fn consumer_config(&self) -> NatsConsumerConfig {
        let mut config = NatsConsumerConfig::new(self.url.clone());
        config.stream.clone_from(&self.config.stream);
        config
            .subject_prefix
            .clone_from(&self.config.subject_prefix);
        config.durable_name = format!("d_{}", self.config.stream.to_lowercase());
        config.ack_wait = Duration::from_secs(2);
        config
    }

    #[must_use]
    pub fn outbox(&self) -> Arc<dyn PublicationOutboxPort> {
        Arc::new(self.pg.clone())
    }

    /// Admit one listenable Fact, capturing its event.
    pub async fn capture(
        &self,
        note: &str,
        ingest_key: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        self.capture_as(self.owner, note, ingest_key).await
    }

    /// The same, for an explicit owner — the owner-binding test needs two.
    pub async fn capture_as(
        &self,
        owner: Owner,
        note: &str,
        ingest_key: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        self.capture_with_source(owner, source(), note, ingest_key)
            .await
    }

    /// The same, under an explicit producer identity — the reinterpretation
    /// test needs a capture made by a DIFFERENT release than the one that
    /// publishes it.
    pub async fn capture_with_source(
        &self,
        owner: Owner,
        source: PublicationSource,
        note: &str,
        ingest_key: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        let payload = ListenableProbeV1 {
            probe_id: Uuid::now_v7(),
            note: note.to_owned(),
        };
        let command = fact_command(ingest_key);
        let plan = PublicationPlan::new(
            PublicationDraft::new(
                ListenableProbeV1::schema_id(),
                SchemaVersion::new(ListenableProbeV1::SCHEMA_VERSION),
                source,
                owner,
                Some("trusted/runner".to_owned()),
                serde_json::to_value(&payload).expect("the probe serializes"),
            ),
            PublicationLimits::default(),
        );
        let authorized = AuthorizedFactWrite::new_for_tests(
            OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
            command,
            None,
            Vec::new(),
        )
        .with_publication_for_tests(plan);
        self.pg
            .ingest_fact_with_typed_sidecar(&authorized, None)
            .await
    }

    /// The stored envelope bytes for one captured Fact.
    pub async fn stored_envelope(&self, t: Uuid) -> Vec<u8> {
        sqlx::query_scalar("SELECT envelope FROM proxima_core.publication_outbox WHERE t = $1")
            .bind(t)
            .fetch_one(self.pg.pool_for_tests())
            .await
            .expect("the capture is there")
    }

    /// The lifecycle state of one captured record.
    pub async fn state(&self, t: Uuid) -> String {
        sqlx::query_scalar("SELECT state::text FROM proxima_core.publication_outbox WHERE t = $1")
            .bind(t)
            .fetch_one(self.pg.pool_for_tests())
            .await
            .expect("the capture is there")
    }

    /// Delete the stream and drop the database. Called by every test, on
    /// every path: a leaked stream outlives the run and a leaked database
    /// outlives the machine.
    pub async fn teardown(&self) {
        if let Ok(client) = async_nats::connect(&self.url).await {
            let context = async_nats::jetstream::new(client);
            let _ = context.delete_stream(&self.config.stream).await;
        }
        self.pg.pool_for_tests().close().await;
        let _ = proxima_pg_testkit::drop_db(&self.db_name).await;
    }
}

/// The producer identity this deployment is configured with.
#[must_use]
pub fn source() -> PublicationSource {
    PublicationSource::new("urn:proxima:outbox-nats-tests").expect("a URN is absolute")
}

async fn register_owner(pool: &sqlx::PgPool, owner: &Owner) {
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind)
         ON CONFLICT (owner_id) DO NOTHING",
    )
    .bind(owner.stored_owner_id())
    .bind(proxima_core::OwnerRefKind::of(owner).as_str())
    .execute(pool)
    .await
    .expect("owner registers");
}

fn fact_command(ingest_key: Option<&str>) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new(ListenableProbeV1::SCHEMA_ID.to_owned()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: ingest_key.map(|_| "probe/source".to_owned()),
        ingest_key: ingest_key.map(ToOwned::to_owned),
        payload: Vec::new(),
        rendered_text: Some("probe".to_owned()),
        lexical_language: None,
        receipt: ingest_key.map(|_| FactReceiptDraft {
            source_id: SourceId::new("probe/source"),
            observed_at: now,
            occurred_at: now,
        }),
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

/// A durable sink that records what it was handed, in order, and can be
/// told to reject or to fail.
#[derive(Debug, Default)]
pub struct RecordingIntake {
    seen: Mutex<Vec<Seen>>,
    reject: Mutex<Vec<String>>,
    fail: Mutex<Vec<String>>,
}

/// One durable intake outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub id: String,
    pub subject: String,
    pub delivered_count: u64,
    pub raw: Vec<u8>,
    pub outcome: SeenOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeenOutcome {
    Accepted,
    Rejected,
    Failed,
}

impl RecordingIntake {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Reject (durably) every event carrying this note.
    pub fn reject_note(&self, note: &str) {
        self.reject.lock().expect("lock").push(note.to_owned());
    }

    /// Fail to record an outcome for every event carrying this note.
    pub fn fail_note(&self, note: &str) {
        self.fail.lock().expect("lock").push(note.to_owned());
    }

    /// Stop failing: the sink recovered.
    pub fn clear_failures(&self) {
        self.fail.lock().expect("lock").clear();
    }

    #[must_use]
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("lock").clone()
    }

    /// Distinct `CloudEvents` ids this sink durably recorded — the
    /// deduplicated view a real sink keys on.
    #[must_use]
    pub fn distinct_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .seen()
            .into_iter()
            .filter(|seen| seen.outcome != SeenOutcome::Failed)
            .map(|seen| seen.id)
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

fn note_of(raw: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| {
            value
                .get("data")
                .and_then(|data| data.get("note"))
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_default()
}

#[async_trait::async_trait]
impl DurableIntake for RecordingIntake {
    async fn accept(&self, event: &ReceivedEvent) -> Result<Intake, IntakeError> {
        let note = note_of(&event.raw);
        let record = |outcome| {
            self.seen.lock().expect("lock").push(Seen {
                id: event.id.clone(),
                subject: event.subject.clone(),
                delivered_count: event.delivered_count,
                raw: event.raw.to_vec(),
                outcome,
            });
        };
        if self.fail.lock().expect("lock").contains(&note) {
            record(SeenOutcome::Failed);
            return Err(IntakeError::new("the sink could not commit")
                .retry_after(Duration::from_millis(200)));
        }
        if self.reject.lock().expect("lock").contains(&note) {
            record(SeenOutcome::Rejected);
            return Ok(Intake::Rejected {
                reason: "the sink refuses this note".to_owned(),
            });
        }
        record(SeenOutcome::Accepted);
        Ok(Intake::Accepted)
    }
}

/// An ack hook that drops the FIRST `n` acknowledgements — the "the intake
/// committed and the ACK never arrived" failure.
#[derive(Debug)]
pub struct DropFirstAcks {
    remaining: Mutex<u32>,
}

impl DropFirstAcks {
    #[must_use]
    pub fn new(n: u32) -> Arc<Self> {
        Arc::new(Self {
            remaining: Mutex::new(n),
        })
    }
}

#[async_trait::async_trait]
impl AckHook for DropFirstAcks {
    async fn before_ack(&self, _event: &ReceivedEvent, _outcome: &Intake) -> AckAction {
        let mut remaining = self.remaining.lock().expect("lock");
        if *remaining == 0 {
            AckAction::Continue
        } else {
            *remaining -= 1;
            AckAction::DropAck
        }
    }
}

/// A batch size every test in this lane uses.
#[must_use]
pub fn batch(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).expect("a non-zero literal")
}
