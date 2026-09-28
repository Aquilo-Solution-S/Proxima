use super::*;

use proxima_blob_s3::CitedBlobReadUrlTs;
use proxima_core::storage_ports::MAX_RECONCILE_SAMPLE;

const STALE_ROWS: usize = MAX_RECONCILE_SAMPLE + 1;
const DISTINCT_LOSSES: usize = 1001;

#[derive(Debug)]
struct Reports {
    global: CitedBlobReconcileOutcome,
    owner: CitedBlobOwnerReconcileOutcome,
}

#[derive(Debug)]
struct RecoveryObserved {
    repaired: Reports,
    fresh_loss: Reports,
    mounted: Reports,
    control_blob_id: Uuid,
    mounted_read_url_uses_original: bool,
    mounted_bytes: Vec<u8>,
}

#[tokio::test]
async fn reupload_clears_stale_losses_and_mounted_live_upload_checks_the_minting_key()
-> TestResult<()> {
    let Some(observed) = with_fixture("recovery", |database, config, client| {
        Box::pin(observe_recovery(database, config, client))
    })
    .await?
    else {
        return Ok(());
    };
    assert_clean(&observed.repaired);
    assert_clean(&observed.mounted);
    assert_eq!(observed.mounted_bytes, SHARED_BODY);
    assert!(observed.mounted_read_url_uses_original);
    for report in [&observed.repaired, &observed.mounted] {
        assert_eq!(
            (report.global.rows_scanned, report.owner.rows_scanned),
            (2, 2)
        );
    }
    assert_eq!(observed.fresh_loss.global.missing_objects, 1);
    assert_eq!(observed.fresh_loss.owner.missing_objects, 1);
    assert_eq!(observed.fresh_loss.global.missing_sample.len(), 1);
    assert_eq!(observed.fresh_loss.owner.missing_sample.len(), 1);
    assert_eq!(
        observed.fresh_loss.global.missing_sample[0].cited_object_id,
        observed.control_blob_id
    );
    assert_eq!(
        observed.fresh_loss.owner.missing_sample[0].cited_object_id,
        observed.control_blob_id
    );
    Ok(())
}

fn assert_clean(reports: &Reports) {
    assert!(reports.global.is_intact());
    assert!(reports.owner.is_intact());
    assert_eq!(
        (
            reports.global.missing_objects,
            reports.global.foreign_locators,
            reports.global.orphan_objects,
            reports.owner.missing_objects,
            reports.owner.foreign_locators,
            reports.owner.orphan_objects,
        ),
        (0, 0, 0, 0, 0, 0),
    );
    assert!(reports.global.missing_sample.is_empty());
    assert!(reports.owner.missing_sample.is_empty());
}

