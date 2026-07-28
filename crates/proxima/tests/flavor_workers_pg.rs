//! Flavor-contributed background workers through the serving runtime:
//! `Proxima::run` spawns what `FlavorBundle::spawn_workers` returns —
//! but only once nothing fallible remains before `RunningProxima` owns
//! them — and `RunningProxima::shutdown` cancels and joins it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use proxima::flavor::{
    FlavorBundle, FlavorRegistry, FlavorRegistryError, FlavorWorker, FlavorWorkerContext,
    NamedMigrator,
};
use proxima::{AppInfo, FlavorApp, Proxima, ProximaError, ToolScope, company_owner};
use proxima_core::{
    AuthError, AuthPath, Authenticator, AuthzContext, Credentials, Owner, Role, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgOwnerAccessResolver;
use tokio::time::{Duration, Instant, sleep, timeout};
use uuid::Uuid;

/// Statics because `spawn_workers` is an associated fn: the tests observe
/// their apps through them. Each static belongs to one app booted by one
/// test, so no cross-talk.
static TICKS: AtomicUsize = AtomicUsize::new(0);
/// Set by the worker's cancellation tail, after a sleep long enough that
/// only a joined worker has set it by the time `shutdown()` returns.
static TAIL_DONE: AtomicBool = AtomicBool::new(false);

struct CountingWorkerApp;

impl FlavorBundle for CountingWorkerApp {
    fn register(_registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }

    fn spawn_workers(ctx: &FlavorWorkerContext) -> Vec<FlavorWorker> {
        let cancel = ctx.cancel.clone();
        vec![FlavorWorker {
            name: "counting-worker",
            handle: tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = cancel.cancelled() => {
                            sleep(Duration::from_millis(50)).await;
                            TAIL_DONE.store(true, Ordering::SeqCst);
                            break;
                        }
                        () = sleep(Duration::from_millis(5)) => {
                            TICKS.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
            }),
        }]
    }
}

impl FlavorApp for CountingWorkerApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "counting-worker-test",
            title: "Counting Worker Test",
            version: "1",
        }
    }
}

#[tokio::test]
async fn run_spawns_flavor_workers_and_shutdown_joins_them() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let running = Proxima::<CountingWorkerApp>::app()
            .database_url(db_url)
            .owner(company_owner(Uuid::now_v7()))
            .tool_scope(ToolScope::All)
            .run()
            .await?;

        // The worker loop must advance while the app is serving.
        let deadline = Instant::now() + Duration::from_secs(5);
        while TICKS.load(Ordering::SeqCst) < 3 {
            assert!(
                Instant::now() < deadline,
                "flavor worker never advanced its counter"
            );
            sleep(Duration::from_millis(10)).await;
        }

        // The timeout is the failure mode for a worker that ignores the
        // cancel token.
        timeout(Duration::from_secs(5), running.shutdown()).await?;

        // TAIL_DONE is what proves the join: shutdown() can only return
        // after the worker's 50ms cancellation tail has run. A runtime
        // that merely cancelled and dropped the handle would reach this
        // assert while the worker is still sleeping.
        assert!(
            TAIL_DONE.load(Ordering::SeqCst),
            "shutdown returned before the worker's cancellation tail ran"
        );

        // And joined means stopped: the counter must not advance again.
        let after_shutdown = TICKS.load(Ordering::SeqCst);
        sleep(Duration::from_millis(60)).await;
        assert_eq!(
            TICKS.load(Ordering::SeqCst),
            after_shutdown,
            "worker loop kept running after shutdown"
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("flavor worker lifecycle integration test failed");
}

/// Counts `spawn_workers` calls instead of spawning: proves whether a
/// run that failed after boot asked the bundle for workers at all.
static FAILED_RUN_SPAWN_CALLS: AtomicUsize = AtomicUsize::new(0);

struct BindProbeApp;

impl FlavorBundle for BindProbeApp {
    fn register(_registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }

    fn spawn_workers(_ctx: &FlavorWorkerContext) -> Vec<FlavorWorker> {
        FAILED_RUN_SPAWN_CALLS.fetch_add(1, Ordering::SeqCst);
        Vec::new()
    }
}

impl FlavorApp for BindProbeApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "bind-probe-test",
            title: "Bind Probe Test",
            version: "1",
        }
    }
}

#[derive(Debug)]
struct TestAuthenticator {
    subject: UserId,
    owner: Owner,
}

#[async_trait]
impl Authenticator for TestAuthenticator {
    async fn authenticate(&self, _credentials: &Credentials) -> Result<AuthzContext, AuthError> {
        Ok(AuthzContext::for_subject_with_role(
            self.subject,
            [(self.owner, Role::admin())],
            AuthPath::HostBearer,
        ))
    }
}

#[tokio::test]
async fn run_that_fails_to_bind_spawns_no_flavor_workers() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        // Holding this listener open makes the runtime's own bind on the
        // same port fail — the last fallible step of `run`. A worker
        // spawned before that point would be stranded: its join handle
        // and cancel token die with the error return.
        let occupied = std::net::TcpListener::bind("127.0.0.1:0")?;
        let owner = company_owner(Uuid::now_v7());
        let subject = UserId::new(Uuid::now_v7());
        let err = Proxima::<BindProbeApp>::app()
            .database_url(db_url.clone())
            .owner(owner)
            .owner_access(Arc::new(PgOwnerAccessResolver::connect_lazy(&db_url)?))
            .authenticator(Arc::new(TestAuthenticator { subject, owner }))
            .tool_scope(ToolScope::All)
            .with_mcp()
            .mcp_bind(occupied.local_addr()?)
            .run()
            .await
            .expect_err("bind on an occupied port must fail");
        assert!(
            matches!(err, ProximaError::Mcp(_)),
            "unexpected error: {err}"
        );
        assert_eq!(
            FAILED_RUN_SPAWN_CALLS.load(Ordering::SeqCst),
            0,
            "a run that failed to bind must not ask bundles for workers"
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("failed-bind spawn test failed");
}
