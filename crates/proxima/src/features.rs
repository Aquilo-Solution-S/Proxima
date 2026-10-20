//! Runtime features: one start rule each — IF its config is present THEN it
//! starts (docs/configuration.md §Runtime features).
//!
//! The runtime decides every feature once per boot, in `build()` and `run()`
//! alike, logs one INFO line per decision, owns the tasks it started and
//! joins them on shutdown. A half-configured feature never reaches this
//! module: resolve refuses it before storage.

use std::sync::Arc;
use std::time::Duration;

use proxima_core::quiesce::Checkpoint;
use proxima_core::storage_ports::publication::{PublicationOutboxPort, PublicationRetentionPort};
use proxima_core::write_wake::WriteWake;
use proxima_core::{
    Engine, FeatureControlError, FeatureStatus, FlavorServices, QuiesceGate, StorageError,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::RuntimeConfig;
use crate::bundle::FlavorBundle;
use crate::in_process_outbox::{self, Drain, Proceed, Settings};
use crate::object_maintenance::ObjectPurgeHealth;
use crate::workers::{FlavorWorker, FlavorWorkerContext};

/// One runtime feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// Starts when a bind address is configured (`PROXIMA_MCP_BIND` /
    /// `mcp_bind`). `run()` listens; `build()` hands the router to the host.
    Mcp,
    /// Starts when a linked flavor contributes a worker.
    FlavorWorkers,
    /// Starts when an embedding client or router is configured: startup
    /// reconcile, then drain.
    EmbeddingWorker,
    /// Starts when an outbox consumer is configured (`outbox_intake` /
    /// `outbox_consumer`): the captured outbox is delivered to each, in this
    /// process, by one drain per consumer.
    OutboxPublisher,
    /// Starts when the publisher runs AND a retention horizon is configured
    /// (`PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS` / `published_retention`).
    PublishedRecordPrune,
    /// Starts when an object store is configured (`PROXIMA_S3_*` / `s3`),
    /// unless its interval is zero (`PROXIMA_OBJECT_PURGE_INTERVAL_SECS` /
    /// `object_purge_interval`): sweeps expired uploads and drains due
    /// object purge debt, in this process, under the storage maintenance
    /// lock.
    ObjectPurge,
}

impl Feature {
    /// Every feature, in boot-report order.
    pub const ALL: [Self; 6] = [
        Self::Mcp,
        Self::FlavorWorkers,
        Self::EmbeddingWorker,
        Self::OutboxPublisher,
        Self::PublishedRecordPrune,
        Self::ObjectPurge,
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
            Self::ObjectPurge => "object-purge",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Mcp => 0,
            Self::FlavorWorkers => 1,
            Self::EmbeddingWorker => 2,
            Self::OutboxPublisher => 3,
            Self::PublishedRecordPrune => 4,
            Self::ObjectPurge => 5,
        }
    }
}

/// The pause and resume handle of one started claim-based feature.
///
/// [`BuiltProxima::feature_control`](crate::BuiltProxima::feature_control) and
/// [`RunningProxima::feature_control`](crate::RunningProxima::feature_control)
/// return it for the [`Feature::EmbeddingWorker`], the
/// [`Feature::OutboxPublisher`] and the [`Feature::ObjectPurge`] only. It is owned and cheap to clone, so a host
/// can move one into a signal handler and still call `shutdown(self)` later.
///
/// A host cannot build one; it only receives them from the runtime:
///
/// ```compile_fail,E0624
/// let (gate, _worker) = proxima_core::QuiesceGate::new();
/// let _ = proxima::FeatureControl::new(proxima::Feature::EmbeddingWorker, gate);
/// ```
///
/// The publisher runs one drain per consumer; its handle pauses, resumes and
/// reports them together.
#[derive(Debug, Clone)]
pub struct FeatureControl {
    feature: Feature,
    gates: Vec<QuiesceGate>,
}

impl FeatureControl {
    pub(crate) const fn new(feature: Feature, gates: Vec<QuiesceGate>) -> Self {
        Self { feature, gates }
    }

    /// The feature this handle controls.
    #[must_use]
    pub const fn feature(&self) -> Feature {
        self.feature
    }

