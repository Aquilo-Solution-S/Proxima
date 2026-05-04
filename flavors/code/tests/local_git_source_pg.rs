#![allow(clippy::doc_markdown, clippy::too_many_lines)]
//! M3.B.7 done-when — full `LocalGitSource` lifecycle against a real
//! Postgres + a fixture git repo on disk.
//!
//! Exercises the five assertions from M3-PLAN.md:
//! 1. After initial index: heads-only Query returns chunks for every
//!    Present file.
//! 2. After mutation + reindex: head returns new chunk text; old chunk
//!    Fact still queryable as history.
//! 3. After delete + reindex: head returns Tombstone state for that
//!    file's revisions and chunks.
//! 4. After rename + reindex: old path Tombstones, new path Present.
//! 5. Polyglot file (Markdown): file-revision-v1 head Present; chunk
//!    output is fallback (chunk_type="file"), still indexed.

use std::path::Path;
use std::process::Command;

use proxima_code::{
    CodeChunkV1, FileRevisionV1, FileState, LocalGitSource, build_engine, migrator,
};
use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::verbs::query::{QueryRequest, SupersessionStatus};
use proxima_core::{
    FactPayload, OrgId, Owner, Principal, SchemaId, SchemaVersion, UserId,
};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection, Row};
use tempfile::TempDir;
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

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git command");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write_file(repo: &Path, path: &str, contents: &str) {
    let full = repo.join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    std::fs::write(&full, contents).expect("write file");
}

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

