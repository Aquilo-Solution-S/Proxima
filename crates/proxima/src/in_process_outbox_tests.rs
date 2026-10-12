//! In-process outbox delivery against real Postgres: the runtime path, the
//! crash window between `accept` and the mark, and the `accept` timeout.

use std::any::Any;
use std::collections::BTreeSet;
use std::ops::ControlFlow;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt;
use proxima_core::publication::intake::{DurableIntake, Intake, IntakeError, ReceivedEvent};
use proxima_core::publication::{PublicationConfig, PublicationSource};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::in_process_outbox::{AfterAccept, Drain, Proceed, Settings, spawn};
use crate::outbox_test_fixture::{OutboxProbeApp, PROBE_SOURCE, capture, wait_for_outbox};

/// Spends one unit of `counter`, if any is left.
fn take(counter: &AtomicUsize) -> bool {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
}

/// A host intake that deduplicates on `id` and can be told to fail.
#[derive(Debug, Default)]
struct TestIntake {
    /// Calls to refuse with an `IntakeError` before accepting.
    refuse: AtomicUsize,
    /// Calls, after the refusals, that never return.
    hang: AtomicUsize,
    /// Every call, in order: the event id and the `delivered_count` it carried.
    calls: Mutex<Vec<(String, u64)>>,
    /// What a deduplicating intake keeps: one entry per id.
    kept: Mutex<BTreeSet<String>>,
}

impl TestIntake {
    fn calls(&self) -> Vec<(String, u64)> {
        self.calls.lock().expect("calls lock").clone()
    }

    fn kept(&self) -> usize {
        self.kept.lock().expect("kept lock").len()
    }
}

#[async_trait::async_trait]
impl DurableIntake for TestIntake {
    async fn accept(&self, event: &ReceivedEvent) -> Result<Intake, IntakeError> {
        self.calls
            .lock()
            .expect("calls lock")
            .push((event.id.clone(), event.delivered_count));
        if take(&self.refuse) {
            return Err(IntakeError::new("the intake is down"));
        }
        if take(&self.hang) {
            std::future::pending::<()>().await;
        }
        self.kept
            .lock()
            .expect("kept lock")
            .insert(event.id.clone());
        Ok(Intake::Accepted)
    }
}

/// Behaves as though the process died after the first `accept`: the record
/// stays leased, which is the window `mark_published` closes.
#[derive(Debug)]
struct CrashAfterFirstAccept(AtomicBool);

#[async_trait::async_trait]
impl AfterAccept for CrashAfterFirstAccept {
    async fn after_accept(&self, _event_id: &str) -> ControlFlow<()> {
        if self.0.swap(false, Ordering::SeqCst) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }
}

struct World {
    db_name: String,
    built: crate::BuiltProxima,
    owner: proxima_core::Owner,
    /// Platform scope: reads the outbox rows the runtime's own role cannot.
    pool: sqlx::PgPool,
}

impl World {
    /// A booted app over a fresh database; `None` when no Postgres is
    /// configured. With an `intake` the runtime delivers to it; without one
    /// it starts no drain, so a test can run its own beside the outbox.
    async fn new(intake: Option<Arc<TestIntake>>) -> Option<Self> {
        proxima_pg_testkit::admin_url_or_skip()?;
        let db_name = proxima_pg_testkit::unique_db_name("in_process_outbox");
        proxima_pg_testkit::create_db(&db_name)
            .await
            .expect("PG fixture database");
        let (db_url, platform_url) = proxima_pg_testkit::split_role_urls(&db_name)
            .await
            .expect("split fixture roles");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&platform_url)
            .await
            .expect("platform pool");
        sqlx::query("SELECT set_config('app.proxima_scope', 'platform', false)")
            .execute(&pool)
            .await
            .expect("platform scope");
        let owner = crate::company_owner(Uuid::now_v7());
        let app = crate::Proxima::<OutboxProbeApp>::app()
            .database_url(db_url)
            .platform_database_url(platform_url)
            .owner(owner)
            .tool_scope(crate::ToolScope::All)
            .allow_insecure_single_owner()
            .publication(PublicationConfig::new(
                PublicationSource::new(PROBE_SOURCE).expect("publication source"),
            ));
        let app = match intake {
            Some(intake) => app.outbox_intake(intake),
            None => app,
        };
        let built = app.build().await.expect("Proxima builds");
        Some(Self {
            db_name,
            built,
            owner,
            pool,
        })
    }

    /// Tear the world down, then re-raise a panic the test body hit.
    async fn finish(self, outcome: Result<(), Box<dyn Any + Send>>) {
        self.built.shutdown().await;
        let _ = proxima_pg_testkit::drop_db(&self.db_name).await;
        if let Err(payload) = outcome {
            std::panic::resume_unwind(payload);
        }
    }

    /// The state of one outbox record.
    async fn state(&self, t: Uuid) -> String {
        sqlx::query_scalar("SELECT state::text FROM proxima_core.publication_outbox WHERE t = $1")
            .bind(t)
            .fetch_one(&self.pool)
            .await
            .expect("outbox row")
    }

    /// The drain a test runs itself over the outbox: a short lease and poll,
    /// and `hook` between `accept` and the mark.
    fn drain(
        &self,
        intake: &Arc<TestIntake>,
        timeout: Duration,
        hook: Arc<dyn AfterAccept>,
    ) -> (CancellationToken, tokio::task::JoinHandle<()>) {
        let settings = Settings {
            lease: Duration::from_secs(1),
            poll_interval: Duration::from_millis(50),
            accept_timeout: timeout,
            ..Settings::new()
        };
        let drain = Drain::new(
            self.built.outbox_for_tests(),
            intake.clone(),
            settings,
            hook,
        );
        let cancel = CancellationToken::new();
        let (_gate, task) = spawn(drain, cancel.clone(), |_| async {});
        (cancel, task)
    }
}