    /// Stop the feature from claiming, and wait until it holds nothing.
    ///
    /// Closes the gate, then resolves once the worker task itself has parked:
    /// no claim starts after it returns, and the embedding worker's current
    /// `drain_embedding_jobs` call, or the publisher's record in flight, has
    /// settled. Concurrent calls resolve together; on a quiescent feature it
    /// resolves at once. The task stays alive.
    ///
    /// Quiescent is not drained. Serving continues, so writes accepted now
    /// still enqueue embedding jobs and outbox records, and they wait for
    /// [`Self::resume`]. A release that failed leaves its lease to expire
    /// (the outbox drain's 30 s lease; the embedding policy's stale-claim
    /// timeout), so quiescent does not mean no row is `claimed`. A host that
    /// calls `Engine::drain_embedding_jobs` itself is not gated. Dropping the
    /// future does not reopen the gate.
    ///
    /// # Errors
    ///
    /// [`FeatureControlError::Stopped`] when the feature has stopped, also
    /// while the call is pending; [`FeatureControlError::Resumed`] when
    /// [`Self::resume`] reopens the gate while the call is pending.
    pub async fn quiesce(&self) -> Result<(), FeatureControlError> {
        // Polled together, in place: the first poll closes every gate, so
        // the status reads `Quiescing` at once and the pause costs the
        // slowest drain's record in flight, not the sum of them.
        let mut pauses: Vec<_> = self
            .gates
            .iter()
            .map(|gate| (Box::pin(gate.quiesce()), None))
            .collect();
        std::future::poll_fn(|cx| {
            let mut waiting = false;
            for (pause, outcome) in &mut pauses {
                if outcome.is_none() {
                    match pause.as_mut().poll(cx) {
                        std::task::Poll::Ready(settled) => *outcome = Some(settled),
                        std::task::Poll::Pending => waiting = true,
                    }
                }
            }
            if waiting {
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(())
            }
        })
        .await;
        pauses
            .into_iter()
            .filter_map(|(_, outcome)| outcome)
            .fold(Ok(()), Result::and)
    }

    /// Reopen the gate and wake the worker, which claims again at once. It
    /// does not repeat the embedding worker's startup reconcile. `Ok` and no
    /// change on a running feature.
    ///
    /// # Errors
    ///
    /// [`FeatureControlError::Stopped`] when the feature has stopped.
    pub fn resume(&self) -> Result<(), FeatureControlError> {
        // Every gate reopens even when one has stopped.
        self.gates
            .iter()
            .map(QuiesceGate::resume)
            .fold(Ok(()), Result::and)
    }

