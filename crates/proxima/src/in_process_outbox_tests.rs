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
use proxima_core::storage_ports::publication::ConsumerName;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::in_process_outbox::{AfterAccept, Drain, Proceed, Settings, spawn};
use crate::outbox_test_fixture::{OutboxProbeApp, PROBE_SOURCE, capture, wait_for_outbox};
use proxima_core::test_fixtures::authenticated_context;
use proxima_core::{
    GoalId, GoalTransitionRequest, MemoryTarget, PendingWork, SeriesHandle, SimpleTextGoalV1,
};

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
    /// The envelope of each accepted delivery, in order.
    events: Mutex<Vec<serde_json::Value>>,
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
        self.events
            .lock()
            .expect("events lock")
            .push(serde_json::from_slice(&event.raw).expect("an envelope is JSON"));
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
    owner: proxima_core::OwnerId,
    /// Platform scope: reads the outbox rows the runtime's own role cannot.
    pool: sqlx::PgPool,
}

impl World {
    /// A booted app over a fresh database; `None` when no Postgres is
    /// configured. With an `intake` the runtime delivers to it; without one
    /// it starts no drain, so a test can run its own beside the outbox.
    async fn new(intake: Option<Arc<TestIntake>>) -> Option<Self> {
        Self::configured(|app| match intake {
            Some(intake) => app.outbox_intake(intake),
            None => app,
        })
        .await
    }

