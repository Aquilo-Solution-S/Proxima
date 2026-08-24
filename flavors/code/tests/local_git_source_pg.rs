#![allow(clippy::doc_markdown, clippy::too_many_lines)]
//! Full `LocalGitSource` lifecycle against a real Postgres + a fixture git repo
//! on disk.
//!
//! Covers:
//! 1. After initial index: heads-only Query returns chunks for every
//!    Present file.
//! 2. After mutation + reindex: head returns new chunk text; old derived
//!    code-slice Abstraction still queryable as history.
//! 3. After delete + reindex: head returns Tombstone state for that
//!    file's revisions and chunks.
//! 4. After rename + reindex: old path Tombstones, new path Present.
//! 5. Polyglot file (Markdown): file-revision-v1 head Present; chunk
//!    output is fallback (chunk_type="file"), still indexed.

mod common;

use common::{git, migrated_db, test_owner, write_file};
use proxima_code::chunker::MAX_BLOB_BYTES;
use proxima_code::testkit::build_engine;
use proxima_code::{
    CodeChunkV1, CodeFlavorStore, CodeIngestContext, FileRevisionV1, FileState, LocalGitSource,
};
use proxima_core::verbs::query::{QueryRequest, SupersessionStatus};
use proxima_core::{
    AbstractionPayload, AuthPath, AuthzContext, Cursor, FactPayload, Owner, SchemaId, SchemaVersion,
};
use proxima_pg_testkit::drop_db;
use sqlx::Row;
use tempfile::TempDir;
use uuid::Uuid;

fn fixture_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);

    write_file(
        dir.path(),
        "src/lib.rs",
        "pub fn hello() -> &'static str {\n    \"hello\"\n}\n",
    );
    write_file(
        dir.path(),
        "src/main.ts",
        "export function greet(): string {\n  return \"hi\";\n}\n",
    );
    write_file(dir.path(), "README.md", "# Fixture\n\nHello.\n");
    write_file(dir.path(), "src/oversized.rs", "pub fn before() {}\n");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "initial"]);

    dir
}

async fn count_present_chunks(pool: &sqlx::PgPool, owner: &Owner, repo_id: Uuid) -> i64 {
    let owner_id = owner.stored_owner_id();
    let row = sqlx::query(
        "SELECT COUNT(*)::bigint AS c \
         FROM proxima_core.memory_head h \
         JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t \
         JOIN proxima_code.code_chunk_v1 s ON s.t = m.t \
         WHERE m.owner_id = $1 \
           AND s.repo_id = $2 \
           AND s.state = 'Present'",
    )
    .bind(owner_id)
    .bind(repo_id)
    .fetch_one(pool)
    .await
    .expect("count present chunks");
    row.try_get::<i64, _>("c").expect("count column")
}

async fn count_present_chunks_for_path(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
    file_path: &str,
) -> i64 {
    let owner_id = owner.stored_owner_id();
    let row = sqlx::query(
        "SELECT COUNT(*)::bigint AS c \
         FROM proxima_core.memory_head h \
         JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t \
         JOIN proxima_code.code_chunk_v1 s ON s.t = m.t \
         WHERE m.owner_id = $1 \
           AND s.repo_id = $2 \
           AND s.file_path = $3 \
           AND s.state = 'Present'",
    )
    .bind(owner_id)
    .bind(repo_id)
    .bind(file_path)
    .fetch_one(pool)
    .await
    .expect("count present chunks for path");
    row.try_get::<i64, _>("c").expect("count column")
}

async fn fetch_file_revision_state(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
    file_path: &str,
) -> Option<FileState> {
    let owner_id = owner.stored_owner_id();
    let row: Option<(FileState,)> = sqlx::query_as(
        "SELECT s.state \
         FROM proxima_core.memory_head h \
         JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t \
         JOIN proxima_code.file_revision_v1 s ON s.t = m.t \
         WHERE m.owner_id = $1 \
           AND s.repo_id = $2 \
           AND s.file_path = $3",
    )
    .bind(owner_id)
    .bind(repo_id)
    .bind(file_path)
    .fetch_optional(pool)
    .await
    .expect("fetch state");
    row.map(|(s,)| s)
}

