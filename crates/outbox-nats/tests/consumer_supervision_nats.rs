//! Runtime health and ownership oracles for the reference consumer.

#[allow(dead_code)]
mod common;

use std::future::Future;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Fixture, RecordingIntake, nats_url_or_skip};
use proxima_outbox_nats::{
    ConsumerConnectionState, ConsumerPassState, ConsumerTaskState, DurableIntake, Intake,
    IntakeError, JetStreamPublisher, ReceivedEvent, ReferenceConsumer,
};
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;

const ERROR_MARKER: &str = "SYNTHETIC_CONSUMER_SECRET";
const PANIC_MARKER: &str = "SYNTHETIC_CONSUMER_PANIC";
const PAYLOAD_MARKER: &str = "SYNTHETIC_CONSUMER_PAYLOAD_MARKER";

#[derive(Clone)]
struct EventBuffer(Arc<Mutex<Vec<String>>>);

impl<S> Layer<S> for EventBuffer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut fields = EventFields::default();
        event.record(&mut fields);
        self.0.lock().expect("event buffer lock").push(fields.0);
    }
}

#[derive(Default)]
struct EventFields(String);

impl Visit for EventFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        write!(&mut self.0, "{}={value:?} ", field.name()).expect("string write");
    }
}

async fn with_dispatch<F: Future>(dispatch: tracing::Dispatch, future: F) -> F::Output {
    let mut future = Box::pin(future);
    std::future::poll_fn(|cx| {
        tracing::dispatcher::with_default(&dispatch, || future.as_mut().poll(cx))
    })
    .await
}

fn event_dispatch(events: Arc<Mutex<Vec<String>>>) -> tracing::Dispatch {
    tracing::Dispatch::new(tracing_subscriber::registry().with(EventBuffer(events)))
}

