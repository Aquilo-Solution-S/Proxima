//! Runtime features: one start rule each — IF its config is present THEN it
//! starts (docs/10 §Runtime features).
//!
//! The runtime decides every feature once per boot, in `build()` and `run()`
//! alike, logs one INFO line per decision, owns the tasks it started and
//! joins them on shutdown. A half-configured feature never reaches this
//! module: resolve refuses it before storage.

use std::sync::Arc;
#[cfg(feature = "outbox-nats")]
use std::time::Duration;

use proxima_core::{Engine, FlavorServices};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::RuntimeConfig;
use crate::bundle::FlavorBundle;
use crate::workers::{FlavorWorker, FlavorWorkerContext};

#[cfg(feature = "outbox-nats")]
use proxima_core::storage_ports::publication::{
    OriginScope, PublicationOriginEligibilityPort, PublicationOutboxPort, PublicationRetentionPort,
};

/// One runtime feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    /// Starts when a bind address is configured (`PROXIMA_MCP_BIND` /
    /// `mcp_bind`). `run()` listens; `build()` hands the router to the host.
    Mcp,
    /// Starts when a linked flavor contributes a worker.
    FlavorWorkers,
    /// Starts when an embedding client or router is configured: startup
    /// reconcile, then drain.
    EmbeddingWorker,
    /// Starts when a broker is configured (`PROXIMA_NATS_URL` / `nats`).
    OutboxPublisher,
    /// Starts when the publisher runs AND a retention horizon is configured
    /// (`PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS` / `published_retention`).
    PublishedRecordPrune,
    /// Starts when a cleaner section is configured
    /// (`PROXIMA_COPY_CLEANER_URL` / `copy_cleaner`).
    CopyCleaner,
}

impl Feature {
    /// Every feature, in boot-report order.
    pub const ALL: [Self; 6] = [
        Self::Mcp,
        Self::FlavorWorkers,
        Self::EmbeddingWorker,
        Self::OutboxPublisher,
        Self::PublishedRecordPrune,
        Self::CopyCleaner,
    ];

    /// The `feature=` value of this feature's boot-report line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::FlavorWorkers => "flavor-workers",
            Self::EmbeddingWorker => "embedding-worker",
            Self::OutboxPublisher => "outbox-publisher",
            Self::PublishedRecordPrune => "published-record-prune",
            Self::CopyCleaner => "copy-cleaner",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Mcp => 0,
            Self::FlavorWorkers => 1,
            Self::EmbeddingWorker => 2,
            Self::OutboxPublisher => 3,
            Self::PublishedRecordPrune => 4,
            Self::CopyCleaner => 5,
        }
    }
}

impl std::fmt::Display for Feature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// What the start rule decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureState {
    Started,
    Off,
}

impl std::fmt::Display for FeatureState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Started => "started",
            Self::Off => "off",
        })
    }
}

/// One feature's decision and the rule that made it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureDecision {
    pub feature: Feature,
    pub state: FeatureState,
    /// The rule that decided, naming its config key: the `reason=` value.
    pub reason: String,
}

impl FeatureDecision {
    fn started(feature: Feature, reason: impl Into<String>) -> Self {
        Self {
            feature,
            state: FeatureState::Started,
            reason: reason.into(),
        }
    }

    fn off(feature: Feature, reason: impl Into<String>) -> Self {
        Self {
            feature,
            state: FeatureState::Off,
            reason: reason.into(),
        }
    }

    pub(crate) fn mcp_off() -> Self {
        Self::off(
            Feature::Mcp,
            "no bind address configured (PROXIMA_MCP_BIND / mcp_bind)",
        )
    }

    pub(crate) fn mcp_listening(bound: std::net::SocketAddr) -> Self {
        Self::started(
            Feature::Mcp,
            format!("bind address configured (PROXIMA_MCP_BIND / mcp_bind); listening on {bound}"),
        )
    }

    pub(crate) fn mcp_router_handed(bind: std::net::SocketAddr) -> Self {
        Self::started(
            Feature::Mcp,
            format!(
                "bind address {bind} configured (PROXIMA_MCP_BIND / mcp_bind); build() binds no \
                 listener, BuiltProxima::service carries the router"
            ),
        )
    }
}

/// Every feature's decision for one boot, in [`Feature::ALL`] order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootReport {
    decisions: [FeatureDecision; 6],
}

impl BootReport {
    /// This boot's decision for `feature`.
    #[must_use]
    pub const fn get(&self, feature: Feature) -> &FeatureDecision {
        &self.decisions[feature.index()]
    }

    #[must_use]
    pub const fn decisions(&self) -> &[FeatureDecision] {
        &self.decisions
    }