    /// Where the feature is in its pause/resume cycle. Says nothing about
    /// queue depth. Across several drains: `Stopped` when any has stopped,
    /// `Running` or `Quiescent` when all are, `Quiescing` in between.
    #[must_use]
    pub fn status(&self) -> FeatureStatus {
        let statuses: Vec<FeatureStatus> = self.gates.iter().map(QuiesceGate::status).collect();
        if statuses.contains(&FeatureStatus::Stopped) {
            FeatureStatus::Stopped
        } else if statuses
            .iter()
            .all(|status| *status == FeatureStatus::Running)
        {
            FeatureStatus::Running
        } else if statuses
            .iter()
            .all(|status| *status == FeatureStatus::Quiescent)
        {
            FeatureStatus::Quiescent
        } else {
            FeatureStatus::Quiescing
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
    pub(crate) publication: PublicationHandles,
    /// The object store and what draining it needs; `None` without one.
    pub(crate) objects: Option<ObjectPurgeHandles>,
}

/// What the object-purge feature runs over, when an object store is wired.
pub(crate) struct ObjectPurgeHandles {
    pub(crate) storage: proxima_storage_pg::PgStorage,
    pub(crate) blobs: proxima_blob_s3::CitedBlobStore,
    pub(crate) authority: Arc<proxima_core::authz::SystemAuthority>,
    pub(crate) health: ObjectPurgeHealth,
}

/// The host-only publication handles, moved into the tasks that use them.
pub(crate) struct PublicationHandles {
    pub(crate) outbox: Arc<dyn PublicationOutboxPort>,
    pub(crate) retention: Arc<dyn PublicationRetentionPort>,
}

/// The started features: their decisions and the tasks the runtime owns.
pub(crate) struct Features {
    pub(crate) report: BootReport,
    workers: Vec<FlavorWorker>,
    embedding: Option<(QuiesceGate, JoinHandle<()>)>,
    publisher: Option<Publisher>,
    object_purge: Option<(QuiesceGate, JoinHandle<()>)>,
}

/// The outbox publisher's task and one pause gate per consumer drain.
type Publisher = (Vec<QuiesceGate>, JoinHandle<()>);

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
    /// The pause control of `feature`; `None` unless it is a started worker
    /// that claims queue rows through a gate. The match names every feature,
    /// so a new one must decide whether it claims.
    pub(crate) fn feature_control(&self, feature: Feature) -> Option<FeatureControl> {
        let gates = match feature {
            Feature::EmbeddingWorker => self.embedding.as_ref().map(|(gate, _)| vec![gate.clone()]),
            Feature::OutboxPublisher => self.publisher.as_ref().map(|(gates, _)| gates.clone()),
            Feature::ObjectPurge => self
                .object_purge
                .as_ref()
                .map(|(gate, _)| vec![gate.clone()]),
            // Serving continues by definition; the other two claim no
            // queue row through the runtime's gate.
            Feature::Mcp | Feature::FlavorWorkers | Feature::PublishedRecordPrune => None,
        }?;
        Some(FeatureControl::new(feature, gates))
    }

    /// Join every started task. The caller cancelled their token first.
    pub(crate) async fn join(self) {
        for worker in self.workers {
            if let Err(err) = worker.handle.await {
                tracing::warn!(worker = worker.name, error = %err, "flavor worker join failed");
            }
        }
        if let Some((_, embedding)) = self.embedding
            && let Err(err) = embedding.await
        {
            tracing::warn!(error = %err, "embedding worker join failed");
        }
        if let Some((_, publisher)) = self.publisher
            && let Err(err) = publisher.await
        {
            tracing::warn!(error = %err, "outbox publisher join failed");
        }
        if let Some((_, object_purge)) = self.object_purge
            && let Err(err) = object_purge.await
        {
            tracing::warn!(error = %err, "object purge worker join failed");
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

    let (publisher, publisher_decision, prune_decision) = start_publisher(inputs);
    let (object_purge, object_purge_decision) = start_object_purge(inputs);
    let report = BootReport {
        decisions: [
            mcp,
            workers_decision,
            embedding_decision,
            publisher_decision,
            prune_decision,
            object_purge_decision,
        ],
    };
    report.log();
    Features {
        report,
        workers,
        embedding,
        publisher,
        object_purge,
    }
}

/// The expired-upload sweep and purge-debt drain, on by default whenever an
/// object store is configured.
fn start_object_purge(
    inputs: &FeatureInputs<'_>,
) -> (Option<(QuiesceGate, JoinHandle<()>)>, FeatureDecision) {
    let Some(objects) = &inputs.objects else {
        return (
            None,
            FeatureDecision::off(
                Feature::ObjectPurge,
                "no object store configured (PROXIMA_S3_* / s3)",
            ),
        );
    };
    let Some(interval) = inputs.config.object_purge_interval else {
        return (
            None,
            FeatureDecision::off(
                Feature::ObjectPurge,
                "an object store is configured but the interval is 0 \
                 (PROXIMA_OBJECT_PURGE_INTERVAL_SECS / object_purge_interval)",
            ),
        );
    };
    let worker = crate::object_maintenance::ObjectPurgeWorker {
        storage: objects.storage.clone(),
        blobs: objects.blobs.clone(),
        authority: objects.authority.clone(),
        interval,
        health: objects.health.clone(),
    };
    (
        Some(crate::object_maintenance::spawn_object_purge_worker(
            worker,
            inputs.cancel.child_token(),
        )),
        FeatureDecision::started(
            Feature::ObjectPurge,
            format!(
                "an object store is configured (PROXIMA_S3_* / s3); every {}s \
                 (PROXIMA_OBJECT_PURGE_INTERVAL_SECS / object_purge_interval)",
                interval.as_secs()
            ),
        ),
    )
}

/// The publisher, and the prune that rides on it.
fn start_publisher(
    inputs: &FeatureInputs<'_>,
) -> (Option<Publisher>, FeatureDecision, FeatureDecision) {
    let horizon = inputs.config.published_retention;
    let consumers = &inputs.config.outbox_consumers;
    if consumers.is_empty() {
        let prune = match horizon {
            Some(_) => "the outbox publisher is off, so no delivered record exists to prune",
            None => {
                "retention is off (PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS=0 / \
                     published_retention)"
            }
        };
        return (
            None,
            FeatureDecision::off(
                Feature::OutboxPublisher,
                "no intake configured (outbox_intake / outbox_consumer)",
            ),
            FeatureDecision::off(Feature::PublishedRecordPrune, prune),
        );
    }
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
            "retention is off (PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS=0 / \
             published_retention)",
        ),
    };
    let drains = consumers
        .iter()
        .map(|(name, intake)| {
            Drain::new(
                inputs.publication.outbox.clone(),
                name.clone(),
                intake.clone(),
                Settings::new(),
                Arc::new(Proceed),
            )
        })
        .collect();
    let publisher = in_process_outbox::spawn(drains, inputs.cancel.child_token(), move |cancel| {
        prune_published_records(retention, cancel)
    });
    let names: Vec<String> = consumers.names().map(ToString::to_string).collect();
    (
        Some(publisher),
        FeatureDecision::started(
            Feature::OutboxPublisher,
            format!(
                "an intake is configured (outbox_intake / outbox_consumer): {}",
                names.join(", ")
            ),
        ),
        prune,
    )
}

/// The configured reclaim of DELIVERED records, and the handle that can
/// perform it. Absent when the deployment keeps published records forever.
struct PublicationRetention {
    port: Arc<dyn PublicationRetentionPort>,
    older_than: Duration,
}

impl std::fmt::Debug for PublicationRetention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublicationRetention")
            .field("older_than", &self.older_than)
            .finish_non_exhaustive()
    }
}

