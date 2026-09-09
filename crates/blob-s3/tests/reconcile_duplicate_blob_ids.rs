use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use aws_sdk_s3::Client;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{
    BucketVersioningStatus, Delete, ObjectIdentifier, VersioningConfiguration,
};
use proxima_blob_s3::{CitedBlobStore, CitedBlobUploadPrepareTs, S3RuntimeConfig};
use proxima_core::storage_ports::{
    CitedBlobOwnerReconcileOutcome, CitedBlobReconcileOutcome, CitedBlobService,
};
use proxima_core::test_fixtures::owner_fixture;
use proxima_core::{AuthPath, AuthzContext, ColdObjectStore, Engine, FlavorRegistry, OwnerRef};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;
use tokio::task::JoinSet;
use uuid::Uuid;

type TestResult<T> = Result<T, Box<dyn Error>>;
const SHARED_ROWS: usize = 1001;
const SHARED_BODY: &[u8] = b"same bytes may have many completed upload locators";
const CONTROL_BODY: &[u8] = b"different content after the shared group";

#[derive(Debug)]
struct Observed {
    healthy_global: CitedBlobReconcileOutcome,
    missing_owner: CitedBlobOwnerReconcileOutcome,
    foreign_owner: CitedBlobOwnerReconcileOutcome,
    foreign_global: CitedBlobReconcileOutcome,
    shared_blob_id: Uuid,
    control_blob_id: Uuid,
}

#[tokio::test]
async fn reconciliation_visits_each_upload_when_blob_ids_cross_a_page() -> TestResult<()> {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return Ok(());
    }
    let config = S3RuntimeConfig {
        bucket: format!("pg-reconcile-{}", Uuid::now_v7().simple()),
        force_path_style: true,
        ..S3RuntimeConfig::from_env()?
    };
    let client = s3_client(&config).await;
    let database = unique_db_name("proxima_reconcile_duplicates");
    create_db(&database).await?;
    eprintln!(
        "duplicate-blob-id fixture: database={database} bucket={}",
        config.bucket
    );
    let mut bucket_created = false;
    let result: TestResult<_> = async {
        client.create_bucket().bucket(&config.bucket).send().await?;
        bucket_created = true;
        client
            .put_bucket_versioning()
            .bucket(&config.bucket)
            .versioning_configuration(
                VersioningConfiguration::builder()
                    .status(BucketVersioningStatus::Enabled)
                    .build(),
            )
            .send()
            .await?;
        tokio::time::timeout(
            Duration::from_mins(2),
            observe_reports(&database, &config, &client),
        )
        .await?
    }
    .await;
    // All reports, including the baseline's wrong counts, are retained before
    // cleanup. Assertions below cannot leave this fixture's bucket or DB.
    eprintln!("duplicate-blob-id reconciliation observation: {result:?}");
    let bucket_cleanup: TestResult<()> = if bucket_created {
        match tokio::time::timeout(
            Duration::from_secs(30),
            empty_and_delete_bucket(&client, &config.bucket),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => Err(error.into()),
        }
    } else {
        Ok(())
    };
    let database_cleanup = drop_db(&database).await;
    eprintln!("duplicate-blob-id cleanup: bucket={bucket_cleanup:?} database={database_cleanup:?}");
    bucket_cleanup?;
    database_cleanup?;
    let observed = result?;
    assert_complete_reports(&observed)
}

