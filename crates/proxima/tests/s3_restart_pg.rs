use std::error::Error;
use std::sync::Arc;

use aws_sdk_s3::Client;
use aws_sdk_s3::config::Region;
use proxima::{
    AuthPath, AuthzContext, EmbedConfig, EmbeddedProxima, GroupId, OwnerEraseOutcome, OwnerRef,
    ProximaBuilder, Role, UserId,
};
use proxima_blob_s3::{CitedBlobUploadPrepareTs, S3RuntimeConfig};
use proxima_core::storage_ports::CitedBlobService;
use proxima_core::{AgentNoteV1, ColdObjectStore};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::{ColdPurgeRetryOptions, PgStorage};
use uuid::Uuid;

type TestResult<T> = Result<T, Box<dyn Error>>;
const BODY: &[u8] = b"cited S3 bytes must remain owed across a configuration change";

#[derive(Debug)]
struct EraseObservation {
    receipt: OwnerEraseOutcome,
    canonical_debt: Option<String>,
    bucket: String,
    versions_after_erase: usize,
}

#[tokio::test]
async fn reboot_without_s3_preserves_cited_object_purge_debt() -> TestResult<()> {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return Ok(());
    }
    let observed = reboot_fixture(false).await?;
    eprintln!("no-S3 reboot erase observation: {observed:?}");
    let OwnerEraseOutcome::Completed {
        cold_object_purge_pending,
        ..
    } = observed.receipt
    else {
        panic!("the authorized abandoned-owner erase must complete: {observed:?}");
    };
    assert!(
        observed.versions_after_erase == 0
            || (cold_object_purge_pending
                && observed.canonical_debt.as_deref() == Some(observed.bucket.as_str())),
        "retained S3 versions require a pending receipt and durable bucket-specific debt: {observed:?}",
    );
    Ok(())
}

#[tokio::test]
async fn reboot_with_s3_physically_erases_cited_object_versions() -> TestResult<()> {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return Ok(());
    }
    let observed = reboot_fixture(true).await?;
    assert!(
        matches!(
            observed.receipt,
            OwnerEraseOutcome::Completed {
                cold_object_purge_pending: false,
                ..
            }
        ),
        "configured reboot must finish physical erasure: {observed:?}"
    );
    assert_eq!(observed.versions_after_erase, 0);
    assert!(observed.canonical_debt.is_none());
    Ok(())
}

#[tokio::test]
async fn fresh_database_without_s3_still_erases_database_only_facts() -> TestResult<()> {
    let database = unique_db_name("proxima_no_s3_fresh");
    create_db(&database).await?;
    let result = async {
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let boot = boot(&database, owner, None).await?;
        assert!(boot.blobs.is_none());
        let authz = host_context(owner, AuthPath::HostBearer);
        boot.engine
            .ingest_typed_fact(
                &authz,
                "test/no-s3-fresh",
                &AgentNoteV1 {
                    note_id: Uuid::now_v7(),
                    title: "database-only control".into(),
                    body: "no external object exists".into(),
                    tags: Vec::new(),
                    idempotency_key: None,
                },
            )
            .await?;
        assert_eq!(corpus_counts(boot.pool_for_tests()).await?, (0, 1));
        let system = host_context(owner, AuthPath::System);
        let receipt = boot.engine.erase_group_owner(&system, group).await?;
        assert!(matches!(
            receipt,
            OwnerEraseOutcome::Completed {
                cold_object_purge_pending: false,
                ..
            }
        ));
        assert_eq!(corpus_counts(boot.pool_for_tests()).await?, (0, 0));
        let debt: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.cold_purge_pending")
            .fetch_one(boot.pool_for_tests())
            .await?;
        assert_eq!(debt, 0);
        stop(boot).await;
        Ok(())
    }
    .await;
    drop_db(&database).await?;
    result
}

async fn reboot_fixture(keep_s3: bool) -> TestResult<EraseObservation> {
    let database = unique_db_name("proxima_s3_reboot");
    create_db(&database).await?;
    let result = run_reboot(&database, keep_s3).await;
    drop_db(&database).await?;
    result
}