/// Records one prune statement may remove.
///
/// Bounded because the statement runs beside the live write path and must not
/// hold row locks over an unbounded delete. A backlog larger than this is
/// reclaimed by [`prune_until_short`], one statement at a time.
const RETENTION_PRUNE_BATCH: u32 = 1_000;

/// How often a prune sequence runs.
///
/// DELIBERATELY not once per drain pass: the drain polls every 500 ms, while
/// the horizon the prune enforces is at least a minute, so nothing becomes
/// prunable faster than this.
const RETENTION_PRUNE_INTERVAL: Duration = Duration::from_mins(1);

/// One prune sequence: delete batches until one comes back short, so every
/// record past the horizon is gone when it returns. Each batch is its own
/// transaction, which keeps the row locks short; `cancel` ends the sequence
/// between batches. Returns how many records it removed.
async fn prune_until_short(
    port: &dyn PublicationRetentionPort,
    older_than: Duration,
    limit: std::num::NonZeroU32,
    cancel: &CancellationToken,
) -> Result<u64, StorageError> {
    let mut total = 0;
    while !cancel.is_cancelled() {
        let pruned = port.prune_published(older_than, limit).await?;
        total += pruned;
        if pruned < u64::from(limit.get()) {
            break;
        }
    }
    Ok(total)
}

/// Reclaim delivered records older than the configured horizon, until
/// cancellation. A no-op future when no horizon is configured.
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
        match prune_until_short(
            retention.port.as_ref(),
            retention.older_than,
            limit,
            &cancel,
        )
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

/// The longest the embedding worker sleeps between idle drains when no
/// write commits in this process, unless the configured worker interval is
/// longer: how late it finds jobs enqueued by another process.
const EMBED_IDLE_POLL_MAX: Duration = Duration::from_mins(1);