    /// One INFO line per feature:
    /// `feature=<name> state=started|off reason=<rule>`.
    fn log(&self) {
        for decision in &self.decisions {
            tracing::info!(
                feature = %decision.feature,
                state = %decision.state,
                reason = %decision.reason,
            );
        }
    }
}

/// What starting the features needs from boot.
pub(crate) struct FeatureInputs<'a> {
    pub(crate) engine: &'a Arc<Engine>,
    pub(crate) services: &'a FlavorServices,
    pub(crate) cancel: &'a CancellationToken,
    pub(crate) config: &'a RuntimeConfig,
    #[cfg(feature = "outbox-nats")]
    pub(crate) publication: PublicationHandles,
}

/// The host-only publication handles, moved into the tasks that use them.
#[cfg(feature = "outbox-nats")]
pub(crate) struct PublicationHandles {
    pub(crate) outbox: Arc<dyn PublicationOutboxPort>,
    pub(crate) retention: Arc<dyn PublicationRetentionPort>,
    pub(crate) eligibility: Arc<dyn PublicationOriginEligibilityPort>,
    pub(crate) origin_scope: OriginScope,
}

/// The started features: their decisions and the tasks the runtime owns.
pub(crate) struct Features {
    pub(crate) report: BootReport,
    workers: Vec<FlavorWorker>,
    embedding: Option<JoinHandle<()>>,
    #[cfg(feature = "outbox-nats")]
    publisher: Option<(proxima_outbox_nats::PublisherHealthReader, JoinHandle<()>)>,
    #[cfg(feature = "outbox-nats")]
    cleaner: Option<(proxima_outbox_nats::CopyCleanerHealthReader, JoinHandle<()>)>,
}

