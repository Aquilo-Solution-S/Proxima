//! The delivery contract, against a real `JetStream` broker (issue #305).
//!
//! Each test is named after one acceptance criterion. They are skipped
//! locally when `PROXIMA_TEST_NATS_URL` is unset and REQUIRED under
//! `CI=true`: a delivery lane that silently passes without a broker proves
//! nothing.
//!
//! Every test owns its stream and its database and deletes both.

// One delivery property per test, and a delivery property is a sequence of
// broker round trips with an assertion between each: splitting the fenced
// republication proofs into helpers would hide the ordering that IS the
// claim being made.
#![allow(clippy::too_many_lines)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{
    DropFirstAcks, Fixture, RecordingIntake, SeenOutcome, batch, nats_url_or_skip,
    publisher_url_or_skip,
};
use proxima_core::storage_ports::publication::{
    AckOutcome, BrokerReceipt, ClaimToken, PublisherId,
};
use proxima_outbox_nats::{
    HookAction, JetStreamPublisher, NatsPublisherConfig, PublishHook, PublisherError,
    ReferenceConsumer,
};
use tokio_util::sync::CancellationToken;

/// A publish hook that drops the first `n` `PubAck`s and then behaves.
#[derive(Debug)]
struct DropFirstAcksHook {
    remaining: std::sync::Mutex<u32>,
    action: HookAction,
}

impl DropFirstAcksHook {
    fn new(n: u32, action: HookAction) -> Arc<Self> {
        Arc::new(Self {
            remaining: std::sync::Mutex::new(n),
            action,
        })
    }
}

#[async_trait::async_trait]
impl PublishHook for DropFirstAcksHook {
    async fn after_publish_ack(&self, _event_id: &str, _receipt: &BrokerReceipt) -> HookAction {
        let mut remaining = self.remaining.lock().expect("lock");
        if *remaining == 0 {
            HookAction::Continue
        } else {
            *remaining -= 1;
            self.action
        }
    }
}

/// A publish hook that cancels a token once the broker has acknowledged
/// one record — a shutdown arriving in the middle of a batch, at the one
/// moment a test can place it deterministically.
#[derive(Debug)]
struct CancelAfterFirstAck {
    cancel: CancellationToken,
}

#[async_trait::async_trait]
impl PublishHook for CancelAfterFirstAck {
    async fn after_publish_ack(&self, _event_id: &str, _receipt: &BrokerReceipt) -> HookAction {
        self.cancel.cancel();
        HookAction::Continue
    }
}

/// Drain until nothing is left to claim, or `passes` passes have run.
async fn drain_until_idle(publisher: &JetStreamPublisher, passes: u32) -> u64 {
    let mut total = 0u64;
    for _ in 0..passes {
        let pass = publisher.drain_once().await.expect("a drain pass");
        total += pass.published as u64;
        if pass.claimed == 0 {
            break;
        }
    }
    total
}

#[tokio::test]
async fn publisher_works_without_stream_management_rights() {
    let Some(admin_url) = nats_url_or_skip("publisher_works_without_stream_management_rights")
    else {
        return;
    };
    let Some(_publisher_url) =
        publisher_url_or_skip("publisher_works_without_stream_management_rights")
    else {
        return;
    };
    Fixture::new("nats_publisher_permissions", admin_url)
        .await
        .run(async |fixture| {
            fixture
                .capture("publisher has no topology rights", None)
                .await
                .expect("capture succeeds before publication");
            let publisher = JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
                .await
                .expect("publish-only credentials connect without stream management");
            assert_eq!(drain_until_idle(&publisher, 2).await, 1);
        })
        .await;
}

#[tokio::test]
async fn deployment_transform_can_route_to_a_partition_consumer() {
    let Some(admin_url) =
        nats_url_or_skip("deployment_transform_can_route_to_a_partition_consumer")
    else {
        return;
    };
    Fixture::new("nats_partition_transform", admin_url)
        .await
        .run(async |fixture| {
            let durable = format!("partition_{}", fixture.stream.to_lowercase());
            fixture
                .update_stream(|config| {
                    config.subject_transform =
                        Some(async_nats::jetstream::stream::SubjectTransform {
                            source: format!("{}.>", fixture.config.subject_prefix),
                            destination: "partition.>".to_owned(),
                        });
                })
                .await;
            fixture.provision_consumer(&durable, "partition.>").await;

            fixture
                .capture("deployment transform partition", None)
                .await
                .expect("capture succeeds before publication");
            let publisher = JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
                .await
                .expect("publisher does not inspect or reject the transform");
            assert_eq!(drain_until_idle(&publisher, 2).await, 1);

            let intake = RecordingIntake::new();
            let consumer =
                ReferenceConsumer::connect(fixture.consumer_config_named(durable), intake.clone())
                    .await
                    .expect("consumer binds the deployment-provided partition durable");
            consume_until(&consumer, &intake, 1).await;
            assert_eq!(intake.distinct_ids().len(), 1);
            assert_eq!(
                intake.seen()[0].subject.split('.').next(),
                Some("partition")
            );
        })
        .await;
}