#[tokio::test]
async fn local_git_source_full_cycle() {
    let (db_name, pg) = migrated_db().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();

        let engine = build_engine(pg.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
        let ingest_ctx = CodeIngestContext::new(&engine, &authz, &store);

        // Initial index.
        let repo = fixture_repo();
        let repo_id = Uuid::now_v7();
        let source = LocalGitSource::new(repo_id, repo.path().to_path_buf(), owner);
        let cursor = proxima_core::Cursor::empty();
        let (r1, cursor) = source.run_poll(&ingest_ctx, &cursor, &mut |_| {}).await?;
        assert!(r1.commits_emitted >= 1, "expected at least one commit");
        assert!(r1.files_present_emitted >= 3, "expected ≥3 file-revisions");
        assert!(r1.chunks_emitted >= 3, "expected ≥3 chunks");
        let chunks_after_initial = count_present_chunks(pg.pool_for_tests(), &owner, repo_id).await;
        assert!(chunks_after_initial >= 3);

        // Provenance proof — code chunks are derived code-slice
        // Abstractions with `origin` index rows back to their parent
        // `file-revision-v1` Facts.
        let linkage: (i64, i64) = sqlx::query_as(
            "WITH chunks AS ( \
                 SELECT ch.t, m.origins \
                 FROM proxima_code.code_chunk_v1 ch \
                 JOIN proxima_core.memory m ON m.t = ch.t \
                 WHERE ch.repo_id = $1 AND ch.file_path = 'src/lib.rs' \
                   AND ch.state = 'Present' \
                   AND m.kind = 'abstraction' \
             ) \
             SELECT \
                 (SELECT COUNT(*)::bigint FROM chunks), \
                 (SELECT COUNT(*)::bigint FROM chunks c \
                  WHERE EXISTS ( \
                      SELECT 1 FROM proxima_code.file_revision_v1 fr \
                      WHERE fr.repo_id = $1 \
                        AND fr.file_path = 'src/lib.rs' \
                        AND fr.t = ANY(c.origins)))",
        )
        .bind(repo_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(linkage.0 > 0, "expected at least one chunk for src/lib.rs");
        assert_eq!(
            linkage.0, linkage.1,
            "every derived chunk must have file-revision provenance — got {} chunks, {} linked",
            linkage.0, linkage.1,
        );

        // Heads-only chunk Query through the Engine — the path that
        // matters for downstream consumers.
        let q = QueryRequest {
            owner,
            read_owners: vec![owner],
            entity_kind: None,
            schema_id: Some(SchemaId::new(
                <CodeChunkV1 as AbstractionPayload>::SCHEMA_ID.into(),
            )),
            supersession: SupersessionStatus::HeadsOnly,
            goal_state: None,
            assignment: None,
            evidence_contains: None,
            limit: 1000,
            page: proxima_core::verbs::query::QueryPage::default(),
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
        };
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(
                    &owner,
                    proxima_core::AuthPath::HostBearer,
                ),
                &q,
            )
            .await?;
        assert_eq!(
            i64::try_from(resp.memories.len()).unwrap(),
            chunks_after_initial,
            "heads-only chunk Query must agree with raw NK count"
        );

        // ----------------------------------------------------------------
        // Mutate src/lib.rs and reindex.
        write_file(
            repo.path(),
            "src/lib.rs",
            "pub fn hello() -> &'static str {\n    \"hello, world\"\n}\n\n\
             pub fn goodbye() -> &'static str {\n    \"bye\"\n}\n",
        );
        git(repo.path(), &["add", "src/lib.rs"]);
        git(repo.path(), &["commit", "-q", "-m", "expand lib"]);

        let (r2, cursor) = source.run_poll(&ingest_ctx, &cursor, &mut |_| {}).await?;
        // One new commit ("expand lib") → one batch with one
        // file-revision Fact (src/lib.rs) and its chunks. README.md
        // and main.ts aren't in this commit's diff, so they aren't
        // re-emitted.
        assert_eq!(r2.files_present_emitted, 1);

        // src/lib.rs head must now have new content.
        let row: (Vec<u8>,) = sqlx::query_as(
            "SELECT s.content_sha256 \
             FROM proxima_core.memory_head h \
             JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t \
             JOIN proxima_code.file_revision_v1 s ON s.t = m.t \
             WHERE s.repo_id = $1 AND s.file_path = 'src/lib.rs'",
        )
        .bind(repo_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        let new_hash = blake3::hash(
            "pub fn hello() -> &'static str {\n    \"hello, world\"\n}\n\n\
             pub fn goodbye() -> &'static str {\n    \"bye\"\n}\n"
                .as_bytes(),
        );
        assert_eq!(
            row.0,
            new_hash.as_bytes(),
            "lib.rs head must reflect new content"
        );

        // Old revision still exists as history (IncludeSuperseded view).
        let q_all = QueryRequest {
            owner,
            read_owners: vec![owner],
            entity_kind: None,
            schema_id: Some(SchemaId::new(FileRevisionV1::SCHEMA_ID.into())),
            supersession: SupersessionStatus::IncludeSuperseded,
            goal_state: None,
            assignment: None,
            evidence_contains: None,
            limit: 1000,
            page: proxima_core::verbs::query::QueryPage::default(),
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
        };
        let resp_all = engine
            .query(
                &proxima_core::AuthzContext::single_owner(
                    &owner,
                    proxima_core::AuthPath::HostBearer,
                ),
                &q_all,
            )
            .await?;
        // 3 initial revisions + 1 mutation = 4
        assert!(
            resp_all.memories.len() >= 4,
            "history should retain old revisions; got {}",
            resp_all.memories.len()
        );

        // ----------------------------------------------------------------
        // Grow an already-indexed file past the blob cap.
        // It should tombstone the prior chunks instead of leaving stale
        // Present heads behind.
        std::fs::write(
            repo.path().join("src/oversized.rs"),
            vec![b'x'; MAX_BLOB_BYTES + 1],
        )?;
        git(repo.path(), &["add", "src/oversized.rs"]);
        git(repo.path(), &["commit", "-q", "-m", "oversize file"]);

        let (r_big, cursor) = source.run_poll(&ingest_ctx, &cursor, &mut |_| {}).await?;
        assert!(
            r_big.files_tombstoned >= 1,
            "expected oversized file to tombstone prior head"
        );
        let oversized_state =
            fetch_file_revision_state(pg.pool_for_tests(), &owner, repo_id, "src/oversized.rs")
                .await;
        assert_eq!(oversized_state, Some(FileState::Tombstone));
        let oversized_chunks =
            count_present_chunks_for_path(pg.pool_for_tests(), &owner, repo_id, "src/oversized.rs")
                .await;
        assert_eq!(
            oversized_chunks, 0,
            "oversized file must not leave stale present chunks"
        );

        // ----------------------------------------------------------------
        // Delete src/main.ts and reindex.
        std::fs::remove_file(repo.path().join("src/main.ts"))?;
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "drop main.ts"]);

        let (r3, cursor) = source.run_poll(&ingest_ctx, &cursor, &mut |_| {}).await?;
        assert!(r3.files_tombstoned >= 1, "expected tombstone for main.ts");
        let main_state =
            fetch_file_revision_state(pg.pool_for_tests(), &owner, repo_id, "src/main.ts").await;
        assert_eq!(main_state, Some(FileState::Tombstone));

        // ----------------------------------------------------------------
        // Rename README.md → docs/README.md and reindex.
        std::fs::create_dir_all(repo.path().join("docs"))?;
        std::fs::rename(
            repo.path().join("README.md"),
            repo.path().join("docs/README.md"),
        )?;
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "rename README"]);

        let (_r4, cursor) = source.run_poll(&ingest_ctx, &cursor, &mut |_| {}).await?;
        let old_state =
            fetch_file_revision_state(pg.pool_for_tests(), &owner, repo_id, "README.md").await;
        let new_state =
            fetch_file_revision_state(pg.pool_for_tests(), &owner, repo_id, "docs/README.md").await;
        assert_eq!(old_state, Some(FileState::Tombstone));
        assert_eq!(new_state, Some(FileState::Present));

        // ----------------------------------------------------------------
        // Markdown is polyglot: present revision, fallback chunks.
        // (The renamed docs/README.md proves this.)
        let md_chunk_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::bigint FROM proxima_code.code_chunk_v1 s \
             WHERE s.repo_id = $1 AND s.file_path = 'docs/README.md' \
               AND s.state = 'Present'",
        )
        .bind(repo_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(
            md_chunk_count.0 >= 1,
            "markdown should still be chunked via fallback"
        );

        // ----------------------------------------------------------------
        // Re-running index without changes must be idempotent (no new
        // emissions, all unchanged).
        let (r_idem, _cursor) = source.run_poll(&ingest_ctx, &cursor, &mut |_| {}).await?;
        assert_eq!(
            r_idem.files_present_emitted, 0,
            "idempotent reindex emitted files"
        );
        assert_eq!(
            r_idem.chunks_emitted, 0,
            "idempotent reindex emitted chunks"
        );
        assert_eq!(
            r_idem.files_tombstoned, 0,
            "idempotent reindex tombstoned files"
        );

        // schema_version sanity.
        assert_eq!(FileRevisionV1::SCHEMA_VERSION, 1);

        // Suppress unused-binding lint (kept for readability).
        let _ = SchemaVersion::new(1);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("local_git_source_full_cycle failed");
}

