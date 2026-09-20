//! The first end-to-end copy-cleaner oracle: capture, real erase, committed
//! eligibility check, finite production scan slice, and server-visible GET.

#[allow(dead_code)]
mod common;

use std::{future::Future, time::Duration};
use std::{
    net::{SocketAddr, ToSocketAddrs},
    num::NonZeroU32,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use async_nats::jetstream;
use common::{
    COPY_CLEANER_STREAM, COPY_CLEANER_SUBJECT_PREFIX, Fixture, cleaner_config_or_skip,
    nats_url_or_skip, test_admin_client,
};
use futures::StreamExt;
use proxima_core::mcp::{PrefixedUuidClass, format_prefixed_uuid};
use proxima_core::owner_inverse::{EraseAuthorization, OwnerEraseOutcome, OwnerEraseTarget};
use proxima_core::storage_ports::OwnerInversePort;
use proxima_core::storage_ports::publication::{
    AckOutcome, BrokerReceipt, ClaimedPublication, PublicationOriginEligibility,
    PublicationOriginEligibilityPort, PublicationRetentionPort, PublisherId, ReleaseOutcome,
};
use proxima_core::storage_ports::{OwnerTransferPort, OwnerWritePermit, PublicationOutboxPort};
use proxima_core::{
    AccessKind, GroupId, MemoryId, Owner, OwnerRef, SourceId, StorageError, UserId,
};
use proxima_outbox_nats::{JetStreamCopyCleaner, JetStreamPublisher};
use proxima_storage_pg::PgStorage;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Notify, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::{Event, Subscriber, field::Visit};
use tracing_subscriber::{
    Layer,
    filter::{LevelFilter, Targets},
    layer::Context,
    prelude::*,
};

#[tokio::test]
#[allow(clippy::large_futures, clippy::too_many_lines)]
async fn source_erasure_removes_a_published_copy_after_a_finite_scan() {
    let Some(url) = nats_url_or_skip("copy cleaner erase") else {
        return;
    };
    let Some(cleaner_config) = cleaner_config_or_skip("copy cleaner erase") else {
        return;
    };
    let cleaner_config = cleaner_config
        .bounds(
            NonZeroU32::new(1).expect("positive scan slice"),
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(20),
            Duration::from_secs(1),
        )
        .expect("all cleaner waits remain bounded");
    let fixture = Fixture::new_copy_cleaner(url).await;
    fixture
        .run(async |fixture| {
            provision_cleaner_stream(fixture).await;
            let admin = test_admin_client(&fixture.url).await;
            let context = jetstream::new(admin.clone());
            let stream = context
                .get_stream(COPY_CLEANER_STREAM)
                .await
                .expect("the test-owned canonical stream exists");
            let start = stream
                .get_info()
                .await
                .expect("initial stream info answers")
                .state
                .last_sequence;

            let captured = fixture
                .capture("copy-cleaner-erasure", Some("copy-cleaner-erasure"))
                .await
                .expect("the Fact and immutable publication origin commit together");
            let preserved = fixture
                .capture_with_source_scope(
                    fixture.owner,
                    common::source(),
                    Some("cleaner/preserved-source"),
                    "copy-cleaner-preserved",
                    Some("copy-cleaner-preserved"),
                )
                .await
                .expect("the unrelated Fact is captured by the same exclusive publisher");
            let unrelated_owner = Owner::Group(GroupId::new(uuid::Uuid::now_v7()));
            register_owner(fixture.pg.pool_for_tests(), &unrelated_owner).await;
            let unrelated = fixture
                .capture_with_source_scope(
                    unrelated_owner,
                    common::source(),
                    Some("cleaner/other-owner"),
                    "copy-cleaner-other-owner",
                    Some("copy-cleaner-other-owner"),
                )
                .await
                .expect("an unrelated original owner is captured");
            let captured_bytes = fixture
                .stored_envelope(captured.memory_id.into_inner())
                .await;
            let preserved_bytes = fixture
                .stored_envelope(preserved.memory_id.into_inner())
                .await;
            let unrelated_bytes = fixture
                .stored_envelope(unrelated.memory_id.into_inner())
                .await;
            let publisher = JetStreamPublisher::connect(fixture.config.clone(), fixture.outbox())
                .await
                .expect("the ordinary publisher connects");
            let drain = publisher
                .drain_once()
                .await
                .expect("all captured byte strings publish");
            assert_eq!(drain.published, 3);
            let sequence = start + 1;
            let preserved_sequence = sequence + 1;
            let unrelated_sequence = sequence + 2;
            let before_erase = stream
                .get_raw_message(sequence)
                .await
                .expect("the actual event is retained before erasure");
            assert_eq!(before_erase.payload.as_ref(), captured_bytes);
            assert_eq!(
                before_erase
                    .headers
                    .get("Nats-Msg-Id")
                    .map(ToString::to_string),
                serde_json::from_slice::<serde_json::Value>(&captured_bytes)
                    .expect("captured envelope is JSON")["id"]
                    .as_str()
                    .map(ToOwned::to_owned)
            );
            let preserved_before_erase = stream
                .get_raw_message(preserved_sequence)
                .await
                .expect("the unrelated copy is retained before erasure");
            assert_eq!(preserved_before_erase.payload.as_ref(), preserved_bytes);
            let unrelated_before_erase = stream
                .get_raw_message(unrelated_sequence)
                .await
                .expect("the other owner's copy is retained before erasure");
            assert_eq!(unrelated_before_erase.payload.as_ref(), unrelated_bytes);

            let Owner::Personal(user_id) = fixture.owner else {
                panic!("the fixture owner is personal");
            };
            let source_id = SourceId::new("probe/source");
            let authorization =
                EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
                    user_id,
                    source_id: source_id.clone(),
                    drop_event_id: "copy-cleaner-source-erasure".to_owned(),
                });
            let erased = OwnerInversePort::erase_personal_source_scope(
                &fixture.pg,
                &authorization,
                user_id,
                &source_id,
                fixture.pg.surfaces(),
            )
            .await
            .expect("real source erasure commits without a broker request");
            assert!(matches!(erased, OwnerEraseOutcome::Completed { .. }));
            assert_eq!(
                fixture
                    .pg
                    .check_committed(fixture.owner, captured.memory_id)
                    .await
                    .expect("the committed origin check succeeds"),
                PublicationOriginEligibility::Ineligible
            );

            let mut cleaner = JetStreamCopyCleaner::connect(
                cleaner_config.clone(),
                std::sync::Arc::new(fixture.pg.clone()),
                fixture.origin_scope(),
            )
            .await
            .expect("the limited cleaner credential binds to the canonical stream");
            let report = complete_cycle(&mut cleaner, 1).await;
            assert!(report.cycle_complete);
            assert!(report.cycle_clean);
            assert_eq!(report.examined, 3);
            assert_eq!(report.deleted, 1);
            assert_eq!(report.unknown, 0);

            let after = stream
                .get_raw_message(sequence)
                .await
                .expect_err("the JetStream sequence is no longer visible");
            assert!(matches!(
                after.kind(),
                async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
            ));
            let preserved_after_erase = stream
                .get_raw_message(preserved_sequence)
                .await
                .expect("unrelated eligible origin is retained");
            assert_eq!(preserved_after_erase.payload.as_ref(), preserved_bytes);
            assert_eq!(
                fixture
                    .pg
                    .check_committed(fixture.owner, preserved.memory_id)
                    .await
                    .expect("the preserved original is checked under a committed fence"),
                PublicationOriginEligibility::Eligible
            );
            assert_eq!(
                fixture
                    .pg
                    .check_committed(unrelated_owner, unrelated.memory_id)
                    .await
                    .expect("the unrelated original owner remains eligible"),
                PublicationOriginEligibility::Eligible
            );

            // Transfer does not change the captured original owner. Pruning
            // the delivered body also leaves its immutable origin eligible.
            let destination = Owner::Personal(UserId::new(uuid::Uuid::now_v7()));
            register_owner(fixture.pg.pool_for_tests(), &destination).await;
            assert!(
                OwnerTransferPort::transfer_to_owner(
                    &fixture.pg,
                    &OwnerWritePermit::new_for_tests(fixture.owner, AccessKind::Fact),
                    proxima_core::EntityId::Memory(preserved.memory_id),
                    destination,
                    fixture.pg.surfaces(),
                )
                .await
                .expect("real transfer preserves the origin")
            );
            assert_eq!(
                fixture
                    .pg
                    .prune_published(
                        Duration::ZERO,
                        NonZeroU32::new(8).expect("positive prune batch"),
                    )
                    .await
                    .expect("published body retention succeeds"),
                2
            );
            assert_eq!(
                fixture
                    .pg
                    .check_committed(fixture.owner, preserved.memory_id)
                    .await
                    .expect("transfer and body prune do not revoke origin"),
                PublicationOriginEligibility::Eligible
            );
            assert_eq!(
                fixture
                    .pg
                    .check_committed(unrelated_owner, unrelated.memory_id)
                    .await
                    .expect("pruning leaves the other original owner eligible"),
                PublicationOriginEligibility::Eligible
            );
            let mut cleaner = JetStreamCopyCleaner::connect(
                cleaner_config.clone(),
                Arc::new(fixture.pg.clone()),
                fixture.origin_scope(),
            )
            .await
            .expect("the cleaner reconnects for a fresh full snapshot");
            let report = complete_cycle(&mut cleaner, 1).await;
            assert!(report.cycle_complete);
            assert!(report.cycle_clean);
            assert_eq!(report.deleted, 0);
            assert_eq!(
                stream
                    .get_raw_message(preserved_sequence)
                    .await
                    .expect("transferred and pruned origin remains broker-visible")
                    .payload
                    .as_ref(),
                preserved_bytes
            );
            assert_eq!(
                stream
                    .get_raw_message(unrelated_sequence)
                    .await
                    .expect("the unrelated retained copy survives pruning")
                    .payload
                    .as_ref(),
                unrelated_bytes
            );

            // The committed eligibility check finishes before this wrapper
            // pauses. Source erase must commit while the scan owns no DB
            // transaction, and the next complete cycle must find revocation
            // behind the scanner's current position.
            let check_gate = Arc::new(AfterCheckBarrier::new(fixture.pg.clone()));
            let mut cleaner = JetStreamCopyCleaner::connect(
                cleaner_config.clone(),
                check_gate.clone(),
                fixture.origin_scope(),
            )
            .await
            .expect("the cleaner binds before a gated origin check");
            let scan_task = tokio::spawn(async move {
                let result = cleaner.scan_slice().await;
                (cleaner, result)
            });
            tokio::time::timeout(Duration::from_secs(5), check_gate.checked.notified())
                .await
                .expect("the real committed Eligible check reaches its post-commit barrier");
            let source_id = SourceId::new("cleaner/preserved-source");
            let Owner::Personal(user_id) = fixture.owner else {
                panic!("the fixture owner is personal");
            };
            let authorization =
                EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
                    user_id,
                    source_id: source_id.clone(),
                    drop_event_id: "copy-cleaner-check-race".to_owned(),
                });
            let erased = tokio::time::timeout(
                Duration::from_secs(5),
                OwnerInversePort::erase_personal_source_scope(
                    &fixture.pg,
                    &authorization,
                    user_id,
                    &source_id,
                    fixture.pg.surfaces(),
                ),
            )
            .await
            .expect("source erase completes while the test wrapper waits without a DB fence")
            .expect("real source erase commits");
            assert!(matches!(erased, OwnerEraseOutcome::Completed { .. }));
            check_gate.release.notify_one();
            let (mut cleaner, stale_slice) = scan_task.await.expect("finite scan task joins");
            let stale_slice = stale_slice.expect("the pre-erase committed check completed");
            assert_eq!(stale_slice.examined, 1);
            assert!(!stale_slice.cycle_complete);
            let stale_cycle = complete_cycle(&mut cleaner, 1).await;
            assert!(stale_cycle.cycle_complete);
            assert!(stale_cycle.cycle_clean);
            assert_eq!(
                stream
                    .get_raw_message(preserved_sequence)
                    .await
                    .expect("the completed older snapshot leaves the stale positive copy")
                    .payload
                    .as_ref(),
                preserved_bytes
            );
            let revoked_cycle = complete_cycle(&mut cleaner, 1).await;
            assert!(revoked_cycle.cycle_complete);
            assert!(revoked_cycle.cycle_clean);
            assert_eq!(revoked_cycle.deleted, 1);
            assert!(matches!(
                stream.get_raw_message(preserved_sequence).await,
                Err(ref error) if matches!(
                    error.kind(),
                    async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
                )
            ));

            let whole_owner_capture = fixture
                .capture_with_source_scope(
                    fixture.owner,
                    common::source(),
                    Some("cleaner/whole-owner"),
                    "copy-cleaner-whole-owner",
                    Some("copy-cleaner-whole-owner"),
                )
                .await
                .expect("the whole-owner target Fact is captured");
            let whole_owner_bytes = fixture
                .stored_envelope(whole_owner_capture.memory_id.into_inner())
                .await;
            let whole_owner_sequence = stream
                .get_info()
                .await
                .expect("stream info before whole-owner publish")
                .state
                .last_sequence
                + 1;
            assert_eq!(
                publisher
                    .drain_once()
                    .await
                    .expect("whole-owner target publishes")
                    .published,
                1
            );
            assert_eq!(
                stream
                    .get_raw_message(whole_owner_sequence)
                    .await
                    .expect("whole-owner source copy is retained")
                    .payload
                    .as_ref(),
                whole_owner_bytes
            );

            let Owner::Personal(user_id) = fixture.owner else {
                panic!("the fixture owner is personal");
            };
            let authorization =
                EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
                    user_id,
                    drop_event_id: "copy-cleaner-whole-owner-erase".to_owned(),
                });
            let erased = OwnerInversePort::erase_personal_owner(
                &fixture.pg,
                &authorization,
                user_id,
                fixture.pg.surfaces(),
            )
            .await
            .expect("real whole-owner erase commits without a broker request");
            assert!(matches!(erased, OwnerEraseOutcome::Completed { .. }));
            assert_eq!(
                fixture
                    .pg
                    .check_committed(fixture.owner, whole_owner_capture.memory_id)
                    .await
                    .expect("the erased whole-owner origin is checked"),
                PublicationOriginEligibility::Ineligible
            );
            assert_eq!(
                fixture
                    .pg
                    .check_committed(fixture.owner, preserved.memory_id)
                    .await
                    .expect("the transferred original-owner origin is checked"),
                PublicationOriginEligibility::Ineligible
            );
            assert_eq!(
                fixture
                    .pg
                    .check_committed(unrelated_owner, unrelated.memory_id)
                    .await
                    .expect("the unrelated original owner remains eligible"),
                PublicationOriginEligibility::Eligible
            );
            let mut cleaner = JetStreamCopyCleaner::connect(
                cleaner_config.clone(),
                Arc::new(fixture.pg.clone()),
                fixture.origin_scope(),
            )
            .await
            .expect("the cleaner starts a new owner-erasure cycle");
            let report = complete_cycle(&mut cleaner, 1).await;
            assert!(report.cycle_complete);
            assert!(report.cycle_clean);
            assert_eq!(report.deleted, 1);
            assert!(matches!(
                stream.get_raw_message(preserved_sequence).await,
                Err(ref error) if matches!(
                    error.kind(),
                    async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
                )
            ));
            assert!(matches!(
                stream.get_raw_message(whole_owner_sequence).await,
                Err(ref error) if matches!(
                    error.kind(),
                    async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
                )
            ));
            assert_eq!(
                stream
                    .get_raw_message(unrelated_sequence)
                    .await
                    .expect("another owner's retained copy survives whole-owner erase")
                    .payload
                    .as_ref(),
                unrelated_bytes
            );

            // Return a REAL committed claim only after source erasure and a
            // complete cleaner snapshot. The publisher then sends the exact
            // captured bytes late; the next cycle's fresh end catches it.
            let late = fixture
                .capture_with_source_scope(
                    unrelated_owner,
                    common::source(),
                    Some("cleaner/late-publish"),
                    "copy-cleaner-late-publication",
                    Some("copy-cleaner-late-publication"),
                )
                .await
                .expect("late-publication Fact is captured");
            let late_bytes = fixture.stored_envelope(late.memory_id.into_inner()).await;
            let snapshot_end = stream
                .get_info()
                .await
                .expect("pre-publication snapshot info")
                .state
                .last_sequence;
            let claim_gate = Arc::new(ClaimReturnBarrier::new(fixture.pg.clone()));
            let late_publisher =
                JetStreamPublisher::connect(fixture.config.clone(), claim_gate.clone())
                    .await
                    .expect("the ordinary publisher accepts the real claim barrier");
            let drain_task = tokio::spawn(async move { late_publisher.drain_once().await });
            tokio::time::timeout(Duration::from_secs(5), claim_gate.claimed.notified())
                .await
                .expect("the real claim commits before its return is held");

            let Owner::Group(group_id) = unrelated_owner else {
                panic!("the unrelated owner is a group");
            };
            let late_source = SourceId::new("cleaner/late-publish");
            let authorization =
                EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupSourceScope {
                    group_id,
                    source_id: late_source.clone(),
                });
            let erased = OwnerInversePort::erase_group_source_scope(
                &fixture.pg,
                &authorization,
                group_id,
                &late_source,
                fixture.pg.surfaces(),
            )
            .await
            .expect("real source erasure commits while no publication transaction is held");
            assert!(matches!(erased, OwnerEraseOutcome::Completed { .. }));
            assert_eq!(
                fixture
                    .pg
                    .check_committed(unrelated_owner, late.memory_id)
                    .await
                    .expect("late claim origin is now revoked"),
                PublicationOriginEligibility::Ineligible
            );

            let before_late_publish = complete_cycle(&mut cleaner, 1).await;
            assert!(before_late_publish.cycle_complete);
            assert!(before_late_publish.cycle_clean);
            assert_eq!(before_late_publish.deleted, 0);
            assert_eq!(
                stream
                    .get_info()
                    .await
                    .expect("the broker snapshot remains before late PubAck")
                    .state
                    .last_sequence,
                snapshot_end
            );
            claim_gate.release.notify_one();
            let late_drain = drain_task
                .await
                .expect("the actual publisher drain joins")
                .expect("the stale claimed bytes still receive broker PubAck");
            assert_eq!(late_drain.claimed, 1);
            assert_eq!(late_drain.published, 0);
            assert_eq!(late_drain.failed, 0);
            assert_eq!(late_drain.stale, 1);
            let late_sequence = snapshot_end + 1;
            assert!(late_sequence > snapshot_end);
            assert_eq!(
                stream
                    .get_raw_message(late_sequence)
                    .await
                    .expect("the late publisher creates the retained copy")
                    .payload
                    .as_ref(),
                late_bytes
            );
            let after_late_publish = complete_cycle(&mut cleaner, 1).await;
            assert!(after_late_publish.cycle_complete);
            assert!(after_late_publish.cycle_clean);
            assert_eq!(after_late_publish.deleted, 1);
            assert!(matches!(
                stream.get_raw_message(late_sequence).await,
                Err(ref error) if matches!(
                    error.kind(),
                    async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
                )
            ));

            late_publication_beyond_pending_snapshot_is_rechecked(
                fixture,
                unrelated_owner,
                &publisher,
                &stream,
                &mut cleaner,
            )
            .await;

            // Hold the actual server reply to a real DELETE. The production
            // scan slice remains pending while a second real source erase
            // commits; neither transaction nor SQL fence crosses the broker
            // request boundary.
            let held_first = fixture
                .capture_with_source_scope(
                    unrelated_owner,
                    common::source(),
                    Some("cleaner/held-delete-first"),
                    "copy-cleaner-held-delete-first",
                    Some("copy-cleaner-held-delete-first"),
                )
                .await
                .expect("first held-delete Fact is captured");
            let held_second = fixture
                .capture_with_source_scope(
                    unrelated_owner,
                    common::source(),
                    Some("cleaner/held-delete-second"),
                    "copy-cleaner-held-delete-second",
                    Some("copy-cleaner-held-delete-second"),
                )
                .await
                .expect("second held-delete Fact is captured");
            let held_first_bytes = fixture
                .stored_envelope(held_first.memory_id.into_inner())
                .await;
            let held_second_bytes = fixture
                .stored_envelope(held_second.memory_id.into_inner())
                .await;
            let held_sequence_start = stream
                .get_info()
                .await
                .expect("stream info before held-delete publication")
                .state
                .last_sequence;
            let held_first_sequence = held_sequence_start + 1;
            let held_second_sequence = held_sequence_start + 2;
            let held_publish = publisher
                .drain_once()
                .await
                .expect("both held-delete byte strings publish");
            assert_eq!(held_publish.published, 2);
            assert_eq!(
                stream
                    .get_raw_message(held_first_sequence)
                    .await
                    .expect("first held-delete copy is retained before erasure")
                    .payload
                    .as_ref(),
                held_first_bytes
            );
            assert_eq!(
                stream
                    .get_raw_message(held_second_sequence)
                    .await
                    .expect("second held-delete copy is retained before erasure")
                    .payload
                    .as_ref(),
                held_second_bytes
            );
            let first_source = SourceId::new("cleaner/held-delete-first");
            let first_authorization =
                EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupSourceScope {
                    group_id,
                    source_id: first_source.clone(),
                });
            let first_erasure = OwnerInversePort::erase_group_source_scope(
                &fixture.pg,
                &first_authorization,
                group_id,
                &first_source,
                fixture.pg.surfaces(),
            )
            .await
            .expect("first real source erase commits before the cleaner scan");
            assert!(matches!(first_erasure, OwnerEraseOutcome::Completed { .. }));
            assert_eq!(
                fixture
                    .pg
                    .check_committed(unrelated_owner, held_first.memory_id)
                    .await
                    .expect("first held-delete origin has a committed verdict"),
                PublicationOriginEligibility::Ineligible
            );
            assert_eq!(
                fixture
                    .pg
                    .check_committed(unrelated_owner, held_second.memory_id)
                    .await
                    .expect("second held-delete origin remains eligible before its erase"),
                PublicationOriginEligibility::Eligible
            );

            let reply_gate = DeleteReplyGate::new();
            let mut proxy = DeleteReplyProxy::bind(&fixture.url, reply_gate.clone()).await;
            let held_config = proxima_outbox_nats::JetStreamCopyCleanerConfig::new(proxy.url())
                .auth(proxima_outbox_nats::NatsAuth::UserPassword {
                    user: std::env::var(common::ENV_NATS_CLEANER_USER)
                        .expect("cleaner username is configured"),
                    password: std::env::var(common::ENV_NATS_CLEANER_PASSWORD)
                        .expect("cleaner password is configured"),
                })
                .bounds(
                    NonZeroU32::new(128).expect("positive held-delete slice"),
                    Duration::from_secs(5),
                    Duration::from_secs(20),
                    Duration::from_millis(20),
                    Duration::from_secs(1),
                )
                .expect("held-delete bounds stay finite");
            let gated_eligibility = Arc::new(HoldDeleteEligibility::new(
                fixture.pg.clone(),
                reply_gate.clone(),
            ));
            let mut cleaner = JetStreamCopyCleaner::connect(
                held_config,
                gated_eligibility.clone(),
                fixture.origin_scope(),
            )
            .await
            .expect("the restricted cleaner connects through the reply proxy");
            let scan_task = tokio::spawn(async move {
                let result = cleaner.scan_slice().await;
                (cleaner, result)
            });
            tokio::time::timeout(
                Duration::from_secs(5),
                gated_eligibility.ineligible_checked.notified(),
            )
            .await
            .expect("real committed Ineligible check activates the response gate");
            assert!(reply_gate.is_held());
            tokio::time::timeout(Duration::from_secs(5), reply_gate.blocked_write.notified())
                .await
                .expect("proxy holds a server-to-client reply at the socket write boundary");

            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if matches!(
                        stream.get_raw_message(held_first_sequence).await,
                        Err(error)
                            if matches!(
                                error.kind(),
                                async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
                            )
                    ) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("direct admin GET observes that the broker processed DELETE");
            assert!(
                !scan_task.is_finished(),
                "the finite real DELETE future is held"
            );

            let second_source = SourceId::new("cleaner/held-delete-second");
            let second_authorization =
                EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupSourceScope {
                    group_id,
                    source_id: second_source.clone(),
                });
            let second_erasure = tokio::time::timeout(
                Duration::from_secs(5),
                OwnerInversePort::erase_group_source_scope(
                    &fixture.pg,
                    &second_authorization,
                    group_id,
                    &second_source,
                    fixture.pg.surfaces(),
                ),
            )
            .await
            .expect("second DB erase completes while the broker DELETE response is held")
            .expect("second source erase commits");
            assert!(matches!(
                second_erasure,
                OwnerEraseOutcome::Completed { .. }
            ));
            assert!(
                !scan_task.is_finished(),
                "the same finite scan is still pending"
            );
            assert_eq!(
                fixture
                    .pg
                    .check_committed(unrelated_owner, held_second.memory_id)
                    .await
                    .expect("second source erase has a committed verdict"),
                PublicationOriginEligibility::Ineligible
            );

            reply_gate.release();
            let (cleaner, held_report) = tokio::time::timeout(Duration::from_secs(10), scan_task)
                .await
                .expect("held finite scanner resumes after the reply is released")
                .expect("held finite scanner task joins");
            let held_report = held_report.expect("the real scan completes both broker deletes");
            assert_eq!(held_report.examined, 3);
            assert_eq!(held_report.deleted, 2);
            assert!(held_report.cycle_complete);
            assert!(held_report.cycle_clean);
            assert!(matches!(
                stream.get_raw_message(held_first_sequence).await,
                Err(ref error) if matches!(
                    error.kind(),
                    async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
                )
            ));
            assert!(matches!(
                stream.get_raw_message(held_second_sequence).await,
                Err(ref error) if matches!(
                    error.kind(),
                    async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
                )
            ));
            drop(cleaner);
            proxy.shutdown().await;

            cleaner_acl_oracles(&fixture.url).await;
            broker_outage_does_not_delay_erase(
                fixture,
                unrelated_owner,
                &publisher,
                &stream,
                &cleaner_config,
            )
            .await;
            real_sql_fault_retains_cursor(
                fixture,
                unrelated_owner,
                unrelated.memory_id,
                &publisher,
                &stream,
                &cleaner_config,
            )
            .await;
            exact_fact_erasure_removes_only_its_copy(fixture, &publisher, &stream, &cleaner_config)
                .await;
            recreated_cleaner_rechecks_from_first_retained(
                fixture,
                unrelated_owner,
                unrelated.memory_id,
                &stream,
                &cleaner_config,
            )
            .await;
            sparse_snapshot_skips_deleted_sequence_holes(
                fixture,
                unrelated_owner,
                &publisher,
                &stream,
                &cleaner_config,
            )
            .await;
            cleaner_recovers_after_idle_client_disconnect(fixture).await;
            malformed_copies_fail_health_without_starving_later_messages(
                fixture,
                unrelated_owner,
                unrelated_bytes.clone(),
                &publisher,
                &stream,
                &cleaner_config,
            )
            .await;
            a_foreign_installations_copy_halts_the_cycle(
                fixture,
                unrelated_owner,
                unrelated_bytes.clone(),
                &publisher,
                &stream,
                &cleaner_config,
            )
            .await;
        })
        .await;
}

