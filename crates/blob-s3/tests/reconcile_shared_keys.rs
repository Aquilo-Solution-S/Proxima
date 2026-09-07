use std::collections::BTreeSet;
use std::error::Error;
use std::process::Command;
use std::time::Duration;

use proxima_blob_s3::{CitedBlobStore, S3RuntimeConfig};
use proxima_core::storage_ports::{CitedBlobReconcileOutcome, MAX_RECONCILE_SAMPLE};
use proxima_core::{Engine, FlavorRegistry};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;

const CHILD_TEST: &str = "PROXIMA_RECONCILE_SHARED_KEYS_CHILD";
const BUCKET: &str = "reconcile-shared-keys";
const ORPHAN: &str = "objects/unclaimed-fixture";
type TestResult<T> = Result<T, Box<dyn Error>>;

#[test]
fn missing_shared_keys_are_counted_per_row() {
    run_isolated("missing_shared_keys_are_counted_per_row", 2, &[0, 1]);
}

#[test]
fn present_shared_keys_are_not_orphans() {
    run_isolated("present_shared_keys_are_not_orphans", 2, &[]);
}

#[test]
fn missing_shared_keys_are_counted_across_database_pages() {
    // The original and mounted missing rows straddle the 1000-row page.
    // Healthy mounted rows between them must still share their one object.
    run_isolated(
        "missing_shared_keys_are_counted_across_database_pages",
        1001,
        &[0, 1000],
    );
}

