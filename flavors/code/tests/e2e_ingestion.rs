#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_code::RepoScope;
use std::fmt::Write as _;

mod common;

use common::{git, migrated_db, test_owner, write_file};
use proxima_code::payloads::CODE_LEXICAL_LANGUAGE;
use proxima_code::testkit::{
    advance_stage, begin_run, build_engine, get_active_run, mark_failed, mark_succeeded,
    register_repo, start_run, sweep_orphaned_runs,
};
use proxima_code::{
    CodeChunkV1, CodeFlavorStore, CodeIngestContext, LocalGitSource, RunStage, RunStatus,
    StageCounters,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{AuthPath, AuthzContext, Cursor, MemoryId, Owner};
use proxima_pg_testkit::drop_db;
use proxima_storage_pg::sidecars::{PgMemoryPayload, PgSidecarReadCtx};
use tempfile::TempDir;
use uuid::Uuid;

fn owner_cols(owner: &Owner) -> (proxima_core::OwnerRefKind, Uuid) {
    owner.columns()
}

async fn register_test_repo(pool: &sqlx::PgPool, owner: &Owner, repo_id: Uuid) {
    register_repo(
        pool,
        owner,
        repo_id,
        "/tmp/proxima-e2e",
        "proxima-e2e",
        &RepoScope::default(),
    )
    .await
    .expect("register repo");
}

fn make_tiny_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "e2e@example.com"]);
    git(dir.path(), &["config", "user.name", "E2E"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);

    let mut lib = String::from("pub fn callee() -> i32 {\n    let mut n = 1;\n");
    for i in 0..220 {
        writeln!(&mut lib, "    n += {i};").expect("write string");
    }
    lib.push_str("    n\n}\n\npub fn caller() -> i32 {\n    let mut n = callee();\n");
    for i in 0..220 {
        writeln!(&mut lib, "    n += {i};").expect("write string");
    }
    lib.push_str("    n\n}\n");
    write_file(dir.path(), "src/lib.rs", &lib);
    write_file(dir.path(), "README.md", "same blob\n");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "initial"]);

    write_file(dir.path(), "COPY.md", "same blob\n");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "copy blob"]);
    dir
}

#[tokio::test]
async fn start_run_returns_active_row_on_duplicate() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        register_test_repo(pg.pool_for_tests(), &owner, repo_id).await;

        let r1 = start_run(pg.pool_for_tests(), &owner, repo_id).await?;
        let r2 = start_run(pg.pool_for_tests(), &owner, repo_id).await?;
        assert_eq!(r1.run_id, r2.run_id);
        assert_eq!(r1.status, RunStatus::Queued);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("start_run_returns_active_row_on_duplicate failed");
}

#[tokio::test]
async fn run_transitions_and_failure_persist() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        register_test_repo(pg.pool_for_tests(), &owner, repo_id).await;

        let run = start_run(pg.pool_for_tests(), &owner, repo_id).await?;
        let r2 = advance_stage(
            pg.pool_for_tests(),
            run.run_id,
            RunStage::Facts,
            &StageCounters::zeroed(),
        )
        .await?;
        assert_eq!(r2.status, RunStatus::Running);
        assert_eq!(r2.stage, RunStage::Facts);
        let r3 = mark_succeeded(pg.pool_for_tests(), run.run_id, &StageCounters::zeroed()).await?;
        assert_eq!(r3.status, RunStatus::Succeeded);
        assert!(r3.finished_at.is_some());

        let repo_id2 = Uuid::now_v7();
        register_repo(
            pg.pool_for_tests(),
            &owner,
            repo_id2,
            "/tmp/proxima-e2e-2",
            "repo2",
            &RepoScope::default(),
        )
        .await?;
        let failed = start_run(pg.pool_for_tests(), &owner, repo_id2).await?;
        let failed = mark_failed(pg.pool_for_tests(), failed.run_id, "boom").await?;
        assert_eq!(failed.status, RunStatus::Failed);
        assert_eq!(failed.error_message.as_deref(), Some("boom"));
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("run_transitions_and_failure_persist failed");
}

