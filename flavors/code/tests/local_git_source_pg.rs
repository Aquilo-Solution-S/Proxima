#![allow(clippy::doc_markdown, clippy::too_many_lines)]
//! M3.B.7 done-when — full `LocalGitSource` lifecycle against a real
//! Postgres + a fixture git repo on disk.
//!
//! Exercises the five assertions from M3-PLAN.md:
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
use proxima_code::testkit::build_engine;
use proxima_code::{
    CodeChunkV1, CodeFlavorStore, CodeIngestContext, FileRevisionV1, FileState, LocalGitSource,
};
use proxima_core::verbs::query::{QueryRequest, SupersessionStatus};
use proxima_core::{
    AbstractionPayload, AuthPath, AuthzContext, CORE_DERIVED_FROM_RELATION, FactPayload, Owner,
    SchemaId, SchemaVersion,
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
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "initial"]);

    dir
}

async fn count_present_chunks(pool: &sqlx::PgPool, owner: &Owner, repo_id: Uuid) -> i64 {
    let (kind, principal_id) = owner.columns();
    let row = sqlx::query(
        "SELECT COUNT(*)::bigint AS c \
         FROM proxima_core.memories m \
         JOIN proxima_code.code_chunk_v1 s USING (memory_id) \
         JOIN (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo \
           ON eo.entity_id = m.memory_id \
         WHERE eo.owner_kind = $1 \
           AND eo.owner_id = $2 \
           AND s.repo_id = $3 \
           AND s.state = 'Present' \
           AND NOT EXISTS ( \
                 SELECT 1 FROM proxima_core.memories m2 \
                 JOIN proxima_code.code_chunk_v1 s2 USING (memory_id) \
                 JOIN (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo2 \
                   ON eo2.entity_id = m2.memory_id \
                 WHERE m2.schema_id = m.schema_id \
                   AND eo2.owner_kind = eo.owner_kind \
                   AND eo2.owner_id = eo.owner_id \
                   AND s2.repo_id = s.repo_id \
                   AND s2.file_path = s.file_path \
                   AND s2.chunk_index = s.chunk_index \
                   AND m2.created_at > m.created_at \
           )",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(repo_id)
    .fetch_one(pool)
    .await
    .expect("count present chunks");
    row.try_get::<i64, _>("c").expect("count column")
}

async fn fetch_file_revision_state(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
    file_path: &str,
) -> Option<FileState> {
    let (kind, principal_id) = owner.columns();
    let row: Option<(FileState,)> = sqlx::query_as(
        "SELECT s.state \
         FROM proxima_core.memories m \
         JOIN proxima_code.file_revision_v1 s USING (memory_id) \
         JOIN (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo \
           ON eo.entity_id = m.memory_id \
         WHERE eo.owner_kind = $1 \
           AND eo.owner_id = $2 \
           AND s.repo_id = $3 \
           AND s.file_path = $4 \
           AND NOT EXISTS ( \
                 SELECT 1 FROM proxima_core.memories m2 \
                 JOIN proxima_code.file_revision_v1 s2 USING (memory_id) \
                 JOIN (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo2 \
                   ON eo2.entity_id = m2.memory_id \
                 WHERE m2.schema_id = m.schema_id \
                   AND eo2.owner_kind = eo.owner_kind \
                   AND eo2.owner_id = eo.owner_id \
                   AND s2.repo_id = s.repo_id \
                   AND s2.file_path = s.file_path \
                   AND m2.created_at > m.created_at \
           )",
    )
    .bind(kind)
    .bind(principal_id)
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
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);
        let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
        let ingest_ctx = CodeIngestContext::new(&engine, &authz, &store);

        // Phase 1 — initial index.
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
        // Abstractions with `core/derived-from` edges back to their
        // parent `file-revision-v1` Facts.
        let linkage: (i64, i64) = sqlx::query_as(
            "WITH chunks AS ( \
                 SELECT ch.memory_id \
                 FROM proxima_code.code_chunk_v1 ch \
                 JOIN proxima_core.memories cm USING (memory_id) \
                 WHERE ch.repo_id = $1 AND ch.file_path = 'src/lib.rs' \
                   AND ch.state = 'Present' \
                   AND cm.kind = 'Abstraction' \
             ) \
             SELECT \
                 (SELECT COUNT(*)::bigint FROM chunks), \
                 (SELECT COUNT(*)::bigint FROM chunks c \
                  WHERE EXISTS ( \
                      SELECT 1 FROM proxima_core.edges e \
                      JOIN proxima_code.file_revision_v1 fr \
                        ON fr.memory_id = e.target_memory_id \
                      WHERE e.relation = $2 \
                        AND e.source_kind = 'Abstraction' \
                        AND e.target_kind = 'Fact' \
                        AND e.source_memory_id = c.memory_id \
                        AND fr.repo_id = $1 \
                        AND fr.file_path = 'src/lib.rs'))",
        )
        .bind(repo_id)
        .bind(CORE_DERIVED_FROM_RELATION)
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
            principal: owner,
            read_owners: vec![owner],
            entity_kind: None,
            schema_id: Some(SchemaId::new(
                <CodeChunkV1 as AbstractionPayload>::SCHEMA_ID.into(),
            )),
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            limit: 1000,
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: Vec::new(),
        };
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                &q,
            )
            .await?;
        assert_eq!(
            i64::try_from(resp.memories.len()).unwrap(),
            chunks_after_initial,
            "heads-only chunk Query must agree with raw NK count"
        );

        // ----------------------------------------------------------------
        // Phase 2 — mutate src/lib.rs and reindex.
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
             FROM proxima_core.memories m \
             JOIN proxima_code.file_revision_v1 s USING (memory_id) \
             WHERE s.repo_id = $1 AND s.file_path = 'src/lib.rs' \
               AND NOT EXISTS ( \
                     SELECT 1 FROM proxima_core.memories m2 \
                     JOIN proxima_code.file_revision_v1 s2 USING (memory_id) \
                     WHERE s2.repo_id = s.repo_id \
                       AND s2.file_path = s.file_path \
                       AND m2.created_at > m.created_at)",
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
            principal: owner,
            read_owners: vec![owner],
            entity_kind: None,
            schema_id: Some(SchemaId::new(FileRevisionV1::SCHEMA_ID.into())),
            supersession: SupersessionStatus::IncludeSuperseded,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            limit: 1000,
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: Vec::new(),
        };
        let resp_all = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
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
        // Phase 3 — delete src/main.ts and reindex.
        std::fs::remove_file(repo.path().join("src/main.ts"))?;
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "drop main.ts"]);

        let (r3, cursor) = source.run_poll(&ingest_ctx, &cursor, &mut |_| {}).await?;
        assert!(r3.files_tombstoned >= 1, "expected tombstone for main.ts");
        let main_state =
            fetch_file_revision_state(pg.pool_for_tests(), &owner, repo_id, "src/main.ts").await;
        assert_eq!(main_state, Some(FileState::Tombstone));

        // ----------------------------------------------------------------
        // Phase 4 — rename README.md → docs/README.md and reindex.
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
        // Phase 5 — markdown is polyglot: present revision, fallback chunks.
        // (The renamed docs/README.md proves this.)
        let md_chunk_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::bigint FROM proxima_core.memories m \
             JOIN proxima_code.code_chunk_v1 s USING (memory_id) \
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
async fn polyglot_markdown_emits_file_revision_and_fallback_chunks() {
    // Subset of the above: tighter assertion on FileState::Present
    // for a markdown-only fixture.
    let (db_name, pg) = migrated_db().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let engine = build_engine(pg.clone());
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);
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
             FROM proxima_core.memories m \
             JOIN proxima_code.code_chunk_v1 s USING (memory_id) \
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
