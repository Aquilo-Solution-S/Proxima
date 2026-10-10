//! Quiesce and resume of the embedding worker (docs/10 §Quiesce and resume)
//! against a real Postgres: the worker stops claiming and holds nothing
//! while serving continues, and resume drains what queued meanwhile.
//!
//! The worker interval is an hour in every case that does not count
//! intervals, so only the gate's wake-ups can end an idle sleep.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use proxima::flavor::{FlavorBundle, FlavorRegistry, FlavorRegistryError, NamedMigrator};
use proxima::{
    AppInfo, Feature, FeatureControl, FeatureControlError, FeatureStatus, FlavorApp, Proxima,
    ToolScope, company_owner,
};
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::{
    AgentNoteV1, AuthError, AuthPath, Authenticator, AuthzContext, Credentials, Owner, Role, UserId,
};
use proxima_pg_testkit::{SplitRoleDb, db_url};
use tokio::sync::Notify;
use uuid::Uuid;

const MODEL: &str = "feature-control-embed";

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
            id: "feature-control",
            title: "Feature Control",
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

/// An embedding client that can hold its next call: `entered` fires when the
/// call is made, and the call returns only after `release`.
#[derive(Debug)]
struct GatedEmbedding {
    inner: ConstantEmbedding,
    hold_next: AtomicBool,
    entered: Notify,
    release: Notify,
}

impl GatedEmbedding {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: ConstantEmbedding::prefixed(MODEL, &[0.25, 0.5, 0.75]),
            hold_next: AtomicBool::new(false),
            entered: Notify::new(),
            release: Notify::new(),
        })
    }

    fn hold_next_call(&self) {
        self.hold_next.store(true, Ordering::Release);
    }

    async fn in_flight(&self) {
        tokio::time::timeout(Duration::from_secs(15), self.entered.notified())
            .await
            .expect("an embedding call is in flight");
    }
}

#[async_trait]
impl EmbeddingClient for GatedEmbedding {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        if self.hold_next.swap(false, Ordering::AcqRel) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.embed(text).await
    }

    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn dim(&self) -> usize {
        self.inner.dim()
    }
}

struct World {
    db: SplitRoleDb,
    inspection: sqlx::PgPool,
    owner: Owner,
    subject: UserId,
    client: Arc<GatedEmbedding>,
}

/// Rows of `proxima_core.embedding_jobs` by status, and finished vectors.
#[derive(Debug, PartialEq, Eq)]
struct Queue {
    pending: i64,
    processing: i64,
    embedded: i64,
}

impl World {
    async fn new(prefix: &str) -> Self {
        let db = SplitRoleDb::create(prefix, &[])
            .await
            .expect("PG required for tests");
        let inspection = sqlx::PgPool::connect(&db_url(db.name()))
            .await
            .expect("inspection pool");
        Self {
            db,
            inspection,
            owner: company_owner(Uuid::now_v7()),
            subject: UserId::new(Uuid::now_v7()),
            client: GatedEmbedding::new(),
        }
    }

    fn app(&self, worker_interval: Duration) -> Proxima<BareApp> {
        Proxima::<BareApp>::app()
            .database_url(self.db.runtime_url())
            .platform_database_url(self.db.platform_url())
            .owner(self.owner)
            .authenticator(Arc::new(StubAuth {
                subject: self.subject,
                owner: self.owner,
            }))
            .tool_scope(ToolScope::All)
            .embed_client(self.client.clone())
            .embedding_runtime_policy(
                proxima::EmbeddingRuntimePolicy::new(
                    Duration::from_mins(2),
                    32,
                    worker_interval,
                    Duration::from_mins(15),
                )
                .expect("embedding policy"),
            )
    }

    /// Accept a write: it enqueues one embedding job.
    async fn write_note(&self, engine: &proxima::Engine, title: &str) {
        let authz = proxima_core::test_fixtures::authenticated_context(
            AuthzContext::for_subject_with_role(
                self.subject,
                [(self.owner, Role::admin())],
                AuthPath::HostBearer,
            ),
        )
        .narrowed_to_owner(self.owner)
        .expect("trusted host resolved this exact owner");
        let note = AgentNoteV1 {
            note_id: Uuid::now_v7(),
            title: title.into(),
            body: title.into(),
            tags: Vec::new(),
            idempotency_key: Some(title.into()),
        };
        engine
            .ingest_fact(
                &authz,
                proxima::FactWrite::new(self.owner, "test/feature-control", &note),
            )
            .await
            .expect("a quiescent worker does not stop writes");
    }