async fn wait_until(mut condition: impl FnMut() -> bool, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[derive(Debug)]
struct CancelOnFailureIntake {
    cancel: CancellationToken,
}

#[async_trait::async_trait]
impl DurableIntake for CancelOnFailureIntake {
    async fn accept(&self, _event: &ReceivedEvent) -> Result<Intake, IntakeError> {
        self.cancel.cancel();
        Err(IntakeError::new(ERROR_MARKER))
    }
}

#[derive(Debug)]
struct PanicIntake;

#[async_trait::async_trait]
impl DurableIntake for PanicIntake {
    async fn accept(&self, _event: &ReceivedEvent) -> Result<Intake, IntakeError> {
        panic!("{PANIC_MARKER}");
    }
}

async fn capture_and_publish(
    fixture: &Fixture,
    publisher: &JetStreamPublisher,
    note: &str,
) -> (uuid::Uuid, Vec<u8>) {
    let outcome = fixture.capture(note, None).await.expect("Fact is captured");
    let id = outcome.memory_id.into_inner();
    let raw = fixture.stored_envelope(id).await;
    let report = publisher.drain_once().await.expect("Fact is published");
    assert_eq!(report.published, 1, "{report:?}");
    (id, raw)
}

async fn publisher_for(fixture: &Fixture) -> JetStreamPublisher {
    JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
        .await
        .expect("the test publisher connects")
}

#[tokio::test]
async fn observed_consumer_guard_handles_prepoll_abort_connected_abort_and_panic() {
    let Some(url) =
        nats_url_or_skip("observed_consumer_guard_handles_prepoll_abort_connected_abort_and_panic")
    else {
        return;
    };
    Fixture::new("nats_consumer_health_lifetime", url)
        .await
        .run(async |fixture| {
            let publisher = publisher_for(fixture).await;
            let mut proxy = ConsumerProxy::bind(&fixture.url).await;

            let mut config = fixture.consumer_config();
            config.url = proxy.url();
            let unpolled = ReferenceConsumer::connect(config, RecordingIntake::new())
                .await
                .expect("pre-poll consumer connects");
            let (prepoll_health, unpolled_future) =
                unpolled.into_observed_parts(CancellationToken::new());
            assert_eq!(prepoll_health.snapshot().task, ConsumerTaskState::Starting);
            drop(unpolled_future);
            assert_eq!(prepoll_health.snapshot().task, ConsumerTaskState::Stopped);
            assert_eq!(
                prepoll_health.snapshot().connection,
                ConsumerConnectionState::NotObserved
            );
            proxy.wait_bridges_closed().await;

            let mut config = fixture.consumer_config();
            config.url = proxy.url();
            let connected_abort = ReferenceConsumer::connect(config, RecordingIntake::new())
                .await
                .expect("connected-abort consumer connects");
            let (abort_health, abort_future) =
                connected_abort.into_observed_parts(CancellationToken::new());
            let events = Arc::new(Mutex::new(Vec::new()));
            let abort_task = tokio::spawn(with_dispatch(event_dispatch(events), abort_future));
            wait_until(
                || {
                    abort_health.snapshot().connection == ConsumerConnectionState::Connected
                        && abort_health.snapshot().pass == ConsumerPassState::Clean
                },
                "the connected consumer to complete one clean pass",
            )
            .await;
            abort_task.abort();
            let abort_error = abort_task.await.expect_err("task abort is joined");
            assert!(abort_error.is_cancelled());
            assert_eq!(abort_health.snapshot().task, ConsumerTaskState::Stopped);
            assert_eq!(
                abort_health.snapshot().connection,
                ConsumerConnectionState::NotObserved
            );
            proxy.wait_bridges_closed().await;

            let _ = capture_and_publish(fixture, &publisher, "panic after real delivery").await;
            let mut config = fixture.consumer_config();
            config.url = proxy.url();
            let panicking = ReferenceConsumer::connect(config, Arc::new(PanicIntake))
                .await
                .expect("panic consumer connects");
            let (panic_health, panic_future) =
                panicking.into_observed_parts(CancellationToken::new());
            let events = Arc::new(Mutex::new(Vec::new()));
            let mut panic_task = tokio::spawn(with_dispatch(event_dispatch(events), panic_future));
            let panic = tokio::time::timeout(Duration::from_secs(10), &mut panic_task)
                .await
                .expect("consumer panic task finishes")
                .expect_err("injected intake panic reaches the caller's JoinHandle");
            assert!(panic.is_panic());
            assert_eq!(panic_health.snapshot().task, ConsumerTaskState::Stopped);
            assert_eq!(
                panic_health.snapshot().connection,
                ConsumerConnectionState::NotObserved
            );
            assert!(!format!("{panic_health:?}").contains(PANIC_MARKER));
            proxy.wait_bridges_closed().await;
            proxy.shutdown().await;
        })
        .await;
}

#[tokio::test]
async fn observed_consumer_intake_failure_sanitizes_logs_and_health() {
    let Some(url) = nats_url_or_skip("observed_consumer_intake_failure_sanitizes_logs_and_health")
    else {
        return;
    };
    Fixture::new("nats_consumer_health_sanitation", url)
        .await
        .run(async |fixture| {
            let publisher = publisher_for(fixture).await;
            let (fact_id, raw) = capture_and_publish(fixture, &publisher, PAYLOAD_MARKER).await;
            let event_id = serde_json::from_slice::<serde_json::Value>(&raw)
                .expect("captured envelope is JSON")["id"]
                .as_str()
                .expect("captured envelope has an id")
                .to_owned();
            let cancel = CancellationToken::new();
            let consumer = ReferenceConsumer::connect(
                fixture.consumer_config(),
                Arc::new(CancelOnFailureIntake {
                    cancel: cancel.clone(),
                }),
            )
            .await
            .expect("consumer connects for marker error intake");
            let (health, future) = consumer.into_observed_parts(cancel);
            let events = Arc::new(Mutex::new(Vec::new()));
            let task = tokio::spawn(with_dispatch(event_dispatch(events.clone()), future));
            task.await
                .expect("consumer loop exits after the marked failure");
            let state = health.snapshot();
            assert_eq!(state.task, ConsumerTaskState::Stopped);
            assert_eq!(state.connection, ConsumerConnectionState::NotObserved);
            assert_eq!(state.pass, ConsumerPassState::Failed);
            assert!(!format!("{health:?}").contains(ERROR_MARKER));
            assert!(!format!("{health:?}").contains(&event_id));
            assert!(!format!("{health:?}").contains(PAYLOAD_MARKER));

            let output = events.lock().expect("event buffer lock").join("\n");
            assert!(
                output.contains("Intake"),
                "intake category missing: {output}"
            );
            assert!(
                output.contains("PartialPass"),
                "partial category missing: {output}"
            );
            for marker in [ERROR_MARKER, PAYLOAD_MARKER, event_id.as_str()] {
                assert!(
                    !output.contains(marker),
                    "consumer log leaked {marker}: {output}"
                );
            }
            assert!(fixture.state(fact_id).await == "published");
        })
        .await;
}

struct ActiveBridge(Arc<AtomicUsize>);

impl Drop for ActiveBridge {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct ConsumerProxy {
    addr: SocketAddr,
    active: Arc<AtomicUsize>,
    bridge_tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    cancel: CancellationToken,
    accept_task: Option<JoinHandle<()>>,
}

impl ConsumerProxy {
    async fn bind(target_url: &str) -> Self {
        let target = target_url
            .split(',')
            .next()
            .expect("one NATS server URL")
            .trim()
            .rsplit('@')
            .next()
            .expect("NATS server authority");
        let authority = target.strip_prefix("nats://").unwrap_or(target);
        let target_addr = authority
            .to_socket_addrs()
            .expect("NATS server address resolves")
            .next()
            .expect("NATS server address exists");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("proxy listener binds");
        let addr = listener.local_addr().expect("proxy address");
        let active = Arc::new(AtomicUsize::new(0));
        let bridge_tasks = Arc::new(Mutex::new(Vec::new()));
        let cancel = CancellationToken::new();
        let accept_task = {
            let active = active.clone();
            let bridge_tasks = bridge_tasks.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                loop {
                    let accepted = tokio::select! {
                        () = cancel.cancelled() => break,
                        accepted = listener.accept() => accepted,
                    };
                    let Ok((mut downstream, _)) = accepted else {
                        continue;
                    };
                    let upstream = tokio::select! {
                        () = cancel.cancelled() => break,
                        upstream = TcpStream::connect(target_addr) => upstream,
                    };
                    let Ok(mut upstream) = upstream else {
                        continue;
                    };
                    active.fetch_add(1, Ordering::AcqRel);
                    let active_guard = ActiveBridge(active.clone());
                    let bridge_cancel = cancel.clone();
                    let task = tokio::spawn(async move {
                        let _active_guard = active_guard;
                        tokio::select! {
                            () = bridge_cancel.cancelled() => {}
                            _ = copy_bidirectional(&mut downstream, &mut upstream) => {}
                        }
                    });
                    bridge_tasks
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(task);
                }
            })
        };
        Self {
            addr,
            active,
            bridge_tasks,
            cancel,
            accept_task: Some(accept_task),
        }
    }

    fn url(&self) -> String {
        format!("nats://{}", self.addr)
    }

    async fn wait_bridges_closed(&self) {
        wait_until(
            || self.active.load(Ordering::Acquire) == 0,
            "proxy bridges to close",
        )
        .await;
    }

    async fn abort_bridges(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let tasks = std::mem::take(
                &mut *self
                    .bridge_tasks
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            for task in &tasks {
                task.abort();
            }
            for task in tasks {
                let _ = task.await;
            }
            if self.active.load(Ordering::Acquire) == 0 {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "proxy bridges did not close"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn shutdown(&mut self) {
        self.cancel.cancel();
        if let Some(task) = self.accept_task.take() {
            let _ = task.await;
        }
        self.abort_bridges().await;
    }
}

impl Drop for ConsumerProxy {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = self.accept_task.take() {
            task.abort();
        }
        for task in self
            .bridge_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
        {
            task.abort();
        }
    }
}