#[tokio::test]
async fn head_snapshot_repeated_after_change_and_delete_is_idempotent() {
    let (db_name, pg) = migrated_db().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let engine = build_engine(pg.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
        let ingest_ctx = CodeIngestContext::new(&engine, &authz, &store);

        let repo = fixture_repo();
        let repo_id = Uuid::now_v7();
        let source = LocalGitSource::new(repo_id, repo.path().to_path_buf(), owner);

        let initial = source
            .run_head_snapshot(&ingest_ctx, &Cursor::empty())
            .await?;
        assert!(
            initial.report.files_present_emitted >= 3,
            "initial snapshot should index present files"
        );

        write_file(
            repo.path(),
            "src/lib.rs",
            "pub fn hello() -> &'static str {\n    \"snapshot-v2\"\n}\n",
        );
        git(repo.path(), &["add", "src/lib.rs"]);
        git(repo.path(), &["commit", "-q", "-m", "snapshot lib v2"]);

        let changed = source
            .run_head_snapshot(&ingest_ctx, &initial.cursor)
            .await?;
        assert_eq!(changed.report.files_present_emitted, 1);
        assert!(changed.report.chunks_emitted >= 1);

        let unchanged = source
            .run_head_snapshot(&ingest_ctx, &changed.cursor)
            .await?;
        assert_eq!(unchanged.report.files_present_emitted, 0);
        assert_eq!(unchanged.report.files_tombstoned, 0);
        assert_eq!(unchanged.report.chunks_emitted, 0);
        assert_eq!(unchanged.report.chunks_tombstoned, 0);

        std::fs::remove_file(repo.path().join("src/main.ts"))?;
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "snapshot delete main"]);

        let deleted = source
            .run_head_snapshot(&ingest_ctx, &unchanged.cursor)
            .await?;
        assert_eq!(deleted.report.files_tombstoned, 1);
        assert!(deleted.report.chunks_tombstoned >= 1);

        let unchanged_after_delete = source
            .run_head_snapshot(&ingest_ctx, &deleted.cursor)
            .await?;
        assert_eq!(unchanged_after_delete.report.files_present_emitted, 0);
        assert_eq!(unchanged_after_delete.report.files_tombstoned, 0);
        assert_eq!(unchanged_after_delete.report.chunks_emitted, 0);
        assert_eq!(unchanged_after_delete.report.chunks_tombstoned, 0);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("head_snapshot_repeated_after_change_and_delete_is_idempotent failed");
}