/// Consume until `want` distinct ids have been durably recorded, or the
/// deadline passes.
async fn consume_until(consumer: &ReferenceConsumer, intake: &RecordingIntake, want: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        consumer
            .process_once(batch(32), Duration::from_millis(500))
            .await
            .expect("a consume pass");
        if intake.distinct_ids().len() >= want {
            return;
        }
    }
}

#[tokio::test]
async fn publishes_captured_envelope_verbatim() {
    let Some(url) = nats_url_or_skip("publishes_captured_envelope_verbatim") else {
        return;
    };
    Fixture::new("nats_verbatim", url)
        .await
        .run(async |fixture| {
            let outcome = fixture.capture("verbatim", None).await.expect("captured");
            let t = outcome.memory_id.into_inner();
            let stored = fixture.stored_envelope(t).await;

            let publisher = JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
                .await
                .expect("the publisher connects");
            let report = publisher.drain_once().await.expect("a drain pass");
            assert_eq!(report.published, 1, "{report:?}");
            assert_eq!(fixture.state(t).await, "published");

            let intake = RecordingIntake::new();
            let consumer = ReferenceConsumer::connect(fixture.consumer_config(), intake.clone())
                .await
                .expect("the consumer binds");
            consume_until(&consumer, &intake, 1).await;

            let seen = intake.seen();
            assert_eq!(seen.len(), 1, "{seen:?}");
            assert_eq!(
                seen[0].raw, stored,
                "the publisher must ship the captured bytes, not a re-serialization"
            );
            assert!(
                seen[0]
                    .subject
                    .starts_with(&format!("{}.", fixture.config.subject_prefix)),
                "unexpected subject {}",
                seen[0].subject
            );
        })
        .await;
}

#[tokio::test]
async fn late_commit_is_delivered_after_restart() {
    let Some(url) = nats_url_or_skip("late_commit_is_delivered_after_restart") else {
        return;
    };
    Fixture::new("nats_late_commit", url)
        .await
        .run(async |fixture| {
            let early = fixture.capture("early", None).await.expect("captured");

            let publisher = JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
                .await
                .expect("the publisher connects");
            assert_eq!(publisher.drain_once().await.expect("drain").published, 1);
            // The publisher goes away. A Fact commits while it is down: capture is
            // a property of the Fact's transaction, not of the publisher's uptime.
            drop(publisher);
            let late = fixture.capture("late", None).await.expect("captured");
            assert_eq!(fixture.state(late.memory_id.into_inner()).await, "pending");

            let restarted = JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
                .await
                .expect("the publisher reconnects");
            assert_eq!(drain_until_idle(&restarted, 4).await, 1);
            assert_eq!(
                fixture.state(late.memory_id.into_inner()).await,
                "published"
            );
            assert_eq!(
                fixture.state(early.memory_id.into_inner()).await,
                "published"
            );

            let intake = RecordingIntake::new();
            let consumer = ReferenceConsumer::connect(fixture.consumer_config(), intake.clone())
                .await
                .expect("the consumer binds");
            consume_until(&consumer, &intake, 2).await;
            assert_eq!(intake.distinct_ids().len(), 2);
        })
        .await;
}

