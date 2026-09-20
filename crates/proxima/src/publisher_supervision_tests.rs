use std::borrow::Cow;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_nats::jetstream::stream::{
    Config as StreamConfig, DiscardPolicy, RetentionPolicy, StorageType,
};
use futures::FutureExt;
use proxima_core::StorageError;
use proxima_core::flavor::{
    EmbeddingRecipe, FlavorContract, ProjectionDecl, Provenance, SchemaContract, SchemaRef,
    SearchProjectionDecl, TransferRule,
};
use proxima_core::publication::PublicationConfig;
use proxima_core::storage_ports::publication::{
    AckOutcome, BrokerReceipt, ClaimToken, ClaimedPublication, PublicationOutboxPort, PublisherId,
    ReleaseOutcome,
};
use proxima_core::{FactPayload, Owner};
use sqlx::SqlSafeStr;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use tokio::sync::Notify;
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use uuid::Uuid;

use super::spawn_publication_publisher_supervised;

const ENV_NATS_URL: &str = "PROXIMA_TEST_NATS_URL";
const ENV_ADMIN_USER: &str = "PROXIMA_TEST_NATS_ADMIN_USER";
const ENV_ADMIN_PASSWORD: &str = "PROXIMA_TEST_NATS_ADMIN_PASSWORD";
const ENV_PUBLISHER_USER: &str = "PROXIMA_TEST_NATS_PUBLISHER_USER";
const ENV_PUBLISHER_PASSWORD: &str = "PROXIMA_TEST_NATS_PUBLISHER_PASSWORD";
const HEALTH_SOURCE: &str = "urn:proxima:runtime-publisher-supervision";
const STORAGE_MARKER: &str = "SYNTHETIC_HEALTH_SECRET_MARKER";
const PROBE_MIGRATION_VERSION: i64 = 20_260_920_000_011;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PublisherHealthProbeV1 {
    probe_id: Uuid,
    note: String,
}

impl FactPayload for PublisherHealthProbeV1 {
    const SCHEMA_ID: &'static str = "publisher-supervision/probe-v1";
    const SCHEMA_VERSION: u32 = 1;
    const LISTENABLE: bool = true;

    fn receipt_key(&self) -> Vec<u8> {
        self.probe_id.as_bytes().to_vec()
    }

    fn render(&self) -> String {
        self.note.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("publisher_supervision.probe_v1")
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "required": ["probe_id", "note"],
            "properties": {
                "probe_id": { "type": "string", "format": "uuid" },
                "note": { "type": "string" }
            }
        }))
    }
}

crate::flavor::pg_sidecar! {
    payload: PublisherHealthProbeV1,
    row: PublisherHealthProbeRow,
    kinds: [Fact],
    table: "publisher_supervision.probe_v1",
    key: t,
    fields: {
        probe_id => probe_id: (uuid),
        note => note: (text),
    },
}

static HEALTH_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: "publisher-supervision",
    ordinal: 19,
    schemas: &[SchemaContract {
        id: SchemaRef::new("publisher-supervision", "probe", 1),
        kind: proxima_core::verbs::schema::PayloadKind::Fact,
        sidecar_table: Some("publisher_supervision.probe_v1"),
        search: SearchProjectionDecl::None {
            why: "a publisher supervision fixture, not a search surface",
        },
        embedding: EmbeddingRecipe::Never {
            why: "a publisher supervision fixture, not a memory",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: &[],
    }],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
    projection: ProjectionDecl::None {
        why: "a publisher supervision fixture has no projection",
    },
};