    /// A booted app over a fresh database, with whatever outbox consumers
    /// `configure` adds; `None` when no Postgres is configured.
    async fn configured(
        configure: impl FnOnce(crate::Proxima<OutboxProbeApp>) -> crate::Proxima<OutboxProbeApp>,
    ) -> Option<Self> {
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
            .store_number(0)
            .platform_database_url(platform_url)
            .owner(owner)
            .served_surface(crate::ServedSurface::All)
            .allow_insecure_single_owner()
            .publication(PublicationConfig::new(
                PublicationSource::new(PROBE_SOURCE).expect("publication source"),
            ));
        let built = configure(app).build().await.expect("Proxima builds");
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

    /// The state of `consumer`'s delivery lane on one record.
    async fn lane_state(&self, t: Uuid, consumer: &str) -> String {
        sqlx::query_scalar(
            "SELECT state::text FROM proxima_core.publication_delivery
              WHERE t = $1 AND consumer = $2",
        )
        .bind(t)
        .bind(consumer)
        .fetch_one(&self.pool)
        .await
        .expect("delivery lane")
    }

    /// Wait until `consumer`'s lane on one record reaches `wanted`. The
    /// record as a whole also owes the `default` lane, so its own state is
    /// not the signal here.
    async fn wait_for_lane(&self, t: Uuid, consumer: &str, wanted: &str) {
        tokio::time::timeout(Duration::from_secs(15), async {
            while self.lane_state(t, consumer).await != wanted {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("delivery lane changes");
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
            ConsumerName::default(),
            intake.clone(),
            settings,
            hook,
        );
        let cancel = CancellationToken::new();
        let (_gate, task) = spawn(vec![drain], cancel.clone(), |_| async {});
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
            world.lane_state(t, "default").await,
            "claimed",
            "an accepted lane leaves `claimed` only through mark_published"
        );
        assert_eq!(world.state(t).await, "pending");
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

/// Sets a goal through a unit of work, then pauses it through the engine.
async fn set_and_pause(world: &World, pending: PendingWork) -> (GoalId, GoalId) {
    use proxima_core::verbs::goal_write::{
        GoalAssignmentTarget, GoalAuthorship, GoalCreateRequest, GoalTopologyWrite,
        GoalTransitionKind, IdempotencyKey,
    };
    use proxima_core::{DerivedMemory, InterpretationSubjectKind, InterpretationV1};
    let authz = authenticated_context(world.built.single_owner_authz().expect("single owner"));
    let engine = world.built.host().engine();
    let fact = capture(&world.built, world.owner, "grounding").await;
    let assignment = DerivedMemory::interpretation(
        MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
        world.owner,
        "assignment",
        InterpretationV1 {
            claim: "claim".into(),
            confidence: 80,
            subject_memory_ids: vec![fact],
            subject_kinds: vec![InterpretationSubjectKind::Fact],
            model_id: "test".into(),
            client_name: "test".into(),
            client_version: "1".into(),
        },
    );
    let assignment = engine
        .derive_memory(&authz, assignment)
        .await
        .expect("assignment");
    let topology = GoalTopologyWrite::new(
        GoalAssignmentTarget::perspective(assignment.memory_id),
        Vec::new(),
        Vec::new(),
    );
    let mut uow = engine.unit_of_work(&authz).await.expect("unit of work");
    let set = uow
        .create_goal(GoalCreateRequest {
            owner: world.owner,
            topology: topology.expect("topology"),
            title: "feed".into(),
            text: "feed".into(),
            payload: SimpleTextGoalV1 {},
            request_id: IdempotencyKey::new("feed-set").expect("request id"),
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        })
        .await
        .expect("set")
        .goal_id;
    uow.commit().await.expect("commit");
    let pause = GoalTransitionRequest {
        owner: world.owner,
        prior_goal_id: set,
        transition: GoalTransitionKind::Pause { pending },
        authorship: GoalAuthorship::User,
        request_id: IdempotencyKey::new("feed-pause").expect("request id"),
        author_self_perspective_id: None,
    };
    let pause = engine.transition_goal(&authz, &pause).await.expect("pause");
    (set, pause.goal_id)
}

#[tokio::test]
async fn a_pause_that_discards_pending_work_reaches_the_intake_as_a_goal_change() {
    let intake = Arc::new(TestIntake::default());
    let Some(world) = World::new(Some(intake.clone())).await else {
        return;
    };
    let outcome = AssertUnwindSafe(async {
        let (set, paused) = set_and_pause(&world, PendingWork::Discard).await;
        let records: Vec<Uuid> = sqlx::query_scalar(
            "SELECT t FROM proxima_core.publication_outbox WHERE schema_id = 'core/goal-change-v1'",
        )
        .fetch_all(&world.pool)
        .await
        .expect("goal change records");
        assert_eq!(records.len(), 2);
        for t in records {
            wait_for_outbox(&world.pool, t, "published").await;
        }
        let events = intake.events.lock().expect("events lock").clone();
        let data = |kind: &str| {
            let changes = events.iter().filter(|e| e["type"] == "core/goal-change-v1");
            changes
                .map(|e| e["data"].clone())
                .find(|d| d["change"]["kind"] == kind)
        };
        let set_data = data("set").expect("the set event");
        assert_eq!(set_data["prior"], serde_json::Value::Null);
        assert_eq!(
            data("pause").expect("the pause event"),
            serde_json::json!({
                "series": set_data["series"],
                "goal": paused.into_inner().to_string(),
                "prior": set.into_inner().to_string(),
                "revision": 2,
                "state": "Paused",
                "change": {"kind": "pause", "pending": "discard"},
            })
        );
    })
    .catch_unwind()
    .await;
    world.finish(outcome).await;
}

#[tokio::test]
async fn without_an_intake_a_goal_write_writes_no_goal_change() {
    let Some(world) = World::new(None).await else {
        return;
    };
    let outcome = AssertUnwindSafe(async {
        set_and_pause(&world, PendingWork::Keep).await;
        let written: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM proxima_core.memory WHERE schema_id = 'core/goal-change-v1')
                  + (SELECT count(*) FROM proxima_core.publication_outbox
                      WHERE schema_id = 'core/goal-change-v1')",
        )
        .fetch_one(&world.pool)
        .await
        .expect("goal change count");
        assert_eq!(written, 0);
    })
    .catch_unwind()
    .await;
    world.finish(outcome).await;
}

#[tokio::test]
async fn an_intake_without_a_publication_source_fails_boot_naming_the_goal_change_schema() {
    if proxima_pg_testkit::admin_url_or_skip().is_none() {
        return;
    }
    let db_name = proxima_pg_testkit::unique_db_name("in_process_outbox_boot");
    proxima_pg_testkit::create_db(&db_name)
        .await
        .expect("PG fixture database");
    let (db_url, platform_url) = proxima_pg_testkit::split_role_urls(&db_name)
        .await
        .expect("split fixture roles");
    let refused = crate::Proxima::<OutboxProbeApp>::app()
        .database_url(db_url)
        .store_number(0)
        .platform_database_url(platform_url)
        .owner(crate::company_owner(Uuid::now_v7()))
        .served_surface(crate::ServedSurface::All)
        .allow_insecure_single_owner()
        .outbox_intake(Arc::new(TestIntake::default()))
        .build()
        .await;
    let _ = proxima_pg_testkit::drop_db(&db_name).await;
    let message = refused.expect_err("boot is refused").to_string();
    assert!(message.contains("core/goal-change-v1"), "{message}");
}

/// Two consumers configured on the builder each receive every Fact, on
/// drains of their own: one whose intake is down delays only itself, and the
/// record is published once both have it.
#[tokio::test]
async fn two_consumers_each_receive_every_fact_and_one_failing_does_not_hold_up_the_other() {
    let steady = Arc::new(TestIntake::default());
    let failing = Arc::new(TestIntake {
        refuse: AtomicUsize::new(3),
        ..TestIntake::default()
    });
    let audit = ConsumerName::new("audit").expect("valid consumer name");
    let Some(world) = World::configured(|app| {
        app.outbox_intake(steady.clone())
            .outbox_consumer(audit.clone(), failing.clone())
    })
    .await
    else {
        return;
    };
    let outcome = AssertUnwindSafe(async {
        let reason = &world
            .built
            .boot_report()
            .get(crate::Feature::OutboxPublisher)
            .reason;
        assert!(reason.contains("audit, default"), "{reason}");
        let mut consumers = world
            .built
            .outbox_consumers()
            .registered()
            .await
            .expect("registered consumers");
        consumers.sort();
        assert_eq!(consumers, vec![audit.clone(), ConsumerName::default()]);

        let t = capture(&world.built, world.owner, "both consumers want this").await;
        tokio::time::timeout(Duration::from_secs(15), async {
            while world.lane_state(t, "default").await != "published" {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the steady consumer is not held up by the failing one");
        assert_eq!(steady.kept(), 1);

        wait_for_outbox(&world.pool, t, "published").await;
        assert_eq!(failing.kept(), 1, "the failing consumer still gets it");
        let id = failing.calls()[0].0.clone();
        assert!(
            failing.calls().iter().all(|(call, _)| *call == id),
            "the failing consumer was offered only this event"
        );
        assert_eq!(
            steady.calls(),
            vec![(id, 1)],
            "the steady consumer saw one delivery, unaffected by audit's retries"
        );
    })
    .catch_unwind()
    .await;
    world.finish(outcome).await;
}

/// The built-in HTTP sink (#462) behind the real drain: a receiver that
/// answers 503 and then 200 gets the event, and the record is published once.
#[tokio::test]
async fn the_http_sink_retries_a_503_and_publishes_once_the_receiver_accepts() {
    use crate::outbox_http::tests::{Ca, Files, Receiver, Reply, intake};
    let ca = Ca::new();
    let receiver = Receiver::start(&ca, &[Reply::Status(503), Reply::Status(200)]).await;
    let files = Files::new(&ca);
    files.rotate();
    let sink = Arc::new(intake(&receiver, &files));
    let Some(world) =
        World::configured(|app| app.outbox_consumer(crate::outbox_http::consumer_name(), sink))
            .await
    else {
        return;
    };
    let outcome = AssertUnwindSafe(async {
        let t = capture(&world.built, world.owner, "delivered over HTTP").await;
        world.wait_for_lane(t, "http", "published").await;
        let seen = receiver.seen();
        assert_eq!(seen.len(), 2, "one 503, one 200");
        assert_eq!(
            seen[0].body, seen[1].body,
            "the retry carries the same bytes"
        );
        assert_eq!(
            seen.iter()
                .map(|s| s.header("proxima-delivery-count").map(str::to_owned))
                .collect::<Vec<_>>(),
            [Some("1".to_owned()), Some("2".to_owned())]
        );
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert_eq!(
            receiver.seen().len(),
            2,
            "published means delivered no more"
        );
    })
    .catch_unwind()
    .await;
    world.finish(outcome).await;
}

/// A 422 is the receiver's durable refusal: the lane settles (a rejection is
/// a decision, docs/host-integration.md §Delivery) and the receiver is hit exactly once.
#[tokio::test]
async fn the_http_sink_settles_a_422_after_one_request() {
    use crate::outbox_http::tests::{Ca, Files, Receiver, Reply, intake};
    let ca = Ca::new();
    let receiver = Receiver::start(&ca, &[Reply::Status(422)]).await;
    let files = Files::new(&ca);
    files.rotate();
    let sink = Arc::new(intake(&receiver, &files));
    let Some(world) =
        World::configured(|app| app.outbox_consumer(crate::outbox_http::consumer_name(), sink))
            .await
    else {
        return;
    };
    let outcome = AssertUnwindSafe(async {
        let t = capture(&world.built, world.owner, "refused over HTTP").await;
        world.wait_for_lane(t, "http", "published").await;
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert_eq!(receiver.seen().len(), 1, "a rejection is never retried");
    })
    .catch_unwind()
    .await;
    world.finish(outcome).await;
}

/// A sink every drain in a test delivers to, so a second delivery of one
/// event shows up whichever drain made it. The first `accept` can be held:
/// it parks on `reached`/`resume` before it records anything.
#[derive(Debug)]
struct GatedIntake {
    gate_first: AtomicBool,
    reached: Arc<tokio::sync::Barrier>,
    resume: Arc<tokio::sync::Barrier>,
    accepted: Arc<Mutex<Vec<String>>>,
}

impl GatedIntake {
    fn new(gate_first: bool, accepted: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            gate_first: AtomicBool::new(gate_first),
            reached: Arc::new(tokio::sync::Barrier::new(2)),
            resume: Arc::new(tokio::sync::Barrier::new(2)),
            accepted,
        }
    }
}

#[async_trait::async_trait]
impl DurableIntake for GatedIntake {
    async fn accept(&self, event: &ReceivedEvent) -> Result<Intake, IntakeError> {
        if self.gate_first.swap(false, Ordering::SeqCst) {
            self.reached.wait().await;
            self.resume.wait().await;
        }
        self.accepted
            .lock()
            .expect("accepted lock")
            .push(event.id.clone());
        Ok(Intake::Accepted)
    }
}

impl World {
    /// One drain whose passes a test runs itself, with a lease no test
    /// outlives: expiry happens only when a test forces it in SQL.
    fn manual_drain(&self, intake: Arc<dyn DurableIntake>) -> Arc<Drain> {
        let settings = Settings {
            lease: Duration::from_hours(1),
            accept_timeout: Duration::from_hours(1),
            ..Settings::new()
        };
        Arc::new(Drain::new(
            self.built.outbox_for_tests(),
            ConsumerName::default(),
            intake,
            settings,
            Arc::new(Proceed),
        ))
    }

    /// Run one pass of `drain` to completion on its own task.
    fn pass(drain: &Arc<Drain>) -> tokio::task::JoinHandle<usize> {
        let drain = drain.clone();
        tokio::spawn(async move {
            let (_gate, worker) = proxima_core::QuiesceGate::new();
            drain
                .pass(&CancellationToken::new(), &worker)
                .await
                .unwrap_or_default()
        })
    }

    async fn event_id(&self, t: Uuid) -> String {
        sqlx::query_scalar("SELECT event_id FROM proxima_core.publication_outbox WHERE t = $1")
            .bind(t)
            .fetch_one(&self.pool)
            .await
            .expect("outbox row")
    }

    /// Hard-delete one Fact row, the way an erase finally does: its
    /// dependent rows first, then the `memory` row, whose deletion trigger
    /// writes the erase witness the claim filter reads. The probe app's
    /// sidecar is not in the storage verb's registry, so the test runs the
    /// deletes itself rather than the verb.
    async fn mark_erased(&self, t: Uuid) {
        let mut tx = self.pool.begin().await.expect("begin erase");
        // The dependents come from the catalog: every foreign key onto
        // `memory`. A session-local function deletes them, so the Rust side
        // sends only fixed text.
        sqlx::query(
            "CREATE FUNCTION pg_temp.delete_fact_dependents(fact uuid) RETURNS void
             LANGUAGE plpgsql AS $fn$
             DECLARE dep record;
             BEGIN
               FOR dep IN
                 SELECT con.conrelid::regclass AS tbl, att.attname AS col
                   FROM pg_constraint con
                   JOIN pg_attribute att
                     ON att.attrelid = con.conrelid AND att.attnum = con.conkey[1]
                  WHERE con.contype = 'f'
                    AND con.confrelid = 'proxima_core.memory'::regclass
               LOOP
                 EXECUTE format('DELETE FROM %s WHERE %I = $1', dep.tbl, dep.col)
                   USING fact;
               END LOOP;
             END $fn$",
        )
        .execute(&mut *tx)
        .await
        .expect("create the dependent-delete helper");
        sqlx::query("SELECT pg_temp.delete_fact_dependents($1)")
            .bind(t)
            .execute(&mut *tx)
            .await
            .expect("delete dependent rows");
        sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
            .bind(t)
            .execute(&mut *tx)
            .await
            .expect("delete the memory row");
        tx.commit().await.expect("commit erase");
        let witnessed: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM proxima_core.erased_pin_target WHERE t = $1)",
        )
        .bind(t)
        .fetch_one(&self.pool)
        .await
        .expect("erase witness");
        assert!(witnessed, "the delete left the erase witness");
    }
}

/// A record erased after the drain claimed it, but before the drain got to
/// it, is never handed to the intake.
#[tokio::test]
async fn erase_between_claim_and_accept_is_not_delivered() {
    let Some(world) = World::new(None).await else {
        return;
    };
    let outcome = AssertUnwindSafe(async {
        let first = capture(&world.built, world.owner, "delivered").await;
        let second = capture(&world.built, world.owner, "erased while claimed").await;
        let erased_id = world.event_id(second).await;
        let accepted = Arc::new(Mutex::new(Vec::new()));
        let intake = Arc::new(GatedIntake::new(true, accepted.clone()));
        let drain = world.manual_drain(intake.clone());

        let pass = World::pass(&drain);
        intake.reached.wait().await;
        world.mark_erased(second).await;
        intake.resume.wait().await;
        assert_eq!(pass.await.expect("pass joins"), 2, "one pass claimed both");

        let accepted = accepted.lock().expect("accepted lock").clone();
        let first_id = world.event_id(first).await;
        assert_eq!(accepted.len(), 1, "only the unerased Fact is delivered");
        assert!(
            accepted[0] == first_id,
            "the delivered event is the unerased one"
        );
        assert!(
            !accepted.contains(&erased_id),
            "the erased record is not delivered"
        );
    })
    .catch_unwind()
    .await;
    world.finish(outcome).await;
}

/// The record is erased after the pre-accept re-check passed, while the
/// consumer is accepting it. The consumer has the event already (at-least-once
/// cannot take that back), but the outbox must not record the erased record
/// as delivered: the mark re-checks deliverability under the lane lock.
#[tokio::test]
async fn erase_between_renew_and_accept_is_not_marked_delivered() {
    let Some(world) = World::new(None).await else {
        return;
    };
    let outcome = AssertUnwindSafe(async {
        let fact = capture(&world.built, world.owner, "erased during accept").await;
        let accepted = Arc::new(Mutex::new(Vec::new()));
        // The gate sits inside `accept`, so the pre-accept renew has passed.
        let intake = Arc::new(GatedIntake::new(true, accepted.clone()));
        let drain = world.manual_drain(intake.clone());

        let pass = World::pass(&drain);
        intake.reached.wait().await;
        world.mark_erased(fact).await;
        intake.resume.wait().await;
        assert_eq!(pass.await.expect("pass joins"), 1, "one record claimed");

        let state: String = sqlx::query_scalar(
            "SELECT lane.state::text
               FROM proxima_core.publication_delivery lane
              WHERE lane.t = $1",
        )
        .bind(fact)
        .fetch_one(&world.pool)
        .await
        .expect("lane state");
        assert!(
            state != "published",
            "an erased record is not marked delivered (lane state {state})"
        );
    })
    .catch_unwind()
    .await;
    world.finish(outcome).await;
}

/// A claim whose lease expired while its drain was busy is taken over by a
/// second drain. When the first drain gets to the record, its claim is no
/// longer current and it must not deliver the record again.
#[tokio::test]
async fn expired_lease_delivered_by_one_process() {
    let Some(world) = World::new(None).await else {
        return;
    };
    let outcome = AssertUnwindSafe(async {
        let _held = capture(&world.built, world.owner, "holds drain A").await;
        let contested = capture(&world.built, world.owner, "lease expires").await;
        let contested_id = world.event_id(contested).await;
        let accepted = Arc::new(Mutex::new(Vec::new()));
        let intake_a = Arc::new(GatedIntake::new(true, accepted.clone()));
        let drain_a = world.manual_drain(intake_a.clone());
        let drain_b = world.manual_drain(Arc::new(GatedIntake::new(false, accepted.clone())));

        let pass_a = World::pass(&drain_a);
        intake_a.reached.wait().await;
        // Drain A holds both claims and is busy with the first record.
        sqlx::query(
            "UPDATE proxima_core.publication_delivery
                SET lease_expires_at = now() - interval '1 second'
              WHERE t = $1 AND consumer = 'default'",
        )
        .bind(contested)
        .execute(&world.pool)
        .await
        .expect("force the lease to expire");
        assert_eq!(World::pass(&drain_b).await.expect("pass joins"), 1);
        intake_a.resume.wait().await;
        pass_a.await.expect("pass joins");

        let deliveries = accepted
            .lock()
            .expect("accepted lock")
            .iter()
            .filter(|id| **id == contested_id)
            .count();
        assert_eq!(deliveries, 1, "the record is delivered by one process");
    })
    .catch_unwind()
    .await;
    world.finish(outcome).await;
}