#[tokio::test]
async fn lost_puback_republishes_same_bytes() {
    let Some(url) = nats_url_or_skip("lost_puback_republishes_same_bytes") else {
        return;
    };
    Fixture::new("nats_lost_puback", url)
        .await
        .run(async |fixture| {
            let outcome = fixture
                .capture("lost-puback", None)
                .await
                .expect("captured");
            let t = outcome.memory_id.into_inner();
            let stored = fixture.stored_envelope(t).await;

            // The broker accepted the bytes; the acknowledgement never came back.
            let dropping = JetStreamPublisher::connect_with_hook(
                fixture.config.clone(),
                fixture.outbox(),
                DropFirstAcksHook::new(1, HookAction::DropAck),
            )
            .await
            .expect("the publisher connects");
            let report = dropping.drain_once().await.expect("a drain pass");
            assert_eq!(report.claimed, 1);
            assert_eq!(report.published, 0, "the marker never committed");
            assert_eq!(fixture.state(t).await, "claimed");

            // The lease expires and a second pass republishes the SAME bytes. The
            // broker deduplicates on `Nats-Msg-Id`, so the stream still holds one.
            tokio::time::sleep(fixture.config.lease + Duration::from_millis(500)).await;
            let honest = JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
                .await
                .expect("the publisher connects");
            assert_eq!(drain_until_idle(&honest, 4).await, 1);
            assert_eq!(fixture.state(t).await, "published");

            let intake = RecordingIntake::new();
            let consumer = ReferenceConsumer::connect(fixture.consumer_config(), intake.clone())
                .await
                .expect("the consumer binds");
            consume_until(&consumer, &intake, 1).await;
            let seen = intake.seen();
            assert_eq!(
                seen.len(),
                1,
                "the broker's dedup window must absorb the republication: {seen:?}"
            );
            assert_eq!(seen[0].raw, stored);
        })
        .await;
}

#[tokio::test]
async fn crash_after_puback_before_ack_republishes() {
    let Some(url) = nats_url_or_skip("crash_after_puback_before_ack_republishes") else {
        return;
    };
    Fixture::new("nats_crash_after_ack", url)
        .await
        .run(async |fixture| {
            let first = fixture.capture("crash-a", None).await.expect("captured");
            let second = fixture.capture("crash-b", None).await.expect("captured");

            // The process dies between the broker's acknowledgement and the
            // outbox marker. Both records stay leased.
            let crashing = JetStreamPublisher::connect_with_hook(
                fixture.config.clone(),
                fixture.outbox(),
                DropFirstAcksHook::new(1, HookAction::AbortDrain),
            )
            .await
            .expect("the publisher connects");
            let report = crashing.drain_once().await.expect("a drain pass");
            assert_eq!(report.published, 0, "{report:?}");

            tokio::time::sleep(fixture.config.lease + Duration::from_millis(500)).await;
            let restarted = JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
                .await
                .expect("the publisher restarts");
            assert_eq!(drain_until_idle(&restarted, 6).await, 2);
            assert_eq!(
                fixture.state(first.memory_id.into_inner()).await,
                "published"
            );
            assert_eq!(
                fixture.state(second.memory_id.into_inner()).await,
                "published"
            );

            let intake = RecordingIntake::new();
            let consumer = ReferenceConsumer::connect(fixture.consumer_config(), intake.clone())
                .await
                .expect("the consumer binds");
            consume_until(&consumer, &intake, 2).await;
            // `seen()`, not `distinct_ids()`: the deduplicated view cannot
            // observe a duplicate, so asserting on it would hold whatever
            // the broker did. This is the raw delivery count.
            assert_eq!(
                intake.seen().len(),
                2,
                "a crash before the marker must not duplicate the event: {:?}",
                intake.seen()
            );
            assert_eq!(intake.distinct_ids().len(), 2);
        })
        .await;
}

