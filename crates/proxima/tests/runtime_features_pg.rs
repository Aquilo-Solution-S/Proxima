//! Runtime features (docs/10 §Runtime features): IF a feature's config is
//! present THEN it starts. Each rule both ways — configured is started,
//! absent is off with its boot-report line — `build()` and `run()` start the
//! same features, and `shutdown()` stops and joins every one of them.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::ThreadId;
use std::time::Duration;

use async_trait::async_trait;
use proxima::flavor::{
    FlavorBundle, FlavorRegistry, FlavorRegistryError, FlavorWorker, FlavorWorkerContext,
    NamedMigrator,
};
use proxima::{
    AppInfo, BootReport, Feature, FeatureState, FlavorApp, Proxima, ProximaError, ToolScope,
    company_owner,
};
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::{
    AuthError, AuthPath, Authenticator, AuthzContext, Credentials, Owner, Role, UserId,
};
use proxima_pg_testkit::SplitRoleDb;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use uuid::Uuid;

/// Links no flavor and contributes no worker.
struct BareApp;

impl FlavorBundle for BareApp {
    fn register(_registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for BareApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "runtime-features-bare",
            title: "Runtime Features Bare",
            version: "1",
        }
    }
}

static WORKERS_STARTED: AtomicUsize = AtomicUsize::new(0);
static WORKERS_STOPPED: AtomicUsize = AtomicUsize::new(0);

/// Contributes one worker that runs until the runtime cancels it.
struct WorkerApp;

impl FlavorBundle for WorkerApp {
    fn register(_registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }

    fn spawn_workers(ctx: &FlavorWorkerContext) -> Vec<FlavorWorker> {
        let cancel = ctx.cancel.clone();
        WORKERS_STARTED.fetch_add(1, Ordering::SeqCst);
        vec![FlavorWorker {
            name: "until-cancelled",
            handle: tokio::spawn(async move {
                cancel.cancelled().await;
                WORKERS_STOPPED.fetch_add(1, Ordering::SeqCst);
            }),
        }]
    }
}

impl FlavorApp for WorkerApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "runtime-features-worker",
            title: "Runtime Features Worker",
            version: "1",
        }
    }
}

#[derive(Debug)]
struct StubAuth {
    subject: UserId,
    owner: Owner,
}

#[async_trait]
impl Authenticator for StubAuth {
    async fn authenticate(&self, _credentials: &Credentials) -> Result<AuthzContext, AuthError> {
        Ok(AuthzContext::for_subject_with_role(
            self.subject,
            [(self.owner, Role::admin())],
            AuthPath::HostBearer,
        ))
    }
}

/// Every event of this test binary as `(thread, "field=value ...")`.
///
/// Global, not `set_default`: a thread-scoped dispatcher races the callsite
/// interest cache against the other tests booting in parallel. Each test
/// runs on its own current-thread runtime, so the thread selects its boot.
#[derive(Clone)]
struct Captured(Arc<Mutex<Vec<(ThreadId, String)>>>);

impl<S: Subscriber> Layer<S> for Captured {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.0
            .lock()
            .expect("capture lock")
            .push((std::thread::current().id(), fields.0));
    }
}

#[derive(Default)]
struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        write!(&mut self.0, "{}={value:?} ", field.name()).expect("string write");
    }
}

static CAPTURED: OnceLock<Captured> = OnceLock::new();

fn captured() -> &'static Captured {
    CAPTURED.get_or_init(|| {
        let captured = Captured(Arc::default());
        tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(captured.clone()),
        )
        .expect("this test binary owns its tracing subscriber");
        captured
    })
}

impl Captured {
    /// This thread's boot-report line for `feature`, exactly one per boot.
    fn line(&self, feature: Feature) -> String {
        let prefix = format!("feature={feature} ");
        let thread = std::thread::current().id();
        let lines: Vec<String> = self
            .0
            .lock()
            .expect("capture lock")
            .iter()
            .filter(|(from, line)| *from == thread && line.starts_with(&prefix))
            .map(|(_, line)| line.clone())
            .collect();
        assert_eq!(
            lines.len(),
            1,
            "one boot-report line for {feature}: {lines:?}"
        );
        lines.into_iter().next().unwrap_or_default()
    }
}

fn states(report: &BootReport) -> Vec<(Feature, FeatureState)> {
    report
        .decisions()
        .iter()
        .map(|decision| (decision.feature, decision.state))
        .collect()
}

fn assert_all(report: &BootReport, state: FeatureState) {
    for decision in report.decisions() {
        assert_eq!(decision.state, state, "{decision:?}");
    }
}