/// A copy stamped by a DIFFERENT installation stops the whole cycle, and a
/// revoked copy behind it survives.
///
/// This is the deployment fault the stamp exists for: a cleaner pointed at
/// a stream some other Proxima publishes to would find no origin row for
/// any of it — because the rows are in that installation's database, not
/// this one — and read the entire stream as revoked. Nothing in the message
/// body distinguishes that from a genuine revocation, so the stamp is the
/// only place it can be caught.
///
/// The retained message behind the foreign one is the real assertion. It IS
/// ineligible here and would be deleted on any ordinary cycle; proving it
/// survives proves the halt is a halt and not a skip.
#[allow(clippy::too_many_lines)] // one real mixed-installation stream proves the halt
async fn a_foreign_installations_copy_halts_the_cycle(
    fixture: &Fixture,
    group_owner: OwnerRef,
    canonical_bytes: Vec<u8>,
    publisher: &JetStreamPublisher,
    stream: &jetstream::stream::Stream,
    cleaner_config: &proxima_outbox_nats::JetStreamCopyCleanerConfig,
) {
    let admin = test_admin_client(&fixture.url).await;
    let context = jetstream::new(admin.clone());
    let event: serde_json::Value =
        serde_json::from_slice(&canonical_bytes).expect("a canonical captured envelope parses");
    let (kind, owner_id) = group_owner.columns();
    let expected_subject = proxima_outbox_nats::subject_for(
        COPY_CLEANER_SUBJECT_PREFIX,
        kind.as_str(),
        owner_id,
        event["type"].as_str().expect("the event names its type"),
    );

    // Canonical in every structural respect. Only the stamp is another
    // installation's, which is exactly the case no other check can see.
    let foreign_payload = envelope_with_fresh_fact_id(&event);
    let foreign_id = serde_json::from_slice::<serde_json::Value>(&foreign_payload)
        .expect("the foreign event parses")["id"]
        .as_str()
        .expect("the foreign event id is a string")
        .to_owned();
    let foreign_scope =
        proxima_core::storage_ports::publication::OriginScope::new(uuid::Uuid::now_v7());
    assert_ne!(foreign_scope, fixture.origin_scope());
    let foreign_sequence = stream
        .get_info()
        .await
        .expect("stream info before the foreign copy")
        .state
        .last_sequence
        + 1;
    publish_raw_copy(
        &context,
        &expected_subject,
        cleaner_headers(
            &foreign_id,
            proxima_outbox_nats::CONTENT_TYPE_CLOUDEVENTS,
            foreign_scope,
        ),
        foreign_payload,
    )
    .await;

    // Behind it: this installation's own copy, revoked for real.
    let source = SourceId::new("cleaner/behind-foreign");
    let captured = fixture
        .capture_with_source_scope(
            group_owner,
            common::source(),
            Some("cleaner/behind-foreign"),
            "copy-cleaner-behind-foreign",
            Some("copy-cleaner-behind-foreign"),
        )
        .await
        .expect("a Fact behind the foreign copy is captured");
    let drain = publisher
        .drain_once()
        .await
        .expect("the copy behind the foreign one publishes");
    assert_eq!(drain.published, 1);
    let revoked_sequence = stream
        .get_info()
        .await
        .expect("stream info after the revoked copy")
        .state
        .last_sequence;
    let Owner::Group(group_id) = group_owner else {
        panic!("the behind-foreign Fact has its Group owner");
    };
    assert!(matches!(
        erase_group_source(
            fixture,
            group_id,
            &source,
            "the copy behind the foreign one is revoked",
        )
        .await,
        OwnerEraseOutcome::Completed { .. }
    ));
    assert_eq!(
        fixture
            .pg
            .check_committed(group_owner, captured.memory_id)
            .await
            .expect("the committed origin check succeeds"),
        PublicationOriginEligibility::Ineligible,
        "the copy behind the foreign one would be deleted by an ordinary cycle"
    );

    let mut cleaner = JetStreamCopyCleaner::connect(
        cleaner_config.clone(),
        Arc::new(fixture.pg.clone()),
        fixture.origin_scope(),
    )
    .await
    .expect("cleaner connects to the mixed-installation stream");
    let mut failure = None;
    for _ in 0..128 {
        match tokio::time::timeout(Duration::from_secs(10), cleaner.scan_slice())
            .await
            .expect("one finite scan slice finishes")
        {
            Ok(slice) => assert!(
                !slice.cycle_complete,
                "a cycle carrying a foreign copy must not complete"
            ),
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    assert_eq!(
        failure,
        Some(proxima_outbox_nats::CopyCleanerFailure::ForeignOriginScope)
    );
    assert!(
        stream.get_raw_message(foreign_sequence).await.is_ok(),
        "the foreign installation's copy is retained, never erased"
    );
    assert!(
        stream.get_raw_message(revoked_sequence).await.is_ok(),
        "the revoked copy behind the foreign one survives: the cycle stopped"
    );

    // Leave the shared stream clean for any later helper.
    stream
        .delete_message(foreign_sequence)
        .await
        .expect("the test removes its own synthetic foreign copy");
}

async fn provision_cleaner_stream(fixture: &Fixture) {
    let client = test_admin_client(&fixture.url).await;
    let context = jetstream::new(client);
    match context.get_stream(COPY_CLEANER_STREAM).await {
        Ok(_) => panic!("refusing to reuse a pre-existing cleaner stream"),
        Err(error) => match error.kind() {
            async_nats::jetstream::context::GetStreamErrorKind::JetStream(error)
                if error.kind() == async_nats::jetstream::ErrorCode::STREAM_NOT_FOUND => {}
            kind => panic!("canonical stream lookup failed outside the not-found case: {kind}"),
        },
    }
    context
        .create_stream(jetstream::stream::Config {
            name: COPY_CLEANER_STREAM.to_owned(),
            subjects: vec![format!("{COPY_CLEANER_SUBJECT_PREFIX}.>")],
            storage: jetstream::stream::StorageType::File,
            retention: jetstream::stream::RetentionPolicy::Limits,
            discard: jetstream::stream::DiscardPolicy::New,
            max_age: Duration::ZERO,
            max_bytes: 1024 * 1024 * 1024,
            max_message_size: -1,
            description: Some("test-owned Proxima copy-cleaner stream".to_owned()),
            ..jetstream::stream::Config::default()
        })
        .await
        .expect("the test-owned canonical stream is provisioned");
    fixture.mark_stream_created();
}

async fn register_owner(pool: &sqlx::PgPool, owner: &Owner) {
    let owner_ref = *owner;
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind)
         ON CONFLICT (owner_id) DO NOTHING",
    )
    .bind(owner_ref.stored_owner_id())
    .bind(owner_ref.columns().0.as_str())
    .execute(pool)
    .await
    .expect("the transfer destination owner registers");
}

