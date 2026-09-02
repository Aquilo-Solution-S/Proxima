//! Same file path is one series: later ingest reuses `handle`.

mod common;

use common::{migrated_db, owner_write_permit, test_owner};
use proxima_code::testkit::{build_engine, ingest_commit, ingest_file_revision, register_repo};
use proxima_code::{CommitV1, FileRevisionV1, FileState, RepoScope};
use proxima_core::storage_ports::OwnerTransferPort;
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

fn commit(repo_id: Uuid) -> CommitV1 {
    let now = time::OffsetDateTime::now_utc();
    CommitV1 {
        repo_id,
        sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
        parents: Vec::new(),
        author_name: "Ada".to_string(),
        author_email: "ada@example.test".to_string(),
        author_time: now,
        committer_name: "Ada".to_string(),
        committer_email: "ada@example.test".to_string(),
        committer_time: now,
        message: "initial".to_string(),
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
async fn code_stateful_ingest_reuses_handle() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let engine = build_engine(pg.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
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
        let second = ingest_file_revision(
            &engine,
            &authz,
            &file_revision(repo_id, file_path, "v2"),
            now,
        )
        .await?;
        assert_eq!(first.handle, second.handle, "same path is one series");
        assert_ne!(
            first.memory_id, second.memory_id,
            "new observation is a new t"
        );

        ingest_commit(&engine, &authz, &commit(repo_id), now).await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("code_stateful_ingest_reuses_handle failed");
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