/// Every feature this build can start is started.
fn assert_configured_started(report: &BootReport) {
    #[cfg(feature = "outbox-nats")]
    assert_all(report, FeatureState::Started);
    #[cfg(not(feature = "outbox-nats"))]
    for feature in [
        Feature::Mcp,
        Feature::FlavorWorkers,
        Feature::EmbeddingWorker,
    ] {
        assert_eq!(report.get(feature).state, FeatureState::Started);
    }
}

/// The health readers a runtime handle exposes for its started features.
#[cfg(feature = "outbox-nats")]
struct Health(
    proxima::PublisherHealthReader,
    proxima::CopyCleanerHealthReader,
);

#[cfg(not(feature = "outbox-nats"))]
struct Health;

#[cfg(feature = "outbox-nats")]
macro_rules! health {
    ($handle:expr) => {
        Health(
            $handle.publisher_health().expect("publisher health"),
            $handle.copy_cleaner_health().expect("cleaner health"),
        )
    };
}

#[cfg(not(feature = "outbox-nats"))]
macro_rules! health {
    ($handle:expr) => {
        Health
    };
}

impl Health {
    /// After shutdown every task reports itself stopped.
    #[cfg_attr(
        not(feature = "outbox-nats"),
        expect(clippy::unused_self, reason = "no health reader without outbox-nats")
    )]
    fn assert_stopped(&self) {
        #[cfg(feature = "outbox-nats")]
        {
            assert_eq!(self.0.snapshot().task, proxima::PublisherTaskState::Stopped);
            assert_eq!(
                self.1.snapshot().task,
                proxima::CopyCleanerTaskState::Stopped
            );
        }
    }
}

/// A broker nothing listens on: the tasks start, retry, and still observe
/// cancellation.
#[cfg(feature = "outbox-nats")]
const UNREACHABLE_BROKER: &str = "nats://127.0.0.1:1";

#[tokio::test]
async fn an_unconfigured_boot_starts_nothing_and_says_why_per_feature() {
    let db = SplitRoleDb::create("proxima_features_off", &[])
        .await
        .expect("PG required");
    let captured = captured();

    let built = Proxima::<BareApp>::app()
        .database_url(db.runtime_url())
        .platform_database_url(db.platform_url())
        .owner(company_owner(Uuid::now_v7()))
        .tool_scope(ToolScope::All)
        .build()
        .await
        .expect("bare boot");

    let report = built.boot_report().clone();
    assert_all(&report, FeatureState::Off);
    assert!(built.service().is_none(), "no bind address, no MCP router");
    #[cfg(feature = "outbox-nats")]
    {
        assert!(built.publisher_health().is_none());
        assert!(built.copy_cleaner_health().is_none());
    }
    for decision in report.decisions() {
        let line = captured.line(decision.feature);
        assert!(line.contains("state=off "), "{line}");
        assert!(
            line.contains(&format!("reason={} ", decision.reason)),
            "{line}"
        );
    }
    let reason = |feature| report.get(feature).reason.clone();
    assert!(reason(Feature::Mcp).contains("PROXIMA_MCP_BIND"));
    assert!(reason(Feature::FlavorWorkers).contains("no linked flavor"));
    assert!(reason(Feature::EmbeddingWorker).contains("embed_client"));
    #[cfg(feature = "outbox-nats")]
    {
        assert!(reason(Feature::OutboxPublisher).contains("PROXIMA_NATS_URL"));
        assert!(reason(Feature::PublishedRecordPrune).contains("no retention"));
        assert!(reason(Feature::CopyCleaner).contains("PROXIMA_COPY_CLEANER_URL"));
    }
    #[cfg(not(feature = "outbox-nats"))]
    for feature in [
        Feature::OutboxPublisher,
        Feature::PublishedRecordPrune,
        Feature::CopyCleaner,
    ] {
        assert!(
            reason(feature).contains("outbox-nats"),
            "{}",
            reason(feature)
        );
    }

    tokio::time::timeout(Duration::from_secs(10), built.shutdown())
        .await
        .expect("shutdown with nothing started");
}