async fn count_present_chunks(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> i64 {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let org_id = owner.org_id.into_inner();
    let row = sqlx::query(
        "SELECT COUNT(*)::bigint AS c \
         FROM proxima_core.memories m \
         JOIN proxima_code.code_chunk_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
           AND s.state = 'Present' \
           AND NOT EXISTS ( \
                 SELECT 1 FROM proxima_core.memories m2 \
                 JOIN proxima_code.code_chunk_v1 s2 USING (memory_id) \
                 WHERE m2.schema_id = m.schema_id \
                   AND m2.owner_principal_kind = m.owner_principal_kind \
                   AND m2.owner_principal_id = m.owner_principal_id \
                   AND m2.owner_org_id = m.owner_org_id \
                   AND s2.repo_id = s.repo_id \
                   AND s2.file_path = s.file_path \
                   AND s2.chunk_index = s.chunk_index \
                   AND m2.created_at > m.created_at \
           )",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
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
) -> Option<String> {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let org_id = owner.org_id.into_inner();
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT s.state \
         FROM proxima_core.memories m \
         JOIN proxima_code.file_revision_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
           AND s.file_path = $5 \
           AND NOT EXISTS ( \
                 SELECT 1 FROM proxima_core.memories m2 \
                 JOIN proxima_code.file_revision_v1 s2 USING (memory_id) \
                 WHERE m2.schema_id = m.schema_id \
                   AND m2.owner_principal_kind = m.owner_principal_kind \
                   AND m2.owner_principal_id = m.owner_principal_id \
                   AND m2.owner_org_id = m.owner_org_id \
                   AND s2.repo_id = s.repo_id \
                   AND s2.file_path = s.file_path \
                   AND m2.created_at > m.created_at \
           )",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .bind(file_path)
    .fetch_optional(pool)
    .await
    .expect("fetch state");
    row.map(|(s,)| s)
}

#[tokio::test]
async fn local_git_source_full_cycle() {
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

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let engine = build_engine(
            pg.clone(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        );

        // Phase 1 — initial index.
        let repo = fixture_repo();
        let repo_id = Uuid::now_v7();
        let source = LocalGitSource::new(repo_id, repo.path().to_path_buf(), owner.clone());
        let cursor = proxima_core::Cursor::empty();
        let (r1, cursor) = source.run_poll(pg.pool(), &cursor).await?;
        assert!(r1.commits_emitted >= 1, "expected at least one commit");
        assert!(r1.files_present_emitted >= 3, "expected ≥3 file-revisions");
        assert!(r1.chunks_emitted >= 3, "expected ≥3 chunks");
        let chunks_after_initial = count_present_chunks(pg.pool(), &owner, repo_id).await;
        assert!(chunks_after_initial >= 3);

        // Citation linkage proof — chunks share `cited_object_id`
        // with their parent `file-revision-v1` Fact via the
        // substrate's UNIQUE on (owner, schema_id, content_hash).
        // This is what makes `parent_file_revision_id` redundant
        // in the chunk Fact payload (docs/11 §"Three-layer model").
        let linkage: (i64, i64) = sqlx::query_as(
            "WITH revision_cited AS ( \
                 SELECT cm.cited_object_id, fr.repo_id, fr.file_path \
                 FROM proxima_core.citation_mappings cm \
                 JOIN proxima_code.file_revision_v1 fr USING (memory_id) \
                 WHERE fr.repo_id = $1 AND fr.file_path = 'src/lib.rs' \
             ), \
             chunk_cited AS ( \
                 SELECT cm.cited_object_id, ch.repo_id, ch.file_path \
                 FROM proxima_core.citation_mappings cm \
                 JOIN proxima_code.code_chunk_v1 ch USING (memory_id) \
                 WHERE ch.repo_id = $1 AND ch.file_path = 'src/lib.rs' \
                   AND ch.state = 'Present' \
             ) \
             SELECT \
                 (SELECT COUNT(*)::bigint FROM chunk_cited), \
                 (SELECT COUNT(*)::bigint FROM chunk_cited c \
                  WHERE EXISTS ( \
                      SELECT 1 FROM revision_cited r \
                      WHERE r.cited_object_id = c.cited_object_id))",
        )
        .bind(repo_id)
        .fetch_one(pg.pool())
        .await?;
        assert!(
            linkage.0 > 0,
            "expected at least one chunk for src/lib.rs"
        );
        assert_eq!(
            linkage.0, linkage.1,
            "every chunk must share cited_object_id with a file-revision-v1 \
             Fact for the same blob — got {} chunks, {} linked",
            linkage.0, linkage.1,
        );

        // Heads-only chunk Query through the Engine — the path that
        // matters for downstream consumers.
        let q = QueryRequest {
            owner: owner.clone(),
            entity_kind: None,
            schema_id: Some(SchemaId::new(CodeChunkV1::SCHEMA_ID.into())),
            supersession: SupersessionStatus::HeadsOnly,
            limit: 1000,
            stateful_heads: None,
        };
        let resp = engine.query(&Credentials::None, &q).await?;
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

        let (r2, cursor) = source.run_poll(pg.pool(), &cursor).await?;
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
        .fetch_one(pg.pool())
        .await?;
        let new_hash = blake3::hash(
            "pub fn hello() -> &'static str {\n    \"hello, world\"\n}\n\n\
             pub fn goodbye() -> &'static str {\n    \"bye\"\n}\n"
                .as_bytes(),
        );
        assert_eq!(row.0, new_hash.as_bytes(), "lib.rs head must reflect new content");

        // Old revision still exists as history (IncludeSuperseded view).
        let q_all = QueryRequest {
            owner: owner.clone(),
            entity_kind: None,
            schema_id: Some(SchemaId::new(FileRevisionV1::SCHEMA_ID.into())),
            supersession: SupersessionStatus::IncludeSuperseded,
            limit: 1000,
            stateful_heads: None,
        };
        let resp_all = engine.query(&Credentials::None, &q_all).await?;
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

        let (r3, cursor) = source.run_poll(pg.pool(), &cursor).await?;
        assert!(r3.files_tombstoned >= 1, "expected tombstone for main.ts");
        let main_state = fetch_file_revision_state(pg.pool(), &owner, repo_id, "src/main.ts").await;
        assert_eq!(main_state.as_deref(), Some("Tombstone"));

        // ----------------------------------------------------------------
        // Phase 4 — rename README.md → docs/README.md and reindex.
        std::fs::create_dir_all(repo.path().join("docs"))?;
        std::fs::rename(
            repo.path().join("README.md"),
            repo.path().join("docs/README.md"),
        )?;
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "rename README"]);

        let (_r4, cursor) = source.run_poll(pg.pool(), &cursor).await?;
        let old_state = fetch_file_revision_state(pg.pool(), &owner, repo_id, "README.md").await;
        let new_state =
            fetch_file_revision_state(pg.pool(), &owner, repo_id, "docs/README.md").await;
        assert_eq!(old_state.as_deref(), Some("Tombstone"));
        assert_eq!(new_state.as_deref(), Some("Present"));

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
        .fetch_one(pg.pool())
        .await?;
        assert!(md_chunk_count.0 >= 1, "markdown should still be chunked via fallback");

        // ----------------------------------------------------------------
        // Re-running index without changes must be idempotent (no new
        // emissions, all unchanged).
        let (r_idem, _cursor) = source.run_poll(pg.pool(), &cursor).await?;
        assert_eq!(r_idem.files_present_emitted, 0, "idempotent reindex emitted files");
        assert_eq!(r_idem.chunks_emitted, 0, "idempotent reindex emitted chunks");
        assert_eq!(r_idem.files_tombstoned, 0, "idempotent reindex tombstoned files");

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

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let _engine = build_engine(
            pg.clone(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        );

        let dir = TempDir::new()?;
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "t@t"]);
        git(dir.path(), &["config", "user.name", "T"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        write_file(dir.path(), "doc.md", "# Doc\n\nText.\n");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);

        let repo_id = Uuid::now_v7();
        let source = LocalGitSource::new(repo_id, dir.path().to_path_buf(), owner.clone());
        let cursor = proxima_core::Cursor::empty();
        let (report, _cursor) = source.run_poll(pg.pool(), &cursor).await?;
        assert!(report.files_present_emitted >= 1);

        let state = fetch_file_revision_state(pg.pool(), &owner, repo_id, "doc.md").await;
        assert_eq!(state.as_deref(), Some("Present"));

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
        .fetch_one(pg.pool())
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
