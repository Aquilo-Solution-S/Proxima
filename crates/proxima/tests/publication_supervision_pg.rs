use std::borrow::Cow;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_nats::jetstream::stream::{
    Config as StreamConfig, DiscardPolicy, RetentionPolicy, StorageType,
};
use futures::FutureExt;
use proxima::flavor::{
    FactPayload, FlavorBundle, FlavorDescriptor, FlavorProvenance, FlavorRegistry,
    FlavorRegistryError,
};
use proxima::{AppInfo, FlavorApp, Proxima, RuntimeBuilder, ToolScope, company_owner};
use proxima_core::flavor::{
    EmbeddingRecipe, FlavorContract, ProjectionDecl, Provenance, SchemaContract, SchemaRef,
    SearchProjectionDecl, TransferRule,
};
use proxima_core::publication::{PublicationConfig, PublicationSource};
use proxima_core::test_fixtures::authenticated_context;
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{AuthzContext, Engine, Owner, OwnerRefKind};
use sqlx::SqlSafeStr;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use uuid::Uuid;

const ENV_NATS_URL: &str = "PROXIMA_TEST_NATS_URL";
const ENV_NATS_ADMIN_USER: &str = "PROXIMA_TEST_NATS_ADMIN_USER";
const ENV_NATS_ADMIN_PASSWORD: &str = "PROXIMA_TEST_NATS_ADMIN_PASSWORD";
const ENV_NATS_PUBLISHER_USER: &str = "PROXIMA_TEST_NATS_PUBLISHER_USER";
const ENV_NATS_PUBLISHER_PASSWORD: &str = "PROXIMA_TEST_NATS_PUBLISHER_PASSWORD";
const PROBE_MIGRATION_VERSION: i64 = 20_260_920_000_012;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct HealthProbeV1 {
    probe_id: Uuid,
    note: String,
}

impl FactPayload for HealthProbeV1 {
    const SCHEMA_ID: &'static str = "publisher-health/probe-v1";
    const SCHEMA_VERSION: u32 = 1;
    const LISTENABLE: bool = true;

    fn receipt_key(&self) -> Vec<u8> {
        self.probe_id.as_bytes().to_vec()
    }

    fn render(&self) -> String {
        self.note.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("publisher_health.probe_v1")
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

proxima::flavor::pg_sidecar! {
    payload: HealthProbeV1,
    row: PublisherHealthProbeRow,
    kinds: [Fact],
    table: "publisher_health.probe_v1",
    key: t,
    fields: {
        probe_id => probe_id: (uuid),
        note => note: (text),
    },
}

static HEALTH_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: "publisher-health",
    ordinal: 19,
    schemas: &[SchemaContract {
        id: SchemaRef::new("publisher-health", "probe", 1),
        kind: PayloadKind::Fact,
        sidecar_table: Some("publisher_health.probe_v1"),
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
    let mut registry = FlavorRegistry::new();
    PublisherApp::register(&mut registry).expect("publisher fixture registers");
    let registry = registry.try_freeze().expect("publisher fixture freezes");
    let mut sidecars = proxima::flavor::PgSidecarRegistry::new();
    proxima_storage_pg::register_core_pg_sidecars(&mut sidecars);
    PublisherApp::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars
        .freeze_against(&registry)
        .expect("publisher fixture PG sidecar matches its contract");

    let mut statements = vec![
        "CREATE SCHEMA publisher_health".to_owned(),
        "CREATE TABLE publisher_health.probe_v1 (
            t uuid PRIMARY KEY,
            probe_id uuid NOT NULL,
            note text NOT NULL
        )"
        .to_owned(),
        "ALTER TABLE publisher_health.probe_v1 ENABLE ROW LEVEL SECURITY".to_owned(),
        "ALTER TABLE publisher_health.probe_v1 FORCE ROW LEVEL SECURITY".to_owned(),
        "CREATE POLICY proxima_owner_read ON publisher_health.probe_v1 FOR SELECT TO PUBLIC USING (EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = probe_v1.t AND parent.owner_id = ANY(COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))))".to_owned(),
        "CREATE POLICY proxima_owner_write ON publisher_health.probe_v1 FOR ALL TO PUBLIC USING (EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = probe_v1.t AND parent.owner_id = ANY(COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[])))) WITH CHECK (EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = probe_v1.t AND parent.owner_id = ANY(COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))))".to_owned(),
        "CREATE POLICY proxima_platform ON publisher_health.probe_v1 FOR ALL TO CURRENT_USER USING (current_setting('app.proxima_scope', true) = 'platform') WITH CHECK (current_setting('app.proxima_scope', true) = 'platform')".to_owned(),
        "INSERT INTO proxima_core.flavor_surface (table_name, flavor_id) VALUES
             ('publisher_health.probe_v1', 'publisher-health')"
            .to_owned(),
    ];
    statements.extend(
        sidecars
            .declaration_trigger_artifacts("publisher-health")
            .expect("publisher fixture declaration triggers")
            .into_iter()
            .map(|artifact| artifact.forward),
    );
    statements.extend(
        sidecars
            .presence_trigger_artifacts("publisher-health")
            .expect("publisher fixture presence triggers")
            .into_iter()
            .map(|artifact| artifact.forward),
    );

    Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            PROBE_MIGRATION_VERSION,
            Cow::Borrowed("publisher health probe sidecar"),
            MigrationType::Simple,
            sqlx::AssertSqlSafe(statements.join(";\n")).into_sql_str(),
            false,
        )]),
        ..Migrator::DEFAULT
    }
}