impl std::fmt::Debug for Features {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Features")
            .field("report", &self.report)
            .field(
                "workers",
                &self
                    .workers
                    .iter()
                    .map(|worker| worker.name)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Features {
    /// The running publisher's health; `None` when it is off.
    #[cfg(feature = "outbox-nats")]
    pub(crate) fn publisher_health(&self) -> Option<proxima_outbox_nats::PublisherHealthReader> {
        self.publisher.as_ref().map(|(reader, _)| reader.clone())
    }

    /// The running cleaner's health; `None` when it is off.
    #[cfg(feature = "outbox-nats")]
    pub(crate) fn copy_cleaner_health(
        &self,
    ) -> Option<proxima_outbox_nats::CopyCleanerHealthReader> {
        self.cleaner.as_ref().map(|(reader, _)| reader.clone())
    }

    /// Join every started task. The caller cancelled their token first.
    pub(crate) async fn join(self) {
        for worker in self.workers {
            if let Err(err) = worker.handle.await {
                tracing::warn!(worker = worker.name, error = %err, "flavor worker join failed");
            }
        }
        if let Some(embedding) = self.embedding
            && let Err(err) = embedding.await
        {
            tracing::warn!(error = %err, "embedding worker join failed");
        }
        #[cfg(feature = "outbox-nats")]
        if let Some((_, publisher)) = self.publisher
            && let Err(err) = publisher.await
        {
            tracing::warn!(error = %err, "outbox publisher join failed");
        }
        #[cfg(feature = "outbox-nats")]
        if let Some((_, cleaner)) = self.cleaner
            && let Err(err) = cleaner.await
        {
            tracing::warn!(error = %err, "copy cleaner join failed");
        }
    }
}

/// Decide every feature, start the ones whose config is present, and log
/// the boot report. Infallible on purpose: it runs after the last fallible
/// boot step, so an early return can never leave a task running detached.
/// Every task observes a child of `cancel`, so none can trigger shutdown.
pub(crate) fn start<A: FlavorBundle>(mcp: FeatureDecision, inputs: &FeatureInputs<'_>) -> Features {
    let worker_ctx = FlavorWorkerContext {
        engine: inputs.engine.clone(),
        cancel: inputs.cancel.child_token(),
        services: inputs.services.clone(),
    };
    let workers = A::spawn_workers(&worker_ctx);
    let workers_decision = if workers.is_empty() {
        FeatureDecision::off(
            Feature::FlavorWorkers,
            "no linked flavor contributes a worker",
        )
    } else {
        let names: Vec<&str> = workers.iter().map(|worker| worker.name).collect();
        FeatureDecision::started(
            Feature::FlavorWorkers,
            format!("linked flavors contribute {}", names.join(", ")),
        )
    };

    let (embedding, embedding_decision) = if inputs.engine.embedding_router().is_some() {
        (
            Some(spawn_embedding_worker(
                inputs.engine.clone(),
                inputs.cancel.child_token(),
            )),
            FeatureDecision::started(
                Feature::EmbeddingWorker,
                "an embedding client or router is configured (embed_client / embedding_router)",
            ),
        )
    } else {
        (
            None,
            FeatureDecision::off(
                Feature::EmbeddingWorker,
                "no embedding client or router configured (embed_client / embedding_router)",
            ),
        )
    };

    #[cfg(feature = "outbox-nats")]
    {
        let (publisher, publisher_decision, prune_decision) = start_publisher(inputs);
        let (cleaner, cleaner_decision) = start_cleaner(inputs);
        let report = BootReport {
            decisions: [
                mcp,
                workers_decision,
                embedding_decision,
                publisher_decision,
                prune_decision,
                cleaner_decision,
            ],
        };
        report.log();
        Features {
            report,
            workers,
            embedding,
            publisher,
            cleaner,
        }
    }
    #[cfg(not(feature = "outbox-nats"))]
    {
        let _ = inputs.config;
        let uncompiled = "built without the outbox-nats cargo feature";
        let report = BootReport {
            decisions: [
                mcp,
                workers_decision,
                embedding_decision,
                FeatureDecision::off(Feature::OutboxPublisher, uncompiled),
                FeatureDecision::off(Feature::PublishedRecordPrune, uncompiled),
                FeatureDecision::off(Feature::CopyCleaner, uncompiled),
            ],
        };
        report.log();
        Features {
            report,
            workers,
            embedding,
        }
    }
}

#[cfg(feature = "outbox-nats")]
type StartedPublisher = Option<(proxima_outbox_nats::PublisherHealthReader, JoinHandle<()>)>;

/// The publisher, and the prune that rides on it.
#[cfg(feature = "outbox-nats")]
fn start_publisher(
    inputs: &FeatureInputs<'_>,
) -> (StartedPublisher, FeatureDecision, FeatureDecision) {
    let horizon = inputs.config.published_retention;
    let Some(mut nats) = inputs.config.nats.clone() else {
        let prune = match horizon {
            Some(_) => "a retention is configured but the outbox publisher is off",
            None => {
                "no retention configured (PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS / \
                     published_retention)"
            }
        };
        return (
            None,
            FeatureDecision::off(
                Feature::OutboxPublisher,
                "no broker configured (PROXIMA_NATS_URL / nats)",
            ),
            FeatureDecision::off(Feature::PublishedRecordPrune, prune),
        );
    };
    // Stamped here, from the same database that answers eligibility, never
    // from configuration: see `Booted::origin_scope`.
    nats.origin_scope = Some(inputs.publication.origin_scope);
    let retention = horizon.map(|older_than| PublicationRetention {
        port: inputs.publication.retention.clone(),
        older_than,
    });
    let prune = match horizon {
        Some(horizon) => FeatureDecision::started(
            Feature::PublishedRecordPrune,
            format!(
                "the outbox publisher runs and a retention of {}s is configured \
                 (PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS / published_retention)",
                horizon.as_secs()
            ),
        ),
        None => FeatureDecision::off(
            Feature::PublishedRecordPrune,
            "no retention configured (PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS / \
             published_retention)",
        ),
    };
    let supervised = spawn_publication_publisher_supervised(
        inputs.publication.outbox.clone(),
        nats,
        retention,
        inputs.cancel.child_token(),
    );
    (
        Some(supervised.into_parts()),
        FeatureDecision::started(
            Feature::OutboxPublisher,
            "a broker is configured (PROXIMA_NATS_URL / nats)",
        ),
        prune,
    )
}

#[cfg(feature = "outbox-nats")]
fn start_cleaner(
    inputs: &FeatureInputs<'_>,
) -> (
    Option<(proxima_outbox_nats::CopyCleanerHealthReader, JoinHandle<()>)>,
    FeatureDecision,
) {
    let Some(config) = inputs.config.copy_cleaner.clone() else {
        return (
            None,
            FeatureDecision::off(
                Feature::CopyCleaner,
                "no cleaner section configured (PROXIMA_COPY_CLEANER_URL / copy_cleaner)",
            ),
        );
    };
    (
        Some(proxima_outbox_nats::spawn_supervised_copy_cleaner(
            config,
            inputs.publication.eligibility.clone(),
            inputs.publication.origin_scope,
            inputs.cancel.child_token(),
        )),
        FeatureDecision::started(
            Feature::CopyCleaner,
            "a cleaner section is configured (PROXIMA_COPY_CLEANER_URL / copy_cleaner)",
        ),
    )
}

/// The configured reclaim of DELIVERED records, and the handle that can
/// perform it. Absent when the deployment keeps published records forever.
#[cfg(feature = "outbox-nats")]
struct PublicationRetention {
    port: Arc<dyn PublicationRetentionPort>,
    older_than: Duration,
}

#[cfg(feature = "outbox-nats")]
impl std::fmt::Debug for PublicationRetention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublicationRetention")
            .field("older_than", &self.older_than)
            .finish_non_exhaustive()
    }
}