#[tokio::test]
async fn concurrent_publishers_and_stale_workers_lose_nothing() {
    let Some(url) = nats_url_or_skip("concurrent_publishers_and_stale_workers_lose_nothing") else {
        return;
    };
    Fixture::new("nats_concurrent", url)
        .await
        .run(async |fixture| {
            for n in 0..8 {
                fixture
                    .capture(&format!("concurrent-{n}"), None)
                    .await
                    .expect("captured");
            }

            let mut left = fixture.config.clone();
            left.publisher_id = PublisherId::new("worker-left").expect("a valid label");
            left.batch = batch(3);
            let mut right = fixture.config.clone();
            right.publisher_id = PublisherId::new("worker-right").expect("a valid label");
            right.batch = batch(3);

            let a = JetStreamPublisher::connect(left, fixture.outbox())
                .await
                .expect("left connects");
            let b = JetStreamPublisher::connect(right, fixture.outbox())
                .await
                .expect("right connects");
            let (from_a, from_b) = tokio::join!(drain_until_idle(&a, 8), drain_until_idle(&b, 8));
            assert_eq!(from_a + from_b, 8, "every record is published exactly once");

            // A worker that comes back after its lease expired is FENCED, on both
            // of the two paths that can have overtaken it.
            let outbox = fixture.outbox();
            let ghost = PublisherId::new("worker-ghost").expect("a valid label");
            let live = PublisherId::new("worker-live").expect("a valid label");

            // (a) another worker re-claimed the record in the meantime.
            let reclaimed = fixture
                .capture("stale-reclaim", None)
                .await
                .expect("captured");
            let held = outbox
                .claim(&ghost, batch(1), Duration::from_secs(1))
                .await
                .expect("the ghost claims");
            assert_eq!(held.len(), 1);
            tokio::time::sleep(Duration::from_millis(1_500)).await;
            let taken = outbox
                .claim(&live, batch(1), Duration::from_secs(30))
                .await
                .expect("a live worker re-claims the expired record");
            assert_eq!(taken.len(), 1);
            assert_eq!(taken[0].id, held[0].id, "the same record changed hands");
            let receipt = BrokerReceipt {
                stream: fixture.stream.clone(),
                sequence: 9_999,
            };
            assert_eq!(
                outbox
                    .mark_published(held[0].id, held[0].claim, &receipt)
                    .await
                    .expect("the port answers"),
                AckOutcome::StaleClaim,
                "an expired claim must not mark a record another worker now holds"
            );
            assert_eq!(
                outbox
                    .mark_published(taken[0].id, taken[0].claim, &receipt)
                    .await
                    .expect("the port answers"),
                AckOutcome::Published,
                "and the live claim still works"
            );
            assert_eq!(
                fixture.state(reclaimed.memory_id.into_inner()).await,
                "published"
            );

            // (b) the record was fully delivered while the ghost was away.
            let delivered = fixture
                .capture("stale-delivered", None)
                .await
                .expect("captured");
            let held = outbox
                .claim(&ghost, batch(1), Duration::from_secs(1))
                .await
                .expect("the ghost claims");
            assert_eq!(held.len(), 1);
            let ghost_claim: ClaimToken = held[0].claim;
            tokio::time::sleep(Duration::from_millis(1_500)).await;
            assert_eq!(drain_until_idle(&a, 4).await, 1, "the live worker delivers");
            assert_eq!(
                outbox
                    .mark_published(held[0].id, ghost_claim, &receipt)
                    .await
                    .expect("the port answers"),
                AckOutcome::AlreadyPublished,
                "a delivered record is terminal; a late worker cannot rewrite its receipt"
            );
            assert_eq!(
                fixture.state(delivered.memory_id.into_inner()).await,
                "published"
            );

            let intake = RecordingIntake::new();
            let consumer = ReferenceConsumer::connect(fixture.consumer_config(), intake.clone())
                .await
                .expect("the consumer binds");
            consume_until(&consumer, &intake, 9).await;
            assert_eq!(
                intake.distinct_ids().len(),
                9,
                "eight concurrent records plus the one the live worker rescued: {:?}",
                intake.seen()
            );
        })
        .await;
}

