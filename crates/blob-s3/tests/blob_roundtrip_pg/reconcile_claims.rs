//! Reconcile claims every key an upload row implies and lists `pending/`.
//!
//! A staged upload's canonical object, or a crash-window row's, is the only
//! copy a stage replay reads. Reporting it as an orphan invites an operator
//! to delete it. Each case runs in a bucket of its own, so the counts are
//! exact.

use super::implied_keys::{fixture_in_own_bucket, versions_left};
use super::*;

fn bound_authority(store: &CitedBlobStore) -> proxima_core::authz::SystemAuthority {
    let (_, authority) =
        Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests()).into_system_authority();
    store
        .bind_system_authority(&authority)
        .expect("test store binds to its maintenance engine");
    authority
}

async fn reconcile(store: &CitedBlobStore) -> CitedBlobReconcileOutcome {
    let authority = bound_authority(store);
    store.reconcile_all(&authority).await.expect("reconcile")
}

/// Acceptance: a staged pending row and a crash-window row each claim their
/// canonical object, and an in-flight transfer key is not stale. A completed
/// row's leftover transfer key is stale, a key with purge debt is owed, and
/// a key with neither row nor debt is the one orphan.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one bucket holds every class so the counts are exact
async fn reconcile_claims_every_key_a_row_implies() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let f = fixture_in_own_bucket().await;

    // A staged pending row: the canonical object is its only copy.
    let (staged_id, _) = prepare_and_put_upload(
        &f.pool,
        &f.store,
        &f.config,
        &f.ctx,
        f.owner,
        "staged.pdf",
        "application/pdf",
        b"staged-bytes",
    )
    .await;
    f.store
        .stage_upload(
            &f.ctx,
            CitedBlobUploadCompleteTs {
                owner: f.owner,
                upload_id: staged_id.clone(),
            },
        )
        .await
        .expect("stage");

    // A crash-window row: canonical PUT landed, row still names pending.
    let (crash_id, crash_pending) = prepare_and_put_upload(
        &f.pool,
        &f.store,
        &f.config,
        &f.ctx,
        f.owner,
        "crash.pdf",
        "application/pdf",
        b"crash-bytes",
    )
    .await;
    put_object_via_sdk(&f.config, &format!("objects/{crash_id}"), b"crash-bytes").await;

    // A completed upload whose transfer-key delete failed.
    let completed = upload_one_object(
        &f.pg,
        &f.store,
        &f.config,
        &f.pool,
        &f.ctx,
        f.owner,
        b"completed-bytes",
    )
    .await
    .expect("upload");
    let leftover = completed.object_key.replacen("objects/", "pending/", 1);
    put_object_via_sdk(&f.config, &leftover, b"completed-bytes").await;

    // An expired, unswept prepare: its transfer key is no longer in flight.
    let (expired_id, expired_pending) = prepare_and_put_upload(
        &f.pool,
        &f.store,
        &f.config,
        &f.ctx,
        f.owner,
        "expired.pdf",
        "application/pdf",
        b"expired-bytes",
    )
    .await;
    sqlx::query(
        "UPDATE proxima_core.blob_uploads SET expires_at = now() - interval '1 second'
          WHERE upload_id = $1",
    )
    .bind(Uuid::parse_str(&expired_id).expect("upload id"))
    .execute(&f.pool)
    .await
    .expect("expire");

    // Keys with no row: one owed to this store, one owed to nobody.
    let owed_key = format!("objects/{}", Uuid::now_v7());
    let orphan_key = format!("objects/{}", Uuid::now_v7());
    put_object_via_sdk(&f.config, &owed_key, b"owed").await;
    put_object_via_sdk(&f.config, &orphan_key, b"orphan").await;
    sqlx::query(
        "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, object_store)
         VALUES ($1, NULL, $2)",
    )
    .bind(&owed_key)
    .bind(f.store.store_id().as_str())
    .execute(&f.pool)
    .await
    .expect("owe the key");

    let outcome = reconcile(&f.store).await;
    assert_eq!(
        outcome.orphan_sample,
        vec![orphan_key.clone()],
        "only the key with no row and no debt is an orphan: {outcome:?}"
    );
    assert_eq!(outcome.orphan_objects, 1);
    assert_eq!(outcome.owed_objects, 1, "{outcome:?}");
    let mut stale = vec![leftover.clone(), expired_pending.clone()];
    stale.sort();
    assert_eq!(outcome.stale_transfer_sample, stale, "{outcome:?}");
    assert_eq!(outcome.stale_transfer_objects, 2);
    assert!(
        !outcome.stale_transfer_sample.contains(&crash_pending),
        "an in-flight transfer key is not stale"
    );
    assert!(outcome.is_intact(), "{outcome:?}");
    f.finish().await;
}

