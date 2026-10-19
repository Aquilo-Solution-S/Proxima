//! A purge debt names the object store that holds its bytes.
//!
//! Two processes that reach the same bucket through different endpoints, or
//! two stores that share an endpoint and bucket, are different stores unless
//! the operator says otherwise with `PROXIMA_S3_STORE_ID`. A drain wired to
//! the wrong one must never delete through it or clear the debt.

use super::*;
use lifecycle::lifecycle_store;

async fn owe(pool: &sqlx::PgPool, store: &str, key: &str) {
    sqlx::query(
        "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, object_store)
         VALUES ($1, NULL, $2)",
    )
    .bind(key)
    .bind(store)
    .execute(pool)
    .await
    .expect("record a purge debt");
}

async fn debts(pool: &sqlx::PgPool, key: &str) -> Vec<(String, time::OffsetDateTime)> {
    sqlx::query_as(
        "SELECT object_store, enqueued_at FROM proxima_core.cold_purge_pending
          WHERE object_key = $1 ORDER BY object_store",
    )
    .bind(key)
    .fetch_all(pool)
    .await
    .expect("read purge debts")
}

/// The same bucket under another spelling of the endpoint host.
fn alias_endpoint(config: &S3RuntimeConfig) -> Option<S3RuntimeConfig> {
    let endpoint = config.endpoint_url.as_deref()?;
    let alias = if endpoint.contains("127.0.0.1") {
        endpoint.replace("127.0.0.1", "localhost")
    } else if endpoint.contains("localhost") {
        endpoint.replace("localhost", "127.0.0.1")
    } else {
        return None;
    };
    Some(S3RuntimeConfig {
        endpoint_url: Some(alias),
        ..config.clone()
    })
}

async fn drain(
    pg: &PgStorage,
    store: &CitedBlobStore,
) -> Result<proxima_storage_pg::ColdPurgeRetryOutcome, StorageError> {
    pg.clone()
        .with_cold(Arc::new(store.cold_store()))
        .retry_cold_object_purges(proxima_storage_pg::ColdPurgeRetryOptions {
            batch_size: 10,
            dry_run: false,
        })
        .await
}

/// Acceptance: a debt owed to store A is never discharged by a drain wired
/// to store B, whether B differs by endpoint or by operator id. The bytes and
/// the debt row both survive; wiring A back purges them.
#[tokio::test]
async fn a_drain_wired_to_another_store_leaves_the_debt_and_the_bytes() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store_a = lifecycle_store(&pg, &config);
    let key = format!("objects/{}", Uuid::now_v7());
    put_object_via_sdk(&config, &key, b"bytes owed to store A").await;
    owe(&pool, store_a.store_id().as_str(), &key).await;
    let before = debts(&pool, &key).await;

    let mut others = vec![S3RuntimeConfig {
        store_id: Some("x".to_owned()),
        ..config.clone()
    }];
    others.extend(alias_endpoint(&config));
    assert!(others.len() == 2, "the dev endpoint has an alias host");
    for other in others {
        let store_b = lifecycle_store(&pg, &other);
        assert_ne!(store_b.store_id(), store_a.store_id());
        let refused = drain(&pg, &store_b)
            .await
            .expect_err("a debt for another store is not this drain's to clear");
        assert!(
            refused.to_string().contains(store_a.store_id().as_str()),
            "the refusal names the store that is owed: {refused}"
        );
        assert_eq!(
            debts(&pool, &key).await,
            before,
            "the debt row is unchanged"
        );
        assert!(
            head(&config.client(), &config.bucket, &key).await.is_some(),
            "nothing was deleted through the wrong store"
        );
    }

    let drained = drain(&pg, &store_a).await.expect("store A drains its debt");
    assert_eq!(
        (drained.selected, drained.purged, drained.failed),
        (1, 1, 0)
    );
    assert!(debts(&pool, &key).await.is_empty());
    assert!(head(&config.client(), &config.bucket, &key).await.is_none());

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}

/// One key owed in two stores is two obligations: both rows coexist, and
/// discharging one leaves the other.
#[tokio::test]
async fn the_same_key_owed_in_two_stores_is_two_debts() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store_a = lifecycle_store(&pg, &config);
    let store_b = lifecycle_store(
        &pg,
        &S3RuntimeConfig {
            store_id: Some("second-store".to_owned()),
            ..config.clone()
        },
    );
    let key = format!("objects/{}", Uuid::now_v7());
    owe(&pool, store_a.store_id().as_str(), &key).await;
    owe(&pool, store_b.store_id().as_str(), &key).await;
    let stores: Vec<String> = debts(&pool, &key)
        .await
        .into_iter()
        .map(|(store, _)| store)
        .collect();
    let mut expected = vec![
        store_a.store_id().to_string(),
        store_b.store_id().to_string(),
    ];
    expected.sort();
    assert_eq!(stores, expected);

    // Re-enqueueing under one store renews that store's debt only.
    let renewed = sqlx::query(
        "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, object_store)
         VALUES ($1, NULL, $2)
         ON CONFLICT (object_store, object_key) DO UPDATE SET enqueued_at = now()",
    )
    .bind(&key)
    .bind(store_b.store_id().as_str())
    .execute(&pool)
    .await
    .expect("renew one store's debt");
    assert_eq!(renewed.rows_affected(), 1);
    assert_eq!(debts(&pool, &key).await.len(), 2);

    drop(pool);
    drop_db(&db_name).await.expect("drop db");
}