#[tokio::test]
async fn sweep_retires_orphans_and_unblocks_start_run() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let queued_repo = Uuid::now_v7();
        let running_repo = Uuid::now_v7();
        register_repo(
            pg.pool_for_tests(),
            &owner,
            queued_repo,
            "/tmp/proxima-sweep-q",
            "queued",
            &RepoScope::default(),
        )
        .await?;
        register_repo(
            pg.pool_for_tests(),
            &owner,
            running_repo,
            "/tmp/proxima-sweep-r",
            "running",
            &RepoScope::default(),
        )
        .await?;

        let queued = start_run(pg.pool_for_tests(), &owner, queued_repo).await?;
        assert_eq!(queued.status, RunStatus::Queued);
        let running_seed = start_run(pg.pool_for_tests(), &owner, running_repo).await?;
        let running = begin_run(pg.pool_for_tests(), running_seed.run_id)
            .await?
            .expect("begin_run claims queued row");
        assert_eq!(running.status, RunStatus::Running);

        let swept = sweep_orphaned_runs(pg.pool_for_tests()).await?;
        assert_eq!(swept, 2);

        for repo_id in [queued_repo, running_repo] {
            assert!(
                get_active_run(pg.pool_for_tests(), &owner, repo_id)
                    .await?
                    .is_none(),
                "active run for {repo_id} should be cleared after sweep",
            );
        }

        let (kind, principal_id) = owner_cols(&owner);
        let messages: Vec<(RunStatus, Option<String>)> = sqlx::query_as(
            "SELECT status, error_message \
             FROM proxima_code.repo_ingestion_runs \
             WHERE owner_kind = $1 AND owner_id = $2 \
             ORDER BY started_at ASC",
        )
        .bind(kind)
        .bind(principal_id)
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert_eq!(messages.len(), 2);
        for (status, msg) in &messages {
            assert_eq!(*status, RunStatus::Failed);
            assert_eq!(msg.as_deref(), Some("abandoned by process restart"));
        }

        // Sweep is idempotent — second call retires nothing.
        assert_eq!(sweep_orphaned_runs(pg.pool_for_tests()).await?, 0);

        // The partial unique index admits a fresh run.
        let fresh = start_run(pg.pool_for_tests(), &owner, queued_repo).await?;
        assert_eq!(fresh.status, RunStatus::Queued);
        assert_ne!(fresh.run_id, queued.run_id);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("sweep_retires_orphans_and_unblocks_start_run failed");
}

