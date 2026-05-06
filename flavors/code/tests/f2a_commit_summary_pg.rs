#![allow(clippy::doc_markdown, clippy::too_many_lines)]
//! M5 done-when — F→A consolidation produces typed Abstractions
//! over closed source-batches, with provenance edges and embeddings.
//!
//! Asserts the ROADMAP M5 criterion: "querying Abstractions returns
//! a coherent typed summary of the ingested commits." The LLM is
//! stubbed (deterministic JSON keyed off the commit message) so the
//! test stays hermetic — no Ollama needed in CI.
//!
//! With `RUN_OLLAMA_INTEGRATION=1`, swap the stubs for real Ollama
//! (gemma4:31b + qwen3-embedding:8b) — out of scope for this test;
//! the M5 done-when is the substrate-shape assertion, not the model
//! quality assertion.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use proxima_code::{LocalGitSource, build_engine, erase_repo, migrator, register_repo};
use proxima_core::auth::NoAuth;
use proxima_core::operators::{EmbeddingClient, LlmClient, OperatorError};
use proxima_core::{Cursor, OrgId, Owner, Principal, UserId};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection};
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

fn run_git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git spawn");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Build a synthetic repo with 2 commits — one initial commit
/// adding a Rust file with a function, and a follow-up commit
/// modifying that function.
fn build_synthetic_repo(repo: &Path) {
    Command::new("git")
        .args(["init", "--quiet", "-b", "main"])
        .arg(repo)
        .status()
        .expect("git init");
    run_git(repo, &["config", "user.email", "m5@example.com"]);
    run_git(repo, &["config", "user.name", "M5 Probe"]);
    run_git(repo, &["config", "commit.gpgsign", "false"]);

    std::fs::write(
        repo.join("lib.rs"),
        "pub fn greet(name: &str) -> String {\n    format!(\"hello, {name}\")\n}\n",
    )
    .expect("write lib.rs");
    run_git(repo, &["add", "lib.rs"]);
    run_git(repo, &["commit", "-q", "-m", "feat: add greet helper"]);

    std::fs::write(
        repo.join("lib.rs"),
        "pub fn greet(name: &str) -> String {\n    format!(\"hello, {name}!\")\n}\n\npub fn farewell(name: &str) -> String {\n    format!(\"bye, {name}\")\n}\n",
    )
    .expect("rewrite lib.rs");
    run_git(repo, &["add", "lib.rs"]);
    run_git(repo, &["commit", "-q", "-m", "feat: add farewell function"]);
}

// =====================================================================
// Stub LLM + Embed
// =====================================================================

#[derive(Debug)]
struct StubLlm;

#[async_trait]
impl LlmClient for StubLlm {
    fn model_id(&self) -> &'static str {
        "stub-llm"
    }

    async fn complete_json(
        &self,
        _system_prompt: &str,
        user_prompt: &str,
    ) -> Result<serde_json::Value, OperatorError> {
        // Pull the commit message line "Message:\n<msg>" from the
        // prompt — gives us a deterministic summary that varies per
        // commit so we can tell them apart in assertions.
        let msg = user_prompt
            .split("Message:\n")
            .nth(1)
            .and_then(|s| s.lines().next())
            .unwrap_or("(no-message)")
            .to_string();
        let summary = format!("STUB SUMMARY: {msg}");
        // Pick a key_file from the rendered "Changed files" section
        // if present — passes operator-side sanitization.
        let key_file = user_prompt
            .lines()
            .find(|l| l.starts_with(" - "))
            .and_then(|l| l.strip_prefix(" - "))
            .and_then(|l| l.split_whitespace().next())
            .map(std::string::ToString::to_string);

        let mut key_files = Vec::new();
        if let Some(kf) = key_file {
            key_files.push(kf);
        }
        Ok(serde_json::json!({
            "summary": summary,
            "key_files": key_files,
            "change_kind": "feature"
        }))
    }
}

#[derive(Debug)]
struct StubEmbed;

const STUB_EMBED_DIM: usize = 4;

#[async_trait]
impl EmbeddingClient for StubEmbed {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, OperatorError> {
        // Deterministic 4-dim embedding from text bytes. Not
        // semantically meaningful — just enough for the row to land.
        let bytes = text.as_bytes();
        let mut out = [0.0f32; STUB_EMBED_DIM];
        for (i, b) in bytes.iter().take(STUB_EMBED_DIM * 16).enumerate() {
            out[i % STUB_EMBED_DIM] += f32::from(*b) / 255.0;
        }
        Ok(out.to_vec())
    }

    fn model_id(&self) -> &'static str {
        "stub-embed"
    }

    fn dim(&self) -> usize {
        STUB_EMBED_DIM
    }
}