struct AfterCheckBarrier {
    pg: PgStorage,
    armed: AtomicBool,
    checked: Notify,
    release: Notify,
}

impl AfterCheckBarrier {
    fn new(pg: PgStorage) -> Self {
        Self {
            pg,
            armed: AtomicBool::new(true),
            checked: Notify::new(),
            release: Notify::new(),
        }
    }
}

#[async_trait::async_trait]
impl PublicationOriginEligibilityPort for AfterCheckBarrier {
    async fn check_committed(
        &self,
        original_owner: OwnerRef,
        fact_id: MemoryId,
    ) -> Result<PublicationOriginEligibility, StorageError> {
        let eligibility = self.pg.check_committed(original_owner, fact_id).await?;
        if self.armed.swap(false, Ordering::AcqRel) {
            self.checked.notify_one();
            self.release.notified().await;
        }
        Ok(eligibility)
    }

    async fn check_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        original_owner: OwnerRef,
        fact_id: MemoryId,
    ) -> Result<PublicationOriginEligibility, StorageError> {
        PublicationOriginEligibilityPort::check_in_transaction(
            &self.pg,
            tx,
            original_owner,
            fact_id,
        )
        .await
    }
}

struct ClaimReturnBarrier {
    pg: PgStorage,
    armed: AtomicBool,
    claimed: Notify,
    release: Notify,
}

