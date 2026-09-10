//! Publication capture and drain, against real `PostgreSQL` (issue #305).
//!
//! In-crate rather than an external test binary, because the capture is a
//! property of the transaction body: proving "the Fact write rolled back"
//! needs the same `pub(crate)` seam the write ports use, and an external
//! test could only observe the port's error, not what the transaction did.

use std::num::NonZeroU32;
use std::time::Duration;

use proxima_core::publication::{
    PublicationDraft, PublicationLimits, PublicationPlan, PublicationSource, SealedPublication,
};
use proxima_core::storage_ports::publication::{
    AckOutcome, BrokerReceipt, ClaimToken, PublicationOutboxPort, PublicationRetentionPort,
    PublisherId, ReleaseOutcome,
};
use proxima_core::storage_ports::{OwnerWritePermit, WriteSessionFactory};
use proxima_core::test_fixtures::{ListenableProbeV1, UnlistenableProbeV1};
use proxima_core::verbs::fact_ingest::{
    AuthorizedFactWrite, FactIngestOutcome, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::{
    AccessKind, FactIngestPort, FactPayload, Owner, OwnerRef, SchemaId, SchemaVersion, SourceId,
    StorageError, UserId,
};
use proxima_pg_testkit::drop_db;
use uuid::Uuid;

use crate::PgStorage;
use crate::test_fixtures::fresh_pg;

const PUBLISHER: &str = "publication-outbox-tests";

fn source() -> PublicationSource {
    PublicationSource::new("urn:proxima:publication-outbox-tests").expect("a URN is absolute")
}

fn owner_fixture() -> Owner {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
}

async fn register_owner(pool: &sqlx::PgPool, owner: &Owner) {
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind)
         ON CONFLICT (owner_id) DO NOTHING",
    )
    .bind(owner.stored_owner_id())
    .bind(proxima_core::OwnerRefKind::of(owner).as_str())
    .execute(pool)
    .await
    .expect("owner registers");
}

fn probe(note: &str) -> ListenableProbeV1 {
    ListenableProbeV1 {
        probe_id: Uuid::now_v7(),
        note: note.to_owned(),
    }
}

fn draft_for(owner: Owner, payload: &ListenableProbeV1) -> PublicationDraft {
    PublicationDraft::new(
        ListenableProbeV1::schema_id(),
        SchemaVersion::new(ListenableProbeV1::SCHEMA_VERSION),
        source(),
        owner,
        Some("trusted/runner".to_owned()),
        serde_json::to_value(payload).expect("the probe serializes"),
    )
}

/// The capture instruction core would hand storage: the draft plus the
/// limits the ENGINE was configured with.
///
/// Storage has no limits of its own to override, which is the point — a
/// test that wants a small ceiling has to say so where a deployment says
/// so, on the plan travelling with the write.
fn plan_for(
    owner: Owner,
    payload: &ListenableProbeV1,
    limits: PublicationLimits,
) -> PublicationPlan {
    PublicationPlan::new(draft_for(owner, payload), limits)
}

