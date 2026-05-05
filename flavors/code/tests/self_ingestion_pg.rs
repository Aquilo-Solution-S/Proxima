#![allow(clippy::doc_markdown, clippy::too_many_lines)]
//! M4.B.5 done-when — self-ingestion against a tmp clone of the
//! Proxima repo.
//!
//! Asserts the ROADMAP.md M4 criterion:
//! * every commit on `main` appears as a Code Fact (commit-v1)
//! * a new commit on the clone surfaces via `Subscribe` within 5s
//! * a follow-up no-op poll emits zero events (idempotency)

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use proxima_code::{LocalGitSource, build_engine, migrator};
use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::storage::Storage;
use proxima_core::verbs::subscribe::SubscribeRequest;
use proxima_core::{
    ChangeEventKind, Cursor, EntityKind, OrgId, Owner, Principal, SchemaId, UserId,
};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection};
use tempfile::TempDir;
use tokio_stream::StreamExt;
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://postgres@localhost/postgres";

async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

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
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
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
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return;
    }
    let url = format!("postgres://postgres@localhost/{db_name}");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        migrator().run(pg.pool()).await?;
        pg.start_outbox().await?;

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let engine = build_engine(
            pg.clone(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        );
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
        let (r1, cursor) = source.run_poll(pg.pool(), &cursor).await?;
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

        // Phase 2 — live streaming. Open Subscribe BEFORE the new commit.
        let sub_req = SubscribeRequest {
            owner: owner.clone(),
            since: None,
        };
        let mut stream = engine.subscribe(&Credentials::None, sub_req).await?;

        // Append a new empty commit on main.
        run(Command::new("git").arg("-C").arg(&clone_path).args([
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "M4 self-ingestion probe",
        ]));
        let main_count_after = count_main_commits(&clone_path);
        assert_eq!(main_count_after, main_count_before + 1);

        let (r2, cursor) = source.run_poll(pg.pool(), &cursor).await?;
        assert_eq!(
            r2.commits_emitted, 1,
            "second poll should emit exactly the one new commit"
        );

        let commit_schema = SchemaId::new("proxima-code/commit-v1".into());
        let mut saw_commit_event = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let Ok(Some(ce)) = tokio::time::timeout(remaining, stream.next()).await else {
                break;
            };
            if let ChangeEventKind::EntityAppend {
                entity_kind,
                schema_id,
                ..
            } = &ce.kind
                && entity_kind == &EntityKind::Fact
                && schema_id == &commit_schema
            {
                saw_commit_event = true;
                break;
            }
        }
        assert!(
            saw_commit_event,
            "expected a commit-v1 EntityAppend on Subscribe within 5s"
        );

        let facts_after_live = count_commit_v1_facts(pg.pool(), &owner, repo_id).await;
        assert_eq!(
            facts_after_live,
            facts_after_initial + 1,
            "live commit should have added exactly one new commit-v1 Fact"
        );

        // Phase 3 — idempotency. Third poll, no new commits.
        let (r3, _cursor) = source.run_poll(pg.pool(), &cursor).await?;
        assert_eq!(r3.commits_emitted, 0);
        assert_eq!(r3.files_present_emitted, 0);
        assert_eq!(r3.files_tombstoned, 0);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("self_ingestion_pg test failed");
}