impl ClaimReturnBarrier {
    fn new(pg: PgStorage) -> Self {
        Self {
            pg,
            armed: AtomicBool::new(true),
            claimed: Notify::new(),
            release: Notify::new(),
        }
    }
}

struct HoldDeleteEligibility {
    pg: PgStorage,
    gate: DeleteReplyGate,
    armed: AtomicBool,
    ineligible_checked: Notify,
}

impl HoldDeleteEligibility {
    fn new(pg: PgStorage, gate: DeleteReplyGate) -> Self {
        Self {
            pg,
            gate,
            armed: AtomicBool::new(true),
            ineligible_checked: Notify::new(),
        }
    }
}

#[async_trait::async_trait]
impl PublicationOriginEligibilityPort for HoldDeleteEligibility {
    async fn check_committed(
        &self,
        original_owner: OwnerRef,
        fact_id: MemoryId,
    ) -> Result<PublicationOriginEligibility, StorageError> {
        let eligibility = self.pg.check_committed(original_owner, fact_id).await?;
        if eligibility == PublicationOriginEligibility::Ineligible
            && self.armed.swap(false, Ordering::AcqRel)
        {
            self.gate.hold();
            self.ineligible_checked.notify_one();
        }
        Ok(eligibility)
    }

    async fn check_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        original_owner: OwnerRef,
        fact_id: MemoryId,
    ) -> Result<PublicationOriginEligibility, StorageError> {
        PublicationOriginEligibilityPort::check_in_transaction(
            &self.pg,
            tx,
            original_owner,
            fact_id,
        )
        .await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplyGateState {
    Open,
    Held,
    Released,
}

#[derive(Clone)]
struct DeleteReplyGate {
    state: watch::Sender<ReplyGateState>,
    blocked_write: Arc<Notify>,
}

impl DeleteReplyGate {
    fn new() -> Self {
        let (state, _receiver) = watch::channel(ReplyGateState::Open);
        Self {
            state,
            blocked_write: Arc::new(Notify::new()),
        }
    }

    fn hold(&self) {
        self.state.send_replace(ReplyGateState::Held);
    }

    fn is_held(&self) -> bool {
        *self.state.borrow() == ReplyGateState::Held
    }

    fn release(&self) {
        self.state.send_replace(ReplyGateState::Released);
    }

    fn subscribe(&self) -> watch::Receiver<ReplyGateState> {
        self.state.subscribe()
    }

    async fn before_server_write(&self, state: &mut watch::Receiver<ReplyGateState>) {
        loop {
            if *state.borrow_and_update() != ReplyGateState::Held {
                return;
            }
            self.blocked_write.notify_one();
            if state.changed().await.is_err() {
                return;
            }
        }
    }
}

struct DeleteReplyProxy {
    addr: SocketAddr,
    cancel: CancellationToken,
    accept_task: Option<JoinHandle<()>>,
    bridges: Arc<Mutex<Vec<JoinHandle<()>>>>,
    allow_upstream: Arc<AtomicBool>,
}

impl DeleteReplyProxy {
    async fn bind(target_url: &str, gate: DeleteReplyGate) -> Self {
        let target_addr = target_url
            .split(',')
            .next()
            .expect("one NATS target")
            .trim()
            .rsplit('@')
            .next()
            .expect("NATS authority")
            .strip_prefix("nats://")
            .unwrap_or(target_url)
            .to_socket_addrs()
            .expect("NATS target resolves")
            .next()
            .expect("NATS target address exists");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("DELETE reply proxy listener binds");
        let addr = listener.local_addr().expect("proxy listener address");
        let cancel = CancellationToken::new();
        let bridges = Arc::new(Mutex::new(Vec::new()));
        let allow_upstream = Arc::new(AtomicBool::new(true));
        let accept_task = {
            let cancel = cancel.clone();
            let bridges = bridges.clone();
            let allow_upstream = allow_upstream.clone();
            tokio::spawn(async move {
                loop {
                    let accepted = tokio::select! {
                        () = cancel.cancelled() => break,
                        accepted = listener.accept() => accepted,
                    };
                    let Ok((downstream, _)) = accepted else {
                        continue;
                    };
                    if !allow_upstream.load(Ordering::Acquire) {
                        continue;
                    }
                    let upstream = tokio::select! {
                        () = cancel.cancelled() => break,
                        upstream = TcpStream::connect(target_addr) => upstream,
                    };
                    let Ok(upstream) = upstream else {
                        continue;
                    };
                    let bridge_cancel = cancel.clone();
                    let bridge_gate = gate.clone();
                    let bridge = tokio::spawn(async move {
                        let (mut downstream_read, mut downstream_write) =
                            tokio::io::split(downstream);
                        let (mut upstream_read, mut upstream_write) = tokio::io::split(upstream);
                        let mut gate_state = bridge_gate.subscribe();
                        tokio::select! {
                            () = bridge_cancel.cancelled() => {}
                            _ = tokio::io::copy(&mut downstream_read, &mut upstream_write) => {}
                            _ = copy_server_replies(
                                &mut upstream_read,
                                &mut downstream_write,
                                &bridge_gate,
                                &mut gate_state,
                            ) => {}
                        }
                    });
                    bridges
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(bridge);
                }
            })
        };
        Self {
            addr,
            cancel,
            accept_task: Some(accept_task),
            bridges,
            allow_upstream,
        }
    }

    fn url(&self) -> String {
        format!("nats://{}", self.addr)
    }

    fn reject_connections(&self) {
        self.allow_upstream.store(false, Ordering::Release);
    }

    fn allow_connections(&self) {
        self.allow_upstream.store(true, Ordering::Release);
    }

    async fn disconnect_clients(&self) {
        let bridges = std::mem::take(
            &mut *self
                .bridges
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for bridge in &bridges {
            bridge.abort();
        }
        for bridge in bridges {
            let _ = bridge.await;
        }
    }

    async fn shutdown(&mut self) {
        self.cancel.cancel();
        if let Some(task) = self.accept_task.take() {
            let _ = task.await;
        }
        let bridges = std::mem::take(
            &mut *self
                .bridges
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for bridge in &bridges {
            bridge.abort();
        }
        for bridge in bridges {
            let _ = bridge.await;
        }
    }
}

impl Drop for DeleteReplyProxy {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = self.accept_task.take() {
            task.abort();
        }
        for bridge in self
            .bridges
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
        {
            bridge.abort();
        }
    }
}

async fn copy_server_replies<R, W>(
    reader: &mut R,
    writer: &mut W,
    gate: &DeleteReplyGate,
    state: &mut watch::Receiver<ReplyGateState>,
) -> std::io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = [0; 8192];
    let mut copied = 0_u64;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            writer.shutdown().await?;
            return Ok(copied);
        }
        gate.before_server_write(state).await;
        writer.write_all(&buffer[..read]).await?;
        copied = copied.saturating_add(read as u64);
    }
}

#[async_trait::async_trait]
impl PublicationOutboxPort for ClaimReturnBarrier {
    async fn claim(
        &self,
        publisher: &PublisherId,
        limit: NonZeroU32,
        lease: Duration,
    ) -> Result<Vec<ClaimedPublication>, StorageError> {
        let rows = PublicationOutboxPort::claim(&self.pg, publisher, limit, lease).await?;
        if !rows.is_empty() && self.armed.swap(false, Ordering::AcqRel) {
            self.claimed.notify_one();
            self.release.notified().await;
        }
        Ok(rows)
    }

    async fn mark_published(
        &self,
        id: uuid::Uuid,
        claim: proxima_core::storage_ports::ClaimToken,
        receipt: &BrokerReceipt,
    ) -> Result<AckOutcome, StorageError> {
        PublicationOutboxPort::mark_published(&self.pg, id, claim, receipt).await
    }

    async fn release(
        &self,
        id: uuid::Uuid,
        claim: proxima_core::storage_ports::ClaimToken,
    ) -> Result<ReleaseOutcome, StorageError> {
        PublicationOutboxPort::release(&self.pg, id, claim).await
    }

    async fn pending_count(&self) -> Result<u64, StorageError> {
        PublicationOutboxPort::pending_count(&self.pg).await
    }
}

async fn complete_cycle(
    cleaner: &mut JetStreamCopyCleaner,
    item_bound: u32,
) -> proxima_outbox_nats::CopyCleanerSliceReport {
    let mut total = proxima_outbox_nats::CopyCleanerSliceReport::default();
    for _ in 0..128 {
        let slice = tokio::time::timeout(Duration::from_secs(10), cleaner.scan_slice())
            .await
            .expect("one finite production scan slice finishes")
            .expect("the real stream scan succeeds");
        assert!(
            slice.examined <= item_bound,
            "configured item bound is obeyed"
        );
        total.examined = total.examined.saturating_add(slice.examined);
        total.deleted = total.deleted.saturating_add(slice.deleted);
        total.unknown = total.unknown.saturating_add(slice.unknown);
        if slice.cycle_complete {
            total.cycle_complete = true;
            total.cycle_clean = slice.cycle_clean;
            return total;
        }
    }
    panic!("bounded slices did not complete the finite snapshot");
}