#[tokio::test]
async fn consumer_durable_intake_then_lost_ack_redelivers_idempotently() {
    let Some(url) =
        nats_url_or_skip("consumer_durable_intake_then_lost_ack_redelivers_idempotently")
    else {
        return;
    };
    // A one-second deployment-configured dedup window and a two-second
    // deployment-configured `ack_wait`, so the
    // redelivery provably lands OUTSIDE the window the broker deduplicates
    // in: idempotency at the sink is what carries this case, not the
    // broker's `Nats-Msg-Id` memory.
    Fixture::new("nats_lost_ack", url)
        .await
        .run(async |fixture| {
            let duplicate_window = Duration::from_secs(1);
            fixture
                .update_stream(|config| config.duplicate_window = duplicate_window)
                .await;
            fixture.capture("lost-ack", None).await.expect("captured");
            let publisher = JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
                .await
                .expect("the publisher connects");
            assert_eq!(drain_until_idle(&publisher, 4).await, 1);

            let intake = RecordingIntake::new();
            let consumer = ReferenceConsumer::connect_with_hook(
                fixture.consumer_config(),
                intake.clone(),
                DropFirstAcks::new(1),
            )
            .await
            .expect("the consumer binds");
            let report = consumer
                .process_once(batch(8), Duration::from_secs(2))
                .await
                .expect("a consume pass");
            assert_eq!(report.accepted, 1, "{report:?}");
            assert_eq!(report.unacked, 1, "the acknowledgement was dropped");

            // `ack_wait` elapses and the broker redelivers. The sink sees the same
            // `CloudEvents` id a second time and deduplicates on it.
            let first_delivery = std::time::Instant::now();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            while intake.seen().len() < 2 && tokio::time::Instant::now() < deadline {
                consumer
                    .process_once(batch(8), Duration::from_millis(500))
                    .await
                    .expect("a consume pass");
            }
            let elapsed = first_delivery.elapsed();
            let seen = intake.seen();
            assert_eq!(seen.len(), 2, "the broker must redeliver: {seen:?}");
            assert!(
                elapsed > duplicate_window,
                "the redelivery must land beyond the broker's {duplicate_window:?} dedup window \
                 to prove the sink's own idempotency carries it, took {elapsed:?}"
            );
            assert_eq!(seen[0].id, seen[1].id, "the same event, twice");
            assert_eq!(seen[0].raw, seen[1].raw, "byte-identical");
            assert!(
                seen[1].delivered_count > seen[0].delivered_count,
                "the redelivery is observable: {seen:?}"
            );
            assert_eq!(
                intake.distinct_ids().len(),
                1,
                "one event, once, to the sink"
            );
        })
        .await;
}

#[tokio::test]
async fn consumer_retains_rejection_before_ack_and_holds_failed_intake() {
    let Some(url) =
        nats_url_or_skip("consumer_retains_rejection_before_ack_and_holds_failed_intake")
    else {
        return;
    };
    Fixture::new("nats_rejection", url)
        .await
        .run(async |fixture| {
            fixture.capture("rejected", None).await.expect("captured");
            fixture.capture("undecided", None).await.expect("captured");
            let publisher = JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
                .await
                .expect("the publisher connects");
            assert_eq!(drain_until_idle(&publisher, 4).await, 2);

            let intake = RecordingIntake::new();
            intake.reject_note("rejected");
            intake.fail_note("undecided");
            let consumer = ReferenceConsumer::connect(fixture.consumer_config(), intake.clone())
                .await
                .expect("the consumer binds");
            let mut rejected = 0;
            let mut deferred = 0;
            for _ in 0..4 {
                let report = consumer
                    .process_once(batch(8), Duration::from_millis(500))
                    .await
                    .expect("a consume pass");
                rejected += report.rejected;
                deferred += report.deferred;
            }
            assert!(rejected >= 1, "the rejection is durable and acknowledged");
            assert!(
                deferred >= 1,
                "an intake that could not commit must NOT be acknowledged"
            );
            assert!(
                intake
                    .seen()
                    .iter()
                    .any(|seen| seen.outcome == SeenOutcome::Rejected)
            );

            // The sink recovers; the held message is redelivered and accepted.
            intake.clear_failures();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            while !intake
                .seen()
                .iter()
                .any(|seen| seen.outcome == SeenOutcome::Accepted)
                && tokio::time::Instant::now() < deadline
            {
                consumer
                    .process_once(batch(8), Duration::from_millis(500))
                    .await
                    .expect("a consume pass");
            }
            assert!(
                intake
                    .seen()
                    .iter()
                    .any(|seen| seen.outcome == SeenOutcome::Accepted),
                "the held event must survive the sink's outage: {:?}",
                intake.seen()
            );
        })
        .await;
}

#[tokio::test]
async fn broker_capacity_backpressure_is_explicit() {
    let Some(url) = nats_url_or_skip("broker_capacity_backpressure_is_explicit") else {
        return;
    };
    // A stream too small to hold one envelope. `discard: New` means the
    // broker REFUSES rather than evicting, which is the whole point: an
    // outbox that lost records to make room would be a delivery hole.
    Fixture::new("nats_capacity", url)
        .await
        .run(async |fixture| {
            fixture
                .update_stream(|config| {
                    config.max_bytes = 1024;
                    config.max_message_size = 256;
                })
                .await;
            let outcome = fixture
                .capture(&"x".repeat(2048), None)
                .await
                .expect("capture is unaffected by the broker's ceiling");
            let t = outcome.memory_id.into_inner();

            let publisher = JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
                .await
                .expect("the publisher connects");
            let error = publisher
                .drain_once()
                .await
                .expect_err("an oversized envelope must be an explicit refusal");
            assert!(
                matches!(error, PublisherError::BrokerCapacity(_)),
                "expected explicit backpressure, got {error}"
            );
            assert_eq!(
                fixture.state(t).await,
                "pending",
                "a refused record returns to the queue; nothing is dropped"
            );
        })
        .await;
}