async fn run_reboot(database: &str, keep_s3: bool) -> TestResult<EraseObservation> {
    let config = S3RuntimeConfig {
        force_path_style: true,
        ..S3RuntimeConfig::from_env()?
    };
    let group = GroupId::new(Uuid::now_v7());
    let owner = OwnerRef::Group(group);
    let initial = boot(database, owner, Some(config.clone())).await?;
    let store = initial
        .blobs
        .as_ref()
        .expect("configured facade exposes blob store");
    let authz = host_context(owner, AuthPath::HostBearer);
    let prepared = store
        .prepare_upload(
            &authz,
            CitedBlobUploadPrepareTs {
                owner,
                filename: "restart-proof.pdf".into(),
                mime: "application/pdf".into(),
                byte_len: BODY.len() as u64,
            },
        )
        .await?;
    // The same public S3 adapter writes the transfer bytes. Completion still
    // runs through the facade's composed engine and authentic admission path.
    store
        .cold_store()
        .put(&format!("pending/{}", prepared.upload_id), BODY)
        .await?;
    let service = CitedBlobService::new(Arc::new(store.clone()));
    let completed = initial
        .engine
        .complete_upload_as_fact(&service, &authz, owner, &prepared.upload_id, &[])
        .await?;
    let canonical = format!("objects/{}", prepared.upload_id);
    let status: (String, Uuid) = sqlx::query_as(
        "SELECT status::text, blob_id FROM proxima_core.blob_uploads WHERE upload_id = $1",
    )
    .bind(Uuid::parse_str(&prepared.upload_id)?)
    .fetch_one(initial.pool_for_tests())
    .await?;
    assert_eq!(
        status,
        (
            "completed".into(),
            Uuid::parse_str(&completed.blob.cited_object_id)?
        )
    );
    assert_eq!(corpus_counts(initial.pool_for_tests()).await?, (1, 1));
    assert_eq!(store.cold_store().get(&canonical).await?, BODY);
    assert_eq!(object_versions(&config, &canonical).await?, 1);
    drop(service);
    stop(initial).await;

    // Reconnect and compose from scratch, without injecting or replacing any
    // storage port. The second boot therefore uses the actual facade wiring.
    let restarted = boot(database, owner, keep_s3.then(|| config.clone())).await?;
    assert_eq!(restarted.blobs.is_some(), keep_s3);
    let system = host_context(owner, AuthPath::System);
    let receipt = restarted.engine.erase_group_owner(&system, group).await?;
    assert_eq!(corpus_counts(restarted.pool_for_tests()).await?, (0, 0));
    let uploads: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.blob_uploads")
        .fetch_one(restarted.pool_for_tests())
        .await?;
    assert_eq!(uploads, 0);
    let canonical_debt: Option<String> = sqlx::query_scalar(
        "SELECT backend FROM proxima_core.cold_purge_pending WHERE object_key = $1",
    )
    .bind(&canonical)
    .fetch_optional(restarted.pool_for_tests())
    .await?;
    let versions_after_erase = object_versions(&config, &canonical).await?;
    stop(restarted).await;

    recover_and_cleanup(
        database,
        owner,
        &config,
        &canonical,
        canonical_debt.as_deref(),
        versions_after_erase,
    )
    .await?;
    Ok(EraseObservation {
        receipt,
        canonical_debt,
        bucket: config.bucket,
        versions_after_erase,
    })
}

async fn recover_and_cleanup(
    database: &str,
    owner: OwnerRef,
    config: &S3RuntimeConfig,
    canonical: &str,
    canonical_debt: Option<&str>,
    versions_after_erase: usize,
) -> TestResult<()> {
    // Observations are already saved. Restore configuration, then exercise
    // the public maintenance retry with this fresh facade's real S3 adapter.
    // This never supplies the no-S3 boot's original erase service.
    let cleanup = boot(database, owner, Some(config.clone())).await?;
    let cold = cleanup.blobs.as_ref().expect("cleanup store").cold_store();
    if versions_after_erase > 0 {
        assert_eq!(
            cold.get(canonical).await?,
            BODY,
            "retained versions contain the original bytes"
        );
    }
    if canonical_debt.is_some() {
        let maintenance = PgStorage::connect(&db_url(database))
            .await?
            .with_cold(Arc::new(cold.clone()));
        let retried = maintenance
            .retry_cold_object_purges(ColdPurgeRetryOptions {
                batch_size: 10,
                dry_run: false,
            })
            .await?;
        assert_eq!(
            (
                retried.selected,
                retried.purged,
                retried.failed,
                retried.remaining
            ),
            (1, 1, 0, 0)
        );
        let debt: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.cold_purge_pending WHERE object_key = $1",
        )
        .bind(canonical)
        .fetch_one(cleanup.pool_for_tests())
        .await?;
        assert_eq!(debt, 0);
        maintenance.pool_for_tests().close().await;
    } else if versions_after_erase > 0 {
        // Only the red baseline can retain bytes with no debt to retry.
        // Clean that exact key after saving the negative observation.
        cold.delete(canonical).await?;
    }
    assert_eq!(object_versions(config, canonical).await?, 0);
    stop(cleanup).await;
    Ok(())
}