async fn cleaner_acl_oracles(url: &str) {
    let admin = test_admin_client(url).await;
    let (client, _events) = cleaner_client_with_events(url).await;
    let inbox = client.new_inbox();
    assert!(inbox.starts_with("PROXIMA_PURGE_INBOX."));
    let mut subscription = client
        .subscribe(inbox.clone())
        .await
        .expect("cleaner subscribes in its own reply namespace");
    client
        .flush()
        .await
        .expect("own inbox subscription flushes");
    let marker = "cleaner-own-inbox-positive-control";
    // `Client::flush` empties this connection's write buffer; it is not a
    // server round trip, and the marker travels on the admin connection. So
    // the SUB above and the PUB below race, and core NATS drops a publish
    // that lands before the subscription is registered. Republish until the
    // interest is live: the control asserts the cleaner MAY receive here,
    // and a dropped fire-and-forget publish is not evidence that it may not.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let message = loop {
        admin
            .publish(inbox.clone(), marker.as_bytes().to_vec().into())
            .await
            .expect("admin sends the cleaner's own-inbox marker");
        admin
            .flush()
            .await
            .expect("positive marker publish flushes");
        match tokio::time::timeout(Duration::from_millis(100), subscription.next()).await {
            Ok(received) => break received.expect("own inbox remains open"),
            Err(_) => assert!(
                std::time::Instant::now() < deadline,
                "own inbox marker never arrives on {inbox}"
            ),
        }
    };
    assert_eq!(message.payload.as_ref(), marker.as_bytes());
    drop(subscription);
    drop(client);

    let (client, mut events) = cleaner_client_with_events(url).await;
    let wrong_stream = jetstream::new(client.clone())
        .get_stream("PROXIMA_CLEANER_DENIED_STREAM")
        .await;
    assert!(
        wrong_stream.is_err(),
        "cleaner cannot inspect another stream"
    );
    client
        .flush()
        .await
        .expect("wrong-stream request is processed");
    expect_authorization_event(&mut events, "wrong stream INFO").await;
    drop(client);

    let (client, mut events) = cleaner_client_with_events(url).await;
    let result = jetstream::new(client.clone())
        .create_stream(jetstream::stream::Config {
            name: "PROXIMA_CLEANER_DENIED_STREAM".to_owned(),
            subjects: vec!["cleaner.denied.>".to_owned()],
            ..jetstream::stream::Config::default()
        })
        .await;
    assert!(result.is_err(), "cleaner cannot create topology");
    client
        .flush()
        .await
        .expect("denied topology request is processed");
    expect_authorization_event(&mut events, "stream creation").await;
    drop(client);

    assert_cleaner_publish_denied(
        url,
        "cleaner.denied.publish",
        "cleaner-denied-publish-marker",
    )
    .await;
    assert_cleaner_publish_denied(
        url,
        "$JS.ACK.PROXIMA_FACTS.cleaner-denied.1.1.1.1.1",
        "cleaner-denied-ack-marker",
    )
    .await;
    assert_cleaner_subscription_denied(
        url,
        common::INBOX_PUBLISHER_PREFIX.to_owned() + ".cross-role",
        "cleaner-cross-role-inbox-marker",
    )
    .await;
    assert_cleaner_subscription_denied(url, "_INBOX.>".to_owned(), "cleaner-broad-inbox-marker")
        .await;
}

async fn cleaner_client_with_events(
    url: &str,
) -> (async_nats::Client, tokio::sync::mpsc::UnboundedReceiver<()>) {
    let user =
        std::env::var(common::ENV_NATS_CLEANER_USER).expect("CI fixture provides cleaner user");
    let password = std::env::var(common::ENV_NATS_CLEANER_PASSWORD)
        .expect("CI fixture provides cleaner password");
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = async_nats::ConnectOptions::new()
        .user_and_password(user, password)
        .custom_inbox_prefix(proxima_outbox_nats::COPY_CLEANER_INBOX_PREFIX)
        .event_callback(move |event| {
            let event_tx = event_tx.clone();
            async move {
                let denied = match event {
                    async_nats::Event::ServerError(
                        async_nats::ServerError::AuthorizationViolation,
                    ) => true,
                    async_nats::Event::ServerError(async_nats::ServerError::Other(message)) => {
                        message.to_ascii_lowercase().contains("permission")
                    }
                    _ => false,
                };
                if denied {
                    let _ = event_tx.send(());
                }
            }
        })
        .connect(url)
        .await
        .expect("restricted cleaner credential connects");
    (client, event_rx)
}

async fn expect_authorization_event(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
    action: &str,
) {
    tokio::time::timeout(Duration::from_secs(3), events.recv())
        .await
        .unwrap_or_else(|_| panic!("broker emits a real AuthorizationViolation for {action}"))
        .unwrap_or_else(|| panic!("authorization event stream remains open for {action}"));
}

async fn assert_cleaner_publish_denied(url: &str, subject: &str, marker: &str) {
    let admin = test_admin_client(url).await;
    let mut admin_subscription = admin
        .subscribe(subject.to_owned())
        .await
        .expect("admin observes the denied publish subject");
    admin.flush().await.expect("admin observer is active");
    let (client, mut events) = cleaner_client_with_events(url).await;
    client
        .publish(subject.to_owned(), marker.as_bytes().to_vec().into())
        .await
        .expect("client queues the denied publish");
    client
        .flush()
        .await
        .expect("server processes denied publish");
    expect_authorization_event(&mut events, "out-of-scope publish or ACK").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), admin_subscription.next())
            .await
            .is_err(),
        "admin observes no marker from the denied publish"
    );
}

async fn assert_cleaner_subscription_denied(url: &str, subject: String, marker: &str) {
    let admin = test_admin_client(url).await;
    let (client, mut events) = cleaner_client_with_events(url).await;
    let mut subscription = client
        .subscribe(subject.clone())
        .await
        .expect("client queues the forbidden subscription");
    client
        .flush()
        .await
        .expect("server processes forbidden subscription");
    expect_authorization_event(&mut events, "out-of-scope inbox subscription").await;
    admin
        .publish(subject, marker.as_bytes().to_vec().into())
        .await
        .expect("admin sends the no-delivery marker");
    admin.flush().await.expect("marker publish flushes");
    assert!(
        tokio::time::timeout(Duration::from_millis(200), subscription.next())
            .await
            .is_err(),
        "forbidden subscription receives no marker"
    );
}

fn cleaner_config_at(url: &str) -> proxima_outbox_nats::JetStreamCopyCleanerConfig {
    proxima_outbox_nats::JetStreamCopyCleanerConfig::new(url).auth(
        proxima_outbox_nats::NatsAuth::UserPassword {
            user: std::env::var(common::ENV_NATS_CLEANER_USER)
                .expect("CI fixture provides cleaner user"),
            password: std::env::var(common::ENV_NATS_CLEANER_PASSWORD)
                .expect("CI fixture provides cleaner password"),
        },
    )
}

async fn erase_group_source(
    fixture: &Fixture,
    group_id: GroupId,
    source_id: &SourceId,
    drop_event_id: &str,
) -> OwnerEraseOutcome {
    let authorization = EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupSourceScope {
        group_id,
        source_id: source_id.clone(),
    });
    OwnerInversePort::erase_group_source_scope(
        &fixture.pg,
        &authorization,
        group_id,
        source_id,
        fixture.pg.surfaces(),
    )
    .await
    .expect(drop_event_id)
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

#[allow(clippy::too_many_lines)] // one real outage covers erase, reconnect, and the later sweep
async fn broker_outage_does_not_delay_erase(
    fixture: &Fixture,
    group_owner: OwnerRef,
    publisher: &JetStreamPublisher,
    stream: &jetstream::stream::Stream,
    cleaner_config: &proxima_outbox_nats::JetStreamCopyCleanerConfig,
) {
    let Owner::Group(group_id) = group_owner else {
        panic!("outage control owner is the surviving group");
    };
    let source_id = SourceId::new("cleaner/broker-outage");
    let captured = fixture
        .capture_with_source_scope(
            group_owner,
            common::source(),
            Some("cleaner/broker-outage"),
            "copy-cleaner-outage-erasure",
            Some("copy-cleaner-outage-erasure"),
        )
        .await
        .expect("outage-control Fact is captured");
    let sequence = stream
        .get_info()
        .await
        .expect("stream info before outage-control publish")
        .state
        .last_sequence
        + 1;
    assert_eq!(
        publisher
            .drain_once()
            .await
            .expect("outage-control copy publishes")
            .published,
        1
    );
    assert!(stream.get_raw_message(sequence).await.is_ok());

    let unavailable_config = cleaner_config_at("nats://127.0.0.1:1")
        .bounds(
            NonZeroU32::new(1).expect("positive outage scan slice"),
            Duration::from_secs(1),
            Duration::from_millis(250),
            Duration::from_millis(50),
            Duration::from_secs(1),
        )
        .expect("outage cleaner bounds remain finite");
    let cancel = CancellationToken::new();
    let (health, task) = proxima_outbox_nats::spawn_supervised_copy_cleaner(
        unavailable_config,
        Arc::new(fixture.pg.clone()),
        fixture.origin_scope(),
        cancel.clone(),
    );
    wait_until(
        || health.snapshot().last_failure == Some(proxima_outbox_nats::CopyCleanerFailure::Connect),
        "the cleaner to report its unavailable broker",
    )
    .await;
    assert!(!health.snapshot().is_ready());

    let erased = tokio::time::timeout(
        Duration::from_secs(5),
        erase_group_source(
            fixture,
            group_id,
            &source_id,
            "real source erase completes without the cleaner broker",
        ),
    )
    .await
    .expect("database erasure does not await an unavailable cleaner broker");
    assert!(matches!(erased, OwnerEraseOutcome::Completed { .. }));
    assert_eq!(
        fixture
            .pg
            .check_committed(group_owner, captured.memory_id)
            .await
            .expect("outage erasure has a committed origin verdict"),
        PublicationOriginEligibility::Ineligible
    );
    cancel.cancel();
    task.await
        .expect("disconnected cleaner task joins on cancellation");
    assert_eq!(
        health.snapshot().task,
        proxima_outbox_nats::CopyCleanerTaskState::Stopped
    );
    assert_eq!(
        health.snapshot().connection,
        proxima_outbox_nats::CopyCleanerConnectionState::NotObserved
    );

    let mut cleaner = JetStreamCopyCleaner::connect(
        cleaner_config.clone(),
        Arc::new(fixture.pg.clone()),
        fixture.origin_scope(),
    )
    .await
    .expect("real cleaner reconnects after the outage test");
    let report = complete_cycle(&mut cleaner, 1).await;
    assert!(report.cycle_complete);
    assert!(report.cycle_clean);
    assert_eq!(report.deleted, 1);
    assert!(matches!(
        stream.get_raw_message(sequence).await,
        Err(ref error) if matches!(
            error.kind(),
            async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
        )
    ));
}

struct RecordingOriginEligibility {
    pg: PgStorage,
    checked: Mutex<Vec<MemoryId>>,
}

impl RecordingOriginEligibility {
    fn new(pg: PgStorage) -> Self {
        Self {
            pg,
            checked: Mutex::new(Vec::new()),
        }
    }

    fn checked(&self) -> Vec<MemoryId> {
        self.checked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait::async_trait]
impl PublicationOriginEligibilityPort for RecordingOriginEligibility {
    async fn check_committed(
        &self,
        original_owner: OwnerRef,
        fact_id: MemoryId,
    ) -> Result<PublicationOriginEligibility, StorageError> {
        self.checked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(fact_id);
        self.pg.check_committed(original_owner, fact_id).await
    }