    async fn queue(&self) -> Queue {
        let (pending, processing, embedded) = sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT \
               (SELECT count(*) FROM proxima_core.embedding_jobs WHERE status = 'pending')::bigint, \
               (SELECT count(*) FROM proxima_core.embedding_jobs WHERE status = 'processing')::bigint, \
               (SELECT count(*) FROM proxima_core.embeddings)::bigint",
        )
        .fetch_one(&self.inspection)
        .await
        .expect("queue counts");
        Queue {
            pending,
            processing,
            embedded,
        }
    }

    /// Wait until `embedded` vectors exist and no job is left.
    async fn wait_until_drained(&self, embedded: i64) {
        let drained = Queue {
            pending: 0,
            processing: 0,
            embedded,
        };
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if self.queue().await == drained {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the queue did not drain to {drained:?}"));
    }

    async fn close(self) {
        self.inspection.close().await;
    }
}

async fn serving(
    client: &reqwest::Client,
    base: &str,
    owner: Owner,
) -> (reqwest::StatusCode, reqwest::StatusCode) {
    let ready = client
        .get(format!("{base}/readyz"))
        .send()
        .await
        .expect("readyz answers")
        .status();
    let initialize = client
        .post(format!("{base}/mcp"))
        .header("Origin", "http://localhost")
        .header("Authorization", "Bearer any")
        .header("X-Proxima-Owner", proxima_mcp_server::owner_key(owner))
        .header("MCP-Protocol-Version", "2025-03-26")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "feature-control-test", "version": "0"}
            }
        }))
        .send()
        .await
        .expect("MCP answers")
        .status();
    (ready, initialize)
}

/// A settlement that never comes fails the test instead of hanging it.
async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .expect("the quiesce settles")
}

/// The acceptance path: after `quiesce()` returns, a write enqueues its job
/// and nothing claims it; serving answers the same before, during and after;
/// a direct `drain_embedding_jobs` call is outside the gate; `resume()` drains
/// what queued without waiting out the (hour-long) interval.
#[tokio::test]
async fn a_quiescent_worker_claims_nothing_while_serving_continues_and_resume_drains() {
    let world = World::new("feature_control_serving").await;
    let running = world
        .app(Duration::from_hours(1))
        .mcp_bind("127.0.0.1:0".parse().expect("loopback bind"))
        .health_endpoints(true)
        .run()
        .await
        .expect("run");
    let base = format!("http://{}", running.mcp_addr().expect("MCP bound"));
    let http = reqwest::Client::new();
    let control = running
        .feature_control(Feature::EmbeddingWorker)
        .expect("a configured embedding client starts the worker");
    assert_eq!(control.feature(), Feature::EmbeddingWorker);

    let before = serving(&http, &base, world.owner).await;
    assert_eq!(before.0, reqwest::StatusCode::OK);
    assert!(before.1.is_success(), "initialize: {}", before.1);

    // Quiesce first: whether the worker was in its startup reconcile, in its
    // first pass or already idle, it parks before it can claim anything.
    assert_eq!(within(control.quiesce()).await, Ok(()));
    assert_eq!(control.status(), FeatureStatus::Quiescent);

    let engine = running.host().engine();
    world.write_note(engine, "written while quiescent").await;
    assert_eq!(
        world.queue().await,
        Queue {
            pending: 1,
            processing: 0,
            embedded: 0
        },
        "the write enqueued its job and the parked worker did not claim it"
    );
    assert_eq!(
        serving(&http, &base, world.owner).await,
        before,
        "serving is unaffected"
    );

    // Not drained, not gated: a host that drains by hand is outside the gate.
    let direct = engine
        .drain_embedding_jobs(8)
        .await
        .expect("a direct drain");
    assert_eq!(direct.processed, 1);
    assert_eq!(control.status(), FeatureStatus::Quiescent);
    world.wait_until_drained(1).await;

    world
        .write_note(engine, "second write while quiescent")
        .await;
    assert_eq!(
        world.queue().await,
        Queue {
            pending: 1,
            processing: 0,
            embedded: 1
        }
    );

    assert_eq!(control.resume(), Ok(()));
    world.wait_until_drained(2).await;
    assert_eq!(control.status(), FeatureStatus::Running);
    assert_eq!(
        serving(&http, &base, world.owner).await,
        before,
        "serving after resume"
    );

    running.shutdown().await;
    world.close().await;
}