/// A heavily-churned file can hold more live series (distinct
/// `(repo, path, index)` handles) than one authorized-read batch
/// (`MAX_AUTHZ_CANDIDATES` = 2,000). `owned_chunk_series_heads` lists every
/// owned head; it must not truncate.
#[tokio::test]
async fn head_snapshot_delete_tombstones_all_indexes_beyond_one_authz_batch() {
    const EXTRA_PRESENT_ROWS: i32 = 2_050; // > MAX_AUTHZ_CANDIDATES = 2_000
    const SEED_INDEX_BASE: i32 = 10_000; // clear of any real chunk index

    let (db_name, pg) = migrated_db().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let engine = build_engine(pg.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
        let ingest_ctx = CodeIngestContext::new(&engine, &authz, &store);

        let dir = TempDir::new()?;
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "t@t"]);
        git(dir.path(), &["config", "user.name", "T"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        write_file(dir.path(), "hot.rs", "pub fn hot_v1() {}\n");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);

        let repo_id = Uuid::now_v7();
        let source = LocalGitSource::new(repo_id, dir.path().to_path_buf(), owner);
        let initial = source
            .run_head_snapshot(&ingest_ctx, &Cursor::empty())
            .await?;
        assert_eq!(initial.report.files_present_emitted, 1);
        let real_chunks = initial.report.chunks_emitted;
        assert!(real_chunks >= 1, "fixture file must produce chunks");

        // Simulate a long churn history: seed Present-state chunk rows at
        // distinct indexes (distinct natural keys, so every one is a live
        // head the deletion pass must tombstone). Deterministic memory ids
        // let the three FK-ordered inserts (source_batches -> memories ->
        // code_chunk_v1) share the same id set without a temp table.
        let owner_id = owner.stored_owner_id();
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
        )
        .bind(owner_id)
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             SELECT ('7a5b0000-0000-4000-8000-' || lpad(to_hex(g.i), 12, '0'))::uuid,
                    'abstraction', $1, $2,
                    ('7a5b0000-0000-4000-8000-' || lpad(to_hex(g.i), 12, '0'))::uuid
               FROM generate_series($3::int, $4::int) AS g(i)",
        )
        .bind(<CodeChunkV1 as AbstractionPayload>::SCHEMA_ID)
        .bind(owner_id)
        .bind(SEED_INDEX_BASE)
        .bind(SEED_INDEX_BASE + EXTRA_PRESENT_ROWS - 1)
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
             SELECT $1, $4,
                    decode(lpad(to_hex(g.i), 64, '0'), 'hex')
               FROM generate_series($2::int, $3::int) AS g(i)",
        )
        .bind(owner_id)
        .bind(SEED_INDEX_BASE)
        .bind(SEED_INDEX_BASE + EXTRA_PRESENT_ROWS - 1)
        .bind(<CodeChunkV1 as AbstractionPayload>::SCHEMA_ID)
        .execute(pg.pool_for_tests())
        .await?;
        let fact_handle = Uuid::now_v7();
        let fact_t = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/test-fact-v1', $2, $3)",
        )
        .bind(fact_handle)
        .bind(owner_id)
        .bind(fact_t)
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
             VALUES ($1, $2, 'fact', $3, 'core/test-fact-v1')",
        )
        .bind(fact_handle)
        .bind(fact_t)
        .bind(owner_id)
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory
                (handle, t, kind, owner_id, schema_id, origins, content_id,
                 sidecar_tables)
             SELECT ('7a5b0000-0000-4000-8000-' || lpad(to_hex(g.i), 12, '0'))::uuid,
                    ('7a5b0000-0000-4000-8000-' || lpad(to_hex(g.i), 12, '0'))::uuid,
                    'abstraction', $1, $4, ARRAY[$5]::uuid[], c.content_id,
                    ARRAY['proxima_code.code_chunk_v1']
               FROM generate_series($2::int, $3::int) AS g(i)
               JOIN proxima_core.content c
                 ON c.owner_id = $1
                AND c.schema_id = $4
                AND c.content_hash = decode(lpad(to_hex(g.i), 64, '0'), 'hex')",
        )
        .bind(owner_id)
        .bind(SEED_INDEX_BASE)
        .bind(SEED_INDEX_BASE + EXTRA_PRESENT_ROWS - 1)
        .bind(<CodeChunkV1 as AbstractionPayload>::SCHEMA_ID)
        .bind(fact_t)
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_code.code_chunk_v1
                (t, repo_id, file_path, chunk_index, text, language, chunk_type,
                 byte_range_start, byte_range_end, line_range_start, line_range_end, state)
             SELECT ('7a5b0000-0000-4000-8000-' || lpad(to_hex(g.i), 12, '0'))::uuid,
                    $1, 'hot.rs', g.i, 'churn seed', 'rust', 'block', 0, 4, 1, 1, 'Present'
               FROM generate_series($2::int, $3::int) AS g(i)",
        )
        .bind(repo_id)
        .bind(SEED_INDEX_BASE)
        .bind(SEED_INDEX_BASE + EXTRA_PRESENT_ROWS - 1)
        .execute(pg.pool_for_tests())
        .await?;

        std::fs::remove_file(dir.path().join("hot.rs"))?;
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "delete hot.rs"]);

        let deleted = source
            .run_head_snapshot(&ingest_ctx, &initial.cursor)
            .await?;
        assert_eq!(deleted.report.files_tombstoned, 1);
        assert_eq!(
            deleted.report.chunks_tombstoned,
            real_chunks + usize::try_from(EXTRA_PRESENT_ROWS)?,
            "every live chunk index must be tombstoned, including those beyond \
             one 2,000-candidate authorized-read batch"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("head_snapshot_delete_tombstones_all_indexes_beyond_one_authz_batch failed");
}

