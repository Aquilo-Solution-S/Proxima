//! An upload nobody touches again still expires.
//!
//! The sweep expires pending, unstaged uploads past their TTL in bounded
//! batches, owes both of their keys and purges them, so an abandoned
//! prepare leaves no bytes behind.

use super::implied_keys::{Fixture, fixture, versions_left};
use super::*;

fn bound_authority(store: &CitedBlobStore) -> proxima_core::authz::SystemAuthority {
    let (_, authority) =
        Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests()).into_system_authority();
    store
        .bind_system_authority(&authority)
        .expect("test store binds to its maintenance engine");
    authority
}

async fn expire(f: &Fixture, upload_id: &str) {
    sqlx::query(
        "UPDATE proxima_core.blob_uploads SET expires_at = now() - interval '1 second'
          WHERE upload_id = $1",
    )
    .bind(Uuid::parse_str(upload_id).expect("upload id"))
    .execute(&f.pool)
    .await
    .expect("put the upload past its TTL");
}

async fn status(f: &Fixture, upload_id: &str) -> String {
    upload_status(&f.pool, upload_id).await
}

async fn abandoned_prepare(f: &Fixture, name: &str) -> (String, String) {
    prepare_and_put_upload(
        &f.pool,
        &f.store,
        &f.config,
        &f.ctx,
        f.owner,
        name,
        "application/pdf",
        b"abandoned-bytes",
    )
    .await
}

/// Acceptance: an untouched prepare past its TTL is expired by the sweep,
/// owes both keys, and leaves no version of its transfer bytes or of a
/// canonical object planted under its id.
#[tokio::test]
async fn the_sweep_expires_an_untouched_prepare_and_owes_both_keys() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    for versioned in [true, false] {
        let f = fixture(versioned).await;
        let authority = bound_authority(&f.store);
        let (upload_id, pending_key) = abandoned_prepare(&f, "abandoned.pdf").await;
        let canonical_key = format!("objects/{upload_id}");
        put_object_via_sdk(&f.config, &canonical_key, b"abandoned-bytes").await;

        let early = f
            .store
            .sweep_expired_uploads(&authority, 10)
            .await
            .expect("sweep");
        assert_eq!((early.expired, early.remaining), (0, 0), "not yet expired");
        assert_eq!(status(&f, &upload_id).await, "pending");

        expire(&f, &upload_id).await;
        let swept = f
            .store
            .sweep_expired_uploads(&authority, 10)
            .await
            .expect("sweep");
        assert_eq!((swept.expired, swept.owed, swept.remaining), (1, 2, 0));
        assert_eq!(status(&f, &upload_id).await, "expired");
        assert!(
            f.owed_keys().await.is_empty(),
            "the sweep purged what it owed"
        );
        assert_eq!(versions_left(&f.config, &pending_key).await, 0);
        assert_eq!(versions_left(&f.config, &canonical_key).await, 0);
        f.finish().await;
    }
}

/// A staged upload past its TTL is not expirable: only a finish or an abort
/// decides it.
#[tokio::test]
async fn the_sweep_leaves_a_staged_upload_alone() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let f = fixture(false).await;
    let authority = bound_authority(&f.store);
    let (upload_id, _) = abandoned_prepare(&f, "staged.pdf").await;
    let staged = f
        .store
        .stage_upload(
            &f.ctx,
            CitedBlobUploadCompleteTs {
                owner: f.owner,
                upload_id: upload_id.clone(),
            },
        )
        .await
        .expect("stage");
    expire(&f, &upload_id).await;
    let swept = f
        .store
        .sweep_expired_uploads(&authority, 10)
        .await
        .expect("sweep");
    assert_eq!((swept.expired, swept.remaining), (0, 0));
    assert_eq!(status(&f, &upload_id).await, "pending");
    assert!(
        head(
            &f.config.client(),
            &f.config.bucket,
            &staged.payload.object_key
        )
        .await
        .is_some()
    );
    assert!(f.owed_keys().await.is_empty());
    f.finish().await;
}

/// The batch limit bounds one call, and `remaining` says what is left.
#[tokio::test]
async fn the_sweep_is_bounded_and_reports_what_remains() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let f = fixture(false).await;
    let authority = bound_authority(&f.store);
    for name in ["one.pdf", "two.pdf", "three.pdf"] {
        let (upload_id, _) = abandoned_prepare(&f, name).await;
        expire(&f, &upload_id).await;
    }
    let none = f
        .store
        .sweep_expired_uploads(&authority, 0)
        .await
        .expect("sweep");
    assert_eq!((none.expired, none.remaining), (0, 3));
    let first = f
        .store
        .sweep_expired_uploads(&authority, 2)
        .await
        .expect("sweep");
    assert_eq!((first.expired, first.owed, first.remaining), (2, 4, 1));
    let second = f
        .store
        .sweep_expired_uploads(&authority, 2)
        .await
        .expect("sweep");
    assert_eq!((second.expired, second.remaining), (1, 0));
    f.finish().await;
}

/// A row a concurrent stage or abort holds is skipped, not waited on, and
/// stays counted until a later sweep decides it.
#[tokio::test]
async fn the_sweep_skips_a_row_someone_holds() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let f = fixture(false).await;
    let authority = bound_authority(&f.store);
    let (upload_id, pending_key) = abandoned_prepare(&f, "held.pdf").await;
    expire(&f, &upload_id).await;
    let mut holder = f.pool.begin().await.expect("begin holder");
    sqlx::query("SELECT 1 FROM proxima_core.blob_uploads WHERE upload_id = $1 FOR UPDATE")
        .bind(Uuid::parse_str(&upload_id).expect("upload id"))
        .execute(&mut *holder)
        .await
        .expect("hold the row");
    let skipped = tokio::time::timeout(
        Duration::from_secs(10),
        f.store.sweep_expired_uploads(&authority, 10),
    )
    .await
    .expect("the sweep does not wait on a held row")
    .expect("sweep");
    assert_eq!((skipped.expired, skipped.remaining), (0, 1));
    holder.rollback().await.expect("release");
    let swept = f
        .store
        .sweep_expired_uploads(&authority, 10)
        .await
        .expect("sweep");
    assert_eq!((swept.expired, swept.remaining), (1, 0));
    assert_eq!(versions_left(&f.config, &pending_key).await, 0);
    f.finish().await;
}

/// The sweep reaches every owner, so it needs the authority that booted the
/// store.
#[tokio::test]
async fn the_sweep_requires_the_boot_authority() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let f = fixture(false).await;
    let (_, unbound) =
        Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests()).into_system_authority();
    let refused = f
        .store
        .sweep_expired_uploads(&unbound, 10)
        .await
        .expect_err("an unbound store refuses");
    assert!(matches!(refused, StorageError::ConstraintViolation(_)));
    let _bound = bound_authority(&f.store);
    let foreign = f
        .store
        .sweep_expired_uploads(&unbound, 10)
        .await
        .expect_err("another boot's authority is refused");
    assert!(matches!(foreign, StorageError::ConstraintViolation(_)));
    f.finish().await;
}