    async fn check_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        original_owner: OwnerRef,
        fact_id: MemoryId,
    ) -> Result<PublicationOriginEligibility, StorageError> {
        PublicationOriginEligibilityPort::check_in_transaction(
            &self.pg,
            tx,
            original_owner,
            fact_id,
        )
        .await
    }
}

#[allow(clippy::too_many_lines)] // rename, fail, restore, then observe the same cursor retry
async fn real_sql_fault_retains_cursor(
    fixture: &Fixture,
    group_owner: OwnerRef,
    first_retained: MemoryId,
    publisher: &JetStreamPublisher,
    stream: &jetstream::stream::Stream,
    cleaner_config: &proxima_outbox_nats::JetStreamCopyCleanerConfig,
) {
    let Owner::Group(group_id) = group_owner else {
        panic!("SQL-fault control owner is a group");
    };
    let source_id = SourceId::new("cleaner/sql-failure");
    let captured = fixture
        .capture_with_source_scope(
            group_owner,
            common::source(),
            Some("cleaner/sql-failure"),
            "copy-cleaner-sql-failure",
            Some("copy-cleaner-sql-failure"),
        )
        .await
        .expect("SQL-fault Fact is captured");
    let sequence = stream
        .get_info()
        .await
        .expect("stream info before SQL-fault publish")
        .state
        .last_sequence
        + 1;
    assert_eq!(
        publisher
            .drain_once()
            .await
            .expect("SQL-fault copy publishes")
            .published,
        1
    );
    assert!(stream.get_raw_message(sequence).await.is_ok());
    assert!(matches!(
        erase_group_source(
            fixture,
            group_id,
            &source_id,
            "SQL-fault source erase commits",
        )
        .await,
        OwnerEraseOutcome::Completed { .. }
    ));
    assert_eq!(
        fixture
            .pg
            .check_committed(group_owner, captured.memory_id)
            .await
            .expect("SQL-fault Fact has a committed ineligible verdict"),
        PublicationOriginEligibility::Ineligible
    );

    let recording = Arc::new(RecordingOriginEligibility::new(fixture.pg.clone()));
    let mut cleaner = JetStreamCopyCleaner::connect(
        cleaner_config
            .clone()
            .bounds(
                NonZeroU32::new(1).expect("one-message retry slice"),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_millis(20),
                Duration::from_secs(1),
            )
            .expect("SQL-fault bounds stay finite"),
        recording.clone(),
        fixture.origin_scope(),
    )
    .await
    .expect("cleaner connects before the actual SQL fault");

    sqlx::query(
        "ALTER TABLE proxima_core.publication_origin
         RENAME TO publication_origin_hold_for_cleaner",
    )
    .execute(fixture.pg.pool_for_tests())
    .await
    .expect("test temporarily removes the queried origin table name");
    let failure = tokio::time::timeout(Duration::from_secs(5), cleaner.scan_slice())
        .await
        .expect("real origin SQL failure returns within the storage bound")
        .expect_err("missing origin relation is a DB failure, not ineligibility");
    sqlx::query(
        "ALTER TABLE proxima_core.publication_origin_hold_for_cleaner
         RENAME TO publication_origin",
    )
    .execute(fixture.pg.pool_for_tests())
    .await
    .expect("origin table is restored immediately after the injected failure");
    assert_eq!(
        failure,
        proxima_outbox_nats::CopyCleanerFailure::OriginCheck
    );
    let checked_before_restore = recording.checked();
    assert_eq!(checked_before_restore.len(), 1);
    assert_eq!(checked_before_restore[0], first_retained);
    assert!(stream.get_raw_message(sequence).await.is_ok());

    let retried = cleaner
        .scan_slice()
        .await
        .expect("same scan cursor retries successfully after SQL restoration");
    assert_eq!(retried.examined, 1);
    let checked_after_retry = recording.checked();
    assert_eq!(checked_after_retry[1], checked_before_restore[0]);
    let failed_cycle = complete_cycle(&mut cleaner, 1).await;
    assert!(failed_cycle.cycle_complete);
    assert!(!failed_cycle.cycle_clean);
    assert_eq!(failed_cycle.deleted, 1);
    assert!(matches!(
        stream.get_raw_message(sequence).await,
        Err(ref error) if matches!(
            error.kind(),
            async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
        )
    ));
    let recovered_cycle = complete_cycle(&mut cleaner, 1).await;
    assert!(recovered_cycle.cycle_complete);
    assert!(recovered_cycle.cycle_clean);
}

#[allow(clippy::too_many_lines)] // preserve the real claim, pending cursor, old-end gap, and next-cycle proof
async fn late_publication_beyond_pending_snapshot_is_rechecked(
    fixture: &Fixture,
    group_owner: OwnerRef,
    publisher: &JetStreamPublisher,
    stream: &jetstream::stream::Stream,
    cleaner: &mut JetStreamCopyCleaner,
) {
    fixture
        .capture_with_source_scope(
            group_owner,
            common::source(),
            Some("cleaner/pending-snapshot-tail"),
            "copy-cleaner-pending-snapshot-tail",
            Some("copy-cleaner-pending-snapshot-tail"),
        )
        .await
        .expect("old-snapshot tail Fact is captured");
    let tail_sequence = stream
        .get_info()
        .await
        .expect("stream info before old-snapshot tail publication")
        .state
        .last_sequence
        + 1;
    assert_eq!(
        publisher
            .drain_once()
            .await
            .expect("old-snapshot tail publishes")
            .published,
        1
    );
    assert!(stream.get_raw_message(tail_sequence).await.is_ok());
    let old_end = stream
        .get_info()
        .await
        .expect("snapshot end includes its retained tail")
        .state
        .last_sequence;
    assert_eq!(tail_sequence, old_end);
    assert_eq!(
        stream
            .get_info()
            .await
            .expect("old snapshot has exactly the earlier survivor and tail")
            .state
            .messages,
        2
    );

    let late = fixture
        .capture_with_source_scope(
            group_owner,
            common::source(),
            Some("cleaner/late-pending-snapshot"),
            "copy-cleaner-late-pending-snapshot",
            Some("copy-cleaner-late-pending-snapshot"),
        )
        .await
        .expect("late Fact is captured before its source is erased");
    let late_bytes = fixture.stored_envelope(late.memory_id.into_inner()).await;
    let claim_gate = Arc::new(ClaimReturnBarrier::new(fixture.pg.clone()));
    let late_publisher = JetStreamPublisher::connect(fixture.config.clone(), claim_gate.clone())
        .await
        .expect("ordinary publisher accepts a real claim-return barrier");
    let drain_task = tokio::spawn(async move { late_publisher.drain_once().await });
    tokio::time::timeout(Duration::from_secs(5), claim_gate.claimed.notified())
        .await
        .expect("real late claim commits before its return is held");

    let first_old_slice = cleaner
        .scan_slice()
        .await
        .expect("one-item slice advances inside the captured old snapshot");
    assert_eq!(first_old_slice.examined, 1);
    assert_eq!(first_old_slice.deleted, 0);
    assert!(!first_old_slice.cycle_complete);
    assert!(
        tail_sequence > 1,
        "the old end follows retained earlier data"
    );

    let Owner::Group(group_id) = group_owner else {
        panic!("late old-snapshot Fact retains its original Group owner");
    };
    let late_source = SourceId::new("cleaner/late-pending-snapshot");
    assert!(matches!(
        erase_group_source(
            fixture,
            group_id,
            &late_source,
            "late pending-snapshot source erase commits",
        )
        .await,
        OwnerEraseOutcome::Completed { .. }
    ));
    assert_eq!(
        fixture
            .pg
            .check_committed(group_owner, late.memory_id)
            .await
            .expect("late pending-snapshot origin is committed ineligible"),
        PublicationOriginEligibility::Ineligible
    );
    assert!(
        stream
            .delete_message(tail_sequence)
            .await
            .expect("admin deletes the old snapshot's tail")
    );
    assert!(matches!(
        stream.get_raw_message(tail_sequence).await,
        Err(ref error) if matches!(
            error.kind(),
            async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
        )
    ));

    claim_gate.release.notify_one();
    let drain = drain_task
        .await
        .expect("the actual late publisher drain joins")
        .expect("the stale claim still receives broker PubAck");
    assert_eq!(drain.claimed, 1);
    assert_eq!(drain.published, 0);
    assert_eq!(drain.failed, 0);
    assert_eq!(drain.stale, 1);
    let late_sequence = old_end + 1;
    assert!(late_sequence > old_end);
    assert_eq!(
        stream
            .get_raw_message(late_sequence)
            .await
            .expect("late broker message lies above the old snapshot")
            .payload
            .as_ref(),
        late_bytes
    );

    let old_cycle = cleaner
        .scan_slice()
        .await
        .expect("old scan observes the later response beyond its captured end");
    assert_eq!(old_cycle.examined, 0);
    assert_eq!(old_cycle.deleted, 0);
    assert!(old_cycle.cycle_complete);
    assert!(old_cycle.cycle_clean);
    assert!(stream.get_raw_message(late_sequence).await.is_ok());

    let fresh_cycle = complete_cycle(cleaner, 1).await;
    assert!(fresh_cycle.cycle_complete);
    assert!(fresh_cycle.cycle_clean);
    assert_eq!(fresh_cycle.deleted, 1);
    assert!(matches!(
        stream.get_raw_message(late_sequence).await,
        Err(ref error) if matches!(
            error.kind(),
            async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
        )
    ));
}