fn assert_complete_reports(observed: &Observed) -> TestResult<()> {
    let shared = u64::try_from(SHARED_ROWS)?;

    assert_eq!(
        (
            observed.healthy_global.rows_scanned,
            observed.healthy_global.objects_scanned,
            observed.healthy_global.missing_objects,
            observed.healthy_global.orphan_objects,
            observed.healthy_global.foreign_locators,
        ),
        (shared + 1, shared + 1, 0, 0, 0),
        "every present canonical key is owned by a completed upload: {observed:?}"
    );
    assert!(observed.healthy_global.orphan_sample.is_empty());
    assert_eq!(
        (
            observed.missing_owner.rows_scanned,
            observed.missing_owner.objects_scanned,
            observed.missing_owner.missing_objects,
            observed.missing_owner.foreign_locators,
        ),
        (shared + 1, 1, shared, 0),
        "all missing shared locators and the later healthy control must be visited"
    );
    assert!(
        observed
            .missing_owner
            .missing_sample
            .iter()
            .all(|row| row.cited_object_id == observed.shared_blob_id)
    );
    assert!(!observed.missing_owner.missing_sample.is_empty());
    assert_ne!(observed.shared_blob_id, observed.control_blob_id);
    assert_eq!(
        (
            observed.foreign_owner.rows_scanned,
            observed.foreign_owner.objects_scanned,
            observed.foreign_owner.missing_objects,
            observed.foreign_owner.foreign_locators,
            observed.foreign_owner.orphan_objects,
        ),
        (1, 1, 0, shared, 0),
        "foreign rows cannot be lost behind their shared blob id"
    );
    assert_eq!(
        (
            observed.foreign_global.rows_scanned,
            observed.foreign_global.objects_scanned,
            observed.foreign_global.missing_objects,
            observed.foreign_global.orphan_objects,
            observed.foreign_global.foreign_locators,
        ),
        (1, 1, 0, 0, shared),
    );
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn observe_reports(
    database: &str,
    config: &S3RuntimeConfig,
    client: &Client,
) -> TestResult<Observed> {
    let pg = PgStorage::connect(&db_url(database)).await?;
    pg.run_migrations().await?;
    let result: TestResult<_> = async {
        let store = CitedBlobStore::new(pg.pool_for_tests().clone(), config.clone())?;
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let service = CitedBlobService::new(Arc::new(store.clone()));
        let owner = owner_fixture();
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        // Prove this relation is produced by the normal prepare/HTTP PUT/
        // Engine complete path before amplifying its cardinality with SQL.
        let first = upload(&engine, &service, &store, &authz, owner, SHARED_BODY).await?;
        let second = upload(&engine, &service, &store, &authz, owner, SHARED_BODY).await?;
        if first.1 != second.1 || first.0 == second.0 {
            return Err("normal same-bytes uploads did not share one blob identity".into());
        }
        let completed: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.blob_uploads
              WHERE owner_id = $1 AND blob_id = $2 AND status = 'completed'",
        )
        .bind(owner.stored_owner_id())
        .bind(first.1)
        .fetch_one(pg.pool_for_tests())
        .await?;
        if completed != 2 {
            return Err("two normal completed uploads must exist before amplification".into());
        }
        let extra_ids: Vec<_> = (2..SHARED_ROWS).map(|_| Uuid::now_v7()).collect();
        amplify_rows(pg.pool_for_tests(), first.0, &extra_ids).await?;
        let mut shared_keys = vec![canonical(first.0), canonical(second.0)];
        shared_keys.extend(extra_ids.iter().copied().map(canonical));
        put_keys(client, &config.bucket, &shared_keys[2..]).await?;

        // A later normal distinct-content upload must survive the next page.
        // Ensure it is later than every shared upload even within one clock tick.
        tokio::time::sleep(Duration::from_millis(2)).await;
        let control = upload(&engine, &service, &store, &authz, owner, CONTROL_BODY).await?;
        if control.1 <= first.1 || extra_ids.iter().any(|id| *id >= control.0) {
            return Err("control must sort after shared group for both candidate cursors".into());
        }
        let (_, authority) = engine.into_system_authority();
        store.bind_system_authority(&authority)?;
        let healthy_global = store.reconcile_all(&authority).await?;

        // Physical absence is intentional in this phase; retain the distinct
        // control object to prove scanning continues after the shared group.
        delete_keys(&store, &shared_keys).await?;
        if store.cold_store().get(&canonical(control.0)).await? != CONTROL_BODY {
            return Err("later distinct control object was not retained".into());
        }
        let missing_owner = store.reconcile_owner(&authz, owner).await?;

        // This phase explicitly models stored foreign locators, like the
        // existing reconciliation forgery tests. It never accesses that bucket.
        sqlx::query(
            "UPDATE proxima_core.blob_uploads SET bucket = 'foreign-reconcile-fixture'
              WHERE owner_id = $1 AND blob_id = $2",
        )
        .bind(owner.stored_owner_id())
        .bind(first.1)
        .execute(pg.pool_for_tests())
        .await?;
        let foreign_owner = store.reconcile_owner(&authz, owner).await?;
        let foreign_global = store.reconcile_all(&authority).await?;
        Ok(Observed {
            healthy_global,
            missing_owner,
            foreign_owner,
            foreign_global,
            shared_blob_id: first.1,
            control_blob_id: control.1,
        })
    }
    .await;
    pg.pool_for_tests().close().await;
    result
}

fn canonical(upload_id: Uuid) -> String {
    format!("objects/{upload_id}")
}

