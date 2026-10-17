//! Runtime features (docs/configuration.md §Runtime features): IF a feature's config is
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
    AppInfo, BootReport, DurableIntake, Feature, FeatureControl, FeatureControlError, FeatureState,
    FeatureStatus, FlavorApp, Intake, IntakeError, Proxima, ReceivedEvent, ServedSurface,
    company_owner,
};
use proxima_core::publication::{PublicationConfig, PublicationSource};
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::{
    AuthError, AuthPath, Authenticator, AuthzContext, Credentials, OwnerId, Role, UserId,
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

/// An intake that records nothing: enough to start the outbox publisher.
#[derive(Debug)]
struct DiscardIntake;

#[async_trait]
impl DurableIntake for DiscardIntake {
    async fn accept(&self, _event: &ReceivedEvent) -> Result<Intake, IntakeError> {
        Ok(Intake::Accepted)
    }
}

#[derive(Debug)]
struct StubAuth {
    subject: UserId,
    owner: OwnerId,
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

/// Every feature is started.
fn assert_configured_started(report: &BootReport) {
    assert_all(report, FeatureState::Started);
}

/// Only the two workers that claim queue rows through the runtime's gate have
/// a control; every other feature, started or off, has none. Each control
/// quiesces and resumes. Returns the handles for the after-shutdown check.
async fn exercise_controls(
    control: impl Fn(Feature) -> Option<FeatureControl>,
) -> Vec<FeatureControl> {
    let mut controls = Vec::new();
    for feature in Feature::ALL {
        let controlled = match feature {
            Feature::EmbeddingWorker | Feature::OutboxPublisher => true,
            Feature::Mcp | Feature::FlavorWorkers | Feature::PublishedRecordPrune => false,
        };
        let handle = control(feature);
        assert_eq!(handle.is_some(), controlled, "{feature}");
        controls.extend(handle);
    }
    for handle in &controls {
        let feature = handle.feature();
        assert_eq!(handle.status(), FeatureStatus::Running, "{feature}");
        tokio::time::timeout(Duration::from_secs(10), handle.quiesce())
            .await
            .unwrap_or_else(|_| panic!("{feature} quiesces without waiting for anything"))
            .unwrap_or_else(|err| panic!("{feature}: {err}"));
        assert_eq!(handle.status(), FeatureStatus::Quiescent, "{feature}");
        assert_eq!(handle.resume(), Ok(()), "{feature}");
        assert_eq!(handle.status(), FeatureStatus::Running, "{feature}");
    }
    controls
}

/// After shutdown every handle reports the feature stopped.
fn assert_controls_stopped(controls: &[FeatureControl]) {
    for handle in controls {
        assert_eq!(
            handle.status(),
            FeatureStatus::Stopped,
            "{}",
            handle.feature()
        );
        assert_eq!(handle.resume(), Err(FeatureControlError::Stopped));
    }
}

#[tokio::test]
async fn an_unconfigured_boot_starts_nothing_and_says_why_per_feature() {
    let db = SplitRoleDb::create("proxima_features_off", &[])
        .await
        .expect("PG required");
    let captured = captured();

    let built = Proxima::<BareApp>::app()
        .database_url(db.runtime_url())
        .store_number(0)
        .platform_database_url(db.platform_url())
        .owner(company_owner(Uuid::now_v7()))
        .served_surface(ServedSurface::All)
        .build()
        .await
        .expect("bare boot");

    let report = built.boot_report().clone();
    assert_all(&report, FeatureState::Off);
    assert!(built.service().is_none(), "no bind address, no MCP router");
    for feature in Feature::ALL {
        assert!(
            built.feature_control(feature).is_none(),
            "{feature} is off: nothing to control"
        );
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
    assert!(reason(Feature::OutboxPublisher).contains("outbox_intake"));
    assert!(reason(Feature::PublishedRecordPrune).contains("no retention"));

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
        Proxima::<WorkerApp>::app()
            .database_url(db.runtime_url())
            .store_number(0)
            .platform_database_url(db.platform_url())
            .owner(owner)
            .authenticator(Arc::new(StubAuth { subject, owner }))
            .served_surface(ServedSurface::All)
            .mcp_bind("127.0.0.1:0".parse().expect("loopback bind"))
            .embed_client(Arc::new(ConstantEmbedding::prefixed(
                "features-embed",
                &[0.25, 0.5, 0.75],
            )))
            // An intake makes goal changes listenable, which needs a source.
            .publication(PublicationConfig::new(
                PublicationSource::new("urn:proxima:features-test").expect("publication source"),
            ))
            .outbox_intake(Arc::new(DiscardIntake))
            .published_retention(Duration::from_mins(1))
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
    let controls = exercise_controls(|feature| built.feature_control(feature)).await;
    tokio::time::timeout(Duration::from_secs(10), built.shutdown())
        .await
        .expect("build() shutdown joins every task");
    assert_eq!(WORKERS_STOPPED.load(Ordering::SeqCst), 1, "worker joined");
    assert_controls_stopped(&controls);

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
    let controls = exercise_controls(|feature| running.feature_control(feature)).await;
    tokio::time::timeout(Duration::from_secs(10), running.shutdown())
        .await
        .expect("run() shutdown joins every task");
    assert_eq!(WORKERS_STOPPED.load(Ordering::SeqCst), 2, "worker joined");
    assert_controls_stopped(&controls);
}

/// The prune rides on the publisher: a retention alone starts nothing.
#[tokio::test]
async fn a_retention_without_a_publisher_is_off_and_says_so() {
    let db = SplitRoleDb::create("proxima_features_prune", &[])
        .await
        .expect("PG required");
    let built = Proxima::<BareApp>::app()
        .database_url(db.runtime_url())
        .store_number(0)
        .platform_database_url(db.platform_url())
        .owner(company_owner(Uuid::now_v7()))
        .served_surface(ServedSurface::All)
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