fn run_isolated(name: &str, row_count: usize, missing: &[usize]) {
    if std::env::var(CHILD_TEST).as_deref() != Ok(name) {
        // The public store resolves credentials from AWS configuration.
        // Scope fake credentials to a child instead of mutating the parallel
        // test process's environment or consulting any real AWS profile.
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", name, "--nocapture"])
            .env(CHILD_TEST, name)
            .env("AWS_ACCESS_KEY_ID", "local-test-access")
            .env("AWS_SECRET_ACCESS_KEY", "local-test-secret")
            .env_remove("AWS_SESSION_TOKEN")
            .env("AWS_CONFIG_FILE", "/dev/null")
            .env("AWS_SHARED_CREDENTIALS_FILE", "/dev/null")
            .env("AWS_EC2_METADATA_DISABLED", "true")
            .output()
            .expect("run isolated reconciliation test");
        assert!(
            output.status.success(),
            "reconciliation child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let (outcome, expected, request) = runtime
        .block_on(reconcile_fixture(row_count, missing))
        .expect("local reconciliation fixture");
    // The database has already been dropped before these assertions, including
    // the failing assertion that demonstrates the original undercount.
    assert_eq!(outcome.rows_scanned, u64::try_from(row_count).unwrap());
    assert_eq!(
        outcome.missing_objects,
        u64::try_from(missing.len()).unwrap(),
        "each unresolved cited row must be reported, including mounts",
    );
    let actual: Vec<_> = outcome
        .missing_sample
        .iter()
        .map(|row| row.cited_object_id)
        .collect();
    assert_eq!(actual, expected.missing_ids);
    assert_eq!(outcome.objects_scanned, expected.listed_keys.len() as u64);
    assert_eq!(outcome.orphan_objects, 1);
    assert_eq!(outcome.orphan_sample, [ORPHAN.to_owned()]);
    assert_eq!(outcome.foreign_locators, 0);
    assert_eq!(outcome.is_intact(), missing.is_empty());

    let target = request
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap();
    let url = reqwest::Url::parse(&format!("http://localhost{target}")).unwrap();
    assert_eq!(url.path(), format!("/{BUCKET}/"));
    assert!(
        url.query_pairs()
            .any(|(key, value)| key == "list-type" && value == "2")
    );
    assert!(
        url.query_pairs()
            .any(|(key, value)| key == "prefix" && value == "objects/")
    );
}

struct Expected {
    missing_ids: Vec<Uuid>,
    listed_keys: BTreeSet<String>,
}

async fn reconcile_fixture(
    row_count: usize,
    missing: &[usize],
) -> TestResult<(CitedBlobReconcileOutcome, Expected, String)> {
    let database = unique_db_name("proxima_reconcile_shared");
    create_db(&database).await?;
    let result = async {
        let pg = PgStorage::connect(&db_url(&database)).await?;
        pg.run_migrations().await?;
        let expected = seed_rows(pg.pool_for_tests(), row_count, missing).await?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let body = listing_xml(&expected.listed_keys);
        let server = tokio::spawn(serve_listing(listener, body));
        let store = CitedBlobStore::new(
            pg.pool_for_tests().clone(),
            S3RuntimeConfig {
                bucket: BUCKET.into(),
                region: "us-east-1".into(),
                endpoint_url: Some(endpoint),
                force_path_style: true,
                upload_ttl_seconds: 60,
                read_ttl_seconds: 60,
                max_blob_bytes: Some(1024),
            },
        )?;
        let (_, authority) =
            Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests()).into_system_authority();
        store.bind_system_authority(&authority)?;
        let outcome =
            tokio::time::timeout(Duration::from_secs(20), store.reconcile_all(&authority)).await;
        if outcome.as_ref().is_err() || outcome.as_ref().is_ok_and(Result::is_err) {
            server.abort();
        }
        let outcome = outcome??;
        let request = tokio::time::timeout(Duration::from_secs(5), server).await???;
        pg.pool_for_tests().close().await;
        Ok((outcome, expected, request))
    }
    .await;
    drop_db(&database).await?;
    result
}

async fn seed_rows(pool: &sqlx::PgPool, count: usize, missing: &[usize]) -> TestResult<Expected> {
    let blob_ids: Vec<_> = (0..count).map(|i| Uuid::from_u128(i as u128 + 1)).collect();
    let owner_ids: Vec<_> = (0..count)
        .map(|i| Uuid::from_u128(i as u128 + 10_000))
        .collect();
    let upload_ids: Vec<_> = (0..count)
        .map(|i| Uuid::from_u128(i as u128 + 20_000))
        .collect();
    let mut object_keys = Vec::new();
    let mut mounted_from = Vec::new();
    let mut listed_keys = BTreeSet::from([ORPHAN.to_owned()]);
    for i in 0..count {
        let absent = missing.contains(&i);
        let origin = (0..count)
            .find(|index| missing.contains(index) == absent)
            .expect("each row belongs to a nonempty shared-object group");
        let key = format!("objects/{}", upload_ids[origin]);
        if !absent {
            listed_keys.insert(key.clone());
        }
        object_keys.push(key);
        mounted_from.push((i != origin).then_some(upload_ids[origin]));
    }
    // Each row belongs to a different owner, as real cross-owner mounts do.
    // Explicit ordered blob ids guarantee that the boundary case exercises
    // the store's next database page without relying on UUID timing.
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         SELECT owner_id, 'personal' FROM UNNEST($1::uuid[]) AS f(owner_id)",
    )
    .bind(&owner_ids)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.blob (blob_id, owner_id, schema_id, content_hash)
         SELECT blob_id, owner_id, 'core/uploaded-blob-v1', $3
         FROM UNNEST($1::uuid[], $2::uuid[]) AS f(blob_id, owner_id)",
    )
    .bind(&blob_ids)
    .bind(&owner_ids)
    .bind([7_u8; 32].as_slice())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.blob_uploads
            (upload_id, owner_id, blob_id, object_key, mounted_from_upload_id,
             bucket, filename, mime, expected_byte_len, status, expires_at,
             completed_at, sha256, content_hash)
         SELECT upload_id, owner_id, blob_id, object_key, mounted_from_upload_id,
                $6, 'shared.bin', 'application/octet-stream', 7, 'completed',
                now() + interval '1 hour', now(), $7, $7
         FROM UNNEST($1::uuid[], $2::uuid[], $3::uuid[], $4::text[], $5::uuid[])
              AS f(upload_id, owner_id, blob_id, object_key, mounted_from_upload_id)",
    )
    .bind(&upload_ids)
    .bind(&owner_ids)
    .bind(&blob_ids)
    .bind(&object_keys)
    .bind(&mounted_from)
    .bind(BUCKET)
    .bind([7_u8; 32].as_slice())
    .execute(pool)
    .await?;
    Ok(Expected {
        missing_ids: missing
            .iter()
            .take(MAX_RECONCILE_SAMPLE)
            .map(|&i| blob_ids[i])
            .collect(),
        listed_keys,
    })
}

fn listing_xml(keys: &BTreeSet<String>) -> String {
    // Fixture keys contain only ASCII letters, digits, hyphens, and slashes.
    let mut contents = String::new();
    for key in keys {
        contents.push_str("<Contents><Key>");
        contents.push_str(key);
        contents.push_str("</Key></Contents>");
    }
    format!(
        "<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Name>{BUCKET}</Name><IsTruncated>false</IsTruncated>{contents}</ListBucketResult>"
    )
}

async fn serve_listing(listener: TcpListener, body: String) -> std::io::Result<String> {
    let (mut socket, _) = listener.accept().await?;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        let read = socket.read(&mut buffer).await?;
        if read == 0 || request.len() + read > 16 * 1024 {
            return Err(std::io::Error::other(
                "incomplete or oversized request headers",
            ));
        }
        request.extend_from_slice(&buffer[..read]);
    }
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    );
    socket.write_all(response.as_bytes()).await?;
    socket.shutdown().await?;
    String::from_utf8(request).map_err(std::io::Error::other)
}
