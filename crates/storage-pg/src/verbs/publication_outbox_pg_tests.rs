//! Publication capture and drain, against real `PostgreSQL` (issue #305).
//!
//! In-crate rather than an external test binary, because the capture is a
//! property of the transaction body: proving "the Fact write rolled back"
//! needs the same `pub(crate)` seam the write ports use, and an external
//! test could only observe the port's error, not what the transaction did.

use std::num::NonZeroU32;
use std::time::Duration;

use proxima_core::owner_inverse::{EraseAuthorization, OwnerEraseOutcome, OwnerEraseTarget};
use proxima_core::publication::{
    PublicationDraft, PublicationExtensions, PublicationLimits, PublicationPlan, PublicationSource,
    SealedPublication,
};
use proxima_core::storage_ports::publication::{
    AckOutcome, BrokerReceipt, ClaimToken, PublicationOriginEligibility,
    PublicationOriginEligibilityPort, PublicationOutboxPort, PublicationRetentionPort, PublisherId,
    ReleaseOutcome,
};
use proxima_core::storage_ports::{
    MemoryAuthoringPort, OwnerInversePort, OwnerTransferPort, OwnerWritePermit, WriteSessionFactory,
};
use proxima_core::test_fixtures::{ListenableProbeV1, UnlistenableProbeV1};
use proxima_core::verbs::fact_ingest::{
    AuthorizedFactWrite, FactIngestOutcome, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::{
    AccessKind, EntityId, FactIngestPort, FactPayload, GroupId, MemoryId, Owner, OwnerRef,
    SchemaId, SchemaVersion, SidecarPayload, SourceId, StorageError, UserId,
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
        PublicationExtensions::new(),
        serde_json::to_value(payload).expect("the probe serializes"),
    )
}