#[allow(clippy::too_many_lines)]
async fn observe_recovery(
    database: &str,
    config: &S3RuntimeConfig,
    client: &Client,
) -> TestResult<RecoveryObserved> {
    let pg = PgStorage::connect(&db_url(database)).await?;
    pg.run_before_owner_rls_migrations().await?;
    let result: TestResult<_> = async {
        let store = CitedBlobStore::new(pg.pool_for_tests().clone(), config.clone())?;
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let service = CitedBlobService::new(Arc::new(store.clone()));
        let owner = owner_fixture();
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let first = upload(&engine, &service, &store, &authz, owner, SHARED_BODY).await?;
        let stale_ids: Vec<_> = (0..STALE_ROWS).map(|_| Uuid::now_v7()).collect();
        // These rows model old lost objects, with valid canonical identities.
        // More than the sample bound must never hide the next genuine loss.
        amplify_rows(pg.pool_for_tests(), first.0, &stale_ids).await?;
        store.cold_store().delete(&canonical(first.0)).await?;
        let repaired_upload = upload(&engine, &service, &store, &authz, owner, SHARED_BODY).await?;
        if repaired_upload.1 != first.1 {
            return Err("same-bytes repair changed the blob identity".into());
        }
        let control = upload(&engine, &service, &store, &authz, owner, CONTROL_BODY).await?;
        let (_, authority) = engine.into_system_authority();
        store.bind_system_authority(&authority)?;
        let repaired = reports(&store, &authority, &authz, owner).await?;
        store.cold_store().delete(&canonical(control.0)).await?;
        let fresh_loss = reports(&store, &authority, &authz, owner).await?;

        // The newest row now mounts the original older upload. Only that
        // original object exists; checking the newest row's own id is wrong.
        put_one(client, &config.bucket, &canonical(first.0), SHARED_BODY).await?;
        put_one(client, &config.bucket, &canonical(control.0), CONTROL_BODY).await?;
        store
            .cold_store()
            .delete(&canonical(repaired_upload.0))
            .await?;
        sqlx::query(
            "UPDATE proxima_core.blob_uploads
                SET object_key = $1, mounted_from_upload_id = $2
              WHERE upload_id = $3",
        )
        .bind(canonical(first.0))
        .bind(first.0)
        .bind(repaired_upload.0)
        .execute(pg.pool_for_tests())
        .await?;
        let mounted = reports(&store, &authority, &authz, owner).await?;
        let mounted_read_url = store
            .read_url(
                &authz,
                CitedBlobReadUrlTs {
                    owner,
                    cited_object_id: first.1.to_string(),
                },
            )
            .await?
            .read_url;
        let mounted_bytes = reqwest::get(&mounted_read_url)
            .await?
            .error_for_status()?
            .bytes()
            .await?
            .to_vec();
        Ok(RecoveryObserved {
            repaired,
            fresh_loss,
            mounted,
            control_blob_id: control.1,
            mounted_read_url_uses_original: mounted_read_url.contains(&canonical(first.0)),
            mounted_bytes,
        })
    }
    .await;
    pg.pool_for_tests().close().await;
    result
}

async fn reports(
    store: &CitedBlobStore,
    authority: &proxima_core::authz::SystemAuthority,
    authz: &AuthzContext,
    owner: OwnerRef,
) -> TestResult<Reports> {
    Ok(Reports {
        global: store.reconcile_all(authority).await?,
        owner: store.reconcile_owner(authz, owner).await?,
    })
}

async fn put_one(client: &Client, bucket: &str, key: &str, body: &'static [u8]) -> TestResult<()> {
    client
        .put_object()
        .bucket(bucket)
        .key(key)
        .body(ByteStream::from_static(body))
        .send()
        .await?;
    Ok(())
}

#[derive(Debug)]
struct PageObserved {
    reports: Reports,
    expected_sample: Vec<Uuid>,
}

#[tokio::test]
async fn distinct_live_uploads_cross_pages_and_missing_samples_follow_completion_order()
-> TestResult<()> {
    let Some(observed) = with_fixture("live_pages", |database, config, _client| {
        Box::pin(observe_live_pages(database, config))
    })
    .await?
    else {
        return Ok(());
    };
    let losses = u64::try_from(DISTINCT_LOSSES)?;
    let Reports { global, owner } = &observed.reports;
    assert_eq!(
        (
            global.rows_scanned,
            global.objects_scanned,
            global.missing_objects
        ),
        (losses + 1, 1, losses)
    );
    assert_eq!(
        (
            owner.rows_scanned,
            owner.objects_scanned,
            owner.missing_objects
        ),
        (losses + 1, 1, losses)
    );
    assert_eq!((global.orphan_objects, global.foreign_locators), (0, 0));
    assert_eq!((owner.orphan_objects, owner.foreign_locators), (0, 0));
    assert_eq!(
        global
            .missing_sample
            .iter()
            .map(|row| row.cited_object_id)
            .collect::<Vec<_>>(),
        observed.expected_sample
    );
    assert_eq!(
        owner
            .missing_sample
            .iter()
            .map(|row| row.cited_object_id)
            .collect::<Vec<_>>(),
        observed.expected_sample
    );
    Ok(())
}

