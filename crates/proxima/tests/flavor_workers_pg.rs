//! Flavor-contributed background workers through the serving runtime:
//! `Proxima::run` spawns what `FlavorBundle::spawn_workers` returns and
//! `RunningProxima::shutdown` cancels and joins it.

use std::sync::atomic::{AtomicUsize, Ordering};

use proxima::flavor::{
    FlavorBundle, FlavorRegistry, FlavorRegistryError, FlavorWorker, FlavorWorkerContext,
    NamedMigrator,
};
use proxima::{AppInfo, FlavorApp, Proxima, ToolScope, company_owner};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use tokio::time::{Duration, Instant, sleep, timeout};
use uuid::Uuid;

/// Statics because `spawn_workers` is an associated fn: the test observes
/// the worker through them. One test in this binary, so no cross-talk.
static TICKS: AtomicUsize = AtomicUsize::new(0);

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
                        () = cancel.cancelled() => break,
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

        // shutdown() returning proves the worker joined; the timeout is the
        // failure mode for a worker that ignores the cancel token.
        timeout(Duration::from_secs(5), running.shutdown()).await?;

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