/// Absence has no signal, so this one lets whole worker intervals pass. The
/// control half proves the worker drains within them when it is not paused.
#[tokio::test]
async fn a_job_enqueued_after_quiesce_stays_pending_across_several_worker_intervals() {
    const INTERVAL: Duration = Duration::from_secs(1);
    let world = World::new("feature_control_intervals").await;
    let built = world.app(INTERVAL).build().await.expect("build");
    let control = built
        .feature_control(Feature::EmbeddingWorker)
        .expect("a configured embedding client starts the worker");
    let engine = built.host().engine();

    world
        .write_note(engine, "control: the worker is draining")
        .await;
    world.wait_until_drained(1).await;

    assert_eq!(within(control.quiesce()).await, Ok(()));
    world.write_note(engine, "after quiesce").await;
    tokio::time::sleep(INTERVAL * 3).await;
    assert_eq!(
        world.queue().await,
        Queue {
            pending: 1,
            processing: 0,
            embedded: 1
        },
        "three intervals passed and the job was never claimed"
    );

    assert_eq!(control.resume(), Ok(()));
    world.wait_until_drained(2).await;

    built.shutdown().await;
    world.close().await;
}

/// Kick one drain call that stops inside its first embedding call, with all
/// three jobs claimed: the worker is quiesced, three notes queue, and resume
/// sends it into the call.
async fn drain_call_in_flight(world: &World, built: &proxima::BuiltProxima) -> FeatureControl {
    let control = built
        .feature_control(Feature::EmbeddingWorker)
        .expect("a configured embedding client starts the worker");
    assert_eq!(within(control.quiesce()).await, Ok(()));
    for title in ["one", "two", "three"] {
        world.write_note(built.host().engine(), title).await;
    }
    world.client.hold_next_call();
    assert_eq!(control.resume(), Ok(()));
    world.client.in_flight().await;
    assert_eq!(
        world.queue().await,
        Queue {
            pending: 0,
            processing: 3,
            embedded: 0
        },
        "one drain call holds a claimed batch it has not processed"
    );
    control
}

#[tokio::test]
async fn a_quiesce_mid_batch_returns_only_after_the_drain_call_settles_every_claim() {
    let world = World::new("feature_control_midbatch").await;
    let built = world
        .app(Duration::from_hours(1))
        .build()
        .await
        .expect("build");
    let control = drain_call_in_flight(&world, &built).await;

    let mut quiesce = Box::pin(control.quiesce());
    assert!(
        quiesce.as_mut().now_or_never().is_none(),
        "the quiesce waits for the call in progress"
    );
    assert_eq!(control.status(), FeatureStatus::Quiescing);

    world.client.release.notify_one();
    assert_eq!(within(quiesce).await, Ok(()));
    assert_eq!(
        world.queue().await,
        Queue {
            pending: 0,
            processing: 0,
            embedded: 3
        },
        "every claim of the call is completed before the quiesce returns"
    );
    assert_eq!(control.status(), FeatureStatus::Quiescent);

    // Shutdown while quiescent joins, and the handle outlives it.
    tokio::time::timeout(Duration::from_secs(10), built.shutdown())
        .await
        .expect("shutdown of a quiescent worker joins");
    assert_eq!(control.status(), FeatureStatus::Stopped);
    world.close().await;
}