async fn boot(
    database: &str,
    owner: OwnerRef,
    s3: Option<S3RuntimeConfig>,
) -> TestResult<EmbeddedProxima> {
    Ok(ProximaBuilder::new(
        EmbedConfig {
            database_url: db_url(database),
            s3,
        },
        owner,
    )
    .boot()
    .await?)
}

async fn stop(boot: EmbeddedProxima) {
    let pool = boot.pool_for_tests().clone();
    boot.engine.stop(boot.handle);
    pool.close().await;
}

fn host_context(owner: OwnerRef, path: AuthPath) -> AuthzContext {
    // Group authority is an explicit trusted-host subject role. A bare
    // single_owner(group) intentionally grants no access.
    AuthzContext::for_subject_with_role(UserId::new(Uuid::now_v7()), [(owner, Role::admin())], path)
        .narrowed_to_owner(owner)
        .expect("trusted host resolved this exact owner")
}

async fn corpus_counts(pool: &sqlx::PgPool) -> TestResult<(i64, i64)> {
    Ok(sqlx::query_as("SELECT (SELECT count(*) FROM proxima_core.blob), (SELECT count(*) FROM proxima_core.memory)")
        .fetch_one(pool).await?)
}

/// Observe all versions and delete markers of this exact key using the same
/// SDK credential chain as the store. This fixture writes one object version.
async fn object_versions(config: &S3RuntimeConfig, key: &str) -> TestResult<usize> {
    let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(Region::new(config.region.clone()));
    if let Some(endpoint) = &config.endpoint_url {
        loader = loader.endpoint_url(endpoint);
    }
    let client = Client::from_conf(
        aws_sdk_s3::config::Builder::from(&loader.load().await)
            .force_path_style(true)
            .build(),
    );
    let page = client
        .list_object_versions()
        .bucket(&config.bucket)
        .prefix(key)
        .send()
        .await?;
    assert_ne!(
        page.is_truncated(),
        Some(true),
        "one-object fixture must fit one page"
    );
    let exact: Vec<_> = page
        .versions()
        .iter()
        .map(|v| (v.key(), v.version_id()))
        .chain(
            page.delete_markers()
                .iter()
                .map(|v| (v.key(), v.version_id())),
        )
        .filter(|(found, _)| *found == Some(key))
        .collect();
    assert!(
        exact
            .iter()
            .all(|(_, version)| version.is_some_and(|v| !v.is_empty())),
        "each object or marker must have an exact version identity"
    );
    Ok(exact.len())
}

#[derive(Debug)]
struct MissingColdObservation {
    missing_id: MemoryId,
    adapter: Result<Vec<u8>, proxima_core::StorageError>,
    hydration: Result<MemoryHydrationOutcome, ProtocolError>,
    missing_bucket: Result<Vec<u8>, proxima_core::StorageError>,
    missing_rows: (i64, i64),
    healthy_status: proxima_core::MemoryHydrationStatus,
    healthy_payload_equal: bool,
}

#[tokio::test]
async fn missing_s3_cold_object_has_a_typed_hydration_outcome() -> TestResult<()> {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return Ok(());
    }
    let database = unique_db_name("proxima_missing_cold");
    create_db(&database).await?;
    let result = observe_missing_cold(&database).await;
    let cleanup = drop_db(&database).await;
    eprintln!("cold hydration observation: {result:?}; database cleanup: {cleanup:?}");
    cleanup?;
    let observed = result?;
    assert_eq!(
        observed.healthy_status,
        proxima_core::MemoryHydrationStatus::Hydrated
    );
    assert!(observed.healthy_payload_equal);
    assert_eq!(observed.missing_rows, (0, 1));
    assert!(matches!(
        observed.missing_bucket,
        Err(proxima_core::StorageError::Unavailable(_))
    ));
    assert!(
        matches!(observed.adapter, Err(proxima_core::StorageError::NotFound)),
        "an absent key in the live configured bucket is NotFound: {observed:?}"
    );
    assert_eq!(
        observed.hydration?,
        MemoryHydrationOutcome::simple(
            observed.missing_id,
            proxima_core::MemoryHydrationStatus::MissingColdObject
        )
    );
    Ok(())
}

