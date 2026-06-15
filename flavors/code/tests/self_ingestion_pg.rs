#![allow(clippy::doc_markdown, clippy::too_many_lines)]
//! M4.B.5 done-when — self-ingestion against a tmp clone of the
//! Proxima repo.
//!
//! Asserts the ROADMAP.md M4 criterion:
//! * every commit on `main` appears as a Code Fact (commit-v1)
//! * a new commit on the clone surfaces as a new commit-v1 Fact on re-poll
//! * a follow-up no-op poll emits zero events (idempotency)

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use proxima_code::{LocalGitSource, build_engine, migrator};
use proxima_core::storage::Storage;
use proxima_core::{Cursor, OrgId, Owner, Principal, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;
use tempfile::TempDir;
use uuid::Uuid;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn run(cmd: &mut Command) {
    let out = cmd.output().expect("command spawn");
    assert!(
        out.status.success(),
        "{cmd:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_get(cmd: &mut Command) -> String {
    let out = cmd.output().expect("command spawn");
    assert!(
        out.status.success(),
        "{cmd:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

fn clone_workspace_to_tmp(target: &Path) {
    let src = workspace_root();
    run(Command::new("git")
        .arg("clone")
        .arg("--quiet")
        .arg(format!("file://{}", src.display()))
        .arg(target));
    run(Command::new("git")
        .arg("-C")
        .arg(target)
        .args(["config", "user.email", "m4@example.com"]));
    run(Command::new("git")
        .arg("-C")
        .arg(target)
        .args(["config", "user.name", "M4 Probe"]));
    run(Command::new("git")
        .arg("-C")
        .arg(target)
        .args(["config", "commit.gpgsign", "false"]));
    run(Command::new("git")
        .arg("-C")
        .arg(target)
        .args(["checkout", "main"]));
}

fn count_main_commits(repo: &Path) -> usize {
    let s = run_get(Command::new("git").arg("-C").arg(repo).args([
        "rev-list",
        "--count",
        "--first-parent",
        "main",
    ]));
    s.trim().parse().expect("count parse")
}

async fn count_commit_v1_facts(pool: &sqlx::PgPool, owner: &Owner, repo_id: Uuid) -> i64 {
    let kind = proxima_core::OwnerPrincipalKind::of(&owner.principal);
    let principal_id = match &owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    let org_id = owner.org_id.into_inner();
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint \
         FROM proxima_core.memories m \
         JOIN proxima_code.commit_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .fetch_one(pool)
    .await
    .expect("count commit_v1");
    row.0
}

#[tokio::test]
async fn self_ingestion_streams_proxima_main() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        migrator().run(pg.pool()).await?;

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let engine = build_engine(pg.clone());
        let _arc_storage: Arc<dyn Storage> = Arc::new(pg.clone());

        // Clone Proxima itself into a tmpdir.
        let tmp = TempDir::new()?;
        let clone_path = tmp.path().join("clone");
        clone_workspace_to_tmp(&clone_path);

        let main_count_before = count_main_commits(&clone_path);

        // Phase 1 — historical ingestion.
        let repo_id = Uuid::now_v7();
        let source = LocalGitSource::new(repo_id, clone_path.clone(), owner.clone());
        let cursor = Cursor::empty();
        let (r1, cursor) = source.run_poll(pg.pool(), &cursor, &mut |_| {}).await?;
        assert!(
            r1.commits_emitted >= main_count_before,
            "expected ≥{main_count_before} commits emitted, got {}",
            r1.commits_emitted
        );

        let facts_after_initial = count_commit_v1_facts(pg.pool(), &owner, repo_id).await;
        let main_count_i64 =
            i64::try_from(main_count_before).expect("main commit count fits in i64");
        assert!(
            facts_after_initial >= main_count_i64,
            "every commit on main should have a commit-v1 Fact: \
             main_count={main_count_before}, facts={facts_after_initial}"
        );

        // Phase 1.5 — payload round-trip check. Query for commit-v1 facts
        // and verify payload bytes deserialize back to CommitV1.
        let commit_schema = proxima_core::SchemaId::new("proxima-code/commit-v1".into());
        let query_resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                &proxima_core::verbs::query::QueryRequest {
                    principal: owner.principal.clone(),
                    entity_kind: Some(proxima_core::verbs::query::EntityKind::Fact),
                    schema_id: Some(commit_schema.clone()),
                    supersession: proxima_core::verbs::query::SupersessionStatus::IncludeSuperseded,
                    tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
                    personality_roots:
                        proxima_core::verbs::query::PersonalityRootFilter::IncludeInactive,
                    limit: 100,
                    include_payloads: true,
                    memory_ids: Vec::new(),
                    goal_ids: Vec::new(),
                    edge_ids: Vec::new(),
                    stateful_heads: Vec::new(),
                },
            )
            .await?;
        // At least one commit-v1 fact should have a non-empty payload
        let non_empty_payloads: Vec<_> = query_resp
            .memories
            .iter()
            .filter(|m| m.schema_id == commit_schema && !m.payload.is_empty())
            .collect();
        assert!(
            !non_empty_payloads.is_empty(),
            "expected at least one commit-v1 Fact with non-empty payload"
        );
        // Decode the first one and verify it has a non-empty SHA
        let first = &non_empty_payloads[0];
        let commit: proxima_code::payloads::CommitV1 = serde_json::from_slice(&first.payload)
            .map_err(|e| {
                format!(
                    "failed to deserialize commit-v1 payload for memory {:?}: {e}",
                    first.id
                )
            })?;
        assert!(
            !commit.sha.is_empty(),
            "expected non-empty SHA in decoded commit-v1 payload"
        );

        // Phase 1.6 — multi-schema payload dispatch. Query without a
        // schema_id filter so the SQL exercises the CASE-per-schema
        // dispatch end-to-end. Every commit-v1 row in the unfiltered
        // result must still decode to CommitV1; mis-dispatched rows
        // would either be empty or fail to deserialize.
        //
        // High limit so commit-v1 rows aren't drowned out by the much
        // more numerous file-revision-v1 / code-chunk-v1 facts emitted
        // per commit.
        let unfiltered = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                &proxima_core::verbs::query::QueryRequest {
                    principal: owner.principal.clone(),
                    entity_kind: Some(proxima_core::verbs::query::EntityKind::Fact),
                    schema_id: None,
                    supersession: proxima_core::verbs::query::SupersessionStatus::IncludeSuperseded,
                    tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
                    personality_roots:
                        proxima_core::verbs::query::PersonalityRootFilter::IncludeInactive,
                    limit: 100_000,
                    include_payloads: true,
                    memory_ids: Vec::new(),
                    goal_ids: Vec::new(),
                    edge_ids: Vec::new(),
                    stateful_heads: Vec::new(),
                },
            )
            .await?;
        let unfiltered_commits: Vec<_> = unfiltered
            .memories
            .iter()
            .filter(|m| m.schema_id == commit_schema)
            .collect();
        assert!(
            !unfiltered_commits.is_empty(),
            "expected at least one commit-v1 Fact in unfiltered query"
        );
        for m in &unfiltered_commits {
            assert!(
                !m.payload.is_empty(),
                "unfiltered query: commit-v1 row {:?} had empty payload — \
                 likely a CASE-dispatch bug",
                m.id
            );
            let _: proxima_code::payloads::CommitV1 =
                serde_json::from_slice(&m.payload).map_err(|e| {
                    format!(
                        "unfiltered query: commit-v1 payload for {:?} \
                         did not deserialize as CommitV1: {e}",
                        m.id
                    )
                })?;
        }

        // Phase 2 — live append. A new commit on the clone surfaces as a
        // new commit-v1 Fact on the next poll.
        run(Command::new("git").arg("-C").arg(&clone_path).args([
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "M4 self-ingestion probe",
        ]));
        let main_count_after = count_main_commits(&clone_path);
        assert_eq!(main_count_after, main_count_before + 1);

        let (r2, cursor) = source.run_poll(pg.pool(), &cursor, &mut |_| {}).await?;
        assert_eq!(
            r2.commits_emitted, 1,
            "second poll should emit exactly the one new commit"
        );

        let facts_after_live = count_commit_v1_facts(pg.pool(), &owner, repo_id).await;
        assert_eq!(
            facts_after_live,
            facts_after_initial + 1,
            "live commit should have added exactly one new commit-v1 Fact"
        );

        // Phase 3 — idempotency. Third poll, no new commits.
        let (r3, _cursor) = source.run_poll(pg.pool(), &cursor, &mut |_| {}).await?;
        assert_eq!(r3.commits_emitted, 0);
        assert_eq!(r3.files_present_emitted, 0);
        assert_eq!(r3.files_tombstoned, 0);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("self_ingestion_pg test failed");
}
