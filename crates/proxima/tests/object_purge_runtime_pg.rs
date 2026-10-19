//! The runtime's object-purge feature against Postgres and a real object
//! store: a server with an object store and no host code drains what an
//! erase owes, sweeps an expired upload, and its readiness answer counts a
//! debt that keeps failing. Skips without `PROXIMA_S3_*`.

use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use proxima::flavor::{FlavorBundle, FlavorRegistry, FlavorRegistryError, NamedMigrator};
use proxima::{
    AppInfo, AuthPath, AuthzContext, Feature, FeatureState, FlavorApp, GroupId, OwnerId, Proxima,
    Role, RunningProxima, ServedSurface, UserId,
};
use proxima_blob_s3::{CitedBlobUploadPrepareTs, S3KeyPair, S3RuntimeConfig};
use proxima_core::storage_ports::CitedBlobService;
use proxima_pg_testkit::{create_db, db_url, drop_db, split_role_urls, unique_db_name};
use uuid::Uuid;

type TestResult<T> = Result<T, Box<dyn Error>>;
const BODY: &[u8] = b"an erase owes these bytes; the server pays without host code";
const PATIENCE: Duration = Duration::from_secs(30);
const TICK: Duration = Duration::from_secs(1);

struct CoreApp;

impl FlavorBundle for CoreApp {
    fn register(_registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for CoreApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "object-purge-runtime-test",
            title: "Object Purge Runtime Test",
            version: "1",
        }
    }
}

/// Every MCP host names an authenticator; this one admits nobody, since the
/// tests only probe `/readyz`. Nothing here touches purging or sweeping.
struct NoBearer;

#[async_trait::async_trait]
impl proxima_core::Authenticator for NoBearer {
    async fn authenticate(
        &self,
        _credentials: &proxima_core::Credentials,
    ) -> Result<AuthzContext, proxima_core::AuthError> {
        Err(proxima_core::AuthError::InvalidCredentials)
    }
}

fn s3_config() -> Option<S3RuntimeConfig> {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return None;
    }
    Some(S3RuntimeConfig {
        force_path_style: true,
        ..S3RuntimeConfig::from_env().expect("PROXIMA_S3_* parses")
    })
}

/// Same endpoint and bucket, so the same store; every request is refused.
fn refusing(config: &S3RuntimeConfig) -> S3RuntimeConfig {
    S3RuntimeConfig {
        key_pair: S3KeyPair::new(
            std::env::var("PROXIMA_S3_ACCESS_KEY_ID").expect("access key"),
            "not-the-secret".to_owned(),
        ),
        ..config.clone()
    }
}

/// The builder a `serve` host gets: nothing about purging or sweeping.
async fn app(database: &str, owner: OwnerId) -> TestResult<Proxima<CoreApp>> {
    let (runtime_url, platform_url) = split_role_urls(database).await?;
    Ok(Proxima::<CoreApp>::app()
        .database_url(runtime_url)
        .store_number(0)
        .platform_database_url(platform_url)
        .owner(owner)
        .served_surface(ServedSurface::All))
}

/// A server with an object store, health endpoints and no host code.
async fn serve(
    database: &str,
    owner: OwnerId,
    s3: S3RuntimeConfig,
) -> TestResult<(RunningProxima, String)> {
    let running = app(database, owner)
        .await?
        .s3(s3)
        .object_purge_interval(TICK)
        .authenticator(Arc::new(NoBearer))
        .mcp_bind("127.0.0.1:0".parse()?)
        .health_endpoints(true)
        .run()
        .await?;
    assert_eq!(
        running.boot_report().get(Feature::ObjectPurge).state,
        FeatureState::Started,
        "an object store alone starts the drain"
    );
    let base = format!("http://{}", running.mcp_addr().expect("bound"));
    Ok((running, base))
}

async fn readyz(base: &str) -> TestResult<String> {
    Ok(reqwest::get(format!("{base}/readyz")).await?.text().await?)
}