/// Crash window: a stage's canonical PUT lands after its upload ended. The
/// abort or expiry already owed both keys and drained the canonical one as
/// absent, and no locator records the late object. An ended row claims
/// nothing, so reconcile owes the object, and the drain removes it. A
/// completed upload's object keeps its claim.
#[tokio::test]
#[allow(clippy::too_many_lines)] // both ended states share one bucket so the counts are exact
async fn an_ended_uploads_late_canonical_object_is_owed_and_drained() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let f = fixture_in_own_bucket().await;
    let authority = bound_authority(&f.store);

    // Aborted, and the abort's own purge drained both keys.
    let (aborted_id, _) = prepare_and_put_upload(
        &f.pool,
        &f.store,
        &f.config,
        &f.ctx,
        f.owner,
        "aborted.pdf",
        "application/pdf",
        b"aborted-bytes",
    )
    .await;
    let aborted = f
        .store
        .abort_upload(
            &f.ctx,
            CitedBlobUploadAbortTs {
                owner: f.owner,
                upload_id: aborted_id.clone(),
            },
        )
        .await
        .expect("abort");
    assert!(aborted.aborted);

    // Expired by the sweep, which drained both keys too.
    let (expired_id, _) = prepare_and_put_upload(
        &f.pool,
        &f.store,
        &f.config,
        &f.ctx,
        f.owner,
        "expired.pdf",
        "application/pdf",
        b"expired-bytes",
    )
    .await;
    sqlx::query(
        "UPDATE proxima_core.blob_uploads SET expires_at = now() - interval '1 second'
          WHERE upload_id = $1",
    )
    .bind(Uuid::parse_str(&expired_id).expect("upload id"))
    .execute(&f.pool)
    .await
    .expect("put the upload past its TTL");
    let swept = f
        .store
        .sweep_expired_uploads(&authority, 10)
        .await
        .expect("sweep");
    assert_eq!(swept.expired, 1);
    assert!(
        f.owed_keys().await.is_empty(),
        "both ends drained their debt"
    );

    // The late PUTs land; no row records either key.
    let late = [
        format!("objects/{aborted_id}"),
        format!("objects/{expired_id}"),
    ];
    for key in &late {
        put_object_via_sdk(&f.config, key, b"late-canonical-bytes").await;
    }
    let completed = upload_one_object(
        &f.pg,
        &f.store,
        &f.config,
        &f.pool,
        &f.ctx,
        f.owner,
        b"completed-bytes",
    )
    .await
    .expect("upload");

    let outcome = f.store.reconcile_all(&authority).await.expect("reconcile");
    assert_eq!(
        (
            outcome.newly_owed_objects,
            outcome.owed_objects,
            outcome.orphan_objects
        ),
        (2, 2, 0),
        "{outcome:?}"
    );
    let mut expected = late.to_vec();
    expected.sort();
    assert_eq!(f.owed_keys().await, expected);

    let drained = f.drain().await;
    assert_eq!(
        (drained.purged, drained.failed, drained.remaining()),
        (2, 0, 0)
    );
    for key in &late {
        assert_eq!(versions_left(&f.config, key).await, 0, "{key}");
    }
    assert_eq!(
        versions_left(&f.config, &completed.object_key).await,
        1,
        "a completed upload's object is claimed, never owed"
    );
    let again = f.store.reconcile_all(&authority).await.expect("reconcile");
    assert_eq!(
        (again.newly_owed_objects, again.orphan_objects),
        (0, 0),
        "{again:?}"
    );
    f.finish().await;
}