/// Startup reconcile, then drain until cancellation. Started only when the
/// engine has an embedding router. Returns the worker's pause control and
/// its task.
///
/// The gate is checked before every `drain_embedding_jobs` call, which claims
/// jobs and reclaims stale `processing` ones: a quiesce lets the call in
/// progress return, then parks. The startup reconcile is the other site that
/// touches the queue: it enqueues missing jobs and reclaims stale
/// `processing` ones (storage-pg `reconcile_embeddings`), and claims none.
/// It runs before the first checkpoint, so a quiesce that arrives during it
/// waits for it.
fn spawn_embedding_worker(
    engine: Arc<Engine>,
    cancel: CancellationToken,
) -> (QuiesceGate, JoinHandle<()>) {
    let (control, mut gate) = QuiesceGate::new();
    let task = tokio::spawn(async move {
        let policy = engine.embedding_runtime_policy();
        // Boot-time catch-up, not a recurring clock: memories written while
        // no embedding client was configured never got a job (and exhausted
        // `failed` jobs stay dead), so one reconcile pass before the first
        // drain keeps a restart from leaving them silently unsearchable.
        // Recurring scans and reports stay outside the process
        // (`proxima-mcp maintain-embeddings`); owed work drains here.
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
        // Subscribed before the first drain, so a write that commits while
        // one runs wakes the worker right after it.
        let mut writes = WriteWake::subscribe();
        let floor = policy.worker_interval();
        let ceiling = EMBED_IDLE_POLL_MAX.max(floor);
        let mut idle_wait = floor;
        loop {
            if cancel.is_cancelled() {
                break;
            }
            let mut processed = 0usize;
            let mut failed = 0usize;
            let mut errored = false;
            loop {
                if gate.checkpoint(cancel.cancelled()).await == Checkpoint::Cancelled {
                    return;
                }
                match engine.drain_embedding_jobs(policy.batch_size()).await {
                    Ok(outcome) => {
                        if !outcome.configuration_faults.is_empty() {
                            tracing::warn!(
                                faults = ?outcome.configuration_faults,
                                "embedding clients paused for configuration faults"
                            );
                        }
                        processed += outcome.processed;
                        failed += outcome.failed;
                        if outcome.processed == 0 {
                            break;
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "embedding drain failed");
                        errored = true;
                        break;
                    }
                }
            }
            if processed > 0 || failed > 0 {
                tracing::info!(processed, failed, "drained embedding jobs");
            }
            if errored {
                // A failing drain retries on the plain interval, not on
                // every write.
                idle_wait = floor;
                tokio::select! {
                    () = cancel.cancelled() => break,
                    () = gate.changed() => {}
                    () = tokio::time::sleep(floor) => {}
                }
                continue;
            }
            // Idle: sleep, longer each round that found nothing, until a
            // write commits here. Jobs enqueued by another process are found
            // by the poll, within the ceiling.
            let sleep = if processed > 0 || failed > 0 {
                floor
            } else {
                idle_wait
            };
            idle_wait = sleep.saturating_mul(2).min(ceiling).max(floor);
            tokio::select! {
                () = cancel.cancelled() => break,
                () = gate.changed() => {}
                () = writes.changed() => idle_wait = floor,
                () = tokio::time::sleep(sleep) => {}
            }
        }
    });
    (control, task)
}

#[cfg(test)]
mod prune_tests {
    use std::num::NonZeroU32;
    use std::sync::Mutex;

    use super::*;

    /// A port holding `eligible` records past the horizon.
    struct Backlog(Mutex<u64>);

    #[async_trait::async_trait]
    impl PublicationRetentionPort for Backlog {
        async fn prune_published(
            &self,
            _older_than: Duration,
            limit: NonZeroU32,
        ) -> Result<u64, StorageError> {
            let mut eligible = self.0.lock().unwrap();
            let deleted = (*eligible).min(u64::from(limit.get()));
            *eligible -= deleted;
            Ok(deleted)
        }
    }

    #[tokio::test]
    async fn one_prune_sequence_removes_every_eligible_record() {
        let limit = NonZeroU32::new(1_000).unwrap();
        for eligible in [0, 1, 999, 1_000, 1_001, 4_500] {
            let port = Backlog(Mutex::new(eligible));
            let removed = prune_until_short(
                &port,
                Duration::from_mins(1),
                limit,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(removed, eligible);
            assert_eq!(*port.0.lock().unwrap(), 0);
        }
    }

    #[tokio::test]
    async fn a_cancelled_sequence_stops_between_batches() {
        let port = Backlog(Mutex::new(5_000));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let removed = prune_until_short(
            &port,
            Duration::from_mins(1),
            NonZeroU32::new(1_000).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(removed, 0);
    }
}