async fn upload(
    engine: &Engine,
    service: &CitedBlobService,
    store: &CitedBlobStore,
    authz: &AuthzContext,
    owner: OwnerRef,
    body: &'static [u8],
) -> TestResult<(Uuid, Uuid)> {
    let prepared = store
        .prepare_upload(
            authz,
            CitedBlobUploadPrepareTs {
                owner,
                filename: "page-fixture.bin".into(),
                mime: "application/octet-stream".into(),
                byte_len: u64::try_from(body.len())?,
            },
        )
        .await?;
    let mut request = reqwest::Client::new().put(&prepared.upload_url);
    for header in prepared.headers {
        request = request.header(header.name.as_str(), header.value.as_str());
    }
    request.body(body).send().await?.error_for_status()?;
    let completed = engine
        .complete_upload_as_fact(service, authz, owner, &prepared.upload_id, &[])
        .await?;
    Ok((
        Uuid::parse_str(&prepared.upload_id)?,
        Uuid::parse_str(&completed.blob.cited_object_id)?,
    ))
}

async fn amplify_rows(pool: &sqlx::PgPool, source: Uuid, ids: &[Uuid]) -> TestResult<()> {
    // Cardinality fixture only: preserve owner/blob FKs, completed status,
    // staged hashes/length and canonical upload-derived identity. The same
    // bytes are written to every additional object's key in the real bucket.
    sqlx::query(
        "INSERT INTO proxima_core.blob_uploads
            (upload_id, owner_id, bucket, object_key, filename, mime,
             expected_byte_len, status, blob_id, sha256, etag, expires_at,
             completed_at, content_hash)
         SELECT extra.upload_id, u.owner_id, u.bucket,
                'objects/' || extra.upload_id::text, u.filename, u.mime,
                u.expected_byte_len, u.status, u.blob_id, u.sha256, u.etag,
                u.expires_at, u.completed_at, u.content_hash
           FROM proxima_core.blob_uploads u
           CROSS JOIN UNNEST($1::uuid[]) AS extra(upload_id)
          WHERE u.upload_id = $2 AND u.status = 'completed'",
    )
    .bind(ids)
    .bind(source)
    .execute(pool)
    .await?;
    Ok(())
}

async fn put_keys(client: &Client, bucket: &str, keys: &[String]) -> TestResult<()> {
    let mut tasks = JoinSet::new();
    for key in keys {
        let client = client.clone();
        let bucket = bucket.to_owned();
        let key = key.clone();
        tasks.spawn(async move {
            client
                .put_object()
                .bucket(bucket)
                .key(key)
                .body(ByteStream::from_static(SHARED_BODY))
                .send()
                .await
                .map(|_| ())
        });
        if tasks.len() >= 16 {
            tasks.join_next().await.ok_or("missing PUT task")???;
        }
    }
    while let Some(result) = tasks.join_next().await {
        result??;
    }
    Ok(())
}

async fn delete_keys(store: &CitedBlobStore, keys: &[String]) -> TestResult<()> {
    let mut tasks = JoinSet::new();
    for key in keys {
        let cold = store.cold_store();
        let key = key.clone();
        tasks.spawn(async move { cold.delete(&key).await });
        if tasks.len() >= 16 {
            tasks.join_next().await.ok_or("missing DELETE task")???;
        }
    }
    while let Some(result) = tasks.join_next().await {
        result??;
    }
    Ok(())
}

async fn s3_client(config: &S3RuntimeConfig) -> Client {
    let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(Region::new(config.region.clone()));
    if let Some(endpoint) = &config.endpoint_url {
        loader = loader.endpoint_url(endpoint);
    }
    Client::from_conf(
        aws_sdk_s3::config::Builder::from(&loader.load().await)
            .force_path_style(true)
            .build(),
    )
}

async fn empty_and_delete_bucket(client: &Client, bucket: &str) -> TestResult<()> {
    // The UUID bucket was created by this test alone. Drain its first page
    // repeatedly so deleting versions cannot invalidate a later-page cursor.
    loop {
        let page = client.list_object_versions().bucket(bucket).send().await?;
        let ids = page
            .versions()
            .iter()
            .map(|v| (v.key(), v.version_id()))
            .chain(
                page.delete_markers()
                    .iter()
                    .map(|v| (v.key(), v.version_id())),
            )
            .map(|(key, version)| {
                ObjectIdentifier::builder()
                    .key(key.ok_or("missing cleanup key")?)
                    .version_id(version.ok_or("missing cleanup version")?)
                    .build()
                    .map_err(Into::into)
            })
            .collect::<TestResult<Vec<_>>>()?;
        if ids.is_empty() {
            if page.is_truncated() == Some(true) {
                return Err("empty truncated bucket cleanup page".into());
            }
            break;
        }
        let deleted = client
            .delete_objects()
            .bucket(bucket)
            .delete(Delete::builder().set_objects(Some(ids)).build()?)
            .send()
            .await?;
        if !deleted.errors().is_empty() {
            return Err(format!("fixture cleanup errors: {:?}", deleted.errors()).into());
        }
    }
    client.delete_bucket().bucket(bucket).send().await?;
    Ok(())
}