// =====================================================================
// SQL probes
// =====================================================================

fn owner_cols(owner: &Owner) -> (&'static str, Uuid, Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

async fn count_abstractions(pool: &sqlx::PgPool, owner: &Owner) -> i64 {
    let (kind, pid, oid) = owner_cols(owner);
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM proxima_core.memories \
         WHERE owner_principal_kind = $1 AND owner_principal_id = $2 AND owner_org_id = $3 \
           AND kind = 'Abstraction' \
           AND schema_id = 'proxima-code/commit-summary-v1'",
    )
    .bind(kind)
    .bind(pid)
    .bind(oid)
    .fetch_one(pool)
    .await
    .expect("count abstractions");
    row.0
}

async fn count_provenance_edges(pool: &sqlx::PgPool, owner: &Owner) -> i64 {
    let (kind, pid, oid) = owner_cols(owner);
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM proxima_core.edges \
         WHERE owner_principal_kind = $1 AND owner_principal_id = $2 AND owner_org_id = $3 \
           AND relation_class = 'Provenance' \
           AND authorship_kind = 'OperatorFtoA'",
    )
    .bind(kind)
    .bind(pid)
    .bind(oid)
    .fetch_one(pool)
    .await
    .expect("count edges");
    row.0
}

async fn count_embeddings(pool: &sqlx::PgPool, owner: &Owner) -> i64 {
    let (kind, pid, oid) = owner_cols(owner);
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM proxima_core.embeddings \
         WHERE owner_principal_kind = $1 AND owner_principal_id = $2 AND owner_org_id = $3 \
           AND entity_kind = 'Abstraction' AND model_id = 'stub-embed'",
    )
    .bind(kind)
    .bind(pid)
    .bind(oid)
    .fetch_one(pool)
    .await
    .expect("count embeddings");
    row.0
}

async fn count_f2a_rows(pool: &sqlx::PgPool) -> i64 {
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM proxima_core.source_batch_f2a \
         WHERE operator_id = 'proxima-code/commit-summary'",
    )
    .fetch_one(pool)
    .await
    .expect("count source_batch_f2a");
    row.0
}

async fn list_summaries(pool: &sqlx::PgPool) -> Vec<(String, String, String)> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT commit_sha, summary, change_kind \
         FROM proxima_code.commit_summary_v1 \
         ORDER BY commit_sha",
    )
    .fetch_all(pool)
    .await
    .expect("list summaries");
    rows
}

async fn count_repo_sidecars(pool: &sqlx::PgPool, repo_id: Uuid) -> i64 {
    let row: (i64,) = sqlx::query_as(
        "SELECT \
            (SELECT COUNT(*)::bigint FROM proxima_code.commit_v1 WHERE repo_id = $1) + \
            (SELECT COUNT(*)::bigint FROM proxima_code.file_revision_v1 WHERE repo_id = $1) + \
            (SELECT COUNT(*)::bigint FROM proxima_code.code_chunk_v1 WHERE repo_id = $1) + \
            (SELECT COUNT(*)::bigint FROM proxima_code.commit_summary_v1 WHERE repo_id = $1)",
    )
    .bind(repo_id)
    .fetch_one(pool)
    .await
    .expect("count repo sidecars");
    row.0
}

async fn count_repo_registry_rows(pool: &sqlx::PgPool, owner: &Owner, repo_id: Uuid) -> i64 {
    let (kind, pid, oid) = owner_cols(owner);
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint \
         FROM proxima_code.repos \
         WHERE owner_principal_kind = $1 \
           AND owner_principal_id = $2 \
           AND owner_org_id = $3 \
           AND repo_id = $4",
    )
    .bind(kind)
    .bind(pid)
    .bind(oid)
    .bind(repo_id)
    .fetch_one(pool)
    .await
    .expect("count repo registry rows");
    row.0
}

async fn count_dangling_repo_references(pool: &sqlx::PgPool) -> i64 {
    let row: (i64,) = sqlx::query_as(
        "SELECT \
            (SELECT COUNT(*)::bigint \
             FROM proxima_core.edges e \
             LEFT JOIN proxima_core.memories sm ON sm.memory_id = e.source_memory_id \
             LEFT JOIN proxima_core.memories tm ON tm.memory_id = e.target_memory_id \
             LEFT JOIN proxima_core.memories am ON am.memory_id = e.authorship_owner_memory_id \
             WHERE (e.source_memory_id IS NOT NULL AND sm.memory_id IS NULL) \
                OR (e.target_memory_id IS NOT NULL AND tm.memory_id IS NULL) \
                OR (e.authorship_owner_memory_id IS NOT NULL AND am.memory_id IS NULL)) + \
            (SELECT COUNT(*)::bigint \
             FROM proxima_core.embeddings em \
             LEFT JOIN proxima_core.memories m ON m.memory_id = em.entity_id \
             WHERE em.entity_kind IN ('Fact','Abstraction','Perspective') \
               AND m.memory_id IS NULL) + \
            (SELECT COUNT(*)::bigint \
             FROM proxima_core.source_batch_f2a f \
             LEFT JOIN proxima_core.source_batches sb ON sb.id = f.batch_id \
             WHERE sb.id IS NULL)",
    )
    .fetch_one(pool)
    .await
    .expect("count dangling repo references");
    row.0
}