#[tokio::test]
async fn one_unpublishable_record_does_not_stall_the_ones_behind_it() {
    let Some(url) = nats_url_or_skip("one_unpublishable_record_does_not_stall_the_ones_behind_it")
    else {
        return;
    };
    // A stream with room for many envelopes but a per-message ceiling one
    // record is over. That record can NEVER be delivered under this
    // configuration — the case that used to abort the pass, hand back the
    // whole batch, and be re-claimed at the head of the next one forever.
    Fixture::new("nats_poison", url)
        .await
        .run(async |fixture| {
            fixture
                .update_stream(|config| {
                    config.max_bytes = 8 * 1024 * 1024;
                    config.max_message_size = 2048;
                })
                .await;
            let poison = fixture
                .capture(&"p".repeat(8 * 1024), None)
                .await
                .expect("capture is unaffected by the broker's ceiling");
            let poison_t = poison.memory_id.into_inner();
            for n in 0..3 {
                fixture
                    .capture(&format!("behind-{n}"), None)
                    .await
                    .expect("captured");
            }

            let publisher = JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
                .await
                .expect("the publisher connects");
            let report = publisher
                .drain_once()
                .await
                .expect("a pass that delivered three of four records is not a failed pass");
            assert_eq!(
                report.published, 3,
                "the three records behind the refusal must still ship: {report:?}"
            );
            assert_eq!(report.failed, 1, "{report:?}");
            assert_eq!(
                fixture.state(poison_t).await,
                "pending",
                "the refused record returns to the queue rather than holding its lease"
            );

            // And again on the next pass, with fresh work behind it: the
            // refusal costs its own slot, not the queue.
            for n in 3..5 {
                fixture
                    .capture(&format!("behind-{n}"), None)
                    .await
                    .expect("captured");
            }
            let report = publisher.drain_once().await.expect("a second pass");
            assert_eq!(report.published, 2, "{report:?}");
            assert_eq!(report.failed, 1, "{report:?}");
            assert_eq!(fixture.state(poison_t).await, "pending");

            // With nothing but the refusal left, the pass IS a failure —
            // which is what makes the run loop back off instead of spinning.
            let error = publisher
                .drain_once()
                .await
                .expect_err("a pass that delivered nothing must report why");
            assert!(
                matches!(error, PublisherError::BrokerCapacity(_)),
                "expected explicit backpressure, got {error}"
            );

            let intake = RecordingIntake::new();
            let consumer = ReferenceConsumer::connect(fixture.consumer_config(), intake.clone())
                .await
                .expect("the consumer binds");
            consume_until(&consumer, &intake, 5).await;
            assert_eq!(
                intake.distinct_ids().len(),
                5,
                "every deliverable record reached the sink: {:?}",
                intake.seen()
            );
        })
        .await;
}

#[tokio::test]
async fn cancellation_inside_a_batch_hands_the_rest_of_the_claims_back() {
    let Some(url) =
        nats_url_or_skip("cancellation_inside_a_batch_hands_the_rest_of_the_claims_back")
    else {
        return;
    };
    Fixture::new("nats_cancel_mid_batch", url)
        .await
        .run(async |fixture| {
            for n in 0..6 {
                fixture
                    .capture(&format!("shutdown-{n}"), None)
                    .await
                    .expect("captured");
            }

            // The shutdown lands after the first record's PubAck. Without
            // cancellation inside the batch the loop would publish all six
            // first, and a broker that had stopped answering would hold the
            // shutdown for `batch × publish_timeout`.
            let cancel = CancellationToken::new();
            let publisher = JetStreamPublisher::connect_with_hook(
                fixture.config.clone(),
                fixture.outbox(),
                Arc::new(CancelAfterFirstAck {
                    cancel: cancel.clone(),
                }),
            )
            .await
            .expect("the publisher connects");

            let summary = tokio::time::timeout(Duration::from_secs(10), publisher.run(cancel))
                .await
                .expect("a cancelled publisher must stop promptly");
            assert_eq!(summary.published, 1, "{summary:?}");

            // Nothing is left leased: a claim nobody holds any more would
            // cost the next process a full lease before it could try.
            let claimed: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM proxima_core.publication_outbox \
                 WHERE state = 'claimed'",
            )
            .fetch_one(fixture.pg.pool_for_tests())
            .await
            .expect("the count answers");
            assert_eq!(claimed, 0, "a cancelled pass must release what it holds");
            assert_eq!(
                fixture.outbox().pending_count().await.expect("count"),
                5,
                "and the five it did not publish are deliverable again at once"
            );
        })
        .await;
}