/// Records one prune pass may remove.
///
/// Bounded because the statement runs beside the live write path: the
/// table has no index on `published_at` (adding one is a migration this
/// slice does not ship), so each pass is a sequential scan and must not
/// also hold row locks over an unbounded delete. A backlog larger than this
/// is reclaimed over several passes.
#[cfg(feature = "outbox-nats")]
const RETENTION_PRUNE_BATCH: u32 = 1_000;

/// How often the prune runs.
///
/// DELIBERATELY not once per drain pass. The drain polls every
/// `PROXIMA_NATS_POLL_MS` (500 ms by default), and a sequential scan at
/// that rate would cost more than the storage it reclaims — while the
/// horizon it enforces is at least a minute, so nothing becomes prunable
/// faster than this either.
#[cfg(feature = "outbox-nats")]
const RETENTION_PRUNE_INTERVAL: Duration = Duration::from_mins(1);

#[cfg(feature = "outbox-nats")]
fn spawn_publication_publisher_supervised(
    outbox: Arc<dyn PublicationOutboxPort>,
    config: proxima_outbox_nats::NatsPublisherConfig,
    retention: Option<PublicationRetention>,
    cancel: CancellationToken,
) -> proxima_outbox_nats::SupervisedPublisher {
    proxima_outbox_nats::spawn_supervised(config, outbox, cancel, move |cancel| {
        prune_published_records(retention, cancel)
    })
}

/// Reclaim delivered records older than the configured horizon, until
/// cancellation. A no-op future when no horizon is configured.
#[cfg(feature = "outbox-nats")]
async fn prune_published_records(
    retention: Option<PublicationRetention>,
    cancel: CancellationToken,
) {
    let Some(retention) = retention else {
        return;
    };
    let Some(limit) = std::num::NonZeroU32::new(RETENTION_PRUNE_BATCH) else {
        return;
    };
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(RETENTION_PRUNE_INTERVAL) => {}
        }
        match retention
            .port
            .prune_published(retention.older_than, limit)
            .await
        {
            Ok(0) => {}
            Ok(pruned) => tracing::debug!(
                pruned,
                horizon_secs = retention.older_than.as_secs(),
                "reclaimed delivered publication records"
            ),
            // Never fatal. Housekeeping that cannot run is a growing table,
            // which is an operator's problem to see; stopping the publisher
            // over it would turn it into an undelivered backlog.
            Err(_) => {
                tracing::warn!(
                    "publication retention prune failed; delivered records are retained"
                );
            }
        }
    }
}

/// Startup reconcile, then drain until cancellation. Started only when the
/// engine has an embedding router.
fn spawn_embedding_worker(engine: Arc<Engine>, cancel: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        let policy = engine.embedding_runtime_policy();
        // Boot-time catch-up, not a recurring clock: memories written while
        // no embedding client was configured never got a job (and exhausted
        // `failed` jobs stay dead), so one reconcile pass before the first
        // drain keeps a restart from leaving them silently unsearchable.
        // Recurring maintenance stays outside the process
        // (`proxima-mcp maintain-embeddings`).
        match engine
            .reconcile_embeddings(
                proxima_core::EmbeddingReconcileScope::MissingOnly,
                Some(proxima_core::EMBEDDING_RECONCILE_DEFAULT_LIMIT),
            )
            .await
        {
            Ok(outcome) if outcome.enqueued > 0 => {
                tracing::info!(
                    scanned = outcome.scanned,
                    enqueued = outcome.enqueued,
                    "startup embedding reconcile enqueued missing jobs"
                );
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(error = %err, "startup embedding reconcile failed");
            }
        }
        loop {
            if cancel.is_cancelled() {
                break;
            }
            let mut processed = 0usize;
            let mut failed = 0usize;
            loop {
                if cancel.is_cancelled() {
                    return;
                }
                match engine.drain_embedding_jobs(policy.batch_size()).await {
                    Ok(outcome) if outcome.processed > 0 => {
                        processed += outcome.processed;
                        failed += outcome.failed;
                    }
                    Ok(_) => break,
                    Err(err) => {
                        tracing::warn!(error = %err, "embedding drain failed");
                        break;
                    }
                }
            }
            if processed > 0 {
                tracing::info!(processed, failed, "drained embedding jobs");
            }
            tokio::select! {
                () = cancel.cancelled() => break,
                () = tokio::time::sleep(policy.worker_interval()) => {}
            }
        }
    })
}

#[cfg(all(test, feature = "outbox-nats"))]
#[path = "publisher_supervision_tests.rs"]
mod publisher_supervision_tests;