#[allow(clippy::too_many_lines)] // one exact physical erase preserves its same-source sibling
async fn exact_fact_erasure_removes_only_its_copy(
    fixture: &Fixture,
    publisher: &JetStreamPublisher,
    stream: &jetstream::stream::Stream,
    cleaner_config: &proxima_outbox_nats::JetStreamCopyCleanerConfig,
) {
    let owner = Owner::Group(GroupId::new(uuid::Uuid::now_v7()));
    register_owner(fixture.pg.pool_for_tests(), &owner).await;
    let source_id = "cleaner/exact-fact-sibling";
    let erased = fixture
        .capture_with_source_scope(
            owner,
            common::source(),
            Some(source_id),
            "copy-cleaner-exact-erased",
            Some("copy-cleaner-exact-erased"),
        )
        .await
        .expect("exact-erasure target is captured");
    let sibling = fixture
        .capture_with_source_scope(
            owner,
            common::source(),
            Some(source_id),
            "copy-cleaner-exact-sibling",
            Some("copy-cleaner-exact-sibling"),
        )
        .await
        .expect("same-owner same-source sibling is captured");
    let erased_bytes = fixture.stored_envelope(erased.memory_id.into_inner()).await;
    let sibling_bytes = fixture
        .stored_envelope(sibling.memory_id.into_inner())
        .await;
    let first_sequence = stream
        .get_info()
        .await
        .expect("stream info before exact-erasure publish")
        .state
        .last_sequence
        + 1;
    assert_eq!(
        publisher
            .drain_once()
            .await
            .expect("both same-source copies publish")
            .published,
        2
    );
    let erased_sequence = first_sequence;
    let sibling_sequence = first_sequence + 1;
    assert_eq!(
        stream
            .get_raw_message(erased_sequence)
            .await
            .expect("exact-erasure target is retained")
            .payload
            .as_ref(),
        erased_bytes
    );
    assert_eq!(
        stream
            .get_raw_message(sibling_sequence)
            .await
            .expect("same-source sibling is retained")
            .payload
            .as_ref(),
        sibling_bytes
    );

    let context = fixture
        .pg
        .host_state_erase_context()
        .expect("core-only host erase context validates");
    let mut tx = fixture
        .pg
        .pool_for_tests()
        .begin()
        .await
        .expect("begin exact Fact erase");
    proxima_storage_pg::verbs::forget::erase_memory(
        &mut tx,
        fixture.pg.sidecars(),
        &context,
        &owner,
        erased.memory_id.into_inner(),
    )
    .await
    .expect("the exact hot Fact hard-delete commits its origin revocation");
    tx.commit().await.expect("exact Fact erase commits");
    assert_eq!(
        fixture
            .pg
            .check_committed(owner, erased.memory_id)
            .await
            .expect("exact erase has a committed origin verdict"),
        PublicationOriginEligibility::Ineligible
    );
    assert_eq!(
        fixture
            .pg
            .check_committed(owner, sibling.memory_id)
            .await
            .expect("same-source sibling origin remains eligible"),
        PublicationOriginEligibility::Eligible
    );

    let mut cleaner = JetStreamCopyCleaner::connect(
        cleaner_config.clone(),
        Arc::new(fixture.pg.clone()),
        fixture.origin_scope(),
    )
    .await
    .expect("cleaner binds after exact database erase");
    let report = complete_cycle(&mut cleaner, 128).await;
    assert!(report.cycle_complete);
    assert!(report.cycle_clean);
    assert_eq!(report.deleted, 1);
    assert!(matches!(
        stream.get_raw_message(erased_sequence).await,
        Err(ref error) if matches!(
            error.kind(),
            async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
        )
    ));
    assert_eq!(
        stream
            .get_raw_message(sibling_sequence)
            .await
            .expect("sibling remains in JetStream after exact Fact erase")
            .payload
            .as_ref(),
        sibling_bytes
    );
}

async fn recreated_cleaner_rechecks_from_first_retained(
    fixture: &Fixture,
    owner: OwnerRef,
    fact: MemoryId,
    stream: &jetstream::stream::Stream,
    cleaner_config: &proxima_outbox_nats::JetStreamCopyCleanerConfig,
) {
    let first_sequence = stream
        .get_info()
        .await
        .expect("stream info before the first cleaner session")
        .state
        .first_sequence;
    let one_item = cleaner_config
        .clone()
        .bounds(
            NonZeroU32::new(1).expect("one Fact per restart-boundary slice"),
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(20),
            Duration::from_secs(1),
        )
        .expect("restart-boundary work is finite");
    let mut before_erase = JetStreamCopyCleaner::connect(
        one_item.clone(),
        Arc::new(fixture.pg.clone()),
        fixture.origin_scope(),
    )
    .await
    .expect("first cleaner process-local session connects");
    let first = before_erase
        .scan_slice()
        .await
        .expect("first session scans one retained item");
    assert_eq!(first.examined, 1);
    assert_eq!(first.deleted, 0);
    assert!(!first.cycle_complete);
    assert_eq!(
        fixture
            .pg
            .check_committed(owner, fact)
            .await
            .expect("the first position is still eligible before source erasure"),
        PublicationOriginEligibility::Eligible
    );
    drop(before_erase);

    let Owner::Group(group_id) = owner else {
        panic!("restart-boundary Fact has its original Group owner");
    };
    assert!(matches!(
        erase_group_source(
            fixture,
            group_id,
            &SourceId::new("cleaner/other-owner"),
            "restart-boundary source erase commits",
        )
        .await,
        OwnerEraseOutcome::Completed { .. }
    ));
    assert_eq!(
        fixture
            .pg
            .check_committed(owner, fact)
            .await
            .expect("source erase is committed before the new session"),
        PublicationOriginEligibility::Ineligible
    );

    let mut restarted = JetStreamCopyCleaner::connect(
        one_item,
        Arc::new(fixture.pg.clone()),
        fixture.origin_scope(),
    )
    .await
    .expect("new cleaner session starts with no persisted cursor");
    let first_after_restart = restarted
        .scan_slice()
        .await
        .expect("new session rechecks the first retained message");
    assert_eq!(first_after_restart.examined, 1);
    assert_eq!(first_after_restart.deleted, 1);
    assert!(matches!(
        stream.get_raw_message(first_sequence).await,
        Err(ref error) if matches!(
            error.kind(),
            async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
        )
    ));
    let completed = complete_cycle(&mut restarted, 1).await;
    assert!(completed.cycle_complete);
    assert!(completed.cycle_clean);
}

#[allow(clippy::too_many_lines)] // one real sparse snapshot proves wildcard gap traversal
async fn sparse_snapshot_skips_deleted_sequence_holes(
    fixture: &Fixture,
    group_owner: OwnerRef,
    publisher: &JetStreamPublisher,
    stream: &jetstream::stream::Stream,
    cleaner_config: &proxima_outbox_nats::JetStreamCopyCleanerConfig,
) {
    const HOLES: u32 = 64;
    let holes = u64::from(HOLES);
    let context = jetstream::new(test_admin_client(&fixture.url).await);
    let before_holes = stream
        .get_info()
        .await
        .expect("stream info before creating a sparse range")
        .state;
    let first_hole = before_holes.last_sequence + 1;
    for offset in 0..holes {
        publish_raw_copy(
            &context,
            &format!("{COPY_CLEANER_SUBJECT_PREFIX}.gap.{offset}"),
            cleaner_headers(
                &format!("cleaner-gap-{offset}"),
                proxima_outbox_nats::CONTENT_TYPE_CLOUDEVENTS,
                fixture.origin_scope(),
            ),
            b"test-only deleted sequence gap".to_vec(),
        )
        .await;
    }
    context
        .client()
        .flush()
        .await
        .expect("gap publications flush");
    for sequence in first_hole..(first_hole + holes) {
        assert!(
            stream
                .delete_message(sequence)
                .await
                .expect("admin removes each test-owned gap record"),
            "each filler sequence creates a retained hole"
        );
    }
    assert!(matches!(
        stream.get_raw_message(first_hole + holes / 2).await,
        Err(ref error) if matches!(
            error.kind(),
            async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
        )
    ));

    let source_id = SourceId::new("cleaner/sparse-gap");
    let target = fixture
        .capture_with_source_scope(
            group_owner,
            common::source(),
            Some("cleaner/sparse-gap"),
            "copy-cleaner-sparse-gap-target",
            Some("copy-cleaner-sparse-gap-target"),
        )
        .await
        .expect("a canonical Fact follows the real sequence holes");
    let target_sequence = first_hole + holes;
    assert_eq!(
        publisher
            .drain_once()
            .await
            .expect("canonical target after the gaps publishes")
            .published,
        1
    );
    assert!(stream.get_raw_message(target_sequence).await.is_ok());
    let Owner::Group(group_id) = group_owner else {
        panic!("sparse-gap target has its original Group owner");
    };
    assert!(matches!(
        erase_group_source(
            fixture,
            group_id,
            &source_id,
            "sparse-gap target source erase commits",
        )
        .await,
        OwnerEraseOutcome::Completed { .. }
    ));
    assert_eq!(
        fixture
            .pg
            .check_committed(group_owner, target.memory_id)
            .await
            .expect("the sparse-gap source erase has a committed verdict"),
        PublicationOriginEligibility::Ineligible
    );
    let mut cleaner = JetStreamCopyCleaner::connect(
        cleaner_config.clone(),
        Arc::new(fixture.pg.clone()),
        fixture.origin_scope(),
    )
    .await
    .expect("cleaner connects to the sparse stream");
    let report = complete_cycle(&mut cleaner, 128).await;
    assert!(report.cycle_complete);
    assert!(report.cycle_clean);
    assert_eq!(report.deleted, 1);
    assert!(
        report.examined < HOLES,
        "the cleaner examines retained messages, not every deleted sequence"
    );
    assert!(matches!(
        stream.get_raw_message(target_sequence).await,
        Err(ref error) if matches!(
            error.kind(),
            async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
        )
    ));
}

async fn cleaner_recovers_after_idle_client_disconnect(fixture: &Fixture) {
    let gate = DeleteReplyGate::new();
    let mut proxy = DeleteReplyProxy::bind(&fixture.url, gate).await;
    let config = cleaner_config_at(&proxy.url())
        .bounds(
            NonZeroU32::new(128).expect("disconnect recovery scans finite snapshots"),
            Duration::from_secs(5),
            Duration::from_millis(250),
            Duration::from_millis(25),
            Duration::from_secs(1),
        )
        .expect("disconnect recovery bounds remain finite");
    let cancel = CancellationToken::new();
    let (health, task) = proxima_outbox_nats::spawn_supervised_copy_cleaner(
        config,
        Arc::new(fixture.pg.clone()),
        fixture.origin_scope(),
        cancel.clone(),
    );
    wait_until(
        || health.snapshot().is_ready(),
        "cleaner to complete a clean cycle before disconnect",
    )
    .await;
    let initial_cycles = health.snapshot().cycles_completed;
    proxy.reject_connections();
    proxy.disconnect_clients().await;
    wait_until(
        || {
            let snapshot = health.snapshot();
            snapshot.connection != proxima_outbox_nats::CopyCleanerConnectionState::Connected
                && snapshot.scan == proxima_outbox_nats::CopyCleanerScanState::Failed
        },
        "idle cleaner connection loss to become unready and fail its next scan",
    )
    .await;
    assert!(!health.snapshot().is_ready());
    proxy.allow_connections();
    wait_until(
        || {
            let snapshot = health.snapshot();
            snapshot.is_ready() && snapshot.cycles_completed > initial_cycles
        },
        "a clean complete scan after reconnect to recover readiness",
    )
    .await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("disconnected cleaner joins on cancellation")
        .expect("cleaner task completed normally");
    proxy.shutdown().await;
}