#[tokio::test]
async fn bad_config_fails_explicitly() {
    // No broker needed: these are refusals the configuration makes on its
    // own, before anything is opened.
    let env = |pairs: Vec<(&'static str, &'static str)>| {
        move |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    };
    for pairs in [
        vec![
            ("PROXIMA_NATS_URL", "nats://127.0.0.1:4222"),
            ("PROXIMA_NATS_SUBJECT_PREFIX", "proxima.fact.*"),
        ],
        vec![
            ("PROXIMA_NATS_URL", "nats://127.0.0.1:4222"),
            ("PROXIMA_NATS_LEASE_SECS", "0"),
        ],
        vec![
            ("PROXIMA_NATS_URL", "nats://127.0.0.1:4222"),
            ("PROXIMA_NATS_PUBLISH_TIMEOUT_MS", "0"),
        ],
        vec![
            ("PROXIMA_NATS_URL", "nats://127.0.0.1:4222"),
            ("PROXIMA_NATS_TOKEN", "t"),
            ("PROXIMA_NATS_USER", "u"),
            ("PROXIMA_NATS_PASSWORD", "p"),
        ],
    ] {
        let described = format!("{pairs:?}");
        NatsPublisherConfig::from_lookup(env(pairs))
            .expect_err(&format!("{described} must be refused"));
    }

    // And the presence key alone is a complete publisher configuration.
    let ok =
        NatsPublisherConfig::from_lookup(env(vec![("PROXIMA_NATS_URL", "nats://127.0.0.1:4222")]))
            .expect("a bare URL is enough")
            .expect("the presence key is set");
    assert_eq!(ok.subject_prefix, "proxima.fact");
}

#[tokio::test]
async fn upgrade_does_not_reinterpret_captured_event() {
    let Some(url) = nats_url_or_skip("upgrade_does_not_reinterpret_captured_event") else {
        return;
    };
    Fixture::new("nats_upgrade", url)
        .await
        .run(async |fixture| {
            // An older release captured this event under its own producer
            // identity. Nothing about that identity is re-derived later: it was
            // bound at authorization time and sealed into the bytes.
            let legacy_source =
                proxima_core::publication::PublicationSource::new("urn:proxima:an-older-release")
                    .expect("a URN is absolute");
            let old = fixture
                .capture_with_source(
                    fixture.owner,
                    legacy_source,
                    "captured-by-the-old-release",
                    None,
                )
                .await
                .expect("captured");
            let old_t = old.memory_id.into_inner();
            let old_bytes = fixture.stored_envelope(old_t).await;

            // The deployment is upgraded: today's configuration names a different
            // producer identity, and today's code writes a different envelope.
            let new = fixture
                .capture_with_source(
                    fixture.owner,
                    common::source(),
                    "captured-by-this-release",
                    None,
                )
                .await
                .expect("captured");
            let new_bytes = fixture.stored_envelope(new.memory_id.into_inner()).await;
            assert_ne!(old_bytes, new_bytes, "the two releases differ");

            // The capture is append-only in the database itself: not even an
            // operator can rewrite an event after the fact, which is what makes
            // "the bytes are the artifact" a guarantee rather than a convention.
            let rewrite = sqlx::query(
                "UPDATE proxima_core.publication_outbox SET envelope = $2 WHERE t = $1",
            )
            .bind(old_t)
            .bind(&new_bytes)
            .execute(fixture.pg.pool_for_tests())
            .await;
            assert!(
                rewrite.is_err(),
                "the captured envelope must be append-only"
            );

            let publisher = JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
                .await
                .expect("the publisher connects");
            assert_eq!(drain_until_idle(&publisher, 4).await, 2);

            let intake = RecordingIntake::new();
            let consumer = ReferenceConsumer::connect(fixture.consumer_config(), intake.clone())
                .await
                .expect("the consumer binds");
            consume_until(&consumer, &intake, 2).await;

            let seen = intake.seen();
            assert_eq!(seen.len(), 2, "{seen:?}");
            let old_delivered = seen
                .iter()
                .find(|s| s.raw == old_bytes)
                .expect("the old release's event must arrive byte-for-byte as captured");
            let envelope: serde_json::Value =
                serde_json::from_slice(&old_delivered.raw).expect("a CloudEvents envelope");
            assert_eq!(
                envelope["source"], "urn:proxima:an-older-release",
                "the running release must not stamp its own identity onto an older capture"
            );
            assert!(
                seen.iter().any(|s| s.raw == new_bytes),
                "and this release's own event arrives as it was captured too"
            );
        })
        .await;
}