/// Poll `probe` until it holds, or fail after [`PATIENCE`].
async fn eventually<F, Fut>(what: &str, mut probe: F) -> TestResult<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = TestResult<bool>>,
{
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !probe().await? {
        if tokio::time::Instant::now() > deadline {
            return Err(format!("timed out waiting for {what}").into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(())
}

fn host_context(owner: OwnerId, path: AuthPath) -> AuthzContext {
    proxima_core::test_fixtures::authenticated_context(
        AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(owner, Role::admin())],
            path,
        )
        .narrowed_to_owner(owner)
        .expect("trusted host resolved this exact owner"),
    )
}

async fn admin_pool(database: &str) -> TestResult<sqlx::PgPool> {
    Ok(sqlx::PgPool::connect(&db_url(database)).await?)
}

async fn register_group(pool: &sqlx::PgPool, group: GroupId) -> TestResult<()> {
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'group')
         ON CONFLICT (owner_id) DO NOTHING",
    )
    .bind(group.into_inner())
    .execute(pool)
    .await?;
    Ok(())
}

async fn debts(pool: &sqlx::PgPool) -> TestResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.cold_purge_pending")
            .fetch_one(pool)
            .await?,
    )
}

async fn s3_put(config: &S3RuntimeConfig, key: &str, bytes: &[u8]) -> TestResult<()> {
    config
        .client()
        .put_object()
        .bucket(&config.bucket)
        .key(key)
        .body(bytes.to_vec().into())
        .send()
        .await?;
    Ok(())
}

/// Versions and delete markers left under this exact key (zero for a gone
/// object in either bucket mode).
async fn versions(config: &S3RuntimeConfig, key: &str) -> TestResult<usize> {
    let page = config
        .client()
        .list_object_versions()
        .bucket(&config.bucket)
        .prefix(key)
        .send()
        .await?;
    Ok(page
        .versions()
        .iter()
        .map(|v| v.key())
        .chain(page.delete_markers().iter().map(|m| m.key()))
        .filter(|found| *found == Some(key))
        .count())
}

async fn with_database<F, Fut>(prefix: &str, body: F) -> TestResult<()>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = TestResult<()>>,
{
    let database = unique_db_name(prefix);
    create_db(&database).await?;
    let result = body(database.clone()).await;
    drop_db(&database).await?;
    result
}

/// Acceptance: a server with an object store and no host code. An erase's
/// objects are gone after the next drain pass, and the health answer counts
/// a debt that keeps failing.
#[tokio::test]
async fn a_server_drains_an_erase_and_counts_a_debt_that_keeps_failing() -> TestResult<()> {
    let Some(config) = s3_config() else {
        return Ok(());
    };
    with_database("proxima_object_purge_accept", |database| {
        Box::pin(async move {
            let group = GroupId::new(Uuid::now_v7());
            let owner = OwnerId::from(group);
            let admin = admin_pool(&database).await?;
            let canonical = Box::pin(owe_an_erase(&database, &admin, &config, group)).await?;
            Box::pin(drained_by_the_server(
                &database, &admin, &config, owner, &canonical,
            ))
            .await?;
            Box::pin(counted_while_failing(&database, &admin, &config, owner)).await?;
            admin.close().await;
            Ok(())
        })
    })
    .await
}

/// Complete an upload, then erase its owner from a process with no object
/// store, so the erase can only owe the bytes. Returns the canonical key.
async fn owe_an_erase(
    database: &str,
    admin: &sqlx::PgPool,
    config: &S3RuntimeConfig,
    group: GroupId,
) -> TestResult<String> {
    let owner = OwnerId::from(group);
    // An upload completes on a server whose drain is off ...
    let first = app(database, owner)
        .await?
        .s3(config.clone())
        .object_purge_interval(Duration::ZERO)
        .build()
        .await?;
    register_group(admin, group).await?;
    let store = first.host().blobs().expect("configured store").clone();
    let authz = host_context(owner, AuthPath::HostBearer);
    let prepared = store
        .prepare_upload(
            &authz,
            CitedBlobUploadPrepareTs {
                owner,
                filename: "drained.pdf".into(),
                mime: "application/pdf".into(),
                byte_len: BODY.len() as u64,
            },
        )
        .await?;
    s3_put(config, &format!("pending/{}", prepared.upload_id), BODY).await?;
    let service = CitedBlobService::new(Arc::new(store.clone()));
    first
        .host()
        .engine()
        .complete_upload_as_fact(&service, &authz, owner, &prepared.upload_id, &[])
        .await?;
    drop(service);
    drop(store);
    first.shutdown().await;
    let canonical = format!("objects/{}", prepared.upload_id);
    assert_eq!(versions(config, &canonical).await?, 1);

    // ... and is erased by a process with no object store, so the erase
    // can only owe the bytes.
    let eraser = app(database, owner).await?.build().await?;
    eraser
        .host()
        .engine()
        .erase_group_owner(&host_context(owner, AuthPath::System), group)
        .await?;
    eraser.shutdown().await;
    assert!(debts(admin).await? > 0, "the erase owes its keys");
    assert_eq!(versions(config, &canonical).await?, 1);

    Ok(canonical)
}