/// A Fact write command for one probe. `ingest_key` makes the receipt
/// replayable, which is what test (c) needs.
fn fact_command(schema_id: &str, ingest_key: Option<&str>) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new(schema_id.to_owned()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: ingest_key.map(|_| "probe/source".to_owned()),
        ingest_key: ingest_key.map(ToOwned::to_owned),
        payload: Vec::new(),
        rendered_text: Some("probe".to_owned()),
        lexical_language: None,
        receipt: ingest_key.map(|_| FactReceiptDraft {
            source_id: SourceId::new("probe/source"),
            observed_at: now,
            occurred_at: now,
        }),
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

fn witness(owner: &Owner, command: FactWriteCommand) -> AuthorizedFactWrite {
    AuthorizedFactWrite::new_for_tests(
        OwnerWritePermit::new_for_tests(*owner, AccessKind::Fact),
        command,
        None,
        Vec::new(),
    )
}

/// One listenable admission through the typed-sidecar port — the route a
/// flavor's Fact write takes.
async fn ingest_listenable(
    pg: &PgStorage,
    owner: &Owner,
    payload: &ListenableProbeV1,
    ingest_key: Option<&str>,
) -> Result<FactIngestOutcome, StorageError> {
    ingest_listenable_under(pg, owner, payload, ingest_key, PublicationLimits::default()).await
}

/// The same route under an explicit deployment bound.
async fn ingest_listenable_under(
    pg: &PgStorage,
    owner: &Owner,
    payload: &ListenableProbeV1,
    ingest_key: Option<&str>,
    limits: PublicationLimits,
) -> Result<FactIngestOutcome, StorageError> {
    let command = fact_command(ListenableProbeV1::SCHEMA_ID, ingest_key);
    let authorized =
        witness(owner, command).with_publication_for_tests(plan_for(*owner, payload, limits));
    pg.ingest_fact_with_typed_sidecar(&authorized, &[], None)
        .await
}

async fn ingest_unlistenable(
    pg: &PgStorage,
    owner: &Owner,
    ingest_key: Option<&str>,
) -> Result<FactIngestOutcome, StorageError> {
    let command = fact_command(UnlistenableProbeV1::SCHEMA_ID, ingest_key);
    let authorized = witness(owner, command);
    pg.ingest_fact_with_typed_sidecar(&authorized, &[], None)
        .await
}

async fn outbox_rows(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM proxima_core.publication_outbox")
        .fetch_one(pool)
        .await
        .expect("count reads")
}

async fn memory_rows(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory")
        .fetch_one(pool)
        .await
        .expect("count reads")
}

// ── atomicity ───────────────────────────────────────────────────────────

#[tokio::test]
async fn a_listenable_write_captures_exactly_one_cloudevent_keyed_by_the_fact_t() {
    let (pg, db) = fresh_pg("pub_capture_one").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    let payload = probe("the quay is sound");
    let outcome = ingest_listenable(&pg, &owner, &payload, None)
        .await
        .expect("a listenable write commits");
    let t = outcome.memory_id.into_inner();

    let (row_t, event_id, envelope, digest, state, attempts): (
        Uuid,
        String,
        Vec<u8>,
        Vec<u8>,
        String,
        i32,
    ) = sqlx::query_as(
        "SELECT t, event_id, envelope, envelope_digest, state::text, attempts
           FROM proxima_core.publication_outbox",
    )
    .fetch_one(&pool)
    .await
    .expect("exactly one row");
    assert_eq!(row_t, t, "the record is keyed by the Fact's t");
    assert_eq!(event_id, format!("F:{t}"));
    assert_eq!(state, "pending");
    assert_eq!(attempts, 0);
    assert_eq!(outbox_rows(&pool).await, 1);

    // The envelope is the exact `CloudEvents` document, in the declared key
    // order, and the digest is over those bytes.
    let text = String::from_utf8(envelope.clone()).expect("the envelope is UTF-8");
    let keys: Vec<&str> = [
        "\"specversion\"",
        "\"id\"",
        "\"source\"",
        "\"type\"",
        "\"datacontenttype\"",
        "\"dataschema\"",
        "\"time\"",
        "\"proximaowner\"",
        "\"proximamodel\"",
        "\"data\"",
    ]
    .into_iter()
    .collect();
    let mut cursor = 0usize;
    for key in keys {
        let at = text[cursor..]
            .find(key)
            .unwrap_or_else(|| panic!("{key} missing from {text}"));
        cursor += at + key.len();
    }
    let parsed: serde_json::Value = serde_json::from_slice(&envelope).expect("valid JSON");
    assert_eq!(parsed["specversion"], "1.0");
    assert_eq!(parsed["id"], format!("F:{t}"));
    assert_eq!(parsed["source"], source().as_str());
    assert_eq!(parsed["type"], ListenableProbeV1::SCHEMA_ID);
    assert_eq!(parsed["datacontenttype"], "application/json");
    assert_eq!(
        parsed["dataschema"],
        format!("proxima://schema/{}/1", ListenableProbeV1::SCHEMA_ID)
    );
    assert_eq!(parsed["proximamodel"], "trusted/runner");
    assert_eq!(
        parsed["data"],
        serde_json::to_value(&payload).expect("json")
    );

    // `time` is the v7 timestamp of `t`, not the capture's wall clock.
    let (secs, nanos) = t.get_timestamp().expect("a v7 id").to_unix();
    let expected = time::OffsetDateTime::from_unix_timestamp_nanos(
        i128::from(secs) * 1_000_000_000 + i128::from(nanos),
    )
    .expect("in range")
    .replace_nanosecond((nanos / 1_000_000) * 1_000_000)
    .expect("millisecond truncation")
    .format(&time::format_description::well_known::Rfc3339)
    .expect("formats");
    assert_eq!(parsed["time"], expected);

    assert_eq!(digest, blake3::hash(&envelope).as_bytes().to_vec());

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn a_non_listenable_write_captures_nothing() {
    let (pg, db) = fresh_pg("pub_capture_none").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    ingest_unlistenable(&pg, &owner, None)
        .await
        .expect("an ordinary write commits");

    assert_eq!(memory_rows(&pool).await, 1);
    assert_eq!(
        outbox_rows(&pool).await,
        0,
        "a schema that declares nothing must be untouched by capture"
    );

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn a_replayed_receipt_captures_no_second_record() {
    let (pg, db) = fresh_pg("pub_capture_replay").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    let payload = probe("replayed");
    let first = ingest_listenable(&pg, &owner, &payload, Some("replay-1"))
        .await
        .expect("first admission");
    assert!(!first.idempotent_replay);
    let second = ingest_listenable(&pg, &owner, &payload, Some("replay-1"))
        .await
        .expect("replay");
    assert!(second.idempotent_replay, "the receipt must replay");
    assert_eq!(second.memory_id, first.memory_id);
    assert_eq!(
        outbox_rows(&pool).await,
        1,
        "a replay must not put a second copy of one Fact on the wire"
    );

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn a_failing_sidecar_rolls_back_the_fact_and_its_capture() {
    let (pg, db) = fresh_pg("pub_capture_sidecar_fail").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    let payload = probe("doomed");
    let command = fact_command(ListenableProbeV1::SCHEMA_ID, Some("doomed-1"));
    let authorized = witness(&owner, command).with_publication_for_tests(plan_for(
        owner,
        &payload,
        PublicationLimits::default(),
    ));
    let mut tx = pool.begin().await.expect("begin");
    let err = crate::verbs::fact_ingest::ingest_fact_with_sidecar_in_tx(
        &mut tx,
        &authorized,
        None,
        crate::verbs::fact_ingest::FactAdmissionInput {
            natural_key: None,
            sidecar_tables: &[],
            scopes: &[],
            content: crate::verbs::fact_ingest::ContentResolution {
                content_id: None,
                payloads: Some(&[]),
            },
            publication: authorized.publication(),
        },
        |_tx, _outcome| {
            Box::pin(async { Err(StorageError::ConstraintViolation("sidecar said no".into())) })
        },
    )
    .await
    .expect_err("a failing sidecar must fail the write");
    assert!(matches!(err, StorageError::ConstraintViolation(_)), "{err}");
    drop(tx);

    assert_eq!(memory_rows(&pool).await, 0);
    assert_eq!(outbox_rows(&pool).await, 0);
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.ingest_keys")
        .fetch_one(&pool)
        .await
        .expect("count reads");
    assert_eq!(receipts, 0);

    drop(pg);
    let _ = drop_db(&db).await;
}

/// The capture is not the last thing the write does.
///
/// The sidecar test above fails BEFORE the capture, so it proves only that
/// a capture never happened. This one lets the capture happen, observes the
/// record inside the transaction, and then fails a later leg — the
/// embedding job the write path enqueues after the capture. The record must
/// go with the Fact, or the outbox would hold an event for a Fact that does
/// not exist.
#[tokio::test]
async fn a_failure_after_the_capture_takes_the_captured_record_with_it() {
    let (pg, db) = fresh_pg("pub_capture_post_failure").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    let payload = probe("captured, then undone");
    let command = fact_command(ListenableProbeV1::SCHEMA_ID, Some("post-capture-1"));
    let authorized = witness(&owner, command).with_publication_for_tests(plan_for(
        owner,
        &payload,
        PublicationLimits::default(),
    ));

    let mut tx = pool.begin().await.expect("begin");
    let outcome = crate::verbs::fact_ingest::ingest_fact_with_sidecar_in_tx(
        &mut tx,
        &authorized,
        None,
        crate::verbs::fact_ingest::FactAdmissionInput {
            natural_key: None,
            sidecar_tables: &[],
            scopes: &[],
            content: crate::verbs::fact_ingest::ContentResolution {
                content_id: None,
                payloads: Some(&[]),
            },
            publication: authorized.publication(),
        },
        |_tx, _outcome| Box::pin(async { Ok(()) }),
    )
    .await
    .expect("the Fact write and its capture succeed");
    let t = outcome.memory_id.into_inner();

    // Inside the transaction the record is really there. Without this the
    // assertions after the rollback would also pass for a write that never
    // captured anything.
    let captured: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.publication_outbox WHERE t = $1")
            .bind(t)
            .fetch_one(tx.as_mut())
            .await
            .expect("count reads");
    assert_eq!(captured, 1, "the capture is visible inside the transaction");

    // The one leg the write path runs AFTER the capture. Pointed at an
    // owner nobody registered, its foreign key to `owners` fails — the
    // shape a late failure takes in production.
    let stranger = Uuid::now_v7();
    let err = crate::verbs::fact_embeddings::enqueue_embedding_job_in_tx(
        &mut tx,
        proxima_core::OwnerRefKind::Personal,
        Some(stranger),
        proxima_core::EntityKind::Fact,
        t,
        "probe/model",
    )
    .await
    .expect_err("an unregistered owner cannot own an embedding job");
    assert!(
        matches!(err, StorageError::Conflict(ref message) if message.contains("foreign key")),
        "{err}"
    );
    drop(tx);

    assert_eq!(memory_rows(&pool).await, 0, "the Fact rolled back");
    assert_eq!(outbox_rows(&pool).await, 0, "and took its capture with it");
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.ingest_keys")
        .fetch_one(&pool)
        .await
        .expect("count reads");
    assert_eq!(receipts, 0, "and the receipt that would replay it");

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn an_oversized_export_refuses_the_whole_fact_write() {
    let (pg, db) = fresh_pg("pub_capture_oversized").await;
    let limits = PublicationLimits {
        max_pending: 100,
        max_payload_bytes: 16,
    };
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    let payload = probe("far more than sixteen bytes of note");
    let err = ingest_listenable_under(&pg, &owner, &payload, Some("oversized-1"), limits)
        .await
        .expect_err("an oversized envelope must refuse the write");
    assert!(
        matches!(
            err,
            StorageError::PublicationRefused(
                proxima_core::publication::PublicationError::PayloadTooLarge { .. }
            )
        ),
        "{err}"
    );

    assert_eq!(memory_rows(&pool).await, 0, "nothing committed");
    assert_eq!(outbox_rows(&pool).await, 0);
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.ingest_keys")
        .fetch_one(&pool)
        .await
        .expect("count reads");
    assert_eq!(receipts, 0);
    let announced: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.announce")
        .fetch_one(&pool)
        .await
        .expect("count reads");
    assert_eq!(announced, 0);

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn an_exhausted_outbox_refuses_the_next_listenable_write() {
    let (pg, db) = fresh_pg("pub_capture_capacity").await;
    let limits = PublicationLimits {
        max_pending: 2,
        max_payload_bytes: proxima_core::publication::DEFAULT_MAX_PAYLOAD_BYTES,
    };
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    for n in 0..2 {
        ingest_listenable_under(&pg, &owner, &probe(&format!("kept-{n}")), None, limits)
            .await
            .expect("under the bound");
    }
    let err = ingest_listenable_under(&pg, &owner, &probe("refused"), None, limits)
        .await
        .expect_err("the third write is over the bound");
    assert!(
        matches!(
            err,
            StorageError::PublicationRefused(
                proxima_core::publication::PublicationError::CapacityExhausted {
                    pending: 2,
                    max: 2
                }
            )
        ),
        "{err}"
    );
    assert_eq!(outbox_rows(&pool).await, 2, "the first two survive");
    assert_eq!(memory_rows(&pool).await, 2, "and so do their Facts");
    assert_eq!(pg.pending_count().await.expect("count"), 2);

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn the_receipt_only_route_refuses_a_listenable_schema() {
    let (pg, db) = fresh_pg("pub_capture_untyped").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    let payload = probe("no typed payload here");
    let command = fact_command(ListenableProbeV1::SCHEMA_ID, None);
    let authorized = witness(&owner, command).with_publication_for_tests(plan_for(
        owner,
        &payload,
        PublicationLimits::default(),
    ));
    let err = pg
        .ingest_authorized_fact_atomic(&authorized, None)
        .await
        .expect_err("the receipt-only route cannot export a payload it never saw");
    assert!(
        matches!(
            err,
            StorageError::PublicationRefused(
                proxima_core::publication::PublicationError::UntypedListenableWrite { .. }
            )
        ),
        "{err}"
    );
    assert_eq!(memory_rows(&pool).await, 0);
    assert_eq!(outbox_rows(&pool).await, 0);

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn an_uncommitted_write_session_leaves_no_visible_record() {
    let (pg, db) = fresh_pg("pub_capture_uncommitted").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    let payload = probe("dropped");
    let command = fact_command(ListenableProbeV1::SCHEMA_ID, None);
    let authorized = witness(&owner, command).with_publication_for_tests(plan_for(
        owner,
        &payload,
        PublicationLimits::default(),
    ));
    {
        let mut session = pg.begin().await.expect("session begins");
        session
            .ingest_fact_with_typed_sidecar(&authorized, &[], None)
            .await
            .expect("the write succeeds inside the transaction");
        // Dropped without commit.
    }

    assert_eq!(memory_rows(&pool).await, 0);
    assert_eq!(outbox_rows(&pool).await, 0);

    drop(pg);
    let _ = drop_db(&db).await;
}

// ── the drain ───────────────────────────────────────────────────────────

#[tokio::test]
async fn a_late_commit_is_claimed_on_the_next_pass_not_stepped_over() {
    let (pg, db) = fresh_pg("pub_drain_late").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;
    let publisher = PublisherId::new(PUBLISHER).expect("valid");

    // Session A opens and writes but does NOT commit; its `t` is older
    // than B's.
    let slow = probe("slow");
    let slow_command = fact_command(ListenableProbeV1::SCHEMA_ID, None);
    let slow_witness = witness(&owner, slow_command).with_publication_for_tests(plan_for(
        owner,
        &slow,
        PublicationLimits::default(),
    ));
    let mut session_a = pg.begin().await.expect("session A begins");
    let a_outcome = session_a
        .ingest_fact_with_typed_sidecar(&slow_witness, &[], None)
        .await
        .expect("A writes");

    // B writes and commits.
    let b_outcome = ingest_listenable(&pg, &owner, &probe("fast"), None)
        .await
        .expect("B commits");

    let first = pg
        .claim(
            &publisher,
            NonZeroU32::new(10).expect("nonzero"),
            Duration::from_mins(1),
        )
        .await
        .expect("claim");
    assert_eq!(first.len(), 1, "only the committed record is visible");
    assert_eq!(first[0].id, b_outcome.memory_id.into_inner());
    assert_eq!(first[0].attempts, 1);

    pg.mark_published(
        first[0].id,
        first[0].claim,
        &BrokerReceipt {
            stream: "probe".into(),
            sequence: 1,
        },
    )
    .await
    .expect("ack");

    session_a.commit().await.expect("A commits late");

    let second = pg
        .claim(
            &publisher,
            NonZeroU32::new(10).expect("nonzero"),
            Duration::from_mins(1),
        )
        .await
        .expect("claim");
    assert_eq!(
        second.len(),
        1,
        "a record that committed out of t order must still be discovered"
    );
    assert_eq!(second[0].id, a_outcome.memory_id.into_inner());

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn concurrent_publishers_claim_disjoint_sets_that_cover_everything() {
    const TOTAL: usize = 200;

    let (pg, db) = fresh_pg("pub_drain_concurrent").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    for n in 0..TOTAL {
        ingest_listenable(&pg, &owner, &probe(&format!("row-{n}")), None)
            .await
            .expect("capture");
    }
    assert_eq!(pg.pending_count().await.expect("count"), TOTAL as u64);

    let pg = std::sync::Arc::new(pg);
    let mut tasks = Vec::new();
    for worker in 0..8 {
        let pg = std::sync::Arc::clone(&pg);
        tasks.push(tokio::spawn(async move {
            let publisher = PublisherId::new(format!("{PUBLISHER}-{worker}")).expect("valid");
            let mut mine = Vec::new();
            loop {
                let batch = pg
                    .claim(
                        &publisher,
                        NonZeroU32::new(7).expect("nonzero"),
                        Duration::from_mins(2),
                    )
                    .await
                    .expect("claim");
                if batch.is_empty() {
                    break;
                }
                for record in batch {
                    mine.push((record.id, record.claim, record.digest, record.envelope));
                }
            }
            mine
        }));
    }
    let mut all = Vec::new();
    for task in tasks {
        let mine = task.await.expect("worker finishes");
        for (id, claim, digest, envelope) in mine {
            assert_eq!(
                digest,
                *blake3::hash(&envelope).as_bytes(),
                "the delivered bytes are the captured bytes"
            );
            all.push((id, claim));
        }
    }
    let unique: std::collections::BTreeSet<Uuid> = all.iter().map(|(id, _)| *id).collect();
    assert_eq!(all.len(), TOTAL, "no record was claimed twice");
    assert_eq!(unique.len(), TOTAL, "and none was missed");

    for (id, claim) in all {
        assert_eq!(
            pg.mark_published(
                id,
                claim,
                &BrokerReceipt {
                    stream: "probe".into(),
                    sequence: 1,
                },
            )
            .await
            .expect("ack"),
            AckOutcome::Published
        );
    }
    assert_eq!(pg.pending_count().await.expect("count"), 0);

    let unpublished: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.publication_outbox WHERE state <> 'published'",
    )
    .fetch_one(&pool)
    .await
    .expect("count reads");
    assert_eq!(unpublished, 0);

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn a_stale_claim_can_neither_publish_nor_release() {
    let (pg, db) = fresh_pg("pub_drain_fencing").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;
    let publisher = PublisherId::new(PUBLISHER).expect("valid");

    ingest_listenable(&pg, &owner, &probe("fenced"), None)
        .await
        .expect("capture");
    let held = pg
        .claim(
            &publisher,
            NonZeroU32::new(1).expect("nonzero"),
            Duration::from_mins(10),
        )
        .await
        .expect("claim");
    let held = &held[0];

    let forged = ClaimToken::new(Uuid::now_v7());
    assert_eq!(
        pg.mark_published(
            held.id,
            forged,
            &BrokerReceipt {
                stream: "probe".into(),
                sequence: 9,
            },
        )
        .await
        .expect("a stale worker is not an error"),
        AckOutcome::StaleClaim
    );
    assert_eq!(
        pg.release(held.id, forged)
            .await
            .expect("a stale release is not an error"),
        ReleaseOutcome::StaleClaim
    );
    let (state, claimed_by): (String, Option<String>) = sqlx::query_as(
        "SELECT state::text, claimed_by FROM proxima_core.publication_outbox WHERE t = $1",
    )
    .bind(held.id)
    .fetch_one(&pool)
    .await
    .expect("row reads");
    assert_eq!(state, "claimed", "the holder keeps the record");
    assert_eq!(claimed_by.as_deref(), Some(PUBLISHER));

    // The holder's own token still works, and a second ack is idempotent.
    assert_eq!(
        pg.mark_published(
            held.id,
            held.claim,
            &BrokerReceipt {
                stream: "probe".into(),
                sequence: 9,
            },
        )
        .await
        .expect("ack"),
        AckOutcome::Published
    );
    assert_eq!(
        pg.mark_published(
            held.id,
            held.claim,
            &BrokerReceipt {
                stream: "probe".into(),
                sequence: 9,
            },
        )
        .await
        .expect("ack"),
        AckOutcome::AlreadyPublished
    );
    assert_eq!(
        pg.release(held.id, held.claim).await.expect("release"),
        ReleaseOutcome::AlreadyPublished
    );

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn an_expired_lease_is_reclaimed_and_fences_out_the_first_holder() {
    let (pg, db) = fresh_pg("pub_drain_lease").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    ingest_listenable(&pg, &owner, &probe("leased"), None)
        .await
        .expect("capture");

    // The shortest lease `claim` accepts, so the wait below is the real
    // expiry of a real lease rather than a clock the test moved.
    let first_publisher = PublisherId::new("publisher-one").expect("valid");
    let first = pg
        .claim(
            &first_publisher,
            NonZeroU32::new(1).expect("nonzero"),
            Duration::from_secs(1),
        )
        .await
        .expect("claim");
    let first = first.into_iter().next().expect("one record");
    assert_eq!(first.attempts, 1);

    tokio::time::sleep(Duration::from_millis(1_250)).await;

    let second_publisher = PublisherId::new("publisher-two").expect("valid");
    let second = pg
        .claim(
            &second_publisher,
            NonZeroU32::new(1).expect("nonzero"),
            Duration::from_mins(1),
        )
        .await
        .expect("claim");
    let second = second.into_iter().next().expect("the lease expired");
    assert_eq!(second.id, first.id);
    assert_eq!(second.attempts, 2, "attempts counts claims");
    assert_ne!(second.claim, first.claim);

    let receipt = BrokerReceipt {
        stream: "probe".into(),
        sequence: 3,
    };
    assert_eq!(
        pg.mark_published(first.id, first.claim, &receipt)
            .await
            .expect("ack"),
        AckOutcome::StaleClaim,
        "the evicted publisher must not claim another's delivery"
    );
    assert_eq!(
        pg.mark_published(second.id, second.claim, &receipt)
            .await
            .expect("ack"),
        AckOutcome::Published
    );

    let unpublished: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.publication_outbox WHERE state <> 'published'",
    )
    .fetch_one(&pool)
    .await
    .expect("count reads");
    assert_eq!(unpublished, 0);

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn a_released_record_becomes_claimable_again() {
    let (pg, db) = fresh_pg("pub_drain_release").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;
    let publisher = PublisherId::new(PUBLISHER).expect("valid");

    ingest_listenable(&pg, &owner, &probe("released"), None)
        .await
        .expect("capture");
    let held = pg
        .claim(
            &publisher,
            NonZeroU32::new(1).expect("nonzero"),
            Duration::from_mins(1),
        )
        .await
        .expect("claim");
    let held = held.into_iter().next().expect("one record");

    assert!(
        pg.claim(
            &publisher,
            NonZeroU32::new(1).expect("nonzero"),
            Duration::from_mins(1)
        )
        .await
        .expect("claim")
        .is_empty(),
        "a live lease hides the record"
    );
    assert_eq!(
        pg.release(held.id, held.claim).await.expect("release"),
        ReleaseOutcome::Released
    );
    let again = pg
        .claim(
            &publisher,
            NonZeroU32::new(1).expect("nonzero"),
            Duration::from_mins(1),
        )
        .await
        .expect("claim");
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].id, held.id);
    assert_eq!(again[0].attempts, 2);
    assert_eq!(pg.pending_count().await.expect("count"), 1);

    drop(pg);
    let _ = drop_db(&db).await;
}

// ── immutability and erasure ────────────────────────────────────────────

#[tokio::test]
async fn the_captured_envelope_cannot_be_rewritten() {
    let (pg, db) = fresh_pg("pub_immutable").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    let outcome = ingest_listenable(&pg, &owner, &probe("immutable"), None)
        .await
        .expect("capture");
    let t = outcome.memory_id.into_inner();

    for statement in [
        "UPDATE proxima_core.publication_outbox SET envelope = '\\x00'::bytea WHERE t = $1",
        "UPDATE proxima_core.publication_outbox SET event_id = 'F:forged' WHERE t = $1",
        "UPDATE proxima_core.publication_outbox SET envelope_digest = \
         decode(repeat('00', 32), 'hex') WHERE t = $1",
    ] {
        let err = sqlx::query(statement)
            .bind(t)
            .execute(&pool)
            .await
            .expect_err("the capture is immutable by constraint");
        assert!(
            err.to_string().contains("append-only"),
            "unexpected error: {err}"
        );
    }

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn erasing_one_memory_destroys_its_captured_event() {
    let (pg, db) = fresh_pg("pub_erase_memory").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    let kept = ingest_listenable(&pg, &owner, &probe("kept"), None)
        .await
        .expect("capture");
    let erased = ingest_listenable(&pg, &owner, &probe("erased"), None)
        .await
        .expect("capture");
    assert_eq!(outbox_rows(&pool).await, 2);

    let mut tx = pool.begin().await.expect("begin");
    crate::verbs::forget::erase_memory(
        &mut tx,
        pg.sidecars(),
        pg.surfaces(),
        &owner,
        erased.memory_id.into_inner(),
    )
    .await
    .expect("erase");
    tx.commit().await.expect("commit");

    let surviving: Vec<Uuid> = sqlx::query_scalar("SELECT t FROM proxima_core.publication_outbox")
        .fetch_all(&pool)
        .await
        .expect("rows read");
    assert_eq!(surviving, vec![kept.memory_id.into_inner()]);

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn forgetting_a_fact_keeps_the_event_it_already_committed() {
    let (pg, db) = fresh_pg("pub_forget_keeps").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    let outcome = ingest_listenable(&pg, &owner, &probe("cooled"), None)
        .await
        .expect("capture");
    let t = outcome.memory_id.into_inner();

    let cold = crate::verbs::forget::MemoryColdStore::default();
    crate::verbs::forget::forget_memory_oneshot(
        &pool,
        pg.sidecars(),
        pg.surfaces(),
        &cold,
        &crate::verbs::forget::cold_object_key(t),
        t,
        owner.stored_owner_id(),
    )
    .await
    .expect("forget");

    assert_eq!(
        outbox_rows(&pool).await,
        1,
        "cooling the Fact does not un-commit the event captured with it"
    );
    let publisher = PublisherId::new(PUBLISHER).expect("valid");
    let claimed = pg
        .claim(
            &publisher,
            NonZeroU32::new(4).expect("nonzero"),
            Duration::from_mins(1),
        )
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1, "and it is still deliverable");
    assert_eq!(claimed[0].id, t);

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn an_owner_erase_removes_that_owners_records_and_no_others() {
    let (pg, db) = fresh_pg("pub_erase_owner").await;
    let pool = pg.pool_for_tests().clone();
    let erased_owner = owner_fixture();
    let bystander = owner_fixture();
    register_owner(&pool, &erased_owner).await;
    register_owner(&pool, &bystander).await;

    let pending = ingest_listenable(&pg, &erased_owner, &probe("pending"), None)
        .await
        .expect("capture");
    let delivered = ingest_listenable(&pg, &erased_owner, &probe("published"), None)
        .await
        .expect("capture");
    let survivor = ingest_listenable(&pg, &bystander, &probe("bystander"), None)
        .await
        .expect("capture");

    // Put one of the erased owner's records into `published`, so the erase
    // is proven to reach a delivered record as well as a pending one.
    let publisher = PublisherId::new(PUBLISHER).expect("valid");
    let claimed = pg
        .claim(
            &publisher,
            NonZeroU32::new(10).expect("nonzero"),
            Duration::from_mins(1),
        )
        .await
        .expect("claim");
    let target = claimed
        .iter()
        .find(|record| record.id == delivered.memory_id.into_inner())
        .expect("the record was claimed");
    pg.mark_published(
        target.id,
        target.claim,
        &BrokerReceipt {
            stream: "probe".into(),
            sequence: 1,
        },
    )
    .await
    .expect("ack");
    for record in &claimed {
        if record.id != target.id {
            pg.release(record.id, record.claim)
                .await
                .expect("release the rest");
        }
    }

    let Owner::Personal(user_id) = erased_owner else {
        panic!("the fixture owner is personal");
    };
    let auth = proxima_core::owner_inverse::EraseAuthorization::new_for_tests(
        proxima_core::owner_inverse::OwnerEraseTarget::PersonalOwner {
            user_id,
            drop_event_id: "publication-outbox-erase".into(),
        },
    );
    let cold = crate::verbs::forget::MemoryColdStore::default();
    let outcome = crate::verbs::owner_erase::erase_personal_owner(
        &pool,
        &cold,
        &auth,
        user_id,
        pg.surfaces(),
    )
    .await
    .expect("owner erase");
    assert!(
        matches!(
            outcome,
            proxima_core::owner_inverse::OwnerEraseOutcome::Completed { .. }
        ),
        "expected a completed erase, got {outcome:?}"
    );

    let surviving: Vec<Uuid> = sqlx::query_scalar("SELECT t FROM proxima_core.publication_outbox")
        .fetch_all(&pool)
        .await
        .expect("rows read");
    assert_eq!(
        surviving,
        vec![survivor.memory_id.into_inner()],
        "the erase must reach pending AND published records, and only this owner's"
    );
    assert!(!surviving.contains(&pending.memory_id.into_inner()));

    drop(pg);
    let _ = drop_db(&db).await;
}

// ── retention of delivered records (review R7) ──────────────────────────

/// Mark a record published and backdate the publication by `secs`, which
/// is the only thing a test cannot get by waiting.
///
/// `published_at` is a LIFECYCLE column, so the append-only trigger permits
/// this while still refusing any edit to the captured event.
async fn publish_and_backdate(pg: &PgStorage, pool: &sqlx::PgPool, t: Uuid, secs: f64) {
    let publisher = PublisherId::new(PUBLISHER).expect("valid publisher id");
    let claimed = pg
        .claim(
            &publisher,
            NonZeroU32::new(16).expect("nonzero"),
            Duration::from_mins(1),
        )
        .await
        .expect("claim");
    let record = claimed
        .iter()
        .find(|record| record.id == t)
        .expect("the record is claimable");
    pg.mark_published(
        record.id,
        record.claim,
        &BrokerReceipt {
            stream: "probe".into(),
            sequence: 1,
        },
    )
    .await
    .expect("ack");
    // Release every other record this claim swept up, so a later claim in
    // the same test still sees them.
    for other in claimed.iter().filter(|record| record.id != t) {
        pg.release(other.id, other.claim).await.expect("release");
    }
    sqlx::query(
        "UPDATE proxima_core.publication_outbox
            SET published_at = now() - make_interval(secs => $2)
          WHERE t = $1",
    )
    .bind(t)
    .bind(secs)
    .execute(pool)
    .await
    .expect("backdate");
}

async fn states(pool: &sqlx::PgPool) -> Vec<(Uuid, String)> {
    sqlx::query_as("SELECT t, state::text FROM proxima_core.publication_outbox ORDER BY t")
        .fetch_all(pool)
        .await
        .expect("states read")
}

/// Retention reclaims DELIVERED records and nothing else. A record still
/// waiting for a broker is an unkept promise, and no horizon may cancel it
/// however old it is.
#[tokio::test]
async fn retention_prunes_only_published_records_past_the_horizon() {
    let (pg, db) = fresh_pg("pub_retention_scope").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    let old_published = ingest_listenable(&pg, &owner, &probe("delivered long ago"), None)
        .await
        .expect("write")
        .memory_id
        .into_inner();
    let fresh_published = ingest_listenable(&pg, &owner, &probe("delivered just now"), None)
        .await
        .expect("write")
        .memory_id
        .into_inner();
    let still_pending = ingest_listenable(&pg, &owner, &probe("never delivered"), None)
        .await
        .expect("write")
        .memory_id
        .into_inner();

    publish_and_backdate(&pg, &pool, old_published, 7_200.0).await;
    publish_and_backdate(&pg, &pool, fresh_published, 0.0).await;

    // And one record held under a live lease, which is the state a prune
    // would be most tempting to treat as stale.
    let publisher = PublisherId::new(PUBLISHER).expect("valid publisher id");
    let claimed = pg
        .claim(
            &publisher,
            NonZeroU32::new(16).expect("nonzero"),
            Duration::from_mins(30),
        )
        .await
        .expect("claim");
    assert_eq!(
        claimed.iter().map(|record| record.id).collect::<Vec<_>>(),
        vec![still_pending],
        "only the undelivered record is claimable"
    );

    let pruned = pg
        .prune_published(
            Duration::from_hours(1),
            NonZeroU32::new(100).expect("nonzero"),
        )
        .await
        .expect("prune");
    assert_eq!(pruned, 1, "exactly the record published before the horizon");

    let surviving = states(&pool).await;
    assert_eq!(surviving.len(), 2);
    assert!(
        surviving
            .iter()
            .any(|(t, state)| *t == fresh_published && state == "published"),
        "a recently delivered record is inside the horizon: {surviving:?}"
    );
    assert!(
        surviving
            .iter()
            .any(|(t, state)| *t == still_pending && state == "claimed"),
        "a leased, undelivered record is never retention's business: {surviving:?}"
    );

    drop(pg);
    let _ = drop_db(&db).await;
}

/// The bound is a real bound: one pass removes at most `limit` records and
/// leaves the rest for the next one.
#[tokio::test]
async fn retention_removes_at_most_the_requested_limit() {
    let (pg, db) = fresh_pg("pub_retention_limit").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    for n in 0..3 {
        let t = ingest_listenable(&pg, &owner, &probe(&format!("old-{n}")), None)
            .await
            .expect("write")
            .memory_id
            .into_inner();
        publish_and_backdate(&pg, &pool, t, 7_200.0).await;
    }
    assert_eq!(outbox_rows(&pool).await, 3);

    let first = pg
        .prune_published(
            Duration::from_hours(1),
            NonZeroU32::new(2).expect("nonzero"),
        )
        .await
        .expect("prune");
    assert_eq!(first, 2);
    assert_eq!(outbox_rows(&pool).await, 1);

    let second = pg
        .prune_published(
            Duration::from_hours(1),
            NonZeroU32::new(2).expect("nonzero"),
        )
        .await
        .expect("prune");
    assert_eq!(second, 1);
    assert_eq!(outbox_rows(&pool).await, 0);

    let third = pg
        .prune_published(
            Duration::from_hours(1),
            NonZeroU32::new(2).expect("nonzero"),
        )
        .await
        .expect("prune");
    assert_eq!(
        third, 0,
        "an empty table prunes nothing rather than erroring"
    );

    drop(pg);
    let _ = drop_db(&db).await;
}

// ── claim guards (review R15, addendum A2b) ─────────────────────────────

/// A lease under one second expires before the first publish can finish, so
/// every record claimed under it would be re-claimed while it was still in
/// flight. Refused rather than clamped: the caller asked for something that
/// cannot work.
#[tokio::test]
async fn a_lease_under_the_floor_is_refused() {
    let (pg, db) = fresh_pg("pub_claim_lease_floor").await;
    let publisher = PublisherId::new(PUBLISHER).expect("valid publisher id");

    for lease in [Duration::ZERO, Duration::from_millis(999)] {
        let err = pg
            .claim(&publisher, NonZeroU32::new(1).expect("nonzero"), lease)
            .await
            .expect_err("a sub-second lease must be refused");
        assert!(
            matches!(err, StorageError::ConstraintViolation(ref message) if message.contains("floor")),
            "{err}"
        );
    }
    pg.claim(
        &publisher,
        NonZeroU32::new(1).expect("nonzero"),
        Duration::from_secs(1),
    )
    .await
    .expect("the floor itself is acceptable");

    drop(pg);
    let _ = drop_db(&db).await;
}

/// A record the broker keeps refusing must not hold the front of the queue.
/// Ordering by attempt count before `t` demotes it behind every fresher
/// record after its first failure, so a poison record costs one slot per
/// pass rather than the whole batch window.
#[tokio::test]
async fn a_repeatedly_failing_record_is_demoted_behind_fresher_ones() {
    let (pg, db) = fresh_pg("pub_claim_order_attempts").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;
    let publisher = PublisherId::new(PUBLISHER).expect("valid publisher id");

    // The oldest record, claimed and released twice: two failed attempts.
    let poison = ingest_listenable(&pg, &owner, &probe("poison"), None)
        .await
        .expect("write")
        .memory_id
        .into_inner();
    for _ in 0..2 {
        let claimed = pg
            .claim(
                &publisher,
                NonZeroU32::new(1).expect("nonzero"),
                Duration::from_secs(1),
            )
            .await
            .expect("claim");
        assert_eq!(claimed[0].id, poison);
        pg.release(claimed[0].id, claimed[0].claim)
            .await
            .expect("release");
    }

    // A record written afterwards, so its `t` is strictly later.
    let fresh = ingest_listenable(&pg, &owner, &probe("fresh"), None)
        .await
        .expect("write")
        .memory_id
        .into_inner();

    let claimed = pg
        .claim(
            &publisher,
            NonZeroU32::new(2).expect("nonzero"),
            Duration::from_mins(1),
        )
        .await
        .expect("claim");
    assert_eq!(
        claimed.iter().map(|record| record.id).collect::<Vec<_>>(),
        vec![fresh, poison],
        "the twice-failed record must come after the untried one despite its older t"
    );
    assert_eq!(claimed[0].attempts, 1);
    assert_eq!(claimed[1].attempts, 3);

    drop(pg);
    let _ = drop_db(&db).await;
}

/// The seal is deterministic: the same draft and the same `t` produce the
/// same bytes, so a captured record can be re-derived and checked.
#[test]
fn sealing_is_a_function_of_the_draft_and_the_t() {
    let owner = owner_fixture();
    let payload = probe("deterministic");
    let draft = draft_for(owner, &payload);
    let t = Uuid::now_v7();
    let limits = PublicationLimits::default();
    let plan = PublicationPlan::new(draft, limits);
    let first = SealedPublication::seal(&plan, t).expect("seals");
    let second = SealedPublication::seal(&plan, t).expect("seals");
    assert_eq!(first, second);
    assert_eq!(first.digest, *blake3::hash(&first.bytes).as_bytes());
}