#[tokio::test]
async fn owner_binding_follows_authorized_write() {
    let Some(url) = nats_url_or_skip("owner_binding_follows_authorized_write") else {
        return;
    };
    Fixture::new("nats_owner_binding", url)
        .await
        .run(async |fixture| {
            let other =
                proxima_core::OwnerRef::Group(proxima_core::GroupId::new(uuid::Uuid::now_v7()));
            sqlx::query(
                "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, $2::proxima_core.owner_kind)
             ON CONFLICT (owner_id) DO NOTHING",
            )
            .bind(other.stored_owner_id())
            .bind(proxima_core::OwnerRefKind::of(&other).as_str())
            .execute(fixture.pg.pool_for_tests())
            .await
            .expect("the second owner registers");

            fixture.capture("mine", None).await.expect("captured");
            fixture
                .capture_as(other, "theirs", None)
                .await
                .expect("captured");

            let publisher = JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
                .await
                .expect("the publisher connects");
            assert_eq!(drain_until_idle(&publisher, 4).await, 2);

            let intake = RecordingIntake::new();
            let consumer = ReferenceConsumer::connect(fixture.consumer_config(), intake.clone())
                .await
                .expect("the consumer binds");
            consume_until(&consumer, &intake, 2).await;

            let mine = fixture.owner.stored_owner_id().hyphenated().to_string();
            let theirs = other.stored_owner_id().hyphenated().to_string();
            for seen in intake.seen() {
                let envelope: serde_json::Value =
                    serde_json::from_slice(&seen.raw).expect("a CloudEvents envelope");
                let owner = envelope["proximaowner"].as_str().expect("an owner key");
                let (expected_id, expected_kind) = if owner.contains(&mine) {
                    (&mine, "personal")
                } else {
                    (&theirs, "group")
                };
                assert!(
                    seen.subject
                        .contains(&format!("{expected_kind}.{expected_id}")),
                    "the subject must carry the write's own owner kind and id: {} vs {owner}",
                    seen.subject
                );
            }
            assert_eq!(intake.distinct_ids().len(), 2);
        })
        .await;
}

#[tokio::test]
async fn broker_outage_does_not_block_capture() {
    let Some(url) = nats_url_or_skip("broker_outage_does_not_block_capture") else {
        return;
    };
    Fixture::new("nats_outage", url)
        .await
        .run(async |fixture| {
            let mut dead = fixture.config.clone();
            // A port nothing listens on: the broker is down, from this host's view.
            dead.url = "nats://127.0.0.1:1".to_owned();
            dead.publish_timeout = Duration::from_millis(500);
            let error = JetStreamPublisher::connect(dead, fixture.outbox())
                .await
                .expect_err("an unreachable broker must be an explicit error");
            assert!(
                matches!(error, PublisherError::Connect(_)),
                "expected a connect failure, got {error}"
            );

            // Capture is a property of the Fact's transaction. The outage bounds
            // the BACKLOG, it does not fail the write.
            for n in 0..3 {
                fixture
                    .capture(&format!("during-outage-{n}"), None)
                    .await
                    .expect("a Fact write must not depend on the broker");
            }
            assert_eq!(
                fixture.outbox().pending_count().await.expect("count"),
                3,
                "the records wait; nothing is lost"
            );

            // The broker comes back and the backlog drains.
            let publisher = JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
                .await
                .expect("the publisher reconnects");
            assert_eq!(drain_until_idle(&publisher, 4).await, 3);
            assert_eq!(fixture.outbox().pending_count().await.expect("count"), 0);
        })
        .await;
}