async fn observe_missing_cold(database: &str) -> TestResult<MissingColdObservation> {
    let config = S3RuntimeConfig {
        force_path_style: true,
        ..S3RuntimeConfig::from_env()?
    };
    let owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let host = boot(database, owner, Some(config.clone())).await?;
    let cold = host
        .blobs
        .as_ref()
        .ok_or("configured S3 store")?
        .cold_store();
    let mut owned_keys = Vec::new();
    let result = run_missing_cold(&host, owner, &config, &mut owned_keys).await;
    // Hydration currently retains its old object. Clean exactly these fresh
    // admissions, even if the observation failed, without relying on erasure.
    let mut cleanup: TestResult<()> = Ok(());
    for key in &owned_keys {
        let deleted = tokio::time::timeout(std::time::Duration::from_secs(30), cold.delete(key))
            .await
            .map_err(Into::into)
            .and_then(|result| result.map_err(Into::into));
        if deleted.is_err() {
            cleanup = deleted;
        }
    }
    eprintln!("cold hydration exact-key cleanup: {cleanup:?}");
    stop(host).await;
    cleanup?;
    result
}

async fn run_missing_cold(
    host: &EmbeddedProxima,
    owner: OwnerRef,
    config: &S3RuntimeConfig,
    owned_keys: &mut Vec<String>,
) -> TestResult<MissingColdObservation> {
    let authz = host_context(owner, AuthPath::HostBearer);
    let cold = host
        .blobs
        .as_ref()
        .ok_or("configured S3 store")?
        .cold_store();
    let (healthy_id, healthy_note) =
        admit_cold_note(host, &authz, owner, "healthy", owned_keys).await?;
    let (missing_id, _) = admit_cold_note(host, &authz, owner, "missing", owned_keys).await?;
    let missing_key = format!("cold/{}", missing_id.into_inner());
    let healthy = host
        .engine
        .hydrate_memory(&authz, owner, healthy_id)
        .await?;
    let read = host
        .engine
        .get_memory(
            &authz,
            &proxima_core::GetMemoryReadRequest {
                memory_id: healthy_id,
                include_neighbor_edges: false,
            },
        )
        .await?;
    let healthy_payload_equal = read
        .memory
        .as_ref()
        .and_then(|memory| memory.payload.as_ref())
        .and_then(|payload| payload.downcast_ref::<AgentNoteV1>())
        == Some(&healthy_note);
    cold.delete(&missing_key).await?;
    if object_versions(config, &missing_key).await? != 0 {
        return Err("the missing fixture must have no remaining versions or markers".into());
    }
    let adapter = cold.get(&missing_key).await;
    let hydration = host.engine.hydrate_memory(&authz, owner, missing_id).await;
    let missing_rows = sqlx::query_as(
        "SELECT (SELECT count(*) FROM proxima_core.memory WHERE t = $1),
                (SELECT count(*) FROM proxima_core.cooled WHERE t = $1 AND object_key = $2)",
    )
    .bind(missing_id.into_inner())
    .bind(&missing_key)
    .fetch_one(host.pool_for_tests())
    .await?;
    // A missing bucket is a backend/configuration fault, even though its HTTP
    // status is also 404. No bucket is created or deleted for this control.
    let absent_bucket = proxima_blob_s3::CitedBlobStore::new(
        host.pool_for_tests().clone(),
        S3RuntimeConfig {
            bucket: format!("pg-missing-bucket-{}", Uuid::now_v7().simple()),
            ..config.clone()
        },
    )?;
    let missing_bucket = absent_bucket.cold_store().get(&missing_key).await;
    Ok(MissingColdObservation {
        missing_id,
        adapter,
        hydration,
        missing_bucket,
        missing_rows,
        healthy_status: healthy.status,
        healthy_payload_equal,
    })
}

async fn admit_cold_note(
    host: &EmbeddedProxima,
    authz: &AuthzContext,
    owner: OwnerRef,
    label: &str,
    owned_keys: &mut Vec<String>,
) -> TestResult<(MemoryId, AgentNoteV1)> {
    let note = AgentNoteV1 {
        note_id: Uuid::now_v7(),
        title: format!("cold S3 hydration {label}"),
        body: format!("typed payload for the {label} object control"),
        tags: Vec::new(),
        idempotency_key: None,
    };
    let fact = host
        .engine
        .ingest_typed_fact(authz, "test/missing-cold-object", &note)
        .await?;
    let key = format!("cold/{}", fact.memory_id.into_inner());
    owned_keys.push(key.clone());
    host.engine
        .forget_memory(authz, owner, fact.memory_id)
        .await?;
    let bytes = host
        .blobs
        .as_ref()
        .ok_or("configured S3 store")?
        .cold_store()
        .get(&key)
        .await?;
    if !bytes
        .windows(note.body.len())
        .any(|part| part == note.body.as_bytes())
    {
        return Err("actual cold bytes must contain the admitted typed body".into());
    }
    Ok((fact.memory_id, note))
}