#[tokio::test]
async fn polyglot_markdown_emits_file_revision_and_fallback_chunks() {
    // Subset of the above: tighter assertion on FileState::Present
    // for a markdown-only fixture.
    let (db_name, pg) = migrated_db().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let engine = build_engine(pg.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
        let ingest_ctx = CodeIngestContext::new(&engine, &authz, &store);

        let dir = TempDir::new()?;
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "t@t"]);
        git(dir.path(), &["config", "user.name", "T"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        write_file(dir.path(), "doc.md", "# Doc\n\nText.\n");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);

        let repo_id = Uuid::now_v7();
        let source = LocalGitSource::new(repo_id, dir.path().to_path_buf(), owner);
        let cursor = proxima_core::Cursor::empty();
        let (report, _cursor) = source.run_poll(&ingest_ctx, &cursor, &mut |_| {}).await?;
        assert!(report.files_present_emitted >= 1);

        let state = fetch_file_revision_state(pg.pool_for_tests(), &owner, repo_id, "doc.md").await;
        assert_eq!(state, Some(FileState::Present));

        // Markdown lacks a tree-sitter grammar in our deps → fallback
        // chunker; chunk_type = "file".
        let row: (String,) = sqlx::query_as(
            "SELECT s.chunk_type \
             FROM proxima_code.code_chunk_v1 s \
             WHERE s.repo_id = $1 AND s.file_path = 'doc.md' \
             LIMIT 1",
        )
        .bind(repo_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(row.0, "file");

        // FileState enum round-trip sanity.
        let _ = FileState::Present;

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("polyglot_markdown_... failed");
}