#[tokio::test]
async fn build_and_run_start_every_configured_feature_and_shutdown_joins_them() {
    let db = SplitRoleDb::create("proxima_features_on", &[])
        .await
        .expect("PG required");
    let owner = company_owner(Uuid::now_v7());
    let subject = UserId::new(Uuid::now_v7());
    let app = || {
        let app = Proxima::<WorkerApp>::app()
            .database_url(db.runtime_url())
            .platform_database_url(db.platform_url())
            .owner(owner)
            .authenticator(Arc::new(StubAuth { subject, owner }))
            .tool_scope(ToolScope::All)
            .mcp_bind("127.0.0.1:0".parse().expect("loopback bind"))
            .embed_client(Arc::new(ConstantEmbedding::prefixed(
                "features-embed",
                &[0.25, 0.5, 0.75],
            )));
        #[cfg(feature = "outbox-nats")]
        let app = app
            .nats(proxima::NatsPublisherConfig::new(UNREACHABLE_BROKER).expect("broker url"))
            .published_retention(Duration::from_mins(1))
            .copy_cleaner(proxima::JetStreamCopyCleanerConfig::new(UNREACHABLE_BROKER));
        app
    };

    // build(): every feature starts; MCP is assembled and handed over.
    let built = app().build().await.expect("configured build");
    let built_report = built.boot_report().clone();
    assert_configured_started(&built_report);
    assert!(built.service().is_some(), "build() hands the router over");
    assert!(
        built_report
            .get(Feature::Mcp)
            .reason
            .contains("binds no listener")
    );
    assert!(
        built_report
            .get(Feature::FlavorWorkers)
            .reason
            .contains("until-cancelled")
    );
    assert_eq!(
        WORKERS_STARTED.load(Ordering::SeqCst),
        1,
        "build() starts workers"
    );
    let health = health!(built);
    tokio::time::timeout(Duration::from_secs(10), built.shutdown())
        .await
        .expect("build() shutdown joins every task");
    assert_eq!(WORKERS_STOPPED.load(Ordering::SeqCst), 1, "worker joined");
    health.assert_stopped();

    // run(): the same features, MCP now listening.
    let running = app().run().await.expect("configured run");
    assert_eq!(states(running.boot_report()), states(&built_report));
    assert!(running.mcp_addr().is_some(), "run() binds");
    assert!(
        running
            .boot_report()
            .get(Feature::Mcp)
            .reason
            .contains("listening on")
    );
    assert_eq!(
        WORKERS_STARTED.load(Ordering::SeqCst),
        2,
        "run() starts workers"
    );
    let health = health!(running);
    tokio::time::timeout(Duration::from_secs(10), running.shutdown())
        .await
        .expect("run() shutdown joins every task");
    assert_eq!(WORKERS_STOPPED.load(Ordering::SeqCst), 2, "worker joined");
    health.assert_stopped();
}

/// The prune rides on the publisher: a retention alone starts nothing.
#[cfg(feature = "outbox-nats")]
#[tokio::test]
async fn a_retention_without_a_publisher_is_off_and_says_so() {
    let db = SplitRoleDb::create("proxima_features_prune", &[])
        .await
        .expect("PG required");
    let built = Proxima::<BareApp>::app()
        .database_url(db.runtime_url())
        .platform_database_url(db.platform_url())
        .owner(company_owner(Uuid::now_v7()))
        .tool_scope(ToolScope::All)
        .published_retention(Duration::from_mins(1))
        .build()
        .await
        .expect("retention-only boot");
    let prune = built
        .boot_report()
        .get(Feature::PublishedRecordPrune)
        .clone();
    assert_eq!(prune.state, FeatureState::Off);
    assert!(
        prune.reason.contains("publisher is off"),
        "{}",
        prune.reason
    );
    tokio::time::timeout(Duration::from_secs(10), built.shutdown())
        .await
        .expect("shutdown");
}

fn refusal(lookup: &[(&str, &str)]) -> String {
    let lookup: Vec<(String, String)> = lookup
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    match Proxima::<BareApp>::app().from_lookup(move |key| {
        lookup
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.clone())
    }) {
        Err(ProximaError::Config(message)) => message,
        Err(other) => panic!("expected a config refusal, got {other}"),
        Ok(_) => panic!("a half-configured feature must refuse to boot"),
    }
}

/// A section that names no broker refuses boot and names the missing key.
#[cfg(feature = "outbox-nats")]
#[test]
fn a_half_configured_feature_refuses_boot_naming_the_missing_part() {
    let cleaner = refusal(&[("PROXIMA_COPY_CLEANER_TOKEN", "secret")]);
    assert!(cleaner.contains("PROXIMA_COPY_CLEANER_URL"), "{cleaner}");
    assert!(cleaner.contains("PROXIMA_COPY_CLEANER_TOKEN"), "{cleaner}");
    let publisher = refusal(&[("PROXIMA_NATS_SUBJECT_PREFIX", "proxima")]);
    assert!(publisher.contains("PROXIMA_NATS_URL"), "{publisher}");
}

/// A broker this binary was built without cannot be honoured.
#[cfg(not(feature = "outbox-nats"))]
#[test]
fn a_broker_without_the_outbox_nats_build_refuses_boot() {
    for url in ["PROXIMA_NATS_URL", "PROXIMA_COPY_CLEANER_URL"] {
        let message = refusal(&[(url, "nats://127.0.0.1:4222")]);
        assert!(message.contains(url), "{message}");
        assert!(message.contains("outbox-nats"), "{message}");
    }
}