fn probe_migrator() -> Migrator {
    use crate::flavor::FlavorBundle;

    let mut registry = crate::flavor::FlavorRegistry::new();
    PublisherHealthApp::register(&mut registry).expect("publisher fixture registers");
    let registry = registry.try_freeze().expect("publisher fixture freezes");
    let mut sidecars = crate::flavor::PgSidecarRegistry::new();
    proxima_storage_pg::register_core_pg_sidecars(&mut sidecars);
    PublisherHealthApp::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars
        .freeze_against(&registry)
        .expect("publisher fixture PG sidecar matches its contract");

    let mut statements = vec![
        "CREATE SCHEMA publisher_supervision".to_owned(),
        "CREATE TABLE publisher_supervision.probe_v1 (
            t uuid PRIMARY KEY,
            probe_id uuid NOT NULL,
            note text NOT NULL
        )"
        .to_owned(),
        "INSERT INTO proxima_core.flavor_surface (table_name, flavor_id) VALUES
             ('publisher_supervision.probe_v1', 'publisher-supervision')"
            .to_owned(),
    ];
    statements.extend(
        sidecars
            .declaration_trigger_artifacts("publisher-supervision")
            .expect("publisher fixture declaration triggers")
            .into_iter()
            .map(|artifact| artifact.forward),
    );
    statements.extend(
        sidecars
            .presence_trigger_artifacts("publisher-supervision")
            .expect("publisher fixture presence triggers")
            .into_iter()
            .map(|artifact| artifact.forward),
    );

    Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            PROBE_MIGRATION_VERSION,
            Cow::Borrowed("publisher supervision probe sidecar"),
            MigrationType::Simple,
            sqlx::AssertSqlSafe(statements.join(";\n")).into_sql_str(),
            false,
        )]),
        ..Migrator::DEFAULT
    }
}

#[derive(Debug)]
struct PublisherHealthApp;

impl crate::flavor::FlavorBundle for PublisherHealthApp {
    fn register(
        registry: &mut crate::flavor::FlavorRegistry,
    ) -> Result<(), crate::flavor::FlavorRegistryError> {
        registry.try_add_flavor(crate::flavor::FlavorDescriptor {
            flavor_id: "publisher-supervision".to_owned(),
            display_name: "Publisher Supervision".to_owned(),
            package_version: "0".to_owned(),
            author: None,
            provenance: crate::flavor::FlavorProvenance::Builtin,
        })?;
        registry.try_add_fact_schema::<PublisherHealthProbeV1>()?;
        registry.try_add_contract(&HEALTH_CONTRACT)
    }

    fn register_pg_sidecars(registry: &mut crate::flavor::PgSidecarRegistry) {
        registry.add_fact::<PublisherHealthProbeV1>();
    }

    fn migrators() -> Vec<crate::NamedMigrator> {
        vec![crate::NamedMigrator::new(
            "publisher-supervision",
            probe_migrator(),
        )]
    }
}

impl crate::FlavorApp for PublisherHealthApp {
    fn app_info() -> crate::AppInfo {
        crate::AppInfo {
            id: "publisher-supervision-test",
            title: "Publisher Supervision Test",
            version: "0",
        }
    }

    fn configure(builder: crate::RuntimeBuilder) -> crate::RuntimeBuilder {
        builder
    }
}

#[derive(Clone)]
struct EventBuffer(Arc<Mutex<Vec<String>>>);

impl<S> Layer<S> for EventBuffer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut fields = EventFields::default();
        event.record(&mut fields);
        self.0.lock().expect("event buffer lock").push(fields.0);
    }
}

#[derive(Default)]
struct EventFields(String);

impl Visit for EventFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        write!(&mut self.0, "{}={value:?} ", field.name()).expect("string write");
    }
}

static EVENTS: OnceLock<Arc<Mutex<Vec<String>>>> = OnceLock::new();