#[tokio::test]
async fn local_ingestion_lands_facts_citations_edges_and_replays_idempotently() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo = make_tiny_repo();
        let repo_id = Uuid::now_v7();
        let path = repo.path().to_string_lossy().into_owned();
        register_repo(
            pg.pool_for_tests(),
            &owner,
            repo_id,
            &path,
            "fixture",
            &RepoScope::default(),
        )
        .await?;
        let engine = build_engine(pg.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
        let ingest_ctx = CodeIngestContext::new(&engine, &authz, &store);

        let source = LocalGitSource::new(repo_id, repo.path().to_path_buf(), owner);
        let (report, cursor) = source
            .run_poll(&ingest_ctx, &Cursor::empty(), &mut |_| {})
            .await?;
        assert_eq!(report.commits_emitted, 2);

        let facts: (i64, i64, i64) = sqlx::query_as(
            "SELECT \
                (SELECT COUNT(*)::bigint FROM proxima_code.commit_v1 WHERE repo_id = $1), \
                (SELECT COUNT(*)::bigint FROM proxima_code.file_revision_v1 WHERE repo_id = $1), \
                (SELECT COUNT(*)::bigint FROM proxima_code.code_chunk_v1 WHERE repo_id = $1)",
        )
        .bind(repo_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(facts.0 > 0, "expected commit facts");
        assert!(facts.1 > 0, "expected file facts");
        assert!(facts.2 > 0, "expected chunk facts");

        // The chunk schema's contract PINS its configuration, so the
        // ingest draft names no language at all and the row is still
        // stamped: the pin is a literal inside the generated projection
        // statement, not a value the write path carries and could forget.
        let chunk_languages: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT p.lexical_language::text
               FROM proxima_code.projection p
               JOIN proxima_code.code_chunk_v1 c ON c.t = p.memory_id
              WHERE c.repo_id = $1",
        )
        .bind(repo_id)
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert_eq!(
            chunk_languages,
            vec![CODE_LEXICAL_LANGUAGE.to_string()],
            "a pinned schema stamps its declared configuration on every row"
        );

        let (call_pairs, call_sites): (i64, i64) = sqlx::query_as(
            "SELECT \
                (SELECT COUNT(DISTINCT (cc.caller_memory_id, cc.callee_memory_id))::bigint \
                   FROM proxima_code.code_chunk_call_v1 cc \
                   JOIN proxima_code.code_chunk_v1 src ON src.t = cc.caller_memory_id \
                  WHERE src.repo_id = $1), \
                (SELECT COUNT(*)::bigint \
                   FROM proxima_code.code_chunk_call_v1 cc \
                   JOIN proxima_code.code_chunk_v1 src ON src.t = cc.caller_memory_id \
                  WHERE src.repo_id = $1)",
        )
        .bind(repo_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(call_pairs > 0, "expected call sites in chunk payloads");
        assert!(
            report.call_references_emitted > 0,
            "the poll that produced those pairs must have counted them; \
             the counter is what `ingest_head_snapshot` reports to its caller"
        );
        assert!(
            call_sites >= call_pairs,
            "sites live in the payload, so there are at least as many as pairs"
        );

        // Read the caller chunk back through the payload surface: the calls
        // must come back with it, because they ARE the payload now. This is
        // the half a raw row count cannot see.
        let caller_id: Uuid = sqlx::query_scalar(
            "SELECT DISTINCT cc.caller_memory_id
               FROM proxima_code.code_chunk_call_v1 cc
               JOIN proxima_code.code_chunk_v1 src ON src.t = cc.caller_memory_id
              WHERE src.repo_id = $1
              LIMIT 1",
        )
        .bind(repo_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        let loaded = <CodeChunkV1 as PgMemoryPayload>::load_batch(
            PgSidecarReadCtx::from(pg.pool_for_tests()),
            PayloadKind::Abstraction,
            &[MemoryId::new(caller_id)],
        )
        .await?;
        let (_, payload) = loaded.into_iter().next().expect("caller chunk payload");
        let payload = payload
            .downcast_ref::<CodeChunkV1>()
            .expect("code-chunk payload");
        assert!(
            !payload.calls.is_empty(),
            "a caller chunk's payload carries its callees"
        );
        for call in &payload.calls {
            assert!(!call.sites.is_empty(), "a callee entry carries its sites");
        }
        // Call graph is sidecar-local: one payload callee, one index row.
        let index_rows: i64 = sqlx::query_scalar(
            "SELECT count(DISTINCT callee_memory_id)::bigint
               FROM proxima_code.code_chunk_call_v1
              WHERE caller_memory_id = $1",
        )
        .bind(caller_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            index_rows,
            i64::try_from(payload.calls.len()).expect("fits")
        );

        let cursor_before: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT last_cursor FROM proxima_code.repos WHERE repo_id = $1")
                .bind(repo_id)
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert!(cursor_before.is_none());
        proxima_code::testkit::update_cursor(
            pg.pool_for_tests(),
            &owner,
            repo_id,
            cursor.as_bytes(),
            time::OffsetDateTime::now_utc(),
        )
        .await?;
        let cursor_after: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT last_cursor FROM proxima_code.repos WHERE repo_id = $1")
                .bind(repo_id)
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert!(cursor_after.is_some());

        let (report2, _cursor2) = source.run_poll(&ingest_ctx, &cursor, &mut |_| {}).await?;
        assert_eq!(report2.commits_emitted, 0);
        let facts_after: (i64, i64, i64) = sqlx::query_as(
            "SELECT \
                (SELECT COUNT(*)::bigint FROM proxima_code.commit_v1 WHERE repo_id = $1), \
                (SELECT COUNT(*)::bigint FROM proxima_code.file_revision_v1 WHERE repo_id = $1), \
                (SELECT COUNT(*)::bigint FROM proxima_code.code_chunk_v1 WHERE repo_id = $1)",
        )
        .bind(repo_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(facts, facts_after);

        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("local_ingestion_lands_facts_citations_edges_and_replays_idempotently failed");
}

#[tokio::test]
async fn limited_local_ingestion_advances_one_commit_per_poll() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo = make_tiny_repo();
        let repo_id = Uuid::now_v7();
        let source = LocalGitSource::new(repo_id, repo.path().to_path_buf(), owner);
        let engine = build_engine(pg.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
        let ingest_ctx = CodeIngestContext::new(&engine, &authz, &store);

        let mut seen = Vec::new();
        let (first, cursor) = source
            .run_poll_limited(&ingest_ctx, &Cursor::empty(), Some(1), &mut |p| {
                seen.push((p.commit_index, p.total_commits));
            })
            .await?;
        assert_eq!(first.commits_emitted, 1);
        assert_eq!(seen, vec![(0, 1)]);

        let commits_after_first: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM proxima_code.commit_v1 WHERE repo_id = $1",
        )
        .bind(repo_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(commits_after_first, 1);

        let (second, cursor) = source
            .run_poll_limited(&ingest_ctx, &cursor, Some(1), &mut |_| {})
            .await?;
        assert_eq!(second.commits_emitted, 1);

        let commits_after_second: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM proxima_code.commit_v1 WHERE repo_id = $1",
        )
        .bind(repo_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(commits_after_second, 2);

        let (third, _cursor) = source
            .run_poll_limited(&ingest_ctx, &cursor, Some(1), &mut |_| {})
            .await?;
        assert_eq!(third.commits_emitted, 0);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("limited_local_ingestion_advances_one_commit_per_poll failed");
}