/// The orchestration context a host binds to its authorization context —
/// bound out of alphabetical order, because the envelope must not depend on
/// the order the host bound them in.
fn host_extensions() -> PublicationExtensions {
    PublicationExtensions::new()
        .with("stepid", "step-7")
        .expect("a step id is a legal attribute")
        .with("runid", "run-3")
        .expect("a run id is a legal attribute")
        .with("attempt", 2)
        .expect("an integer is a legal value")
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

async fn ingest_listenable_with_source_id(
    pg: &PgStorage,
    owner: &Owner,
    payload: &ListenableProbeV1,
    source_id: Option<&str>,
) -> Result<FactIngestOutcome, StorageError> {
    let ingest_key = source_id.map(|source| format!("origin-{source}-{}", Uuid::now_v7()));
    let mut command = fact_command(ListenableProbeV1::SCHEMA_ID, ingest_key.as_deref());
    command.source_id = source_id.map(ToOwned::to_owned);
    if let (Some(source_id), Some(receipt)) = (source_id, command.receipt.as_mut()) {
        receipt.source_id = SourceId::new(source_id);
    }
    let authorized = witness(owner, command).with_publication_for_tests(plan_for(
        *owner,
        payload,
        PublicationLimits::default(),
    ));
    pg.ingest_fact_with_typed_sidecar(&authorized, &[]).await
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
    pg.ingest_fact_with_typed_sidecar(&authorized, &[]).await
}

/// The same route with host-bound extension attributes on the draft —
/// what core produces from an authorization context that carries them.
async fn ingest_listenable_with_extensions(
    pg: &PgStorage,
    owner: &Owner,
    payload: &ListenableProbeV1,
    extensions: PublicationExtensions,
) -> Result<FactIngestOutcome, StorageError> {
    let command = fact_command(ListenableProbeV1::SCHEMA_ID, None);
    let mut draft = draft_for(*owner, payload);
    draft.extensions = extensions;
    let authorized = witness(owner, command)
        .with_publication_for_tests(PublicationPlan::new(draft, PublicationLimits::default()));
    pg.ingest_fact_with_typed_sidecar(&authorized, &[]).await
}

async fn ingest_unlistenable(
    pg: &PgStorage,
    owner: &Owner,
    ingest_key: Option<&str>,
) -> Result<FactIngestOutcome, StorageError> {
    let command = fact_command(UnlistenableProbeV1::SCHEMA_ID, ingest_key);
    let authorized = witness(owner, command);
    pg.ingest_fact_with_typed_sidecar(&authorized, &[]).await
}

async fn outbox_rows(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM proxima_core.publication_outbox")
        .fetch_one(pool)
        .await
        .expect("count reads")
}

async fn assert_ineligible_under_shared_fence(
    pg: &PgStorage,
    pool: &sqlx::PgPool,
    owner: Owner,
    fact: MemoryId,
) {
    let mut tx = pool.begin().await.expect("begin late eligibility check");
    crate::access::owner_columns::lock_host_lifecycle_fence_shared_tx(&mut tx)
        .await
        .expect("hold shared lifecycle fence for admission check");
    let eligibility =
        PublicationOriginEligibilityPort::check_in_transaction(pg, &mut tx, owner, fact)
            .await
            .expect("revoked or hard-deleted origin is a negative result");
    assert_eq!(eligibility, PublicationOriginEligibility::Ineligible);
    tx.commit().await.expect("finish late eligibility check");
}

async fn memory_rows(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory")
        .fetch_one(pool)
        .await
        .expect("count reads")
}

async fn publication_origin(
    pool: &sqlx::PgPool,
    t: Uuid,
) -> Option<(Uuid, String, Option<String>)> {
    sqlx::query_as(
        "SELECT original_owner_id, original_owner_kind::text, source_id
           FROM proxima_core.publication_origin WHERE t = $1",
    )
    .bind(t)
    .fetch_optional(pool)
    .await
    .expect("publication origin reads")
}

async fn publication_origin_rows(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM proxima_core.publication_origin")
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
    assert_eq!(
        publication_origin(&pool, t).await,
        Some((owner.stored_owner_id(), "personal".into(), None)),
        "a fresh listenable Fact captures its typed original owner and known source absence"
    );

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
async fn a_group_capture_keeps_its_typed_original_owner_and_native_source() {
    let (pg, db) = fresh_pg("pub_capture_group_origin").await;
    let pool = pg.pool_for_tests().clone();
    let owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    register_owner(&pool, &owner).await;

    let outcome = ingest_listenable(&pg, &owner, &probe("group capture"), Some("group-origin-1"))
        .await
        .expect("a group Fact with a native source commits");
    assert_eq!(
        publication_origin(&pool, outcome.memory_id.into_inner()).await,
        Some((
            owner.stored_owner_id(),
            "group".into(),
            Some("probe/source".into())
        )),
        "fresh capture preserves the group kind and native source"
    );

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn origin_eligibility_fails_closed_for_wrong_owner_and_hard_delete_witness() {
    let (pg, db) = fresh_pg("pub_origin_eligibility").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;
    let published = ingest_listenable(&pg, &owner, &probe("eligibility"), Some("eligibility-1"))
        .await
        .expect("capture");
    let t = published.memory_id.into_inner();

    for wrong_owner in [
        OwnerRef::Group(GroupId::new(Uuid::now_v7())),
        OwnerRef::Group(GroupId::new(owner.stored_owner_id())),
    ] {
        let mut wrong_owner_tx = pool.begin().await.expect("begin wrong-owner check");
        crate::access::owner_columns::lock_host_lifecycle_fence_shared_tx(&mut wrong_owner_tx)
            .await
            .expect("hold the caller's shared lifecycle fence");
        let eligibility = PublicationOriginEligibilityPort::check_in_transaction(
            &pg,
            &mut wrong_owner_tx,
            wrong_owner,
            MemoryId::new(t),
        )
        .await
        .expect("wrong owner is a negative result");
        assert_eq!(eligibility, PublicationOriginEligibility::Ineligible);
        wrong_owner_tx
            .commit()
            .await
            .expect("finish wrong-owner check");
    }

    // This direct hard delete intentionally leaves the origin row behind, a
    // stale state that the normal same-transaction inverse prevents. The
    // append-only core witness must still make admission fail closed.
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
        .bind(t)
        .execute(&pool)
        .await
        .expect("hard delete writes the core witness");
    assert_eq!(
        publication_origin(&pool, t).await.unwrap().0,
        owner.stored_owner_id()
    );

    let mut witnessed_tx = pool.begin().await.expect("begin witnessed check");
    crate::access::owner_columns::lock_host_lifecycle_fence_shared_tx(&mut witnessed_tx)
        .await
        .expect("hold the caller's shared lifecycle fence");
    let eligibility = PublicationOriginEligibilityPort::check_in_transaction(
        &pg,
        &mut witnessed_tx,
        owner,
        MemoryId::new(t),
    )
    .await
    .expect("a hard-delete witness is a negative result");
    assert_eq!(eligibility, PublicationOriginEligibility::Ineligible);
    witnessed_tx.commit().await.expect("finish witnessed check");

    drop(pg);
    let _ = drop_db(&db).await;
}

/// Bytes are bytes: no migration, no column, no storage-side awareness.
/// The captured row carries whatever core sealed, extension attributes
/// included, in name order between the substrate's own attributes and
/// `data` — and the digest is over exactly those bytes.
#[tokio::test]
async fn host_bound_extension_attributes_reach_the_captured_row() {
    let (pg, db) = fresh_pg("pub_capture_ext").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    let payload = probe("the lock gates hold");
    ingest_listenable_with_extensions(&pg, &owner, &payload, host_extensions())
        .await
        .expect("a listenable write with bound extensions commits");

    let (envelope, digest): (Vec<u8>, Vec<u8>) =
        sqlx::query_as("SELECT envelope, envelope_digest FROM proxima_core.publication_outbox")
            .fetch_one(&pool)
            .await
            .expect("exactly one row");
    let text = String::from_utf8(envelope.clone()).expect("the envelope is UTF-8");
    let mut cursor = 0usize;
    for key in [
        "\"proximamodel\"",
        "\"attempt\":2",
        "\"runid\":\"run-3\"",
        "\"stepid\":\"step-7\"",
        "\"data\"",
    ] {
        let at = text[cursor..]
            .find(key)
            .unwrap_or_else(|| panic!("{key} missing or out of order in {text}"));
        cursor += at + key.len();
    }
    let parsed: serde_json::Value = serde_json::from_slice(&envelope).expect("valid JSON");
    assert_eq!(parsed["runid"], "run-3");
    assert_eq!(parsed["stepid"], "step-7");
    assert_eq!(parsed["attempt"], 2);
    assert_eq!(parsed["proximamodel"], "trusted/runner");
    assert_eq!(digest, blake3::hash(&envelope).as_bytes().to_vec());

    drop(pg);
    let _ = drop_db(&db).await;
}

/// An unbound set adds nothing at all — not an empty object, not a null.
#[tokio::test]
async fn an_unbound_extension_set_adds_no_attribute() {
    let (pg, db) = fresh_pg("pub_capture_no_ext").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    let payload = probe("no orchestration context here");
    ingest_listenable(&pg, &owner, &payload, None)
        .await
        .expect("a listenable write commits");

    let envelope: Vec<u8> =
        sqlx::query_scalar("SELECT envelope FROM proxima_core.publication_outbox")
            .fetch_one(&pool)
            .await
            .expect("exactly one row");
    let text = String::from_utf8(envelope).expect("the envelope is UTF-8");
    assert!(
        text.contains("\"proximamodel\":\"trusted/runner\",\"data\""),
        "{text}"
    );

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
    assert_eq!(publication_origin_rows(&pool).await, 0);

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
    assert_eq!(publication_origin_rows(&pool).await, 1);

    // Model a previously revoked origin while retaining the receipt/outbox
    // row. An idempotent replay is not fresh publication and must not restore
    // the missing provenance row.
    sqlx::query("DELETE FROM proxima_core.publication_origin WHERE t = $1")
        .bind(first.memory_id.into_inner())
        .execute(&pool)
        .await
        .expect("revoke test origin");
    let replay = ingest_listenable(&pg, &owner, &payload, Some("replay-1"))
        .await
        .expect("replay after origin revocation");
    assert!(replay.idempotent_replay);
    assert_eq!(publication_origin_rows(&pool).await, 0);
    assert_eq!(outbox_rows(&pool).await, 1);

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
        &[],
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
    assert_eq!(publication_origin_rows(&pool).await, 0);
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
        &[],
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
    let err = crate::verbs::fact_embeddings::enqueue_embedding_jobs_in_tx(
        &mut tx,
        stranger,
        t,
        [&proxima_core::EmbeddingSpace::new(
            "probe/model",
            proxima_core::EmbeddingDim::D1024,
        )],
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
    assert_eq!(publication_origin_rows(&pool).await, 0, "and its origin");
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
        .ingest_authorized_fact_atomic(&authorized, &[])
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
async fn the_receipt_only_route_refuses_bound_typed_sidecars() {
    let (pg, db) = fresh_pg("pub_capture_bound_sidecar").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    register_owner(&pool, &owner).await;

    let payload = probe("bound typed payload");
    let authorized = witness(&owner, fact_command(ListenableProbeV1::SCHEMA_ID, None))
        .with_sidecar_payloads_for_tests(vec![SidecarPayload::fact(payload)]);
    let err = pg
        .ingest_authorized_fact_atomic(&authorized, &[])
        .await
        .expect_err("receipt-only persistence must not drop bound typed sidecars");
    assert!(
        matches!(err, StorageError::ConstraintViolation(ref message) if message.contains("typed Fact sidecars")),
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
        let mut session = pg.begin(None).await.expect("session begins");
        session
            .ingest_fact_with_typed_sidecar(&authorized, &[])
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
    let mut session_a = pg.begin(None).await.expect("session A begins");
    let a_outcome = session_a
        .ingest_fact_with_typed_sidecar(&slow_witness, &[])
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

    for rewrite in [
        sqlx::query(
            "UPDATE proxima_core.publication_outbox SET envelope = '\\x00'::bytea WHERE t = $1",
        ),
        sqlx::query(
            "UPDATE proxima_core.publication_outbox SET event_id = 'F:forged' WHERE t = $1",
        ),
        sqlx::query(
            "UPDATE proxima_core.publication_outbox SET envelope_digest = \
             decode(repeat('00', 32), 'hex') WHERE t = $1",
        ),
    ] {
        let err = rewrite
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
    let destination = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    register_owner(&pool, &owner).await;
    register_owner(&pool, &destination).await;

    let kept = ingest_listenable(&pg, &owner, &probe("kept"), None)
        .await
        .expect("capture");
    let erased = ingest_listenable(&pg, &owner, &probe("erased"), None)
        .await
        .expect("capture");
    assert_eq!(outbox_rows(&pool).await, 2);
    let transfer = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    assert!(
        OwnerTransferPort::transfer_to_owner(
            &pg,
            &transfer,
            EntityId::Memory(erased.memory_id),
            destination,
            pg.surfaces(),
            &[],
        )
        .await
        .expect("transfer preserves publication origin")
    );
    assert_eq!(
        publication_origin(&pool, erased.memory_id.into_inner()).await,
        Some((owner.stored_owner_id(), "personal".into(), None)),
        "transfer leaves immutable original publication identity intact"
    );

    let mut tx = pool.begin().await.expect("begin");
    crate::verbs::forget::erase_memory(
        &mut tx,
        pg.sidecars(),
        &pg.host_state_erase_context()
            .expect("storage has a validated erase context"),
        &destination,
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
    assert_eq!(
        publication_origin(&pool, erased.memory_id.into_inner()).await,
        None,
        "exact physical deletion removes copied outbox and origin globally"
    );
    assert_eq!(
        publication_origin(&pool, kept.memory_id.into_inner()).await,
        Some((owner.stored_owner_id(), "personal".into(), None)),
        "an unrelated original publication survives"
    );

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // barriers and assertions establish one same-UoW race proof
async fn eligibility_and_host_write_share_the_transaction_with_source_erase() {
    let (pg, db) = fresh_pg("pub_eligibility_uow").await;
    let pool = pg.pool_for_tests().clone();
    let owner = owner_fixture();
    let OwnerRef::Personal(user_id) = owner else {
        unreachable!("the fixture is a personal owner")
    };
    let group = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    register_owner(&pool, &owner).await;
    register_owner(&pool, &group).await;
    let fact = ingest_listenable(&pg, &owner, &probe("same transaction"), Some("race-key"))
        .await
        .expect("capture");
    let t = fact.memory_id.into_inner();
    let transfer_permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    assert!(
        OwnerTransferPort::transfer_to_owner(
            &pg,
            &transfer_permit,
            EntityId::Memory(fact.memory_id),
            group,
            pg.surfaces(),
            &[],
        )
        .await
        .expect("A-to-B transfer succeeds"),
        "the source erase must target an original owner after transfer"
    );
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
            .bind(t)
            .fetch_one(&pool)
            .await
            .expect("transferred Fact remains live"),
        group.stored_owner_id()
    );

    // Model the GT UnitOfWork's entry fence. Eligibility and the following
    // host-state update both remain in this transaction until commit.
    let mut host_tx = pool.begin().await.expect("begin host UoW");
    crate::access::owner_columns::lock_host_lifecycle_fence_shared_tx(&mut host_tx)
        .await
        .expect("host UoW holds shared lifecycle fence");
    let eligibility = PublicationOriginEligibilityPort::check_in_transaction(
        &pg,
        &mut host_tx,
        owner,
        MemoryId::new(t),
    )
    .await
    .expect("the exact origin check succeeds");
    assert_eq!(eligibility, PublicationOriginEligibility::Eligible);

    let claimed_by = "same-uow-race";
    let claim_token = Uuid::now_v7();
    let updated = sqlx::query(
        "UPDATE proxima_core.publication_outbox
            SET state = 'claimed', claim_token = $2, claimed_by = $3,
                lease_expires_at = now() + interval '30 seconds', attempts = 1
          WHERE t = $1",
    )
    .bind(t)
    .bind(claim_token)
    .bind(claimed_by)
    .execute(&mut *host_tx)
    .await
    .expect("host side effect uses the checked transaction");
    assert_eq!(updated.rows_affected(), 1);

    // Source revocation must wait for this exact transaction. It cannot pass
    // between the eligibility read and the host write/commit, even though the
    // transferred physical Fact is no longer owned by the original publisher.
    let erase_pg = pg.clone();
    let source = SourceId::new("probe/source");
    let erase_source = source.clone();
    let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
        user_id,
        source_id: source,
        drop_event_id: "same-uow-source-erase".into(),
    });
    let mut eraser = tokio::spawn(async move {
        let outcome = OwnerInversePort::erase_personal_source_scope(
            &erase_pg,
            &auth,
            user_id,
            &erase_source,
            erase_pg.surfaces(),
        )
        .await
        .expect("source erase");
        assert!(matches!(outcome, OwnerEraseOutcome::Completed { .. }));
    });

    let key = crate::access::owner_columns::HOST_STATE_LIFECYCLE_FENCE_KEY;
    let wait_seen = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                     SELECT 1 FROM pg_locks
                      WHERE locktype = 'advisory' AND NOT granted
                        AND mode = 'ExclusiveLock'
                        AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
                        AND classid::bigint = (($1::bigint >> 32) & 4294967295)
                        AND objid::bigint = ($1::bigint & 4294967295)
                 )",
            )
            .bind(key)
            .fetch_one(&pool)
            .await
            .expect("inspect lifecycle fence wait");
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        wait_seen.is_ok(),
        "erase must block on the UoW shared fence"
    );

    host_tx
        .commit()
        .await
        .expect("eligibility and host write commit");
    (&mut eraser)
        .await
        .expect("erase task completes without panic");

    let erased_host_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.publication_outbox WHERE t = $1")
            .bind(t)
            .fetch_one(&pool)
            .await
            .expect("erased outbox lookup");
    assert_eq!(
        erased_host_rows, 0,
        "the source inverse removes the committed host row"
    );
    let origin_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM proxima_core.publication_origin WHERE t = $1)",
    )
    .bind(t)
    .fetch_one(&pool)
    .await
    .expect("origin lookup");
    assert!(!origin_exists, "the origin is revoked with source erase");

    assert_eq!(
        sqlx::query_scalar::<_, Uuid>("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
            .bind(t)
            .fetch_one(&pool)
            .await
            .expect("source erase keeps the transferred Fact"),
        group.stored_owner_id(),
        "revoking original source metadata does not hard-erase the live Fact"
    );

    let mut after_erase = pool.begin().await.expect("begin post-erase check");
    crate::access::owner_columns::lock_host_lifecycle_fence_shared_tx(&mut after_erase)
        .await
        .expect("post-erase UoW holds shared fence");
    let eligibility = PublicationOriginEligibilityPort::check_in_transaction(
        &pg,
        &mut after_erase,
        owner,
        MemoryId::new(t),
    )
    .await
    .expect("a missing origin is a negative result, not a read error");
    assert_eq!(eligibility, PublicationOriginEligibility::Ineligible);
    after_erase
        .commit()
        .await
        .expect("check transaction commits");

    let reused = ingest_listenable(
        &pg,
        &owner,
        &probe("source label reused"),
        Some("race-key-2"),
    )
    .await
    .expect("a fresh Fact can reuse the revoked source label");
    assert_ne!(reused.memory_id.into_inner(), t);
    assert_eq!(
        publication_origin(&pool, reused.memory_id.into_inner()).await,
        Some((
            owner.stored_owner_id(),
            "personal".into(),
            Some("probe/source".into())
        )),
        "the source name has no permanent stop marker"
    );
    let mut reuse_check = pool.begin().await.expect("begin reused-source check");
    crate::access::owner_columns::lock_host_lifecycle_fence_shared_tx(&mut reuse_check)
        .await
        .expect("reused-source UoW holds the shared lifecycle fence");
    let eligibility = PublicationOriginEligibilityPort::check_in_transaction(
        &pg,
        &mut reuse_check,
        owner,
        reused.memory_id,
    )
    .await
    .expect("fresh source reuse is eligible");
    assert_eq!(eligibility, PublicationOriginEligibility::Eligible);
    reuse_check
        .commit()
        .await
        .expect("reused-source check commits");

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // three erase orders share one transferred-Fact fixture
async fn late_eligibility_after_owner_source_and_exact_erases_is_ineligible() {
    let (pg, db) = fresh_pg("pub_erase_first_eligibility").await;
    let pool = pg.pool_for_tests().clone();

    // Whole-owner erase first, then a fresh shared-fence UoW admission check.
    let whole_owner = owner_fixture();
    let whole_fact = ingest_listenable_with_source_id(
        &pg,
        &whole_owner,
        &probe("whole owner erase first"),
        Some("whole-owner-late-check"),
    )
    .await
    .expect("capture whole-owner Fact")
    .memory_id;
    let Owner::Personal(whole_user) = whole_owner else {
        unreachable!("fixture is personal")
    };
    let whole_auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
        user_id: whole_user,
        drop_event_id: "whole-owner-first".into(),
    });
    OwnerInversePort::erase_personal_owner(&pg, &whole_auth, whole_user, pg.surfaces())
        .await
        .expect("whole-owner erase commits first");
    assert_ineligible_under_shared_fence(&pg, &pool, whole_owner, whole_fact).await;

    // Source erase first after transfer: the live physical Fact remains at B,
    // but the original A admission can no longer pass the late check.
    let source_owner = owner_fixture();
    let destination = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    register_owner(&pool, &destination).await;
    let source_id = SourceId::new("source-erase-first");
    let source_fact = ingest_listenable_with_source_id(
        &pg,
        &source_owner,
        &probe("source erase first after transfer"),
        Some(source_id.as_str()),
    )
    .await
    .expect("capture source Fact")
    .memory_id;
    assert!(
        OwnerTransferPort::transfer_to_owner(
            &pg,
            &OwnerWritePermit::new_for_tests(source_owner, AccessKind::Fact),
            EntityId::Memory(source_fact),
            destination,
            pg.surfaces(),
            &[],
        )
        .await
        .expect("transfer to current owner")
    );
    let Owner::Personal(source_user) = source_owner else {
        unreachable!("fixture is personal")
    };
    let source_auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
        user_id: source_user,
        source_id: source_id.clone(),
        drop_event_id: "source-first".into(),
    });
    OwnerInversePort::erase_personal_source_scope(
        &pg,
        &source_auth,
        source_user,
        &source_id,
        pg.surfaces(),
    )
    .await
    .expect("original source erase commits first");
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
            .bind(source_fact.into_inner())
            .fetch_one(&pool)
            .await
            .expect("source-erased transferred Fact remains live"),
        destination.stored_owner_id()
    );
    assert_ineligible_under_shared_fence(&pg, &pool, source_owner, source_fact).await;

    // Exact physical erase first removes the origin and leaves its append-only
    // witness; the late eligibility check is denied by that committed state.
    let exact_owner = owner_fixture();
    let exact_fact = ingest_listenable_with_source_id(
        &pg,
        &exact_owner,
        &probe("exact erase first"),
        Some("exact-late-check"),
    )
    .await
    .expect("capture exact Fact")
    .memory_id;
    let context = pg
        .host_state_erase_context()
        .expect("core-only lifecycle context freezes");
    let mut erase_tx = pool.begin().await.expect("begin exact erase");
    crate::verbs::forget::erase_memory(
        &mut erase_tx,
        pg.sidecars(),
        &context,
        &exact_owner,
        exact_fact.into_inner(),
    )
    .await
    .expect("exact Fact erase succeeds");
    erase_tx.commit().await.expect("exact erase commits first");
    assert_ineligible_under_shared_fence(&pg, &pool, exact_owner, exact_fact).await;

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
async fn source_erase_after_outbox_prune_revokes_transferred_origin_only() {
    let (pg, db) = fresh_pg("pub_pruned_origin_erase").await;
    let pool = pg.pool_for_tests().clone();
    let original_owner = owner_fixture();
    let OwnerRef::Personal(user_id) = original_owner else {
        unreachable!("the fixture owner is personal")
    };
    let current_owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    register_owner(&pool, &original_owner).await;
    register_owner(&pool, &current_owner).await;

    let fact = ingest_listenable(
        &pg,
        &original_owner,
        &probe("published body later pruned"),
        Some("pruned-origin-1"),
    )
    .await
    .expect("capture");
    let t = fact.memory_id.into_inner();
    let transfer = OwnerWritePermit::new_for_tests(original_owner, AccessKind::Fact);
    assert!(
        OwnerTransferPort::transfer_to_owner(
            &pg,
            &transfer,
            EntityId::Memory(fact.memory_id),
            current_owner,
            pg.surfaces(),
            &[],
        )
        .await
        .expect("transfer to current owner")
    );
    publish_and_backdate(&pg, &pool, t, 7_200.0).await;
    assert_eq!(
        pg.prune_published(
            Duration::from_hours(1),
            NonZeroU32::new(1).expect("nonzero limit"),
        )
        .await
        .expect("body retention"),
        1
    );
    assert_eq!(outbox_rows(&pool).await, 0);
    assert_eq!(
        publication_origin(&pool, t).await,
        Some((
            original_owner.stored_owner_id(),
            "personal".into(),
            Some("probe/source".into())
        )),
        "body pruning retains exact original-owner/source identity"
    );

    let source_id = SourceId::new("probe/source");
    let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
        user_id,
        source_id: source_id.clone(),
        drop_event_id: "pruned-origin-source-erase".into(),
    });
    let outcome = OwnerInversePort::erase_personal_source_scope(
        &pg,
        &auth,
        user_id,
        &source_id,
        pg.surfaces(),
    )
    .await
    .expect("original source revoke after body prune");
    assert!(matches!(outcome, OwnerEraseOutcome::Completed { .. }));
    assert_eq!(publication_origin(&pool, t).await, None);
    assert_eq!(outbox_rows(&pool).await, 0);
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
            .bind(t)
            .fetch_one(&pool)
            .await
            .expect("transferred Fact survives original-source revocation"),
        current_owner.stored_owner_id()
    );

    let mut check = pool.begin().await.expect("begin late-delivery check");
    crate::access::owner_columns::lock_host_lifecycle_fence_shared_tx(&mut check)
        .await
        .expect("hold lifecycle shared fence");
    let eligibility = PublicationOriginEligibilityPort::check_in_transaction(
        &pg,
        &mut check,
        original_owner,
        fact.memory_id,
    )
    .await
    .expect("missing revoked origin is a negative result");
    assert_eq!(eligibility, PublicationOriginEligibility::Ineligible);
    check.commit().await.expect("finish late-delivery check");

    drop(pg);
    let _ = drop_db(&db).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // the physical/original-selection matrix shares one transaction fixture