fn events() -> Arc<Mutex<Vec<String>>> {
    EVENTS
        .get_or_init(|| {
            let events = Arc::new(Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::registry().with(EventBuffer(events.clone()));
            tracing::subscriber::set_global_default(subscriber)
                .expect("publisher supervision tests own this test binary's tracing subscriber");
            events
        })
        .clone()
}

struct AbortOnDrop(Option<JoinHandle<()>>);

impl AbortOnDrop {
    fn new(task: JoinHandle<()>) -> Self {
        Self(Some(task))
    }

    fn abort(&self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }

    async fn wait(&mut self) -> Result<(), JoinError> {
        self.0.as_mut().expect("task remains owned").await
    }

    async fn join(&mut self) -> Result<(), JoinError> {
        self.0.take().expect("task remains owned").await
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

enum ClaimMode {
    MarkerOnce,
    PanicOnce,
    BlockOnCall {
        call: usize,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    },
}

struct ControlledOutbox {
    inner: Arc<dyn PublicationOutboxPort>,
    mode: ClaimMode,
    calls: AtomicUsize,
}

impl ControlledOutbox {
    fn new(inner: Arc<dyn PublicationOutboxPort>, mode: ClaimMode) -> Arc<Self> {
        Arc::new(Self {
            inner,
            mode,
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait::async_trait]
impl PublicationOutboxPort for ControlledOutbox {
    async fn claim(
        &self,
        publisher: &PublisherId,
        limit: NonZeroU32,
        lease: Duration,
    ) -> Result<Vec<ClaimedPublication>, StorageError> {
        let call = self.calls.fetch_add(1, Ordering::AcqRel) + 1;
        match &self.mode {
            ClaimMode::MarkerOnce if call == 1 => {
                Err(StorageError::Unavailable(STORAGE_MARKER.to_owned()))
            }
            ClaimMode::PanicOnce if call == 1 => panic!("synthetic publisher panic"),
            ClaimMode::BlockOnCall {
                call: blocked_call,
                entered,
                release,
            } if call == *blocked_call => {
                entered.notify_one();
                release.notified().await;
                self.inner.claim(publisher, limit, lease).await
            }
            _ => self.inner.claim(publisher, limit, lease).await,
        }
    }

    async fn mark_published(
        &self,
        id: Uuid,
        claim: ClaimToken,
        receipt: &BrokerReceipt,
    ) -> Result<AckOutcome, StorageError> {
        self.inner.mark_published(id, claim, receipt).await
    }

    async fn release(&self, id: Uuid, claim: ClaimToken) -> Result<ReleaseOutcome, StorageError> {
        self.inner.release(id, claim).await
    }

    async fn pending_count(&self) -> Result<u64, StorageError> {
        self.inner.pending_count().await
    }
}

fn nats_url() -> Option<String> {
    match std::env::var(ENV_NATS_URL) {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => {
            assert!(
                std::env::var("CI").as_deref() != Ok("true"),
                "{ENV_NATS_URL} required under CI=true"
            );
            eprintln!("skipping publisher supervision PG cases: {ENV_NATS_URL} is unset");
            None
        }
    }
}

fn auth_value(user_key: &str, password_key: &str) -> Option<(String, String)> {
    match (std::env::var(user_key), std::env::var(password_key)) {
        (Ok(user), Ok(password)) => Some((user, password)),
        (Err(_), Err(_)) => None,
        _ => panic!("NATS user/password variables must be set together"),
    }
}

async fn admin_client(url: &str) -> async_nats::Client {
    let options = async_nats::ConnectOptions::new();
    let options = match auth_value(ENV_ADMIN_USER, ENV_ADMIN_PASSWORD) {
        Some((user, password)) => options.user_and_password(user, password),
        None => options,
    };
    options.connect(url).await.expect("NATS admin connects")
}

async fn create_stream(url: &str, name: &str, prefix: &str) -> async_nats::jetstream::Context {
    let client = admin_client(url).await;
    assert!(client.server_info().connect_urls.is_empty());
    let js = async_nats::jetstream::new(client);
    js.create_stream(StreamConfig {
        name: name.to_owned(),
        subjects: vec![format!("{prefix}.>")],
        storage: StorageType::File,
        retention: RetentionPolicy::Limits,
        discard: DiscardPolicy::New,
        max_age: Duration::ZERO,
        max_bytes: 8 * 1024 * 1024,
        max_message_size: -1,
        duplicate_window: Duration::from_mins(2),
        ..StreamConfig::default()
    })
    .await
    .expect("fixture stream provisioned");
    js
}

async fn update_max_message_size(
    js: &async_nats::jetstream::Context,
    stream_name: &str,
    maximum: i32,
) {
    let mut stream = js.get_stream(stream_name).await.expect("fixture stream");
    let mut config = stream.info().await.expect("stream info").config.clone();
    config.max_message_size = maximum;
    js.update_stream(config).await.expect("stream update");
}

async fn wait_for<T>(description: &str, mut check: impl FnMut() -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(value) = check() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {description}"))
}

async fn wait_for_health(
    reader: &proxima_outbox_nats::PublisherHealthReader,
    state: proxima_outbox_nats::PublisherDrainState,
) {
    wait_for("publisher drain observation", || {
        let health = reader.snapshot();
        (health.drain == state).then_some(health)
    })
    .await;
}

async fn wait_for_outbox(pool: &sqlx::PgPool, t: Uuid, wanted: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let state: String = sqlx::query_scalar(
                "SELECT state::text FROM proxima_core.publication_outbox WHERE t = $1",
            )
            .bind(t)
            .fetch_one(pool)
            .await
            .expect("outbox state query");
            if state == wanted {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("outbox state changes");
}

async fn capture(built: &super::BuiltProxima, owner: Owner, note: &str) -> Uuid {
    let payload = PublisherHealthProbeV1 {
        probe_id: Uuid::now_v7(),
        note: note.to_owned(),
    };
    built
        .engine()
        .ingest_fact(
            &built.single_owner_authz().expect("single owner authz"),
            crate::FactWrite::new(owner, PublisherHealthProbeV1::SCHEMA_ID, &payload),
        )
        .await
        .expect("genuine publication Fact")
        .memory_id
        .into_inner()
}

async fn assert_published(pool: &sqlx::PgPool, t: Uuid) {
    wait_for_outbox(pool, t, "published").await;
}

struct TestWorld {
    db_name: String,
    stream_name: String,
    js: async_nats::jetstream::Context,
    built: super::BuiltProxima,
    owner: Owner,
    events: Arc<Mutex<Vec<String>>>,
}

impl TestWorld {
    async fn new(url: &str) -> Self {
        let events = events();
        let db_name = proxima_pg_testkit::unique_db_name("publisher_supervision");
        proxima_pg_testkit::create_db(&db_name)
            .await
            .expect("PG fixture database");
        let prefix = format!("publisher_supervision_{}", Uuid::now_v7().simple());
        let stream_name = format!("PS_{}", Uuid::now_v7().simple());
        let js = create_stream(url, &stream_name, &prefix).await;
        let owner = crate::company_owner(Uuid::now_v7());
        let db_url = proxima_pg_testkit::db_url(&db_name);
        let mut values = vec![
            ("PROXIMA_PUBLICATION_SOURCE", HEALTH_SOURCE.to_owned()),
            ("PROXIMA_NATS_URL", url.to_owned()),
            ("PROXIMA_NATS_SUBJECT_PREFIX", prefix),
            ("PROXIMA_NATS_POLL_MS", "100".to_owned()),
            ("PROXIMA_NATS_PUBLISH_TIMEOUT_MS", "1000".to_owned()),
        ];
        if let Some((user, password)) = auth_value(ENV_PUBLISHER_USER, ENV_PUBLISHER_PASSWORD) {
            values.push(("PROXIMA_NATS_USER", user));
            values.push(("PROXIMA_NATS_PASSWORD", password));
        }
        let lookup = move |key: &str| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.clone())
        };
        let built = crate::Proxima::<PublisherHealthApp>::app()
            .from_lookup(lookup)
            .expect("runtime config")
            .database_url(db_url)
            .owner(owner)
            .tool_scope(crate::ToolScope::All)
            .allow_insecure_single_owner()
            .publication(PublicationConfig::new(
                proxima_core::publication::PublicationSource::new(HEALTH_SOURCE)
                    .expect("publication source"),
            ))
            .build()
            .await
            .expect("Proxima builds");
        Self {
            db_name,
            stream_name,
            js,
            built,
            owner,
            events,
        }
    }

    async fn teardown(self) {
        self.built.shutdown();
        let _ = self.js.delete_stream(&self.stream_name).await;
        let _ = proxima_pg_testkit::drop_db(&self.db_name).await;
    }
}

#[tokio::test]
async fn publisher_supervision_sidecar_fixture_captures_against_real_pg() {
    if std::env::var_os("PROXIMA_TEST_PG_URL").is_none() {
        assert!(
            std::env::var("CI").as_deref() != Ok("true"),
            "PROXIMA_TEST_PG_URL required under CI=true"
        );
        eprintln!("skipping publisher supervision PG fixture: PROXIMA_TEST_PG_URL is unset");
        return;
    }

    let db_name = proxima_pg_testkit::unique_db_name("publisher_supervision_fixture");
    proxima_pg_testkit::create_db(&db_name)
        .await
        .expect("PG fixture database");
    let mut built = None;
    let outcome = std::panic::AssertUnwindSafe(async {
        let owner = crate::company_owner(Uuid::now_v7());
        built = Some(
            crate::Proxima::<PublisherHealthApp>::app()
                .database_url(proxima_pg_testkit::db_url(&db_name))
                .owner(owner)
                .tool_scope(crate::ToolScope::All)
                .allow_insecure_single_owner()
                .publication(PublicationConfig::new(
                    proxima_core::publication::PublicationSource::new(HEALTH_SOURCE)
                        .expect("publication source"),
                ))
                .build()
                .await
                .expect("publisher fixture builds and migrates its sidecar"),
        );
        let runtime = built.as_ref().expect("fixture was built");
        let id = capture(runtime, owner, "fixture-capture").await;
        let state: String = sqlx::query_scalar(
            "SELECT state::text FROM proxima_core.publication_outbox WHERE t = $1",
        )
        .bind(id)
        .fetch_one(runtime.pool_for_tests())
        .await
        .expect("captured outbox row");
        assert_eq!(state, "pending");
    })
    .catch_unwind()
    .await;
    if let Some(built) = built {
        built.shutdown();
    }
    let _ = proxima_pg_testkit::drop_db(&db_name).await;
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

#[tokio::test]
async fn production_supervisor_reports_and_recovers_from_owned_failures() {
    let Some(url) = nats_url() else {
        return;
    };
    let world = TestWorld::new(&url).await;
    let outcome = std::panic::AssertUnwindSafe(async {
        let mut config = world.built.nats.clone().expect("publisher config");
        config.poll_interval = Duration::from_millis(50);
        config.batch = NonZeroU32::new(16).expect("nonzero batch");
        marker_failure_is_redacted_and_recovers(&world, &config).await;
        real_claim_table_failure_recovers(&world, &config).await;
        claim_panic_stops_supervisor(&world, &config).await;
        blocked_claim_abort_stops_supervisor(&world, &config).await;
        mixed_pass_stays_failed_until_recovery(&world, &config).await;
    })
    .catch_unwind()
    .await;
    world.teardown().await;
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

async fn marker_failure_is_redacted_and_recovers(
    world: &TestWorld,
    config: &proxima_outbox_nats::NatsPublisherConfig,
) {
    let marker_wrapper = ControlledOutbox::new(world.built.outbox.clone(), ClaimMode::MarkerOnce);
    let cancel = CancellationToken::new();
    let supervised = spawn_publication_publisher_supervised(
        marker_wrapper,
        config.clone(),
        None,
        cancel.clone(),
    );
    let (reader, task) = supervised.into_parts();
    let mut task = AbortOnDrop::new(task);
    wait_for_health(&reader, proxima_outbox_nats::PublisherDrainState::Failed).await;
    assert!(!reader.snapshot().is_ready());
    assert!(!format!("{reader:?}").contains(STORAGE_MARKER));
    let log_events = world.events.lock().expect("event lock").clone();
    assert!(
        log_events
            .iter()
            .any(|event| event.contains("failure_category=Storage")),
        "expected concrete storage category in spawned-task logs: {log_events:?}"
    );
    assert!(
        log_events
            .iter()
            .all(|event| !event.contains(STORAGE_MARKER)),
        "synthetic storage error escaped through logging: {log_events:?}"
    );
    wait_for_health(&reader, proxima_outbox_nats::PublisherDrainState::Clean).await;
    cancel.cancel();
    task.join().await.expect("cancelled task joins");
    assert_eq!(
        reader.snapshot().task,
        proxima_outbox_nats::PublisherTaskState::Stopped
    );
}

async fn real_claim_table_failure_recovers(
    world: &TestWorld,
    config: &proxima_outbox_nats::NatsPublisherConfig,
) {
    sqlx::query("ALTER TABLE proxima_core.publication_outbox RENAME TO publication_outbox_hold")
        .execute(world.built.pool_for_tests())
        .await
        .expect("isolate real claim-table failure");
    let cancel = CancellationToken::new();
    let supervised = spawn_publication_publisher_supervised(
        world.built.outbox.clone(),
        config.clone(),
        None,
        cancel.clone(),
    );
    let (reader, task) = supervised.into_parts();
    let mut task = AbortOnDrop::new(task);
    let failed = std::panic::AssertUnwindSafe(async {
        wait_for_health(&reader, proxima_outbox_nats::PublisherDrainState::Failed).await;
    })
    .catch_unwind()
    .await;
    sqlx::query("ALTER TABLE proxima_core.publication_outbox_hold RENAME TO publication_outbox")
        .execute(world.built.pool_for_tests())
        .await
        .expect("restore isolated claim table");
    if let Err(payload) = failed {
        cancel.cancel();
        task.abort();
        let _ = task.join().await;
        std::panic::resume_unwind(payload);
    }
    wait_for_health(&reader, proxima_outbox_nats::PublisherDrainState::Clean).await;
    cancel.cancel();
    task.join().await.expect("SQL-failure task joins");
}

async fn claim_panic_stops_supervisor(
    world: &TestWorld,
    config: &proxima_outbox_nats::NatsPublisherConfig,
) {
    let panic_wrapper = ControlledOutbox::new(world.built.outbox.clone(), ClaimMode::PanicOnce);
    let cancel = CancellationToken::new();
    let supervised =
        spawn_publication_publisher_supervised(panic_wrapper, config.clone(), None, cancel);
    let (reader, task) = supervised.into_parts();
    let mut task = AbortOnDrop::new(task);
    let error = task
        .join()
        .await
        .expect_err("claim panic reaches JoinError");
    assert!(error.is_panic());
    assert_eq!(
        reader.snapshot().task,
        proxima_outbox_nats::PublisherTaskState::Stopped
    );
    assert!(!reader.snapshot().is_ready());
}

async fn blocked_claim_abort_stops_supervisor(
    world: &TestWorld,
    config: &proxima_outbox_nats::NatsPublisherConfig,
) {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let blocked_wrapper = ControlledOutbox::new(
        world.built.outbox.clone(),
        ClaimMode::BlockOnCall {
            call: 1,
            entered: entered.clone(),
            release,
        },
    );
    let cancel = CancellationToken::new();
    let supervised =
        spawn_publication_publisher_supervised(blocked_wrapper, config.clone(), None, cancel);
    let (reader, task) = supervised.into_parts();
    let mut task = AbortOnDrop::new(task);
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("claim enters deterministic blocker");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), task.wait())
            .await
            .is_err()
    );
    task.abort();
    assert!(
        task.join()
            .await
            .expect_err("aborted blocked task")
            .is_cancelled()
    );
    assert_eq!(
        reader.snapshot().task,
        proxima_outbox_nats::PublisherTaskState::Stopped
    );
}

async fn mixed_pass_stays_failed_until_recovery(
    world: &TestWorld,
    config: &proxima_outbox_nats::NatsPublisherConfig,
) {
    let poison = capture(&world.built, world.owner, &"p".repeat(8 * 1024)).await;
    let small = capture(&world.built, world.owner, "small publication").await;
    update_max_message_size(&world.js, &world.stream_name, 2048).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mixed_wrapper = ControlledOutbox::new(
        world.built.outbox.clone(),
        ClaimMode::BlockOnCall {
            call: 2,
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    let cancel = CancellationToken::new();
    let supervised =
        spawn_publication_publisher_supervised(mixed_wrapper, config.clone(), None, cancel.clone());
    let (reader, task) = supervised.into_parts();
    let mut task = AbortOnDrop::new(task);
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("the next claim reaches its pre-SQL barrier");
    assert_eq!(
        reader.snapshot().drain,
        proxima_outbox_nats::PublisherDrainState::Failed,
        "the mixed success/failure pass stays failed while the next claim is blocked"
    );
    assert!(!reader.snapshot().is_ready());
    let partial_logs = world.events.lock().expect("event lock").clone();
    assert!(
        partial_logs
            .iter()
            .any(|event| event.contains("failure_category=PartialPass")),
        "mixed pass event is expected: {partial_logs:?}"
    );
    update_max_message_size(&world.js, &world.stream_name, -1).await;
    release.notify_one();
    wait_for_health(&reader, proxima_outbox_nats::PublisherDrainState::Clean).await;
    assert_published(world.built.pool_for_tests(), poison).await;
    assert_published(world.built.pool_for_tests(), small).await;
    cancel.cancel();
    task.join().await.expect("mixed publisher joins");
}