#[tokio::test]
async fn f2a_commit_summary_full_cycle() {
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

        // Engine wired with flavor-registered operators + stub clients.
        let engine = build_engine(
            pg.clone(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        )
        .with_llm(Arc::new(StubLlm))
        .with_embed(Arc::new(StubEmbed));

        // Synthetic 2-commit repo.
        let tmp = TempDir::new()?;
        let repo_path = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_path)?;
        build_synthetic_repo(&repo_path);

        // Ingest. LocalGitSource talks to the pool directly and closes
        // its own batches; F→A is run via run_pending_f2a after the poll.
        let repo_id = Uuid::now_v7();
        let repo_path_str = repo_path.to_string_lossy().into_owned();
        register_repo(pg.pool(), &owner, repo_id, &repo_path_str, "repo").await?;
        let source = LocalGitSource::new(repo_id, repo_path.clone(), owner.clone());
        let (report, _cursor) = source
            .run_poll(pg.pool(), &Cursor::empty(), &mut |_| {})
            .await?;
        assert_eq!(report.commits_emitted, 2, "expected 2 commits");

        // Run F→A pass — should consolidate both batches.
        let consolidated = engine.run_pending_f2a(&owner).await?;
        assert_eq!(
            consolidated.len(),
            2,
            "expected 2 batches consolidated, got {consolidated:?}"
        );

        // Substrate shape:
        // 2 abstractions, 2 source_batch_f2a rows, 2 embeddings,
        // and at minimum 2 provenance edges (one per Abstraction;
        // each batch has commit + file_revision + chunk Facts so
        // we expect strictly more than 2, but ≥ 2 covers the M5
        // contract: every Abstraction has provenance).
        assert_eq!(count_abstractions(pg.pool(), &owner).await, 2);
        assert_eq!(count_f2a_rows(pg.pool()).await, 2);
        assert_eq!(count_embeddings(pg.pool(), &owner).await, 2);
        assert!(count_repo_sidecars(pg.pool(), repo_id).await > 0);
        let edges = count_provenance_edges(pg.pool(), &owner).await;
        assert!(edges >= 2, "expected ≥ 2 provenance edges, got {edges}");

        // Typed sidecar payloads carry the stubbed summaries.
        let summaries = list_summaries(pg.pool()).await;
        assert_eq!(summaries.len(), 2);
        for (_sha, summary, change_kind) in &summaries {
            assert!(
                summary.starts_with("STUB SUMMARY: "),
                "summary should reflect stub LLM output: {summary}"
            );
            assert_eq!(change_kind, "feature");
        }

        // Idempotency — re-running F→A returns nothing new.
        let consolidated2 = engine.run_pending_f2a(&owner).await?;
        assert!(
            consolidated2.is_empty(),
            "expected no pending batches on re-run, got {consolidated2:?}"
        );
        assert_eq!(count_abstractions(pg.pool(), &owner).await, 2);
        assert_eq!(count_f2a_rows(pg.pool()).await, 2);

        let receipt = erase_repo(pg.pool(), &owner, repo_id).await?;
        assert!(receipt.repo_record_deleted);
        assert!(receipt.facts_deleted > 0);
        assert_eq!(receipt.abstractions_deleted, 2);
        assert!(receipt.edges_deleted >= 2);
        assert_eq!(receipt.embeddings_deleted, 2);
        assert_eq!(receipt.f2a_rows_deleted, 2);
        assert_eq!(
            count_repo_registry_rows(pg.pool(), &owner, repo_id).await,
            0
        );
        assert_eq!(count_repo_sidecars(pg.pool(), repo_id).await, 0);
        assert_eq!(count_abstractions(pg.pool(), &owner).await, 0);
        assert_eq!(count_f2a_rows(pg.pool()).await, 0);
        assert_eq!(count_embeddings(pg.pool(), &owner).await, 0);
        assert_eq!(count_provenance_edges(pg.pool(), &owner).await, 0);
        assert_eq!(count_dangling_repo_references(pg.pool()).await, 0);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("f2a_commit_summary_full_cycle test failed");
}