async fn drained_by_the_server(
    database: &str,
    admin: &sqlx::PgPool,
    config: &S3RuntimeConfig,
    owner: OwnerId,
    canonical: &str,
) -> TestResult<()> {
    // A server with the object store pays it on its next pass.
    let (running, base) = serve(database, owner, config.clone()).await?;
    eventually("the erase's debt to drain", || async {
        Ok(debts(admin).await? == 0)
    })
    .await?;
    assert_eq!(
        versions(config, canonical).await?,
        0,
        "the objects are gone"
    );
    eventually("the health answer to show nothing owed", || async {
        Ok(readyz(&base).await? == "ready\nobject-purge: owed=0 due=0 deferred=0 stuck=0")
    })
    .await?;
    running.shutdown().await;

    Ok(())
}

async fn counted_while_failing(
    database: &str,
    admin: &sqlx::PgPool,
    config: &S3RuntimeConfig,
    owner: OwnerId,
) -> TestResult<()> {
    // Two debts the store keeps refusing: one already past the attempt
    // threshold, one fresh.
    let store_id = config.object_store_id()?.to_string();
    let (stuck_key, fresh_key) = (
        format!("objects/{}", Uuid::now_v7()),
        format!("objects/{}", Uuid::now_v7()),
    );
    sqlx::query(
        "INSERT INTO proxima_core.cold_purge_pending
                 (object_key, owner_id, object_store, attempts)
             VALUES ($1, NULL, $3, 8), ($2, NULL, $3, 0)",
    )
    .bind(&stuck_key)
    .bind(&fresh_key)
    .bind(&store_id)
    .execute(admin)
    .await?;
    let (running, base) = serve(database, owner, refusing(config)).await?;
    eventually("the health answer to count the failing debts", || async {
        Ok(readyz(&base).await?.contains("deferred=2"))
    })
    .await?;
    let answer = readyz(&base).await?;
    // Counts only: debt that keeps failing never makes the replica unready.
    assert_eq!(
        answer,
        "ready\nobject-purge: owed=2 due=0 deferred=2 stuck=1"
    );
    assert!(!answer.contains("foreign="), "{answer}");
    running.shutdown().await;
    // Never dropped: both stay queued, each naming its last error.
    let rows: Vec<(String, i32, Option<String>)> = sqlx::query_as(
        "SELECT object_key, attempts, last_error FROM proxima_core.cold_purge_pending
             ORDER BY attempts DESC",
    )
    .fetch_all(admin)
    .await?;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!((rows[0].0.as_str(), rows[0].1), (stuck_key.as_str(), 9));
    assert_eq!((rows[1].0.as_str(), rows[1].1), (fresh_key.as_str(), 1));
    assert!(rows.iter().all(|row| row.2.is_some()), "{rows:?}");
    Ok(())
}