#[allow(clippy::too_many_lines)] // one serial stream oracle covers unknowns, health, and task cleanup
async fn malformed_copies_fail_health_without_starving_later_messages(
    fixture: &Fixture,
    group_owner: OwnerRef,
    seed_bytes: Vec<u8>,
    publisher: &JetStreamPublisher,
    stream: &jetstream::stream::Stream,
    cleaner_config: &proxima_outbox_nats::JetStreamCopyCleanerConfig,
) {
    let Owner::Group(group_id) = group_owner else {
        panic!("malformed-copy owner is a group");
    };
    let marker = "SYNTHETIC_PRIVACY_MARKER_cleaner_fixture";
    let (_, owner_id) = group_owner.columns();
    let event: serde_json::Value =
        serde_json::from_slice(&seed_bytes).expect("retained group envelope is JSON");
    let event_type = event["type"]
        .as_str()
        .expect("captured event carries its type");
    let expected_subject = proxima_outbox_nats::subject_for(
        proxima_outbox_nats::COPY_CLEANER_SUBJECT_PREFIX,
        group_owner.columns().0.as_str(),
        owner_id,
        event_type,
    );
    let admin = test_admin_client(&fixture.url).await;
    let context = jetstream::new(admin.clone());
    let mut sequence = stream
        .get_info()
        .await
        .expect("stream info before malformed records")
        .state
        .last_sequence;

    let malformed_headers = cleaner_headers(
        &format!("{marker}_malformed"),
        proxima_outbox_nats::CONTENT_TYPE_CLOUDEVENTS,
        fixture.origin_scope(),
    );
    sequence += 1;
    publish_raw_copy(
        &context,
        &expected_subject,
        malformed_headers,
        format!("malformed payload contains {marker}").into_bytes(),
    )
    .await;

    let mismatched_payload = envelope_with_fresh_fact_id(&event);
    let mismatched_id = serde_json::from_slice::<serde_json::Value>(&mismatched_payload)
        .expect("fresh event parses")["id"]
        .as_str()
        .expect("fresh event id is a string")
        .to_owned();
    let identity_headers = cleaner_headers(
        &format!("{mismatched_id}-{marker}"),
        proxima_outbox_nats::CONTENT_TYPE_CLOUDEVENTS,
        fixture.origin_scope(),
    );
    sequence += 1;
    publish_raw_copy(
        &context,
        &expected_subject,
        identity_headers,
        mismatched_payload.clone(),
    )
    .await;

    let subject_payload = envelope_with_fresh_fact_id(&event);
    let subject_id = serde_json::from_slice::<serde_json::Value>(&subject_payload)
        .expect("subject-mismatch event parses")["id"]
        .as_str()
        .expect("subject-mismatch id is a string")
        .to_owned();
    sequence += 1;
    publish_raw_copy(
        &context,
        &format!("{expected_subject}.{marker}"),
        cleaner_headers(
            &subject_id,
            proxima_outbox_nats::CONTENT_TYPE_CLOUDEVENTS,
            fixture.origin_scope(),
        ),
        subject_payload,
    )
    .await;

    let content_payload = envelope_with_fresh_fact_id(&event);
    let content_id = serde_json::from_slice::<serde_json::Value>(&content_payload)
        .expect("content-mismatch event parses")["id"]
        .as_str()
        .expect("content-mismatch id is a string")
        .to_owned();
    sequence += 1;
    publish_raw_copy(
        &context,
        &expected_subject,
        cleaner_headers(
            &content_id,
            &format!("application/octet-stream; {marker}"),
            fixture.origin_scope(),
        ),
        content_payload,
    )
    .await;
    admin
        .flush()
        .await
        .expect("all malformed marker publications flush");
    let malformed_sequences = [sequence - 3, sequence - 2, sequence - 1, sequence];
    for malformed_sequence in malformed_sequences {
        assert!(
            stream.get_raw_message(malformed_sequence).await.is_ok(),
            "every unknown message remains retained"
        );
    }

    let later_source = SourceId::new("cleaner/after-unknown");
    fixture
        .capture_with_source_scope(
            group_owner,
            common::source(),
            Some("cleaner/after-unknown"),
            "copy-cleaner-after-unknown",
            Some("copy-cleaner-after-unknown"),
        )
        .await
        .expect("canonical Fact after malformed messages is captured");
    let later_sequence = sequence + 1;
    assert_eq!(
        publisher
            .drain_once()
            .await
            .expect("later canonical copy publishes")
            .published,
        1
    );
    assert!(stream.get_raw_message(later_sequence).await.is_ok());
    assert!(matches!(
        erase_group_source(
            fixture,
            group_id,
            &later_source,
            "later canonical Fact source erases before scan",
        )
        .await,
        OwnerEraseOutcome::Completed { .. }
    ));

    let mut cleaner = JetStreamCopyCleaner::connect(
        cleaner_config
            .clone()
            .bounds(
                NonZeroU32::new(128).expect("unknown scan processes later sequence"),
                Duration::from_secs(5),
                Duration::from_secs(2),
                Duration::from_millis(50),
                Duration::from_secs(1),
            )
            .expect("unknown-message bounds stay finite"),
        Arc::new(fixture.pg.clone()),
        fixture.origin_scope(),
    )
    .await
    .expect("cleaner connects to unknown-message fixture stream");
    let events = Arc::new(Mutex::new(Vec::new()));
    let report = with_dispatch(
        trace_dispatch(events.clone()),
        complete_cycle(&mut cleaner, 128),
    )
    .await;
    assert!(report.cycle_complete);
    assert!(!report.cycle_clean);
    assert_eq!(report.unknown, 4);
    assert_eq!(report.deleted, 1);
    let trace = events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .join("\n");
    assert!(
        trace.contains("unknown_message"),
        "trace has fixed failure category: {trace}"
    );
    assert!(
        !trace.contains(marker),
        "synthetic payload marker leaked; captured target/level/fields: {trace}"
    );
    for malformed_sequence in malformed_sequences {
        assert!(stream.get_raw_message(malformed_sequence).await.is_ok());
    }
    assert!(matches!(
        stream.get_raw_message(later_sequence).await,
        Err(ref error) if matches!(
            error.kind(),
            async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound
        )
    ));

    let supervised_config = cleaner_config
        .clone()
        .bounds(
            NonZeroU32::new(128).expect("supervised unknown scan bound"),
            Duration::from_secs(5),
            Duration::from_secs(2),
            Duration::from_secs(10),
            Duration::from_secs(1),
        )
        .expect("supervised unknown bounds stay finite");
    let cancel = CancellationToken::new();
    let (health, task) = proxima_outbox_nats::spawn_supervised_copy_cleaner(
        supervised_config.clone(),
        Arc::new(fixture.pg.clone()),
        fixture.origin_scope(),
        cancel.clone(),
    );
    wait_until(
        || {
            let health = health.snapshot();
            health.cycles_completed > 0
                && health.messages_unknown >= 4
                && health.scan == proxima_outbox_nats::CopyCleanerScanState::Failed
        },
        "supervised health to report retained unknown messages",
    )
    .await;
    assert!(!health.snapshot().is_ready());
    assert!(!format!("{health:?}").contains(marker));
    cancel.cancel();
    task.await
        .expect("unknown-message cleaner task joins on cancellation");
    assert_eq!(
        health.snapshot().task,
        proxima_outbox_nats::CopyCleanerTaskState::Stopped
    );
    assert_eq!(
        health.snapshot().connection,
        proxima_outbox_nats::CopyCleanerConnectionState::NotObserved
    );

    let abort_cancel = CancellationToken::new();
    let (abort_health, abort_task) = proxima_outbox_nats::spawn_supervised_copy_cleaner(
        supervised_config.clone(),
        Arc::new(fixture.pg.clone()),
        fixture.origin_scope(),
        abort_cancel,
    );
    wait_until(
        || {
            abort_health.snapshot().connection
                == proxima_outbox_nats::CopyCleanerConnectionState::Connected
        },
        "cleaner reaches an actual broker connection before abort",
    )
    .await;
    abort_task.abort();
    let abort = abort_task.await.expect_err("connected cleaner abort joins");
    assert!(abort.is_cancelled());
    assert_eq!(
        abort_health.snapshot().task,
        proxima_outbox_nats::CopyCleanerTaskState::Stopped
    );
    assert_eq!(
        abort_health.snapshot().connection,
        proxima_outbox_nats::CopyCleanerConnectionState::NotObserved
    );

    let panic_cancel = CancellationToken::new();
    let (panic_health, panic_task) = proxima_outbox_nats::spawn_supervised_copy_cleaner(
        supervised_config,
        Arc::new(PanicOriginEligibility),
        fixture.origin_scope(),
        panic_cancel,
    );
    tokio::time::timeout(Duration::from_secs(5), panic_task)
        .await
        .expect("panicking checker terminates the owned task promptly")
        .expect("supervised cleaner catches the task panic");
    assert_eq!(
        panic_health.snapshot().task,
        proxima_outbox_nats::CopyCleanerTaskState::Stopped
    );
    assert_eq!(
        panic_health.snapshot().connection,
        proxima_outbox_nats::CopyCleanerConnectionState::NotObserved
    );
    assert!(!panic_health.snapshot().is_ready());
    assert!(!format!("{panic_health:?}").contains(marker));
}

/// Headers for a synthetic copy, stamped as this installation's.
///
/// The stamp is what makes these fixtures test what they claim: an
/// unstamped message is retained at the FIRST check, so a malformed
/// subject or content type below would never be reached and the test would
/// pass for the wrong reason.
fn cleaner_headers(
    message_id: &str,
    content_type: &str,
    scope: proxima_core::storage_ports::publication::OriginScope,
) -> async_nats::HeaderMap {
    let mut headers = async_nats::HeaderMap::new();
    headers.insert(proxima_outbox_nats::HEADER_MSG_ID, message_id);
    headers.insert(proxima_outbox_nats::HEADER_CONTENT_TYPE, content_type);
    headers.insert(
        proxima_outbox_nats::HEADER_ORIGIN_SCOPE,
        scope.to_string().as_str(),
    );
    headers
}

async fn publish_raw_copy(
    context: &jetstream::Context,
    subject: &str,
    headers: async_nats::HeaderMap,
    payload: Vec<u8>,
) {
    context
        .publish_with_headers(subject.to_owned(), headers, payload.into())
        .await
        .expect("test broker accepts the synthetic stream publication")
        .await
        .expect("synthetic stream publication receives a real JetStream PubAck");
}

fn envelope_with_fresh_fact_id(event: &serde_json::Value) -> Vec<u8> {
    let mut event = event.clone();
    event["id"] = format_prefixed_uuid(uuid::Uuid::now_v7(), PrefixedUuidClass::Fact).into();
    serde_json::to_vec(&event).expect("synthetic canonical envelope serializes")
}

struct TraceBuffer(Arc<Mutex<Vec<String>>>);

impl<S: Subscriber> Layer<S> for TraceBuffer {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut fields = TraceFields::default();
        event.record(&mut fields);
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!(
                "target={} level={} {}",
                event.metadata().target(),
                event.metadata().level(),
                fields.0
            ));
    }
}

#[derive(Default)]
struct TraceFields(String);

impl Visit for TraceFields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        use std::fmt::Write as _;
        let _ = write!(self.0, "{}={value} ", field.name());
    }
}

fn trace_dispatch(events: Arc<Mutex<Vec<String>>>) -> tracing::Dispatch {
    let filter = Targets::new()
        .with_default(LevelFilter::INFO)
        .with_target("proxima_outbox_nats", LevelFilter::TRACE)
        .with_target("async_nats", LevelFilter::INFO);
    tracing::Dispatch::new(
        tracing_subscriber::registry().with(TraceBuffer(events).with_filter(filter)),
    )
}

async fn with_dispatch<F: Future>(dispatch: tracing::Dispatch, future: F) -> F::Output {
    let mut future = Box::pin(future);
    std::future::poll_fn(|cx| {
        tracing::dispatcher::with_default(&dispatch, || future.as_mut().poll(cx))
    })
    .await
}

struct PanicOriginEligibility;

#[async_trait::async_trait]
impl PublicationOriginEligibilityPort for PanicOriginEligibility {
    async fn check_committed(
        &self,
        _original_owner: OwnerRef,
        _fact_id: MemoryId,
    ) -> Result<PublicationOriginEligibility, StorageError> {
        panic!("synthetic origin checker panic");
    }

    async fn check_in_transaction(
        &self,
        _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        _original_owner: OwnerRef,
        _fact_id: MemoryId,
    ) -> Result<PublicationOriginEligibility, StorageError> {
        Ok(PublicationOriginEligibility::Eligible)
    }
}