#[tokio::test]
async fn a_fact_written_while_the_intake_fails_is_delivered_once_it_recovers() {
    let intake = Arc::new(TestIntake {
        refuse: AtomicUsize::new(1),
        ..TestIntake::default()
    });
    let Some(world) = World::new(Some(intake.clone())).await else {
        return;
    };
    let outcome = AssertUnwindSafe(async {
        assert_eq!(
            world
                .built
                .boot_report()
                .get(crate::Feature::OutboxPublisher)
                .state,
            crate::FeatureState::Started
        );
        let t = capture(
            &world.built,
            world.owner,
            "written while the intake is down",
        )
        .await;
        wait_for_outbox(&world.pool, t, "published").await;

        let id = intake.calls()[0].0.clone();
        assert_eq!(
            intake.calls(),
            vec![(id.clone(), 1), (id, 2)],
            "the refused delivery counted as an attempt; the retry says so"
        );
        assert_eq!(intake.kept(), 1);
        assert_eq!(world.state(t).await, "published");
    })
    .catch_unwind()
    .await;
    world.finish(outcome).await;
}

#[tokio::test]
async fn a_crash_between_accept_and_the_mark_redelivers_and_a_deduplicating_intake_keeps_one() {
    let intake = Arc::new(TestIntake::default());
    let Some(world) = World::new(None).await else {
        return;
    };
    let outcome = AssertUnwindSafe(async {
        let t = capture(&world.built, world.owner, "accepted, then the process died").await;
        let hook = Arc::new(CrashAfterFirstAccept(AtomicBool::new(true)));
        let (cancel, task) = world.drain(&intake, Settings::new().accept_timeout, hook);

        while intake.calls().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            world.state(t).await,
            "claimed",
            "an accepted record leaves `claimed` only through mark_published"
        );
        wait_for_outbox(&world.pool, t, "published").await;
        cancel.cancel();
        task.await.expect("drain joins");

        let calls = intake.calls();
        assert_eq!(calls.iter().map(|call| call.1).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(calls[0].0, calls[1].0, "the same event, delivered twice");
        assert_eq!(intake.kept(), 1, "the intake deduplicates on id");
    })
    .catch_unwind()
    .await;
    world.finish(outcome).await;
}

#[tokio::test]
async fn an_accept_that_never_returns_is_released_and_delivered_again() {
    let intake = Arc::new(TestIntake {
        hang: AtomicUsize::new(1),
        ..TestIntake::default()
    });
    let Some(world) = World::new(None).await else {
        return;
    };
    let outcome = AssertUnwindSafe(async {
        let t = capture(&world.built, world.owner, "the intake hangs once").await;
        let (cancel, task) = world.drain(&intake, Duration::from_millis(100), Arc::new(Proceed));
        wait_for_outbox(&world.pool, t, "published").await;
        cancel.cancel();
        task.await.expect("drain joins");

        let calls = intake.calls();
        assert_eq!(calls.iter().map(|call| call.1).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(intake.kept(), 1, "only the second call returned");
    })
    .catch_unwind()
    .await;
    world.finish(outcome).await;
}