async fn observe_live_pages(database: &str, config: &S3RuntimeConfig) -> TestResult<PageObserved> {
    let pg = PgStorage::connect(&db_url(database)).await?;
    pg.run_before_owner_rls_migrations().await?;
    let result: TestResult<_> = async {
        let store = CitedBlobStore::new(pg.pool_for_tests().clone(), config.clone())?;
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let service = CitedBlobService::new(Arc::new(store.clone()));
        let owner = owner_fixture();
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let source = upload(&engine, &service, &store, &authz, owner, SHARED_BODY).await?;
        let mut ids: Vec<_> = (0..DISTINCT_LOSSES).map(|_| Uuid::now_v7()).collect();
        ids.sort_unstable();
        insert_distinct_lost_uploads(pg.pool_for_tests(), source.0, &ids).await?;
        let (_, authority) = engine.into_system_authority();
        store.bind_system_authority(&authority)?;
        let reports = reports(&store, &authority, &authz, owner).await?;
        // Earlier UUIDs have newer timestamps; groups of ten share a
        // timestamp and require descending upload-id ties. The last two
        // completed timestamps are NULL and still count after the page.
        let mut ordered: Vec<_> = ids.iter().copied().enumerate().collect();
        ordered.sort_unstable_by(|(left_index, left), (right_index, right)| {
            (left_index / 10)
                .cmp(&(right_index / 10))
                .then_with(|| right.cmp(left))
        });
        let expected_sample = ordered
            .into_iter()
            .take(MAX_RECONCILE_SAMPLE)
            .map(|(_, id)| id)
            .collect();
        Ok(PageObserved {
            reports,
            expected_sample,
        })
    }
    .await;
    pg.pool_for_tests().close().await;
    result
}

async fn insert_distinct_lost_uploads(
    pool: &sqlx::PgPool,
    source: Uuid,
    ids: &[Uuid],
) -> TestResult<()> {
    // This is a cardinality/loss fixture, not a second upload path: each
    // absent artefact has a distinct valid blob hash and canonical key.
    // Its source's real Engine completion exercises admission separately.
    sqlx::query(
        "INSERT INTO proxima_core.blob (blob_id, owner_id, schema_id, content_hash)
         SELECT extra.id, b.owner_id, b.schema_id,
                decode(md5(extra.id::text) || md5(extra.id::text), 'hex')
           FROM proxima_core.blob_uploads u
           JOIN proxima_core.blob b ON b.blob_id = u.blob_id
           CROSS JOIN UNNEST($1::uuid[]) AS extra(id)
          WHERE u.upload_id = $2",
    )
    .bind(ids)
    .bind(source)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.blob_uploads
            (upload_id, owner_id, bucket, object_key, filename, mime,
             expected_byte_len, status, blob_id, sha256, etag, expires_at,
             completed_at, content_hash)
         SELECT extra.id, u.owner_id, u.bucket, 'objects/' || extra.id::text,
                u.filename, u.mime, u.expected_byte_len, u.status, extra.id,
                u.sha256, u.etag, u.expires_at,
                CASE WHEN extra.ordinal > cardinality($1::uuid[]) - 2 THEN NULL
                     ELSE u.completed_at -
                          (((extra.ordinal - 1) / 10 + 1) * INTERVAL '1 second') END,
                b.content_hash
           FROM proxima_core.blob_uploads u
           CROSS JOIN UNNEST($1::uuid[]) WITH ORDINALITY AS extra(id, ordinal)
           JOIN proxima_core.blob b ON b.blob_id = extra.id
          WHERE u.upload_id = $2 AND u.status = 'completed'",
    )
    .bind(ids)
    .bind(source)
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug)]
struct SelectedRead {
    expected_key: bool,
    verified_filename: String,
    held_filename: String,
    bytes: Vec<u8>,
}

#[derive(Debug)]
struct OrderingObserved {
    tied: Reports,
    tied_read: SelectedRead,
    newer_time: Reports,
    newer_time_read: SelectedRead,
}

#[tokio::test]
async fn reads_and_reconcile_share_null_last_timestamp_and_upload_id_tie_resolution()
-> TestResult<()> {
    let Some(observed) = with_fixture("live_order", |database, config, client| {
        Box::pin(observe_resolution_order(database, config, client))
    })
    .await?
    else {
        return Ok(());
    };
    assert_clean(&observed.tied);
    assert_clean(&observed.newer_time);
    for (read, filename) in [
        (&observed.tied_read, "higher-upload-id.bin"),
        (&observed.newer_time_read, "lower-id-newer-time.bin"),
    ] {
        assert!(read.expected_key);
        assert_eq!(read.verified_filename, filename);
        assert_eq!(read.held_filename, filename);
        assert_eq!(read.bytes, SHARED_BODY);
    }
    Ok(())
}

