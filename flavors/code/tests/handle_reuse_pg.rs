//! Same file path is one series: later ingest reuses `handle`.

mod common;

use common::{migrated_db, owner_write_permit, test_owner};
use proxima_code::testkit::{build_engine, ingest_file_revision, register_repo};
use proxima_code::{FileRevisionV1, FileState, RepoScope};
use proxima_core::engine::{Engine, FactWrite};
use proxima_core::storage_ports::OwnerTransferPort;
use proxima_core::verbs::fact_ingest::FactIngestOutcome;
use proxima_core::{AccessKind, AuthPath, AuthzContext, EntityId};
use proxima_pg_testkit::drop_db;
use uuid::Uuid;

/// The transfer's registry-resolved legs, over BOTH flavors.
///
/// The code flavor's own projection table is a `Follow` surface, so a
/// core-only registry would leave its rows behind — which is exactly the
/// class the partition exists to refuse, and exactly why the engine builds
/// this from the composed registry.
fn transfer_surfaces() -> proxima_core::owner_inverse::OwnerSurfaces {
    let mut registry = proxima_core::FlavorRegistry::new();
    proxima_code::register(&mut registry).expect("code schema registration");
    proxima_core::owner_inverse::OwnerSurfaces::for_registry(
        &registry.try_freeze().expect("core + code freeze"),
    )
}

fn content_hash(seed: &str) -> [u8; 32] {
    *blake3::hash(seed.as_bytes()).as_bytes()
}

fn file_revision(repo_id: Uuid, file_path: &str, version: &str) -> FileRevisionV1 {
    FileRevisionV1 {
        repo_id,
        file_path: file_path.to_string(),
        language: Some("rust".to_string()),
        content_sha256: content_hash(version),
        size_bytes: u64::try_from(version.len()).unwrap_or(u64::MAX),
        indexed_commit_sha: format!("{version:0<40}"),
        state: FileState::Present,
    }
}

/// Every repo-scoped ingest is fenced on a registered repository, so the
/// fixture has to register one. An unregistered repo id is now a refusal,
/// which is the point of the fence and is pinned in `repo_fence_pg`.
async fn register_fixture_repo(
    pg: &proxima_storage_pg::PgStorage,
    owner: &proxima_core::OwnerRef,
    repo_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    register_repo(
        pg.pool_for_tests(),
        None,
        owner,
        repo_id,
        &format!("/tmp/proxima-handle-reuse-{repo_id}"),
        "handle reuse fixture",
        &RepoScope::default(),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn code_stateful_ingest_mints_after_owner_transfer() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let engine = build_engine(pg.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let permit = owner_write_permit(&owner, AccessKind::Fact).await?;
        let repo_id = Uuid::now_v7();
        register_fixture_repo(&pg, &owner, repo_id).await?;
        let file_path = "src/lib.rs";
        let now = time::OffsetDateTime::now_utc();
        let first = ingest_file_revision(
            &engine,
            &authz,
            &file_revision(repo_id, file_path, "v1"),
            now,
        )
        .await?;
        let destination = proxima_core::OwnerRef::Group(proxima_core::GroupId::new(Uuid::now_v7()));
        let transferred = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(first.memory_id),
                destination,
                &transfer_surfaces(),
                &[],
            )
            .await?;
        assert!(transferred);
        let after = ingest_file_revision(
            &engine,
            &authz,
            &file_revision(repo_id, file_path, "v2"),
            now,
        )
        .await?;
        assert_ne!(
            first.handle, after.handle,
            "a transferred series is a miss for the prior owner"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("code_stateful_ingest_mints_after_owner_transfer failed");
}

/// One `file-revision-v1` write from a fixed source, re-observing a
/// displaced replay when `current` is set.
async fn observe(
    engine: &Engine,
    authz: &AuthzContext,
    payload: &FileRevisionV1,
    current: bool,
) -> Result<FactIngestOutcome, Box<dyn std::error::Error>> {
    let write = FactWrite::new(authz.principal(), "test/reobserve", payload);
    let write = if current {
        write.reobserve_if_displaced()
    } else {
        write
    };
    Ok(engine.ingest_fact(authz, write).await?)
}

async fn head_t(
    pg: &proxima_storage_pg::PgStorage,
    handle: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    Ok(
        sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
            .bind(handle)
            .fetch_one(pg.pool_for_tests())
            .await?,
    )
}

/// A path goes back to a revision the series already holds (a branch
/// checked out again). Replayed, the old Fact stays displaced; re-observed,
/// it heads the series again, as a new Fact under its own replay key, and a
/// retry of that re-observation replays it.
#[tokio::test]
async fn a_displaced_revision_reported_again_heads_its_series() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let engine = build_engine(pg.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let repo_id = Uuid::now_v7();
        register_fixture_repo(&pg, &owner, repo_id).await?;
        let main = file_revision(repo_id, "src/lib.rs", "main");
        let feature = file_revision(repo_id, "src/lib.rs", "feature");

        let first = observe(&engine, &authz, &main, true).await?;
        let branch = observe(&engine, &authz, &feature, true).await?;
        assert_eq!(first.handle, branch.handle, "one path is one series");
        assert_eq!(
            head_t(&pg, first.handle).await?,
            branch.memory_id.into_inner()
        );

        let replayed = observe(&engine, &authz, &main, false).await?;
        assert!(
            replayed.idempotent_replay,
            "without the opt-in a key replays"
        );
        assert_eq!(replayed.memory_id, first.memory_id);
        assert_eq!(
            head_t(&pg, first.handle).await?,
            branch.memory_id.into_inner(),
            "a replay does not move the head"
        );

        let back = observe(&engine, &authz, &main, true).await?;
        assert!(
            !back.idempotent_replay,
            "a displaced revision is admitted again"
        );
        assert_ne!(back.memory_id, first.memory_id);
        assert_eq!(back.handle, first.handle);
        assert_eq!(
            head_t(&pg, first.handle).await?,
            back.memory_id.into_inner()
        );

        let retry = observe(&engine, &authz, &main, true).await?;
        assert!(retry.idempotent_replay, "the head's own revision replays");
        assert_eq!(retry.memory_id, back.memory_id);

        // A second round trip walks the chain one key further each way.
        let branch_again = observe(&engine, &authz, &feature, true).await?;
        assert!(!branch_again.idempotent_replay);
        assert_ne!(branch_again.memory_id, branch.memory_id);
        let back_again = observe(&engine, &authz, &main, true).await?;
        assert!(!back_again.idempotent_replay);
        assert!(![first.memory_id, back.memory_id].contains(&back_again.memory_id));
        assert_eq!(
            head_t(&pg, first.handle).await?,
            back_again.memory_id.into_inner()
        );
        let retry_again = observe(&engine, &authz, &main, true).await?;
        assert!(retry_again.idempotent_replay);
        assert_eq!(retry_again.memory_id, back_again.memory_id);
        let history = observe(&engine, &authz, &feature, false).await?;
        assert!(
            history.idempotent_replay,
            "history still replays its first admission"
        );
        assert_eq!(history.memory_id, branch.memory_id);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("a_displaced_revision_reported_again_heads_its_series failed");
}