async fn source_and_destination_erase_revoke_physical_and_original_publication_sets_once() {
    let (pg, db) = fresh_pg("pub_erase_physical_origin_union").await;
    let pool = pg.pool_for_tests().clone();
    let original = owner_fixture();
    let destination = owner_fixture();
    let Owner::Personal(original_user) = original else {
        unreachable!("the fixture owner is personal")
    };
    let Owner::Personal(destination_user) = destination else {
        unreachable!("the fixture owner is personal")
    };
    register_owner(&pool, &original).await;
    register_owner(&pool, &destination).await;

    let source_transferred = ingest_listenable_with_source_id(
        &pg,
        &original,
        &probe("source erase after transfer"),
        Some("source-transfer"),
    )
    .await
    .expect("source-scoped capture");
    let overlap = ingest_listenable_with_source_id(
        &pg,
        &original,
        &probe("physical and original scope overlap"),
        Some("overlap-source"),
    )
    .await
    .expect("overlap capture");
    let destination_transferred = ingest_listenable_with_source_id(
        &pg,
        &original,
        &probe("destination owner physical erase"),
        None,
    )
    .await
    .expect("source-free capture");
    let destination_source_transferred = ingest_listenable_with_source_id(
        &pg,
        &original,
        &probe("destination source physical erase without origin"),
        Some("destination-source"),
    )
    .await
    .expect("destination source capture");
    let unrelated = ingest_listenable_with_source_id(
        &pg,
        &original,
        &probe("unrelated original publication"),
        None,
    )
    .await
    .expect("unrelated capture");

    let transfer = OwnerWritePermit::new_for_tests(original, AccessKind::Fact);
    for fact in [
        &source_transferred,
        &destination_transferred,
        &destination_source_transferred,
    ] {
        assert!(
            OwnerTransferPort::transfer_to_owner(
                &pg,
                &transfer,
                EntityId::Memory(fact.memory_id),
                destination,
                pg.surfaces(),
                &[],
            )
            .await
            .expect("A-to-B transfer succeeds")
        );
    }

    // A's source erase reaches a transferred Fact through the original
    // locator, and the still-A Fact through both selection legs. The second
    // Fact must be counted once despite matching physical t and origin.
    let source = SourceId::new("source-transfer");
    let source_auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
        user_id: original_user,
        source_id: source.clone(),
        drop_event_id: "source-transfer-erase".into(),
    });
    let source_outcome = OwnerInversePort::erase_personal_source_scope(
        &pg,
        &source_auth,
        original_user,
        &source,
        pg.surfaces(),
    )
    .await
    .expect("original source erase after transfer");
    let OwnerEraseOutcome::Completed {
        counts: source_counts,
        ..
    } = source_outcome
    else {
        panic!("source erase should complete: {source_outcome:?}");
    };
    assert_eq!(source_counts.get("publications"), 1);
    assert_eq!(source_counts.get("publication_origins"), 1);
    let transferred_t = source_transferred.memory_id.into_inner();
    assert_eq!(publication_origin(&pool, transferred_t).await, None);
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
            .bind(transferred_t)
            .fetch_one(&pool)
            .await
            .expect("source-revoked Fact remains live"),
        destination.stored_owner_id()
    );

    let overlap_source = SourceId::new("overlap-source");
    let overlap_auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
        user_id: original_user,
        source_id: overlap_source.clone(),
        drop_event_id: "overlap-source-erase".into(),
    });
    let overlap_outcome = OwnerInversePort::erase_personal_source_scope(
        &pg,
        &overlap_auth,
        original_user,
        &overlap_source,
        pg.surfaces(),
    )
    .await
    .expect("overlapping source erase");
    let OwnerEraseOutcome::Completed {
        counts: overlap_counts,
        ..
    } = overlap_outcome
    else {
        panic!("overlapping source erase should complete: {overlap_outcome:?}");
    };
    assert_eq!(overlap_counts.get("publications"), 1);
    assert_eq!(overlap_counts.get("publication_origins"), 1);
    assert_eq!(
        publication_origin(&pool, overlap.memory_id.into_inner()).await,
        None
    );

    // An unrelated source request is a negative selection: it reports zero
    // publication work and leaves the source-free original copy available.
    let absent_source = SourceId::new("no-such-source");
    let absent_auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
        user_id: original_user,
        source_id: absent_source.clone(),
        drop_event_id: "absent-source-erase".into(),
    });
    let absent_outcome = OwnerInversePort::erase_personal_source_scope(
        &pg,
        &absent_auth,
        original_user,
        &absent_source,
        pg.surfaces(),
    )
    .await
    .expect("unmatched source erase");
    let OwnerEraseOutcome::Completed {
        counts: absent_counts,
        ..
    } = absent_outcome
    else {
        panic!("unmatched source erase should complete: {absent_outcome:?}");
    };
    assert_eq!(absent_counts.get("publications"), 0);
    assert_eq!(absent_counts.get("publication_origins"), 0);
    assert_eq!(
        publication_origin(&pool, unrelated.memory_id.into_inner()).await,
        Some((original.stored_owner_id(), "personal".into(), None)),
        "an unmatched source scope preserves unrelated original attribution"
    );

    // Destination source-scope erase must use the physical Fact selector
    // even if a legacy/malformed retained row has no origin evidence. The
    // outbox copy is still selected by t and removed exactly once.
    let destination_source_t = destination_source_transferred.memory_id.into_inner();
    sqlx::query("DELETE FROM proxima_core.publication_origin WHERE t = $1")
        .bind(destination_source_t)
        .execute(&pool)
        .await
        .expect("remove only the origin evidence to model legacy data");
    assert_eq!(publication_origin(&pool, destination_source_t).await, None);
    let legacy_outbox: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.publication_outbox WHERE t = $1",
    )
    .bind(destination_source_t)
    .fetch_one(&pool)
    .await
    .expect("legacy outbox row remains selected by physical t");
    assert_eq!(legacy_outbox, 1);
    let destination_source = SourceId::new("destination-source");
    let destination_source_auth =
        EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
            user_id: destination_user,
            source_id: destination_source.clone(),
            drop_event_id: "destination-source-erase".into(),
        });
    let destination_source_outcome = OwnerInversePort::erase_personal_source_scope(
        &pg,
        &destination_source_auth,
        destination_user,
        &destination_source,
        pg.surfaces(),
    )
    .await
    .expect("destination source erase selects physical t without origin");
    let OwnerEraseOutcome::Completed {
        counts: destination_source_counts,
        ..
    } = destination_source_outcome
    else {
        panic!("destination source erase should complete: {destination_source_outcome:?}");
    };
    assert_eq!(destination_source_counts.get("publications"), 1);
    assert_eq!(destination_source_counts.get("publication_origins"), 0);
    assert_eq!(publication_origin(&pool, destination_source_t).await, None);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM proxima_core.publication_outbox WHERE t = $1",
        )
        .bind(destination_source_t)
        .fetch_one(&pool)
        .await
        .expect("selected legacy outbox row is gone"),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proxima_core.memory WHERE t = $1",)
            .bind(destination_source_t)
            .fetch_one(&pool)
            .await
            .expect("destination source Fact was physically erased"),
        0
    );

    // The destination owner selects the transferred physical t values.
    // It removes the remaining A-origin outbox/origin for one t while the
    // earlier source-revoked t has no copy to double count.
    let destination_auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
        user_id: destination_user,
        drop_event_id: "destination-whole-owner-erase".into(),
    });
    let destination_outcome = OwnerInversePort::erase_personal_owner(
        &pg,
        &destination_auth,
        destination_user,
        pg.surfaces(),
    )
    .await
    .expect("destination owner erase");
    let OwnerEraseOutcome::Completed {
        counts: destination_counts,
        ..
    } = destination_outcome
    else {
        panic!("destination owner erase should complete: {destination_outcome:?}");
    };
    assert_eq!(destination_counts.get("publications"), 1);
    assert_eq!(destination_counts.get("publication_origins"), 1);
    assert_eq!(
        publication_origin(&pool, destination_transferred.memory_id.into_inner()).await,
        None,
        "physical destination erasure removes the original A copy"
    );
    assert_eq!(
        publication_origin(&pool, unrelated.memory_id.into_inner()).await,
        Some((original.stored_owner_id(), "personal".into(), None)),
        "destination erase leaves an unrelated A original intact"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM proxima_core.publication_outbox WHERE t = $1",
        )
        .bind(unrelated.memory_id.into_inner())
        .fetch_one(&pool)
        .await
        .expect("unrelated outbox survives"),
        1
    );

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

    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    MemoryAuthoringPort::forget_memory(&pg, &permit, outcome.memory_id)
        .await
        .expect("cool through the configured storage port");

    assert_eq!(
        outbox_rows(&pool).await,
        1,
        "cooling the Fact does not un-commit the event captured with it"
    );
    assert_eq!(
        publication_origin(&pool, t).await,
        Some((owner.stored_owner_id(), "personal".into(), None)),
        "cooling preserves its payload-free publication origin"
    );
    let mut cold_check = pool.begin().await.expect("begin cooled eligibility check");
    crate::access::owner_columns::lock_host_lifecycle_fence_shared_tx(&mut cold_check)
        .await
        .expect("hold shared lifecycle fence while cooled");
    let cold_eligibility = PublicationOriginEligibilityPort::check_in_transaction(
        &pg,
        &mut cold_check,
        owner,
        outcome.memory_id,
    )
    .await
    .expect("cooling leaves origin eligibility intact");
    assert_eq!(cold_eligibility, PublicationOriginEligibility::Eligible);
    cold_check.commit().await.expect("finish cooled check");

    let hydrated = MemoryAuthoringPort::hydrate_memories(&pg, &permit, &[outcome.memory_id], &[])
        .await
        .expect("hydrate the same real cooled Fact");
    assert_eq!(hydrated.outcomes.len(), 1);
    assert_eq!(
        hydrated.outcomes[0].status,
        proxima_core::MemoryHydrationStatus::Hydrated
    );
    let mut hydrated_check = pool
        .begin()
        .await
        .expect("begin hydrated eligibility check");
    crate::access::owner_columns::lock_host_lifecycle_fence_shared_tx(&mut hydrated_check)
        .await
        .expect("hold shared lifecycle fence after hydration");
    let hydrated_eligibility = PublicationOriginEligibilityPort::check_in_transaction(
        &pg,
        &mut hydrated_check,
        owner,
        outcome.memory_id,
    )
    .await
    .expect("hydration leaves origin eligibility intact");
    assert_eq!(hydrated_eligibility, PublicationOriginEligibility::Eligible);
    hydrated_check
        .commit()
        .await
        .expect("finish hydrated check");
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
    assert_eq!(
        publication_origin(&pool, pending.memory_id.into_inner()).await,
        None
    );
    assert_eq!(
        publication_origin(&pool, delivered.memory_id.into_inner()).await,
        None
    );
    assert_eq!(
        publication_origin(&pool, survivor.memory_id.into_inner()).await,
        Some((bystander.stored_owner_id(), "personal".into(), None)),
        "whole-owner erase removes source-free origins only for that owner"
    );

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