async fn observe_resolution_order(
    database: &str,
    config: &S3RuntimeConfig,
    client: &Client,
) -> TestResult<OrderingObserved> {
    let pg = PgStorage::connect(&db_url(database)).await?;
    pg.run_before_owner_rls_migrations().await?;
    let result: TestResult<_> = async {
        let store = CitedBlobStore::new(pg.pool_for_tests().clone(), config.clone())?;
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let service = CitedBlobService::new(Arc::new(store.clone()));
        let owner = owner_fixture();
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let first = upload(&engine, &service, &store, &authz, owner, SHARED_BODY).await?;
        let second = upload(&engine, &service, &store, &authz, owner, SHARED_BODY).await?;
        if first.0 >= second.0 || first.1 != second.1 {
            return Err("same-bytes uploads must share a blob and have increasing ids".into());
        }
        let null_id = Uuid::now_v7();
        if null_id <= second.0 {
            return Err("NULL timestamp fixture must have the newest upload id".into());
        }
        amplify_rows(pg.pool_for_tests(), first.0, &[null_id]).await?;
        sqlx::query(
            "UPDATE proxima_core.blob_uploads
                SET completed_at = CASE WHEN upload_id = $1 THEN NULL
                                        ELSE TIMESTAMPTZ '2020-01-01 00:00:00+00' END,
                    filename = CASE WHEN upload_id = $2 THEN 'higher-upload-id.bin'
                                    ELSE 'lower-id-newer-time.bin' END
              WHERE blob_id = $3",
        )
        .bind(null_id)
        .bind(second.0)
        .bind(first.1)
        .execute(pg.pool_for_tests())
        .await?;
        store.cold_store().delete(&canonical(first.0)).await?;
        let (_, authority) = engine.into_system_authority();
        store.bind_system_authority(&authority)?;
        let tied = reports(&store, &authority, &authz, owner).await?;
        let tied_read = selected_read(&store, &authz, owner, first.1, second.0).await?;

        put_one(client, &config.bucket, &canonical(first.0), SHARED_BODY).await?;
        store.cold_store().delete(&canonical(second.0)).await?;
        sqlx::query(
            "UPDATE proxima_core.blob_uploads
                SET completed_at = TIMESTAMPTZ '2020-01-02 00:00:00+00'
              WHERE upload_id = $1",
        )
        .bind(first.0)
        .execute(pg.pool_for_tests())
        .await?;
        let newer_time = reports(&store, &authority, &authz, owner).await?;
        let newer_time_read = selected_read(&store, &authz, owner, first.1, first.0).await?;
        Ok(OrderingObserved {
            tied,
            tied_read,
            newer_time,
            newer_time_read,
        })
    }
    .await;
    pg.pool_for_tests().close().await;
    result
}

async fn selected_read(
    store: &CitedBlobStore,
    authz: &AuthzContext,
    owner: OwnerRef,
    blob_id: Uuid,
    expected_upload: Uuid,
) -> TestResult<SelectedRead> {
    use proxima_core::storage_ports::CitedBlobReadPort;
    use std::num::NonZeroU64;

    let url = store
        .read_url(
            authz,
            CitedBlobReadUrlTs {
                owner,
                cited_object_id: blob_id.to_string(),
            },
        )
        .await?;
    let verified = store
        .collect_verified(
            authz,
            owner,
            blob_id,
            NonZeroU64::new(u64::try_from(SHARED_BODY.len())?).ok_or("empty fixture body")?,
        )
        .await?;
    let held = store
        .find_held_blobs(authz, owner, &[*blake3::hash(SHARED_BODY).as_bytes()])
        .await?;
    let held_filename = match held.as_slice() {
        [row] if row.cited_object_id == blob_id => row.filename.clone(),
        _ => return Err("held lookup did not resolve exactly the shared blob".into()),
    };
    Ok(SelectedRead {
        expected_key: url.read_url.contains(&canonical(expected_upload)),
        verified_filename: verified.filename,
        held_filename,
        bytes: verified.bytes,
    })
}