/// A storage error after the claim must not leave claims behind for the
/// quiesce to outlive. The second vector write of the call is made to fail
/// (a trigger on `embeddings`): the first job is completed, and the other two
/// are still `processing` when the error surfaces.
#[tokio::test]
async fn a_storage_error_mid_batch_returns_the_unsettled_claims_before_the_quiesce_resolves() {
    let world = World::new("feature_control_storage_error").await;
    let built = world
        .app(Duration::from_hours(1))
        .build()
        .await
        .expect("build");
    let control = built
        .feature_control(Feature::EmbeddingWorker)
        .expect("a configured embedding client starts the worker");
    assert_eq!(within(control.quiesce()).await, Ok(()));
    sqlx::query(
        "CREATE FUNCTION proxima_core.fail_second_vector() RETURNS trigger \
         LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$ \
         BEGIN \
           IF (SELECT count(*) FROM proxima_core.embeddings) >= 1 THEN \
             RAISE EXCEPTION 'injected vector write failure'; \
           END IF; \
           RETURN NEW; \
         END $$",
    )
    .execute(&world.inspection)
    .await
    .expect("failure function");
    sqlx::query(
        "CREATE TRIGGER fail_second_vector BEFORE INSERT ON proxima_core.embeddings \
         FOR EACH ROW EXECUTE FUNCTION proxima_core.fail_second_vector()",
    )
    .execute(&world.inspection)
    .await
    .expect("failure trigger");
    for title in ["one", "two", "three"] {
        world.write_note(built.host().engine(), title).await;
    }
    world.client.hold_next_call();
    assert_eq!(control.resume(), Ok(()));
    world.client.in_flight().await;

    let mut quiesce = Box::pin(control.quiesce());
    assert!(quiesce.as_mut().now_or_never().is_none());
    world.client.release.notify_one();
    assert_eq!(within(quiesce).await, Ok(()));
    assert_eq!(
        world.queue().await,
        Queue {
            pending: 2,
            processing: 0,
            embedded: 1
        },
        "the job the call finished stays finished; the two it did not are back to pending, not processing"
    );

    // Nothing was lost or doubled: with the fault gone, resume finishes the rest.
    sqlx::query("DROP TRIGGER fail_second_vector ON proxima_core.embeddings")
        .execute(&world.inspection)
        .await
        .expect("drop failure trigger");
    assert_eq!(control.resume(), Ok(()));
    world.wait_until_drained(3).await;

    built.shutdown().await;
    world.close().await;
}

#[tokio::test]
async fn resume_while_quiescing_returns_resumed_and_the_worker_goes_on() {
    let world = World::new("feature_control_resumed").await;
    let built = world
        .app(Duration::from_hours(1))
        .build()
        .await
        .expect("build");
    let control = drain_call_in_flight(&world, &built).await;

    let mut quiesce = Box::pin(control.quiesce());
    assert!(quiesce.as_mut().now_or_never().is_none());
    assert_eq!(control.resume(), Ok(()));
    assert_eq!(within(quiesce).await, Err(FeatureControlError::Resumed));
    assert_eq!(control.status(), FeatureStatus::Running);

    world.client.release.notify_one();
    world.wait_until_drained(3).await;
    // Still responsive: it did not park behind the abandoned quiesce.
    assert_eq!(within(control.quiesce()).await, Ok(()));

    built.shutdown().await;
    world.close().await;
}

#[tokio::test]
async fn shutdown_while_quiescing_stops_the_pending_quiesce_and_joins() {
    let world = World::new("feature_control_shutdown").await;
    let built = world
        .app(Duration::from_hours(1))
        .build()
        .await
        .expect("build");
    let control = drain_call_in_flight(&world, &built).await;

    // One task polls the quiesce, then starts the shutdown (which cancels the
    // runtime), then releases the call: the worker meets a cancelled token.
    let (quiesced, (), ()) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(control.quiesce(), built.shutdown(), async {
            world.client.release.notify_one();
        })
    })
    .await
    .expect("shutdown joins");
    assert_eq!(quiesced, Err(FeatureControlError::Stopped));

    assert_eq!(control.status(), FeatureStatus::Stopped);
    assert_eq!(control.resume(), Err(FeatureControlError::Stopped));
    assert_eq!(control.quiesce().await, Err(FeatureControlError::Stopped));
    assert_eq!(
        world.queue().await,
        Queue {
            pending: 0,
            processing: 0,
            embedded: 3
        },
        "the call in flight still finished its batch"
    );
    world.close().await;
}