/// The same server sweeps an upload whose TTL passed before its bytes were
/// staged, then drains both keys the sweep owes.
#[tokio::test]
async fn a_server_sweeps_an_expired_upload() -> TestResult<()> {
    let Some(config) = s3_config() else {
        return Ok(());
    };
    with_database("proxima_object_purge_sweep", |database| async move {
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerId::from(group);
        let admin = admin_pool(&database).await?;
        let (running, _base) = serve(&database, owner, config.clone()).await?;
        register_group(&admin, group).await?;
        let store = running.host().blobs().expect("configured store").clone();
        let prepared = store
            .prepare_upload(
                &host_context(owner, AuthPath::HostBearer),
                CitedBlobUploadPrepareTs {
                    owner,
                    filename: "abandoned.pdf".into(),
                    mime: "application/pdf".into(),
                    byte_len: BODY.len() as u64,
                },
            )
            .await?;
        let pending = format!("pending/{}", prepared.upload_id);
        s3_put(&config, &pending, BODY).await?;
        sqlx::query(
            "UPDATE proxima_core.blob_uploads SET expires_at = now() - interval '1 minute'
             WHERE upload_id = $1",
        )
        .bind(Uuid::parse_str(&prepared.upload_id)?)
        .execute(&admin)
        .await?;
        eventually("the sweep to expire the upload", || async {
            let status: String = sqlx::query_scalar(
                "SELECT status::text FROM proxima_core.blob_uploads WHERE upload_id = $1",
            )
            .bind(Uuid::parse_str(&prepared.upload_id)?)
            .fetch_one(&admin)
            .await?;
            Ok(status == "expired" && debts(&admin).await? == 0)
        })
        .await?;
        assert_eq!(versions(&config, &pending).await?, 0);
        drop(store);
        running.shutdown().await;
        admin.close().await;
        Ok(())
    })
    .await
}

/// Zero turns the feature off: the boot report says why, and owed debt
/// stays for whichever pass the operator runs instead.
#[tokio::test]
async fn a_zero_interval_turns_the_drain_off() -> TestResult<()> {
    let Some(config) = s3_config() else {
        return Ok(());
    };
    with_database("proxima_object_purge_off", |database| async move {
        let owner = OwnerId::from(GroupId::new(Uuid::now_v7()));
        let admin = admin_pool(&database).await?;
        let built = app(&database, owner)
            .await?
            .s3(config.clone())
            .object_purge_interval(Duration::ZERO)
            .build()
            .await?;
        sqlx::query(
            "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, object_store)
             VALUES ($1, NULL, $2)",
        )
        .bind(format!("objects/{}", Uuid::now_v7()))
        .bind(config.object_store_id()?.to_string())
        .execute(&admin)
        .await?;
        let decision = built.boot_report().get(Feature::ObjectPurge).clone();
        assert_eq!(decision.state, FeatureState::Off);
        assert!(
            decision.reason.contains("interval is 0"),
            "{}",
            decision.reason
        );
        assert!(built.feature_control(Feature::ObjectPurge).is_none());
        tokio::time::sleep(3 * TICK).await;
        assert_eq!(
            debts(&admin).await?,
            1,
            "nothing drains with the feature off"
        );
        built.shutdown().await;
        admin.close().await;
        Ok(())
    })
    .await
}

/// Stores do not move: a server wired to an object store refuses to start
/// while the purge queue holds debt for another store, and says which.
#[tokio::test]
async fn a_server_refuses_to_start_over_another_stores_debt() -> TestResult<()> {
    let Some(config) = s3_config() else {
        return Ok(());
    };
    with_database("proxima_object_purge_foreign", |database| async move {
        let owner = OwnerId::from(GroupId::new(Uuid::now_v7()));
        let admin = admin_pool(&database).await?;
        // Migrate with no object store, then owe keys to another store.
        app(&database, owner).await?.build().await?.shutdown().await;
        sqlx::query(
            "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, object_store)
             VALUES ($1, NULL, 's3:id/elsewhere'), ($2, NULL, 's3:id/elsewhere')",
        )
        .bind(format!("objects/{}", Uuid::now_v7()))
        .bind(format!("objects/{}", Uuid::now_v7()))
        .execute(&admin)
        .await?;
        let configured = config.object_store_id()?.to_string();
        let Err(refused) = app(&database, owner).await?.s3(config).build().await else {
            return Err("startup must be refused over another store's debt".into());
        };
        let refused = refused.to_string();
        assert!(refused.contains("holds 2 debts"), "{refused}");
        assert!(
            refused.contains(&format!("configured {configured}")),
            "{refused}"
        );
        assert!(refused.contains("found s3:id/elsewhere (2)"), "{refused}");
        assert_eq!(debts(&admin).await?, 2, "nothing is cleared");
        admin.close().await;
        Ok(())
    })
    .await
}
