//! Flavor-contributed background workers through the serving runtime:
//! `Proxima::run` spawns what `FlavorBundle::spawn_workers` returns only
//! once nothing fallible remains before `RunningProxima` owns them.

#[path = "fixtures/split_core_db.rs"]
mod split_core_db;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use proxima::flavor::{
    FlavorBundle, FlavorRegistry, FlavorRegistryError, FlavorWorker, FlavorWorkerContext,
    NamedMigrator,
};
use proxima::{AppInfo, FlavorApp, Proxima, ProximaError, ToolScope, company_owner};
use proxima_core::{
    AuthError, AuthPath, Authenticator, AuthzContext, Credentials, Owner, Role, UserId,
};
use split_core_db::clone_split_core_db;
use uuid::Uuid;

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
    let split_db = clone_split_core_db("proxima_test")
        .await
        .expect("PG required for tests");
    let (db_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

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
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .authenticator(Arc::new(TestAuthenticator { subject, owner }))
            .tool_scope(ToolScope::All)
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

    result.expect("failed-bind spawn test failed");
}