#[derive(Debug)]
struct PublisherApp;

impl FlavorBundle for PublisherApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        registry.try_add_flavor(FlavorDescriptor {
            flavor_id: "publisher-health".to_owned(),
            display_name: "Publisher Health".to_owned(),
            package_version: "0".to_owned(),
            author: None,
            provenance: FlavorProvenance::Builtin,
        })?;
        registry.try_add_fact_schema::<HealthProbeV1>()?;
        registry.try_add_contract(&HEALTH_CONTRACT)
    }

    fn register_pg_sidecars(registry: &mut proxima::flavor::PgSidecarRegistry) {
        registry.add_fact::<HealthProbeV1>();
    }

    fn migrators() -> Vec<proxima::NamedMigrator> {
        vec![proxima::NamedMigrator::new(
            "publisher-health",
            probe_migrator(),
        )]
    }
}

impl FlavorApp for PublisherApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "publisher-health-test",
            title: "Publisher Health Test",
            version: "0",
        }
    }

    fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
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

fn install_event_buffer() -> Arc<Mutex<Vec<String>>> {
    EVENTS
        .get_or_init(|| {
            let events = Arc::new(Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::registry().with(EventBuffer(events.clone()));
            tracing::subscriber::set_global_default(subscriber)
                .expect("publisher test owns this test binary's tracing subscriber");
            events
        })
        .clone()
}

struct PublisherProxy {
    addr: SocketAddr,
    paused: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    bridge_tokens: Arc<Mutex<Vec<CancellationToken>>>,
    bridge_tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    cancel: CancellationToken,
    accept_task: Option<JoinHandle<()>>,
}

impl PublisherProxy {
    async fn bind(target: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("proxy bind");
        let addr = listener.local_addr().expect("proxy address");
        let paused = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let bridge_tokens = Arc::new(Mutex::new(Vec::new()));
        let bridge_tasks = Arc::new(Mutex::new(Vec::new()));
        let cancel = CancellationToken::new();
        let accept_task = {
            let paused = paused.clone();
            let active = active.clone();
            let bridge_tokens = bridge_tokens.clone();
            let bridge_tasks = bridge_tasks.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                loop {
                    let accepted = tokio::select! {
                        () = cancel.cancelled() => break,
                        accepted = listener.accept() => accepted,
                    };
                    let Ok((mut downstream, _)) = accepted else {
                        continue;
                    };
                    if paused.load(Ordering::Acquire) {
                        continue;
                    }
                    let upstream = tokio::select! {
                        () = cancel.cancelled() => break,
                        connected = TcpStream::connect(target) => connected,
                    };
                    let Ok(mut upstream) = upstream else {
                        continue;
                    };
                    if paused.load(Ordering::Acquire) {
                        continue;
                    }
                    let bridge_cancel = CancellationToken::new();
                    bridge_tokens
                        .lock()
                        .expect("proxy token lock")
                        .push(bridge_cancel.clone());
                    active.fetch_add(1, Ordering::AcqRel);
                    let bridge_active = active.clone();
                    let bridge_task = tokio::spawn(async move {
                        struct ActiveGuard(Arc<AtomicUsize>);
                        impl Drop for ActiveGuard {
                            fn drop(&mut self) {
                                self.0.fetch_sub(1, Ordering::AcqRel);
                            }
                        }
                        let _active = ActiveGuard(bridge_active);
                        tokio::select! {
                            () = bridge_cancel.cancelled() => {}
                            _ = copy_bidirectional(&mut downstream, &mut upstream) => {}
                        }
                    });
                    bridge_tasks
                        .lock()
                        .expect("proxy task lock")
                        .push(bridge_task);
                }
            })
        };
        Self {
            addr,
            paused,
            active,
            bridge_tokens,
            bridge_tasks,
            cancel,
            accept_task: Some(accept_task),
        }
    }

    fn url(&self) -> String {
        format!("nats://{}", self.addr)
    }

    fn pause(&self) {
        self.paused.store(true, Ordering::Release);
        for token in self.bridge_tokens.lock().expect("proxy token lock").iter() {
            token.cancel();
        }
    }

    fn resume(&self) {
        self.paused.store(false, Ordering::Release);
    }

    fn active_bridges(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    async fn run(self, body: impl AsyncFnOnce(&Self)) {
        let result = std::panic::AssertUnwindSafe(body(&self))
            .catch_unwind()
            .await;
        self.shutdown().await;
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    async fn shutdown(mut self) {
        self.cancel.cancel();
        for token in self.bridge_tokens.lock().expect("proxy token lock").iter() {
            token.cancel();
        }
        if let Some(task) = self.accept_task.take() {
            task.await.expect("proxy accept task joins");
        }
        let tasks = std::mem::take(&mut *self.bridge_tasks.lock().expect("proxy task lock"));
        for task in tasks {
            task.await.expect("proxy bridge task joins");
        }
        assert_eq!(self.active_bridges(), 0, "all publisher bridges closed");
    }
}

fn configured_url() -> Option<String> {
    match std::env::var(ENV_NATS_URL) {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => {
            assert!(
                std::env::var("CI").as_deref() != Ok("true"),
                "{ENV_NATS_URL} required under CI=true"
            );
            eprintln!("skipping publisher supervision: {ENV_NATS_URL} is unset");
            None
        }
    }
}

fn socket_address(url: &str) -> SocketAddr {
    let server = url.split(',').next().expect("one NATS server");
    let authority = server.strip_prefix("nats://").expect("NATS URL");
    authority
        .rsplit('@')
        .next()
        .expect("NATS authority")
        .parse()
        .expect("NATS address is a socket address")
}

async fn admin_client(url: &str) -> async_nats::Client {
    let options = async_nats::ConnectOptions::new();
    let options = match (
        std::env::var(ENV_NATS_ADMIN_USER),
        std::env::var(ENV_NATS_ADMIN_PASSWORD),
    ) {
        (Ok(user), Ok(password)) => options.user_and_password(user, password),
        (Err(_), Err(_)) => options,
        _ => panic!("admin NATS credentials must be set together"),
    };
    options.connect(url).await.expect("NATS admin connects")
}

fn runtime_lookup(proxy_url: String, prefix: String) -> impl Fn(&str) -> Option<String> {
    let publisher_auth = match (
        std::env::var(ENV_NATS_PUBLISHER_USER),
        std::env::var(ENV_NATS_PUBLISHER_PASSWORD),
    ) {
        (Ok(user), Ok(password)) => Some((user, password)),
        (Err(_), Err(_)) => None,
        _ => panic!("publisher NATS credentials must be set together"),
    };
    move |key| match key {
        "PROXIMA_PUBLICATION_SOURCE" => Some("urn:proxima:publisher-health-tests".to_owned()),
        "PROXIMA_NATS_URL" => Some(proxy_url.clone()),
        "PROXIMA_NATS_SUBJECT_PREFIX" => Some(prefix.clone()),
        "PROXIMA_NATS_POLL_MS" => Some("100".to_owned()),
        "PROXIMA_NATS_PUBLISH_TIMEOUT_MS" => Some("1000".to_owned()),
        "PROXIMA_NATS_USER" => publisher_auth.as_ref().map(|(user, _)| user.clone()),
        "PROXIMA_NATS_PASSWORD" => publisher_auth
            .as_ref()
            .map(|(_, password)| password.clone()),
        _ => None,
    }
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

async fn wait_for_state(pool: &sqlx::PgPool, t: Uuid, wanted: &str) {
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
    .expect("publication outbox state settles");
}

async fn capture(engine: &Arc<Engine>, authz: &AuthzContext, owner: Owner, note: &str) -> Uuid {
    let probe = HealthProbeV1 {
        probe_id: Uuid::now_v7(),
        note: note.to_owned(),
    };
    engine
        .ingest_fact(
            &authenticated_context(authz.clone()),
            proxima::FactWrite::new(owner, HealthProbeV1::SCHEMA_ID, &probe),
        )
        .await
        .expect("genuine listenable Fact capture")
        .memory_id
        .into_inner()
}

async fn captured_record(pool: &sqlx::PgPool, t: Uuid) -> (String, Vec<u8>) {
    sqlx::query_as("SELECT event_id, envelope FROM proxima_core.publication_outbox WHERE t = $1")
        .bind(t)
        .fetch_one(pool)
        .await
        .expect("captured outbox record")
}

async fn assert_delivered(
    pool: &sqlx::PgPool,
    js: &async_nats::jetstream::Context,
    stream_name: &str,
    t: Uuid,
    prefix: &str,
    owner: &Owner,
) {
    wait_for_state(pool, t, "published").await;
    let (event_id, envelope) = captured_record(pool, t).await;
    let kind = OwnerRefKind::of(owner);
    let subject = proxima_outbox_nats::subject_for(
        prefix,
        kind.as_str(),
        owner.stored_owner_id(),
        HealthProbeV1::SCHEMA_ID,
    );
    let stream = js.get_stream(stream_name).await.expect("fixture stream");
    let message = tokio::time::timeout(
        Duration::from_secs(10),
        stream.get_last_raw_message_by_subject(&subject),
    )
    .await
    .expect("published message appears")
    .expect("published subject exists");
    assert_eq!(message.subject.as_str(), subject);
    assert_eq!(message.payload.as_ref(), envelope.as_slice());
    assert_eq!(
        message
            .headers
            .get(proxima_outbox_nats::HEADER_MSG_ID)
            .map(ToString::to_string),
        Some(event_id)
    );
}

async fn wait_ready(reader: &proxima_outbox_nats::PublisherHealthReader) {
    wait_for("publisher readiness", || {
        let snapshot = reader.snapshot();
        snapshot.is_ready().then_some(snapshot)
    })
    .await;
}

struct PublisherTestContext<'a> {
    proxy: &'a PublisherProxy,
    js: &'a async_nats::jetstream::Context,
    stream_name: &'a str,
    prefix: &'a str,
    nats_url: &'a str,
    platform_pool: &'a sqlx::PgPool,
}

async fn exercise_built_supervised(
    context: &PublisherTestContext<'_>,
    built: &proxima::host::BuiltProxima,
    owner: Owner,
    events: &Arc<Mutex<Vec<String>>>,
) {
    let authz = built.single_owner_authz().expect("single owner authz");
    let first_t = capture(&built.engine(), &authz, owner, "built-supervised").await;
    let cancel = CancellationToken::new();
    let supervised = built
        .spawn_publication_publisher_supervised(cancel.clone())
        .expect("broker configured");
    let (reader, task) = supervised.into_parts();
    assert!(!reader.snapshot().is_ready());
    wait_for("sanitized real connect failure", || {
        events
            .lock()
            .expect("event buffer lock")
            .iter()
            .any(|event| event.contains("failure_category=Connect"))
            .then_some(())
    })
    .await;
    assert_eq!(context.proxy.active_bridges(), 0);
    context.proxy.resume();
    wait_ready(&reader).await;
    assert_eq!(
        context.proxy.active_bridges(),
        1,
        "one publisher connection"
    );
    assert_delivered(
        context.platform_pool,
        context.js,
        context.stream_name,
        first_t,
        context.prefix,
        &owner,
    )
    .await;

    context.proxy.pause();
    wait_for("idle broker disconnection", || {
        let snapshot = reader.snapshot();
        (snapshot.connection == proxima_outbox_nats::PublisherConnectionState::Disconnected
            && !snapshot.is_ready()
            && context.proxy.active_bridges() == 0)
            .then_some(snapshot)
    })
    .await;
    context.proxy.resume();
    wait_ready(&reader).await;
    assert_eq!(
        context.proxy.active_bridges(),
        1,
        "recovery reuses one publisher loop"
    );
    task.abort();
    assert!(task.await.expect_err("aborted publisher").is_cancelled());
    assert_eq!(
        reader.snapshot().task,
        proxima_outbox_nats::PublisherTaskState::Stopped
    );
    wait_for("publisher bridge release after abort", || {
        (context.proxy.active_bridges() == 0).then_some(())
    })
    .await;
    assert!(!format!("{reader:?}").contains(context.nats_url));
}

async fn exercise_built_legacy(
    context: &PublisherTestContext<'_>,
    built: &proxima::host::BuiltProxima,
    owner: Owner,
) {
    let authz = built.single_owner_authz().expect("single owner authz");
    let t = capture(&built.engine(), &authz, owner, "built-legacy").await;
    let cancel = CancellationToken::new();
    let task = built
        .spawn_publication_publisher(cancel.clone())
        .expect("legacy publisher entry point remains configured");
    assert_delivered(
        context.platform_pool,
        context.js,
        context.stream_name,
        t,
        context.prefix,
        &owner,
    )
    .await;
    cancel.cancel();
    task.await.expect("legacy task joins");
    wait_for("legacy publisher bridge release", || {
        (context.proxy.active_bridges() == 0).then_some(())
    })
    .await;
}

async fn start_running_proxima(
    context: &PublisherTestContext<'_>,
    database_url: String,
    platform_database_url: String,
    owner: Owner,
) -> proxima::host::RunningProxima {
    Proxima::<PublisherApp>::app()
        .from_lookup(runtime_lookup(
            context.proxy.url(),
            context.prefix.to_owned(),
        ))
        .expect("runtime config")
        .database_url(database_url)
        .platform_database_url(platform_database_url)
        .owner(owner)
        .tool_scope(ToolScope::All)
        .allow_insecure_single_owner()
        .publication(PublicationConfig::new(
            PublicationSource::new("urn:proxima:publisher-health-tests")
                .expect("absolute source URI"),
        ))
        .run()
        .await
        .expect("Proxima run")
}

async fn exercise_running_legacy(
    context: &PublisherTestContext<'_>,
    running: &proxima::host::RunningProxima,
    owner: Owner,
) {
    let authz = running.single_owner_authz().expect("single owner authz");
    let t = capture(&running.engine, &authz, owner, "running-legacy").await;
    let cancel = CancellationToken::new();
    let task = running
        .spawn_publication_publisher(cancel.clone())
        .expect("legacy running entry point remains configured");
    assert_delivered(
        context.platform_pool,
        context.js,
        context.stream_name,
        t,
        context.prefix,
        &owner,
    )
    .await;
    cancel.cancel();
    task.await.expect("running legacy task joins");
    wait_for("running legacy publisher bridge release", || {
        (context.proxy.active_bridges() == 0).then_some(())
    })
    .await;
}

async fn exercise_running_supervised(
    context: &PublisherTestContext<'_>,
    running: &proxima::host::RunningProxima,
    owner: Owner,
) {
    let authz = running.single_owner_authz().expect("single owner authz");
    let t = capture(&running.engine, &authz, owner, "running-supervised").await;
    let cancel = CancellationToken::new();
    let supervised = running
        .spawn_publication_publisher_supervised(cancel.clone())
        .expect("supervised running entry point remains configured");
    let (reader, task) = supervised.into_parts();
    wait_ready(&reader).await;
    assert_delivered(
        context.platform_pool,
        context.js,
        context.stream_name,
        t,
        context.prefix,
        &owner,
    )
    .await;
    cancel.cancel();
    task.await.expect("supervised running task joins");
    assert_eq!(
        reader.snapshot().task,
        proxima_outbox_nats::PublisherTaskState::Stopped
    );
    wait_for("running supervised publisher bridge release", || {
        (context.proxy.active_bridges() == 0).then_some(())
    })
    .await;
}

async fn create_stream(url: &str, name: &str, subject: &str) -> async_nats::jetstream::Context {
    let client = admin_client(url).await;
    assert!(
        client.server_info().connect_urls.is_empty(),
        "the test broker must not advertise alternate connect URLs while the proxy is in use"
    );
    let js = async_nats::jetstream::new(client);
    js.create_stream(StreamConfig {
        name: name.to_owned(),
        subjects: vec![format!("{subject}.>")],
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
    .expect("fixture stream provisioned by admin client");
    js
}

#[tokio::test]
async fn publisher_health_sidecar_fixture_captures_against_real_pg() {
    if std::env::var_os("PROXIMA_TEST_PG_URL").is_none() {
        assert!(
            std::env::var("CI").as_deref() != Ok("true"),
            "PROXIMA_TEST_PG_URL required under CI=true"
        );
        eprintln!("skipping publisher health PG fixture: PROXIMA_TEST_PG_URL is unset");
        return;
    }

    let db_name = proxima_pg_testkit::unique_db_name("publisher_health_fixture");
    proxima_pg_testkit::create_db(&db_name)
        .await
        .expect("PG fixture database");
    let (runtime_url, platform_url) = proxima_pg_testkit::split_role_urls(&db_name)
        .await
        .expect("split fixture roles");
    let platform_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&platform_url)
        .await
        .expect("platform pool");
    sqlx::query("SELECT set_config('app.proxima_scope', 'platform', false)")
        .execute(&platform_pool)
        .await
        .expect("platform scope");
    let mut built = None;
    let outcome = std::panic::AssertUnwindSafe(async {
        let owner = company_owner(Uuid::now_v7());
        built = Some(
            Proxima::<PublisherApp>::app()
                .database_url(runtime_url)
                .platform_database_url(platform_url)
                .owner(owner)
                .tool_scope(ToolScope::All)
                .allow_insecure_single_owner()
                .publication(PublicationConfig::new(
                    PublicationSource::new("urn:proxima:publisher-health-tests")
                        .expect("absolute source URI"),
                ))
                .build()
                .await
                .expect("publisher health fixture builds and migrates its sidecar"),
        );
        let runtime = built.as_ref().expect("fixture was built");
        let authz = runtime.single_owner_authz().expect("single owner authz");
        let id = capture(&runtime.engine(), &authz, owner, "fixture-capture").await;
        let state: String = sqlx::query_scalar(
            "SELECT state::text FROM proxima_core.publication_outbox WHERE t = $1",
        )
        .bind(id)
        .fetch_one(&platform_pool)
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
async fn supervised_and_legacy_facades_publish_and_release_the_client() {
    let Some(nats_url) = configured_url() else {
        return;
    };
    let events = install_event_buffer();
    let nats_address = socket_address(&nats_url);
    let proxy = PublisherProxy::bind(nats_address).await;
    proxy.pause();

    proxy
        .run(async |proxy| {
            let stream_name = format!("PH_{}", Uuid::now_v7().simple());
            let prefix = format!("publisher_health_{}", Uuid::now_v7().simple());
            let js = create_stream(&nats_url, &stream_name, &prefix).await;
            let db_name = proxima_pg_testkit::unique_db_name("publisher_health");
            proxima_pg_testkit::create_db(&db_name)
                .await
                .expect("PG fixture database");
            let (database_url, platform_database_url) =
                proxima_pg_testkit::split_role_urls(&db_name)
                    .await
                    .expect("split fixture roles");
            let platform_pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .connect(&platform_database_url)
                .await
                .expect("platform pool");
            sqlx::query("SELECT set_config('app.proxima_scope', 'platform', false)")
                .execute(&platform_pool)
                .await
                .expect("platform scope");
            let context = PublisherTestContext {
                proxy,
                js: &js,
                stream_name: &stream_name,
                prefix: &prefix,
                nats_url: &nats_url,
                platform_pool: &platform_pool,
            };
            let first_owner = company_owner(Uuid::now_v7());
            let lookup = runtime_lookup(proxy.url(), prefix.clone());
            let built = Proxima::<PublisherApp>::app()
                .from_lookup(lookup)
                .expect("runtime config")
                .database_url(database_url.clone())
                .platform_database_url(platform_database_url.clone())
                .owner(first_owner)
                .tool_scope(ToolScope::All)
                .allow_insecure_single_owner()
                .publication(PublicationConfig::new(
                    PublicationSource::new("urn:proxima:publisher-health-tests")
                        .expect("absolute source URI"),
                ))
                .build()
                .await
                .expect("Proxima build");
            exercise_built_supervised(&context, &built, first_owner, &events).await;
            exercise_built_legacy(&context, &built, first_owner).await;
            built.shutdown();

            let running_owner = company_owner(Uuid::now_v7());
            let running =
                start_running_proxima(&context, database_url, platform_database_url, running_owner)
                    .await;
            exercise_running_legacy(&context, &running, running_owner).await;
            exercise_running_supervised(&context, &running, running_owner).await;
            running.shutdown().await;

            {
                let expected = events.lock().expect("event buffer lock");
                assert!(
                    expected
                        .iter()
                        .any(|event| event.contains("failure_category=Connect")),
                    "real paused proxy must emit its fixed connect category: {expected:?}"
                );
            }
            let _ = proxima_pg_testkit::drop_db(&db_name).await;
            js.delete_stream(&stream_name)
                .await
                .expect("fixture stream teardown");
        })
        .await;
}
