use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

mod common;

use common::{TestDb, test_owner as owner_fixture};
use proxima_code::mcp::{
    CodeEmitExecutionPlanTool, CodeEraseRepoTool, CodeIngestHeadSnapshotTool, CodeListReposTool,
    CodeOpenFileRevisionTool, CodeRegisterRepoTool, CodeRetryExecutionRequestTool,
    CodeSearchChunksTool, CodeSearchCommitsTool,
};
use proxima_code::testkit::register_repo;
use proxima_code::{
    CodeChunkV1, CodeFlavorStore, CommitV1, ExecutionRequestV1, FileRevisionV1, FileState,
};
use proxima_core::engine::Engine;
use proxima_core::mcp::{McpAuthorContext, McpTool, McpToolCtx, McpToolError, McpToolExtensions};
use proxima_core::verbs::fact_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{
    AbstractionPayload, AuthPath, AuthzContext, CORE_DERIVED_FROM_RELATION, CORE_INSPIRES_RELATION,
    FactPayload, FlavorRegistry, FlavorRegistryFrozen, MemoryId, Owner, SchemaId, SchemaVersion,
    SourceBatchId, SourceId,
};
use proxima_storage_pg::PgStorage;
use serde_json::json;
use sqlx::PgPool;
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn register_repo_tool_registers_local_git_repo_idempotently()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    std::process::Command::new("git")
        .arg("init")
        .arg(temp.path())
        .output()?;

    let result = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "Proxima Dogfood" }),
    )
    .await?;

    assert_eq!(result["created"], true);
    assert_eq!(
        result["repo"]["repo_handle"].as_str().expect("repo_handle"),
        format!("R:{}", result["repo"]["repo_id"].as_str().expect("repo_id"))
    );
    assert_eq!(result["repo"]["display_name"], "Proxima Dogfood");
    assert_eq!(
        result["repo"]["canonical_path"].as_str(),
        Some(
            std::fs::canonicalize(temp.path())?
                .to_string_lossy()
                .as_ref()
        )
    );

    let replay = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "Ignored Replay Name" }),
    )
    .await?;
    assert_eq!(replay["created"], false);
    assert_eq!(replay["repo"]["repo_id"], result["repo"]["repo_id"]);
    assert_eq!(replay["repo"]["display_name"], "Proxima Dogfood");

    let list =
        run_tool::<CodeListReposTool>(ctx(fixture.pg.clone(), owner, registry), json!({})).await?;
    let repos = list["repos"].as_array().expect("repos");
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["repo_id"], result["repo"]["repo_id"]);
    Ok(())
}

/// Keyset pages over `(created_at, repo_id)` are disjoint, exhaustive,
/// and terminate; a garbage cursor fails closed.
#[tokio::test]
async fn list_repos_tool_pages_with_opaque_cursor() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let mut temps = Vec::new();
    let mut expected = Vec::new();
    for index in 0..3 {
        let temp = TempDir::new()?;
        std::process::Command::new("git")
            .arg("init")
            .arg(temp.path())
            .output()?;
        let result = run_tool::<CodeRegisterRepoTool>(
            ctx(fixture.pg.clone(), owner, registry.clone()),
            json!({ "path": temp.path().to_string_lossy(), "display_name": format!("Paged Repo {index}") }),
        )
        .await?;
        expected.push(
            result["repo"]["repo_id"]
                .as_str()
                .expect("repo_id")
                .to_string(),
        );
        temps.push(temp);
    }

    let first = run_tool::<CodeListReposTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "limit": 2 }),
    )
    .await?;
    assert_eq!(first["repos"].as_array().expect("repos").len(), 2);
    assert_eq!(first["has_more"], json!(true));
    let token = first["next_cursor"].as_str().expect("cursor").to_string();

    let second = run_tool::<CodeListReposTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "limit": 2, "cursor": token }),
    )
    .await?;
    assert_eq!(second["repos"].as_array().expect("repos").len(), 1);
    assert_eq!(second["has_more"], json!(false));
    assert_eq!(second["next_cursor"], serde_json::Value::Null);

    let mut walked: Vec<String> = first["repos"]
        .as_array()
        .expect("repos")
        .iter()
        .chain(second["repos"].as_array().expect("repos"))
        .map(|repo| repo["repo_id"].as_str().expect("repo_id").to_string())
        .collect();
    walked.sort_unstable();
    expected.sort_unstable();
    assert_eq!(walked, expected, "pages cover every repo exactly once");

    let err = run_tool::<CodeListReposTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "cursor": "garbage" }),
    )
    .await
    .expect_err("garbage cursor must fail closed");
    assert!(
        err.to_string().contains("malformed cursor"),
        "unexpected error: {err}"
    );
    Ok(())
}

#[tokio::test]
async fn ingest_head_snapshot_tool_indexes_current_tree() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    init_git_repo_with_commit(
        temp.path(),
        "src/lib.rs",
        "pub fn proxima_snapshot_marker() -> u64 { 42 }\n",
    )?;

    let registered = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "Snapshot Repo" }),
    )
    .await?;
    let repo_handle = registered["repo"]["repo_id"].as_str().expect("repo_id");

    let snapshot = run_tool::<CodeIngestHeadSnapshotTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": repo_handle }),
    )
    .await?;

    assert_eq!(snapshot["repo"]["has_cursor"], true);
    assert_eq!(snapshot["report"]["commits_emitted"], 0);
    assert_eq!(snapshot["report"]["files_present_emitted"], 1);
    assert!(
        snapshot["report"]["chunks_emitted"]
            .as_u64()
            .expect("chunks_emitted")
            >= 1
    );

    let chunks = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "query": "proxima_snapshot_marker", "repo_handle": repo_handle, "limit": 10 }),
    )
    .await?;
    assert_eq!(chunks["matches"].as_array().expect("matches").len(), 1);
    assert_eq!(chunks["matches"][0]["file_path"], "src/lib.rs");
    Ok(())
}

/// Erasure is the supported way to re-index a repository from scratch, which
/// is what a chunker or render upgrade needs: a HEAD snapshot re-derives only
/// files whose content moved, and a derived Abstraction has to carry its
/// input Facts' `source_batch_id`, so files that never change cannot be
/// re-derived in place. It is also the only way to remove an indexed
/// repository at all — `register_repo` upserts and keeps the cursor.
#[tokio::test]
async fn erase_repo_tool_clears_the_index_and_allows_a_fresh_one()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    init_git_repo_with_commit(
        temp.path(),
        "src/lib.rs",
        "pub fn proxima_erase_marker() -> u64 { 7 }\n",
    )?;
    let repo_path = temp.path().to_string_lossy().to_string();

    let registered = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": repo_path, "display_name": "Erase Repo" }),
    )
    .await?;
    let repo_handle = registered["repo"]["repo_id"].as_str().expect("repo_id");
    let canonical_path = registered["repo"]["canonical_path"]
        .as_str()
        .expect("canonical_path")
        .to_string();

    run_tool::<CodeIngestHeadSnapshotTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": repo_handle }),
    )
    .await?;
    let before = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "query": "proxima_erase_marker", "limit": 10 }),
    )
    .await?;
    assert_eq!(before["matches"].as_array().expect("matches").len(), 1);

    // A wrong confirmation must not destroy anything.
    let refused = run_tool::<CodeEraseRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": repo_handle, "confirm_canonical_path": "/not/this/repo" }),
    )
    .await;
    assert!(refused.is_err(), "mismatched confirmation must be refused");
    let still_there = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "query": "proxima_erase_marker", "limit": 10 }),
    )
    .await?;
    assert_eq!(
        still_there["matches"].as_array().expect("matches").len(),
        1,
        "a refused erase must leave the index intact"
    );

    let receipt = run_tool::<CodeEraseRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": repo_handle, "confirm_canonical_path": canonical_path }),
    )
    .await?;
    assert_eq!(receipt["repo_record_deleted"], true);
    assert!(receipt["abstractions_deleted"].as_u64().expect("count") >= 1);

    let after = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "query": "proxima_erase_marker", "limit": 10 }),
    )
    .await?;
    assert_eq!(
        after["matches"].as_array().expect("matches").len(),
        0,
        "erased chunks must leave search"
    );

    // The path is registerable again, and re-ingest rebuilds the index —
    // this is the round trip an upgrade relies on.
    let reregistered = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": repo_path, "display_name": "Erase Repo" }),
    )
    .await?;
    assert_eq!(reregistered["created"], true);
    let fresh_handle = reregistered["repo"]["repo_id"].as_str().expect("repo_id");
    run_tool::<CodeIngestHeadSnapshotTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": fresh_handle }),
    )
    .await?;
    let rebuilt = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "query": "proxima_erase_marker", "limit": 10 }),
    )
    .await?;
    assert_eq!(
        rebuilt["matches"].as_array().expect("matches").len(),
        1,
        "re-ingest after erase must rebuild the index"
    );
    Ok(())
}

/// A match has to carry enough of its chunk to answer with. Snippets were
/// capped at 480 characters against a chunker that targets 1,500, so a search
/// returned the right chunk with most of it missing and no way to ask for
/// more — and nothing in the response said so.
#[tokio::test]
async fn search_chunks_returns_whole_chunks_and_flags_truncation()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;

    // One function well past the old 480-character cap but still inside
    // MAX_CHUNK_CHARS, so it stays a single chunk, with the value that
    // answers the query at the very end of it.
    let mut body = String::from("pub fn proxima_long_marker() -> u32 {\n");
    for i in 0..40 {
        writeln!(body, "    let filler_{i} = {i}; // padding line").expect("write to String");
    }
    body.push_str("    9_753\n}\n");
    assert!(body.len() > 480, "fixture must exceed the old snippet cap");
    init_git_repo_with_commit(temp.path(), "src/long.rs", &body)?;

    let registered = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "Long Repo" }),
    )
    .await?;
    let repo_handle = registered["repo"]["repo_id"].as_str().expect("repo_id");
    run_tool::<CodeIngestHeadSnapshotTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": repo_handle }),
    )
    .await?;

    let found = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "query": "proxima_long_marker", "limit": 5, "include_calls": false }),
    )
    .await?;
    let top = &found["matches"][0];
    let snippet = top["snippet"].as_str().expect("snippet");
    assert!(
        snippet.len() > 480,
        "default snippet is still capped near 480: {} chars",
        snippet.len()
    );
    assert!(
        snippet.contains("9_753"),
        "the value at the end of the chunk did not survive the default budget"
    );
    assert_eq!(top["snippet_truncated"], false);

    // An explicit small budget truncates, and says so.
    let clipped = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({
            "query": "proxima_long_marker", "limit": 5,
            "include_calls": false, "snippet_max_chars": 50,
        }),
    )
    .await?;
    let clipped_top = &clipped["matches"][0];
    assert_eq!(
        clipped_top["snippet"]
            .as_str()
            .expect("snippet")
            .chars()
            .count(),
        50
    );
    assert_eq!(clipped_top["snippet_truncated"], true);

    // Zero is a mistake, not a request for nothing.
    let rejected = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "query": "proxima_long_marker", "snippet_max_chars": 0 }),
    )
    .await;
    assert!(rejected.is_err(), "snippet_max_chars=0 must be rejected");
    Ok(())
}

/// A plain-English question must reach the right chunk. Without the
/// OR-rescue arm `websearch_to_tsquery` requires every content word in one
/// chunk, which no sentence-shaped query satisfies.
#[tokio::test]
async fn search_chunks_answers_a_natural_language_question()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    init_git_repo_with_commit(
        temp.path(),
        "src/retry.rs",
        "/// Retries the upload with exponential backoff until the deadline.\n\
         pub fn upload_with_backoff() {}\n",
    )?;

    let registered = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "NL Repo" }),
    )
    .await?;
    let repo_handle = registered["repo"]["repo_id"].as_str().expect("repo_id");
    run_tool::<CodeIngestHeadSnapshotTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": repo_handle }),
    )
    .await?;

    // None of these words co-occur as a phrase, and "how"/"does"/"the" are
    // stopwords: only a stemming config plus a rescue arm can answer it.
    let found = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({
            "query": "how does the uploader retry when a request fails",
            "limit": 10,
            "include_calls": false,
        }),
    )
    .await?;
    let matches = found["matches"].as_array().expect("matches");
    assert!(
        !matches.is_empty(),
        "a natural-language question must return something"
    );
    assert_eq!(matches[0]["file_path"], "src/retry.rs");
    Ok(())
}

/// A bug report is prose, and prose matches prose. Measured on the `knip`
/// repository, `search_chunks` answered 5 of 17 real bug reports where plain
/// ripgrep answered 8: every miss returned Markdown from the docs tree, and
/// in every case the file the real fix touched *was* indexed and *did* match
/// the query — it was outranked, because `ts_rank` has no IDF and a doc
/// matching twenty ordinary English words beats the one source chunk holding
/// the identifier the report names.
///
/// Two things fix it and this pins both: the identifier is ranked on its own
/// (`distinctive_terms`), and a chunk a grammar parsed outranks a line window
/// of a file no grammar could read.
#[tokio::test]
async fn search_chunks_ranks_parsed_code_over_prose_that_shares_more_words()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;

    // The doc repeats every ordinary word of the query and never names the
    // identifier. The source file names the identifier once.
    init_git_repo_with_files(
        temp.path(),
        &[
            (
                "docs/entries.md",
                "# Entry configuration\n\n\
                 The config entry resolution returns the entry the config declares.\n\
                 When the config is missing, entry resolution returns a default entry.\n\
                 A missing config entry means the resolution returns nothing at all.\n\
                 Entry resolution reads the config, resolves each entry, and returns\n\
                 the resolved entry list when the config declares entries.\n",
            ),
            (
                "src/resolver.rs",
                "pub fn resolve_from_ast(entry: &str) -> Option<String> {\n\
                 \x20   Some(entry.to_string())\n\
                 }\n",
            ),
        ],
    )?;

    let registered = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "Ranking Repo" }),
    )
    .await?;
    let repo_handle = registered["repo"]["repo_id"].as_str().expect("repo_id");
    run_tool::<CodeIngestHeadSnapshotTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": repo_handle }),
    )
    .await?;

    let found = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({
            "query": "resolve_from_ast returns the wrong entry when the config is missing",
            "limit": 10,
            "include_calls": false,
        }),
    )
    .await?;
    let matches = found["matches"].as_array().expect("matches");
    assert!(!matches.is_empty(), "query must return something");
    assert_eq!(
        matches[0]["file_path"],
        "src/resolver.rs",
        "the chunk naming the identifier must outrank the doc that repeats \
         every other word of the query; got {:?}",
        matches
            .iter()
            .map(|m| m["file_path"].as_str().unwrap_or_default())
            .collect::<Vec<_>>()
    );
    Ok(())
}

/// One file carrying a NUL must not fail the whole snapshot.
///
/// `U+0000` is valid UTF-8, so the chunker's "is it UTF-8" binary heuristic
/// let such a file through, and its chunk text reached a Postgres `text`
/// column — which cannot store a NUL:
///
/// ```text
/// invalid byte sequence for encoding "UTF8": 0x00
/// ```
///
/// That aborts the entire `ingest_head_snapshot`, so a repository with one
/// such file among thousands could not be indexed at all. It is not a
/// contrived input: UTF-16 files git has not marked binary, `.po` files and
/// test fixtures all carry NULs, and this was found when a stray one in a
/// single source file of this repository failed `self_ingestion_pg`.
///
/// The file is skipped as binary — like any other binary file — and every
/// other file in the tree still indexes.
#[tokio::test]
async fn a_file_containing_nul_is_skipped_not_fatal() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;

    init_git_repo_with_files(
        temp.path(),
        &[
            ("src/good.rs", "pub fn good() -> u32 {\n    7\n}\n"),
            // Valid UTF-8, and unstorable as Postgres `text`.
            ("src/has_nul.rs", "pub fn bad() {\u{0}}\n"),
        ],
    )?;

    let registered = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "Nul Repo" }),
    )
    .await?;
    let repo_handle = registered["repo"]["repo_id"].as_str().expect("repo_id");

    // The whole point: this call used to fail outright.
    run_tool::<CodeIngestHeadSnapshotTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": repo_handle }),
    )
    .await?;

    let found = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "query": "good", "repo_handle": repo_handle, "include_calls": false }),
    )
    .await?;
    let paths = found["matches"]
        .as_array()
        .expect("matches")
        .iter()
        .map(|m| m["file_path"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(
        paths.iter().any(|p| p == "src/good.rs"),
        "the rest of the tree must still be indexed; got {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p == "src/has_nul.rs"),
        "the NUL-bearing file is binary and must not be chunked; got {paths:?}"
    );
    Ok(())
}

/// Switching back to a branch that was already indexed must not break ingest.
///
/// A file revision's receipt key is (repo, path, commit, content hash, state)
/// and carries no batch, so re-observing a commit returns the *original*
/// Fact — whose receipt still names the *original* `source_batch_id`.
/// Deriving that Fact's chunks under the current batch then trips
/// `validate_ftoa_input_batch` ("F→A operator `source_batch_id` must match
/// input Facts") and fails the whole snapshot, leaving the cursor unmoved so
/// every retry fails identically.
///
/// The `already_current` skip does not cover this: it compares against the
/// current head, and after `main -> feature -> main` the head is the
/// *feature* revision, so the main revision is re-offered.
#[tokio::test]
async fn ingesting_a_previously_indexed_commit_again_succeeds()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;

    init_git_repo_with_files(temp.path(), &[("src/a.rs", "pub fn a() -> u8 { 1 }\n")])?;
    let registered = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "Branch Repo" }),
    )
    .await?;
    let repo_handle = registered["repo"]["repo_id"].as_str().expect("repo_id");

    let snapshot = |handle: String, reg: _| {
        let pg = fixture.pg.clone();
        async move {
            run_tool::<CodeIngestHeadSnapshotTool>(
                ctx(pg, owner, reg),
                json!({ "repo_handle": handle }),
            )
            .await
        }
    };

    snapshot(repo_handle.to_string(), registry.clone()).await?;

    // A second revision of the same path, on another branch.
    run_git(temp.path(), &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(
        temp.path().join("src/a.rs"),
        "pub fn a() -> u8 { 2 }\npub fn b() -> u8 { 3 }\n",
    )?;
    run_git(temp.path(), &["add", "."])?;
    run_git(
        temp.path(),
        &[
            "-c",
            "user.name=Proxima Test",
            "-c",
            "user.email=proxima-test@example.com",
            "commit",
            "-m",
            "second revision",
        ],
    )?;
    snapshot(repo_handle.to_string(), registry.clone()).await?;

    // Back to the first branch: this exact (commit, path, content) has been
    // observed before, so the Fact replays and its batch is the first one.
    run_git(temp.path(), &["checkout", "-q", "main"])
        .or_else(|_| run_git(temp.path(), &["checkout", "-q", "master"]))?;
    let back = snapshot(repo_handle.to_string(), registry.clone()).await?;
    assert!(
        back["report"].is_object(),
        "returning to an already-indexed commit must ingest, not error"
    );

    // And the chunks for that revision are still searchable afterwards.
    let found = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "query": "src/a.rs", "limit": 10, "include_calls": false }),
    )
    .await?;
    assert!(
        !found["matches"].as_array().expect("matches").is_empty(),
        "the replayed revision's chunks must still be searchable"
    );
    Ok(())
}

#[tokio::test]
async fn search_chunks_returns_only_head_per_nk() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    ingest_code_chunk(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/atlas.rs",
        0,
        "fn atlas_edges_v1() {}",
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;
    ingest_code_chunk(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/atlas.rs",
        0,
        "fn atlas_edges_v2() {}",
    )
    .await?;

    let result = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "query": "atlas_edges", "limit": 10 }),
    )
    .await?;

    let matches = result["matches"].as_array().expect("matches array");
    assert_eq!(
        matches.len(),
        1,
        "head-by-NK must collapse two revisions to one match"
    );
    let snippet = matches[0]["snippet"].as_str().expect("snippet");
    assert!(snippet.contains("v2"), "head must be the later ingest");
    Ok(())
}

#[tokio::test]
async fn search_chunks_excludes_chunk_when_head_is_tombstone()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    ingest_code_chunk(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/atlas.rs",
        0,
        "fn atlas_edges_v1() {}",
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;
    ingest_code_chunk_tombstone(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/atlas.rs",
        0,
    )
    .await?;

    let result = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "query": "atlas_edges", "limit": 10 }),
    )
    .await?;

    let matches = result["matches"].as_array().expect("matches array");
    assert!(
        matches.is_empty(),
        "tombstoned chunk must not surface via revived earlier revision"
    );
    Ok(())
}

#[tokio::test]
async fn search_chunks_excludes_chunk_when_tombstone_has_no_language_and_filter_is_set()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    let present_chunk = ingest_code_chunk(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/atlas.rs",
        0,
        "fn atlas_edges_v1() {}",
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let tombstone_chunk = ingest_code_chunk_tombstone(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/atlas.rs",
        0,
    )
    .await?;
    assert!(
        tombstone_chunk < present_chunk,
        "fixture must cover deterministic UUID tie-breaker inversion"
    );
    force_same_memory_created_at(
        fixture.pg.pool_for_tests(),
        &[present_chunk, tombstone_chunk],
    )
    .await?;

    let result = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "query": "atlas_edges", "language": "rust", "limit": 10 }),
    )
    .await?;

    let matches = result["matches"].as_array().expect("matches array");
    assert!(
        matches.is_empty(),
        "language filter must not revive a present chunk hidden by a newer no-language tombstone"
    );
    Ok(())
}

#[tokio::test]
async fn search_chunks_includes_calls_edges_when_present() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    let source_chunk = ingest_code_chunk(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/a.rs",
        0,
        "fn a() { b(); }",
    )
    .await?;
    let target_chunk = ingest_code_chunk(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/b.rs",
        0,
        "fn b() {}",
    )
    .await?;
    ingest_calls_edge(
        fixture.pg.pool_for_tests(),
        &owner,
        source_chunk,
        target_chunk,
        "b",
    )
    .await?;

    let result = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "query": "fn a", "include_calls": true }),
    )
    .await?;

    let calls = result["calls_edges"].as_array().expect("calls array");
    assert!(!calls.is_empty(), "calls edge must surface");
    assert_eq!(calls[0]["callee_name"], "b");
    Ok(())
}

#[tokio::test]
async fn search_chunks_supports_exact_substring_and_chunk_type_filter()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    ingest_code_chunk_with_type(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        ChunkFixture {
            repo_id,
            file_path: "src/exact.rs",
            chunk_index: 0,
            text: "mod exact_symbol { fn nested() {} }",
            chunk_type: "module",
        },
    )
    .await?;
    ingest_code_chunk_with_type(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        ChunkFixture {
            repo_id,
            file_path: "src/exact.rs",
            chunk_index: 1,
            text: "fn exact_symbol() {}",
            chunk_type: "function",
        },
    )
    .await?;

    let result = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({
            "query": "exact_symbol()",
            "chunk_type": "function",
            "include_calls": false
        }),
    )
    .await?;

    let matches = result["matches"].as_array().expect("matches array");
    assert_eq!(matches.len(), 1, "chunk_type filter must narrow matches");
    assert_eq!(matches[0]["chunk_type"], "function");
    assert!(
        matches[0]["snippet"]
            .as_str()
            .expect("snippet")
            .contains("exact_symbol()"),
        "exact punctuation substring must match"
    );
    assert_eq!(matches[0]["match_kind"], "text_contains");
    assert_eq!(matches[0]["matched_line"], 1);
    assert!(
        matches[0]["matched_excerpt"]
            .as_str()
            .expect("matched excerpt")
            .contains("exact_symbol()")
    );
    Ok(())
}

#[tokio::test]
async fn open_file_revision_returns_head_with_chunks() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    ingest_file_revision(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/atlas.rs",
        "v1",
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;
    ingest_file_revision(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/atlas.rs",
        "v2",
    )
    .await?;
    ingest_code_chunk(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/atlas.rs",
        0,
        "fn a() {\n    call();\n}",
    )
    .await?;
    ingest_code_chunk(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/atlas.rs",
        1,
        "fn b() {}",
    )
    .await?;

    let test_ctx = ctx(fixture.pg.clone(), owner, registry);
    let repo_handle = test_ctx.format_flavor_object("proxima-code/repo", repo_id, 'R');
    let result = run_tool::<CodeOpenFileRevisionTool>(
        test_ctx,
        json!({ "repo_handle": repo_handle, "file_path": "src/atlas.rs" }),
    )
    .await?;

    assert_eq!(result["revision"]["indexed_commit_sha"], "v2");
    let chunks = result["chunks"].as_array().expect("chunks");
    assert_eq!(chunks.len(), 2);
    assert!(
        chunks[0].get("text").is_none(),
        "default output must not include full chunk text"
    );

    let text_result = run_tool::<CodeOpenFileRevisionTool>(
        ctx(fixture.pg.clone(), owner, registry_for_mcp()),
        json!({
            "repo_handle": repo_id.to_string(),
            "file_path": "src/atlas.rs",
            "include_text": true
        }),
    )
    .await?;

    assert_eq!(text_result["chunks"][0]["text"], "fn a() {\n    call();\n}");
    assert!(
        text_result["chunks"][0].get("text_line_range").is_none(),
        "full include_text remains unwindowed"
    );

    let bounded_result = run_tool::<CodeOpenFileRevisionTool>(
        ctx(fixture.pg.clone(), owner, registry_for_mcp()),
        json!({
            "repo_handle": repo_id.to_string(),
            "file_path": "src/atlas.rs",
            "line_start": 2,
            "line_limit": 1,
            "max_text_bytes": 64
        }),
    )
    .await?;

    let bounded_chunks = bounded_result["chunks"].as_array().expect("chunks");
    assert_eq!(bounded_chunks.len(), 1);
    assert_eq!(bounded_chunks[0]["text"], "    call();");
    assert_eq!(bounded_chunks[0]["text_line_range"][0], 2);
    assert_eq!(bounded_chunks[0]["text_line_range"][1], 2);
    Ok(())
}

#[tokio::test]
async fn open_file_revision_returns_no_chunks_for_tombstone_head()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    ingest_file_revision(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/atlas.rs",
        "v1",
    )
    .await?;
    ingest_code_chunk(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/atlas.rs",
        0,
        "fn atlas_edges_v1() {}",
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;
    ingest_file_revision_tombstone(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/atlas.rs",
        "v2",
    )
    .await?;
    ingest_code_chunk_tombstone(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/atlas.rs",
        0,
    )
    .await?;

    let test_ctx = ctx(fixture.pg.clone(), owner, registry);
    let repo_handle = test_ctx.format_flavor_object("proxima-code/repo", repo_id, 'R');
    let result = run_tool::<CodeOpenFileRevisionTool>(
        test_ctx,
        json!({ "repo_handle": repo_handle, "file_path": "src/atlas.rs" }),
    )
    .await?;

    assert_eq!(result["revision"]["state"], "Tombstone");
    let chunks = result["chunks"].as_array().expect("chunks");
    assert!(
        chunks.is_empty(),
        "tombstone file head must not return stale chunks"
    );
    Ok(())
}

#[tokio::test]
async fn open_file_revision_accepts_raw_repo_uuid() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    ingest_file_revision(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/raw.rs",
        "v1",
    )
    .await?;

    let result = run_tool::<CodeOpenFileRevisionTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "repo_handle": repo_id.to_string(), "file_path": "src/raw.rs" }),
    )
    .await?;

    assert_eq!(result["revision"]["indexed_commit_sha"], "v1");
    Ok(())
}

#[tokio::test]
async fn open_file_revision_accepts_unambiguous_repo_display_name()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();
    register_repo(
        fixture.pg.pool_for_tests(),
        &owner,
        repo_id,
        "/tmp/proxima-mcp-display",
        "Proxima",
    )
    .await?;

    ingest_file_revision(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "src/atlas.rs",
        "v1",
    )
    .await?;

    let result = run_tool::<CodeOpenFileRevisionTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "repo_handle": "proxima", "file_path": "src/atlas.rs" }),
    )
    .await?;

    assert_eq!(result["revision"]["indexed_commit_sha"], "v1");
    assert_eq!(
        result["revision"]["repo_handle"].as_str().expect("handle"),
        format!("R:{repo_id}")
    );
    Ok(())
}

#[tokio::test]
async fn search_commits_unions_commit_and_summary_legs() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    ingest_commit(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "deadbeef",
        "fix atlas edges",
    )
    .await?;
    ingest_commit_summary(
        fixture.pg.pool_for_tests(),
        &owner,
        repo_id,
        "deadbeef",
        "Hardens the atlas edge cap.",
        &["src/atlas.rs"],
        "Refactor",
    )
    .await?;

    let result = run_tool::<CodeSearchCommitsTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "query": "atlas", "limit": 10 }),
    )
    .await?;

    assert!(
        !result["commits"].as_array().expect("commits").is_empty(),
        "commit leg"
    );
    assert!(
        !result["summaries"]
            .as_array()
            .expect("summaries")
            .is_empty(),
        "summary leg"
    );
    Ok(())
}

#[tokio::test]
async fn retry_execution_request_succeeds_with_target_perspective()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone());
    let registry = registry_for_mcp();

    let shell_self = seed_perspective(&fixture.pg, &owner, "Shell author").await?;

    // A prior execution-request Fact + sidecar to retry.
    let repo_id = Uuid::now_v7();
    let prior = ingest_execution_request_fixture(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "prior",
    )
    .await?;

    let target = seed_perspective(&fixture.pg, &owner, "Retry Worker").await?;

    let result = run_tool::<CodeRetryExecutionRequestTool>(
        shell_ctx(
            fixture.pg.clone(),
            owner,
            registry,
            MemoryId::new(shell_self),
        ),
        json!({
            "prior_execution_request": format!("F:{prior}"),
            "target_perspective": format!("P:{target}"),
            "idempotency_key": "retry-1",
        }),
    )
    .await?;

    assert_eq!(result["idempotent_replay"], false);
    assert!(
        result["handle"].as_str().expect("handle").starts_with("F:"),
        "new request is a Fact handle"
    );
    assert!(
        result["target_edge_handle"]
            .as_str()
            .expect("target edge")
            .starts_with("E:"),
        "retry assigns the worker via a target edge"
    );
    assert!(
        result["authored_edge_handle"].as_str().is_some(),
        "shell author edge present"
    );

    // The retry request actually landed under its idempotency key.
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_code.work_requested_v1
         WHERE repo_id = $1 AND request_key = $2",
    )
    .bind(repo_id)
    .bind("retry-1")
    .fetch_one(fixture.pg.pool_for_tests())
    .await?;
    assert_eq!(count, 1, "retry request row persisted");
    Ok(())
}

#[tokio::test]
async fn retry_execution_request_uses_owner_write_authority()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone());
    let registry = registry_for_mcp();

    let shell_self = seed_perspective(&fixture.pg, &owner, "Shell author").await?;
    let repo_id = Uuid::now_v7();
    let prior = ingest_execution_request_fixture(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "prior-no-master",
    )
    .await?;
    let target = seed_perspective(&fixture.pg, &owner, "Retry Worker").await?;

    let result = run_tool::<CodeRetryExecutionRequestTool>(
        shell_ctx(
            fixture.pg.clone(),
            owner,
            registry,
            MemoryId::new(shell_self),
        ),
        json!({
            "prior_execution_request": format!("F:{prior}"),
            "target_perspective": format!("P:{target}"),
            "idempotency_key": "retry-no-master",
        }),
    )
    .await?;

    assert_eq!(result["idempotent_replay"], false);
    assert!(
        result["handle"].as_str().expect("handle").starts_with("F:"),
        "authorized non-master retry still writes a Fact"
    );
    Ok(())
}

#[tokio::test]
async fn emit_execution_plan_uses_abstraction_proof_source()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();
    register_repo(
        fixture.pg.pool_for_tests(),
        &owner,
        repo_id,
        "/tmp/proxima-plan-proof",
        "Plan Proof Repo",
    )
    .await?;
    let shell_self = seed_perspective(&fixture.pg, &owner, "Planner Root").await?;
    let goal_activated = seed_active_goal_activation(&fixture.pg, &owner, shell_self).await?;
    let plan_source = abstraction_memory(
        fixture.pg.pool_for_tests(),
        &owner,
        "test/plan-source-v1",
        "planning context",
    )
    .await?;

    let output = run_tool::<CodeEmitExecutionPlanTool>(
        shell_ctx(
            fixture.pg.clone(),
            owner,
            registry,
            MemoryId::new(shell_self),
        ),
        json!({
            "repo_handle": repo_id.to_string(),
            "goal_activated_memory": format!("F:{goal_activated}"),
            "plan_source_memory": format!("A:{plan_source}"),
            "plan_key": "proof-plan-1",
            "plan_summary": "Plan from Abstraction proof source.",
            "evidence": [],
            "items": [{
                "kind": "implementation",
                "key": "work-1",
                "title": "Implement proof-aware plan",
                "instructions": "Use an Abstraction source for the AtoA plan derivation.",
                "idempotency_key": "work-1"
            }]
        }),
    )
    .await?;

    let plan_handle = output["plan_handle"].as_str().expect("plan handle");
    let plan_id = Uuid::parse_str(
        plan_handle
            .strip_prefix("A:")
            .expect("prefixed Abstraction handle"),
    )?;
    let edge: (String, String, Uuid) = sqlx::query_as(
        "SELECT relation, authorship_kind::text, target_memory_id
           FROM proxima_core.edges
          WHERE source_memory_id = $1
            AND relation = 'core/derived-from'
          LIMIT 1",
    )
    .bind(plan_id)
    .fetch_one(fixture.pg.pool_for_tests())
    .await?;
    assert_eq!(edge.0, "core/derived-from");
    assert_eq!(edge.1, "OperatorAtoA");
    assert_eq!(edge.2, plan_source);

    Ok(())
}

#[tokio::test]
async fn retry_execution_request_rejects_unknown_target_perspective()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone());
    let registry = registry_for_mcp();

    let shell_self = seed_perspective(&fixture.pg, &owner, "Shell author").await?;

    let repo_id = Uuid::now_v7();
    let prior = ingest_execution_request_fixture(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        repo_id,
        "prior",
    )
    .await?;

    let ctx = shell_ctx(
        fixture.pg.clone(),
        owner,
        registry,
        MemoryId::new(shell_self),
    );
    let args: <CodeRetryExecutionRequestTool as McpTool>::Args = serde_json::from_value(json!({
        "prior_execution_request": format!("F:{prior}"),
        "target_perspective": format!("P:{}", Uuid::now_v7()),
        "idempotency_key": "retry-1",
    }))?;
    let err = CodeRetryExecutionRequestTool::call(ctx, args)
        .await
        .expect_err("unknown target perspective must reject the retry");
    match err {
        McpToolError::InvalidInput(message) => assert!(
            message.contains("target_perspective not found"),
            "unexpected message: {message}"
        ),
        other => panic!("expected InvalidInput, got {other:?}"),
    }

    // Nothing was authored for the rejected retry.
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_code.work_requested_v1
         WHERE repo_id = $1 AND request_key = $2",
    )
    .bind(repo_id)
    .bind("retry-1")
    .fetch_one(fixture.pg.pool_for_tests())
    .await?;
    assert_eq!(count, 0, "rejected retry left no request row");
    Ok(())
}

/// Every paged read in this flavor rejects `limit: 0` with the same
/// error.
///
/// They used to disagree three ways on the same nonsense input:
/// `search_chunks` rejected it, `search_commits` returned `{"commits":
/// []}` — a well-formed empty page no client can tell apart from "nothing
/// matched" — and `list_repos` clamped to 1 and answered a question that
/// was not asked. The engine has rejected `limit == 0` all along
/// (`engine::query`, `engine::read_verbs`); the tool layer was the only
/// place that hid it.
#[tokio::test]
async fn every_paged_read_rejects_a_zero_limit_the_same_way()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();

    let chunks = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "query": "anything", "limit": 0 }),
    )
    .await
    .expect_err("search_chunks must reject limit: 0");
    let commits = run_tool::<CodeSearchCommitsTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "query": "anything", "limit": 0 }),
    )
    .await
    .expect_err("search_commits must reject limit: 0, not answer with an empty page");
    let repos = run_tool::<CodeListReposTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "limit": 0 }),
    )
    .await
    .expect_err("list_repos must reject limit: 0, not clamp it to 1");

    for (tool, err) in [
        ("search_chunks", &chunks),
        ("search_commits", &commits),
        ("list_repos", &repos),
    ] {
        assert!(
            err.to_string().contains("limit must be at least 1"),
            "{tool} rejected for the wrong reason: {err}"
        );
    }

    // And the neighbouring value still works, so the guard is a floor and
    // not an accidental ban on small pages.
    let one = run_tool::<CodeListReposTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "limit": 1 }),
    )
    .await?;
    assert!(one["repos"].is_array(), "limit: 1 must still answer: {one}");
    Ok(())
}

async fn run_tool<T: McpTool>(
    ctx: McpToolCtx,
    args: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let typed: T::Args = serde_json::from_value(args)?;
    let output = T::call(ctx, typed).await?;
    Ok(serde_json::to_value(output)?)
}

fn ctx(pg: PgStorage, owner: Owner, registry: Arc<FlavorRegistryFrozen>) -> McpToolCtx {
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
    let engine = Arc::new(engine_for_test(pg));
    McpToolCtx {
        owner,
        authz,
        registry,
        author: McpAuthorContext {
            model_id: "test/0".into(),
            client_name: "test".into(),
            client_version: "0".into(),
            caller_self_perspective: None,
        },
        caller_self_perspective: None,
        extensions: McpToolExtensions::with(store),
        engine: Some(engine),
    }
}

/// Shell-author context: carries a `caller_self_perspective` — the shape
/// `McpToolHost` builds for `code_retry_execution_request` callers.
fn shell_ctx(
    pg: PgStorage,
    owner: Owner,
    registry: Arc<FlavorRegistryFrozen>,
    caller_self_perspective: MemoryId,
) -> McpToolCtx {
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
    let engine = Arc::new(engine_for_test(pg));
    McpToolCtx {
        owner,
        authz,
        registry,
        author: McpAuthorContext {
            model_id: "test/0".into(),
            client_name: "test".into(),
            client_version: "0".into(),
            caller_self_perspective: Some(caller_self_perspective),
        },
        caller_self_perspective: Some(caller_self_perspective),
        extensions: McpToolExtensions::with(store),
        engine: Some(engine),
    }
}

async fn seed_active_goal_activation(
    pg: &PgStorage,
    owner: &Owner,
    self_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let goal_id = Uuid::now_v7();
    let memory_id = Uuid::now_v7();
    let edge_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, owner_kind, owner_id, schema_id, schema_version, title, text, payload,
             state, authorship_kind, request_id, idempotency_key)
         VALUES ($1, $2, $3, 'core/simple-text-v1', 1, 'Goal', 'Goal', '{}'::bytea,
                 'Active', 'User', $4, md5($2::text || ':' || $3::text || ':' || $4))",
    )
    .bind(goal_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(format!("goal-{goal_id}"))
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, text)
         VALUES ($1, $2, $3, 'core/goal-activated-v1', 1, 'goal activated')",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.goal_activated_v1
            (memory_id, goal_id, transitioned_at)
         VALUES ($1, $2, now())",
    )
    .bind(memory_id)
    .bind(goal_id)
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, owner_kind, owner_id, relation, relation_class, source_kind, source_goal_id,
             target_kind, target_memory_id, authorship_kind)
         VALUES ($1, $2, $3, $4, 'Causal', 'Goal', $5,
                 'Perspective', $6, 'PerspectiveGoalLink')",
    )
    .bind(edge_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(CORE_INSPIRES_RELATION)
    .bind(goal_id)
    .bind(self_id)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(memory_id)
}

async fn seed_perspective(
    pg: &PgStorage,
    owner: &Owner,
    label: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/mcp-perspective-v1', 1, 'Perspective', $4,
                 'AtoP', '00000000-0000-0000-0000-000000000461'::uuid,
                 '00000000-0000-0000-0000-000000000462'::uuid, NULL,
                 'test/0', 'test')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(label)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(memory_id)
}

/// Mint a prior execution-request Fact + sidecar row that a retry targets.
async fn ingest_execution_request_fixture(
    pool: &PgPool,
    engine: &Engine,
    owner: Owner,
    repo_id: Uuid,
    request_key: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let payload = format!("execution-request:{repo_id}:{request_key}");
    let memory_id = fact_memory(
        engine,
        owner,
        ExecutionRequestV1::SCHEMA_ID,
        payload.as_bytes(),
    )
    .await?;
    sqlx::query(
        "INSERT INTO proxima_code.work_requested_v1
            (memory_id, repo_id, title, instructions, request_key)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(memory_id)
    .bind(repo_id)
    .bind("Prior execution request")
    .bind("Implement the prior request; this run is being retried.")
    .bind(request_key)
    .execute(pool)
    .await?;
    Ok(memory_id)
}

fn registry_for_mcp() -> Arc<FlavorRegistryFrozen> {
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry).unwrap();
    Arc::new(registry.freeze_or_panic_for_tests())
}

fn registry_for_engine() -> FlavorRegistryFrozen {
    let mut flavor = FlavorRegistry::new();
    proxima_code::register(&mut flavor).unwrap();
    flavor.freeze_or_panic_for_tests().with_additional_schemas([
        SchemaInfo::opaque(
            SchemaId::new("test/cited_blob".into()),
            SchemaVersion::new(1),
            PayloadKind::CitedObject,
        ),
        SchemaInfo::opaque(
            SchemaId::new("test/citation_blob".into()),
            SchemaVersion::new(1),
            PayloadKind::CitationMapping,
        ),
    ])
}

fn init_git_repo_with_commit(
    repo: &std::path::Path,
    relative_path: &str,
    contents: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    init_git_repo_with_files(repo, &[(relative_path, contents)])
}

fn init_git_repo_with_files(
    repo: &std::path::Path,
    files: &[(&str, &str)],
) -> Result<(), Box<dyn std::error::Error>> {
    run_git(repo, &["init"])?;
    for (relative_path, contents) in files {
        let file_path = repo.join(relative_path);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file_path, contents)?;
    }
    run_git(repo, &["add", "."])?;
    run_git(
        repo,
        &[
            "-c",
            "user.name=Proxima Test",
            "-c",
            "user.email=proxima-test@example.com",
            "commit",
            "-m",
            "initial snapshot",
        ],
    )?;
    Ok(())
}

fn run_git(repo: &std::path::Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

fn engine_for_test(pg: PgStorage) -> Engine {
    Engine::new(registry_for_engine()).with_storage_ports(Arc::new(pg).storage_ports())
}

/// A deterministic stand-in for an embedding model.
///
/// Every text that mentions one of `TOPIC_MARKERS` embeds to the same basis
/// vector; everything else embeds to a different one. Cosine similarity is
/// then exactly 1.0 within a topic and 0.0 across topics, which is what
/// makes the semantic assertions below about *Proxima's* behaviour rather
/// than about how well some real model happens to score. The markers are
/// chosen so that a query and its intended chunk share no content word, so
/// no lexical arm can reach the answer.
#[derive(Debug)]
struct TopicEmbedding;

const TOPIC_MARKERS: [&str; 2] = ["halt_iteration", "stop going round again"];

#[async_trait::async_trait]
impl proxima_core::llm::EmbeddingClient for TopicEmbedding {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, proxima_core::llm::LlmError> {
        let mut embedding = vec![0.0; proxima_core::llm::EMBEDDING_DIM];
        let on_topic = TOPIC_MARKERS.iter().any(|marker| text.contains(marker));
        embedding[usize::from(!on_topic)] = 1.0;
        Ok(embedding)
    }

    fn model_id(&self) -> &'static str {
        "test-topic-embed"
    }

    fn dim(&self) -> usize {
        proxima_core::llm::EMBEDDING_DIM
    }
}

/// `ctx`, but the engine carries an embedding model, so chunks are embedded
/// on ingest and the semantic arm has something to search.
fn embedding_ctx(pg: PgStorage, owner: Owner, registry: Arc<FlavorRegistryFrozen>) -> McpToolCtx {
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
    let engine = Arc::new(engine_for_test(pg).with_embed(Arc::new(TopicEmbedding)));
    McpToolCtx {
        owner,
        authz,
        registry,
        author: McpAuthorContext {
            model_id: "test/0".into(),
            client_name: "test".into(),
            client_version: "0".into(),
            caller_self_perspective: None,
        },
        caller_self_perspective: None,
        extensions: McpToolExtensions::with(store),
        engine: Some(engine),
    }
}

/// Register and ingest a two-file repo whose second file is on-topic for
/// [`TopicEmbedding`] while sharing no content word with the topic query.
async fn ingest_topic_repo(
    fixture: &TestDb,
    owner: Owner,
    registry: &Arc<FlavorRegistryFrozen>,
    temp: &TempDir,
) -> Result<String, Box<dyn std::error::Error>> {
    init_git_repo_with_files(
        temp.path(),
        &[
            (
                "docs/notes.md",
                "# Notes\n\nThis document is about packaging, releases and changelogs.\n",
            ),
            (
                "src/control.rs",
                "pub fn halt_iteration(count: usize) -> bool {\n\
                 \x20   count > 3\n\
                 }\n",
            ),
        ],
    )?;
    let registered = run_tool::<CodeRegisterRepoTool>(
        embedding_ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "Topic Repo" }),
    )
    .await?;
    let repo_handle = registered["repo"]["repo_id"]
        .as_str()
        .expect("repo_id")
        .to_string();
    run_tool::<CodeIngestHeadSnapshotTool>(
        embedding_ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": repo_handle }),
    )
    .await?;

    // Chunks are not embedded by the write that creates them; a host drains
    // the durable job queue afterwards. Doing it here is what a deployment
    // does, and it is also the reason `degraded_to_lexical` matters: between
    // ingest and this drain, a freshly indexed repository is lexical-only.
    let engine = engine_for_test(fixture.pg.clone()).with_embed(Arc::new(TopicEmbedding));
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    engine
        .backfill_missing_embeddings(&authz, &owner, 1_000)
        .await?;
    let drained = engine.drain_embedding_jobs(1_000).await?;
    assert!(
        drained.processed > 0 && drained.failed == 0,
        "the semantic assertions need embedded chunks; drained {drained:?}"
    );
    Ok(repo_handle)
}

/// The reason the semantic arm exists: a question phrased in words the
/// answer does not contain.
///
/// "stop going round again" shares no content word with
/// `pub fn halt_iteration(count: usize) -> bool`, so every lexical band is
/// empty and both substring arms miss. Lexical search cannot answer this
/// and the assertion below proves it does not; semantic search returns the
/// function.
#[tokio::test]
async fn semantic_search_finds_a_chunk_sharing_no_word_with_the_query()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    ingest_topic_repo(&fixture, owner, &registry, &temp).await?;

    let query = "stop going round again";
    let lexical = run_tool::<CodeSearchChunksTool>(
        embedding_ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "query": query, "mode": "lexical", "include_calls": false }),
    )
    .await?;
    let lexical_hits = lexical["matches"]
        .as_array()
        .expect("matches")
        .iter()
        .filter(|m| m["file_path"] == "src/control.rs")
        .count();
    assert_eq!(
        lexical_hits, 0,
        "the premise of this test is that no lexical arm reaches the answer; \
         got {:?}",
        lexical["matches"]
    );

    let semantic = run_tool::<CodeSearchChunksTool>(
        embedding_ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "query": query, "mode": "semantic", "include_calls": false }),
    )
    .await?;
    let matches = semantic["matches"].as_array().expect("matches");
    assert_eq!(
        matches[0]["file_path"], "src/control.rs",
        "semantic search must reach the chunk lexical search cannot; got {matches:?}"
    );
    assert!(
        matches[0]["similarity_score"].as_f64().unwrap_or_default() > 0.9,
        "similarity must be reported, got {:?}",
        matches[0]["similarity_score"]
    );
    assert_eq!(semantic["degraded_to_lexical"], json!(false));

    // Hybrid inherits the recall without being asked for it — the default.
    let hybrid = run_tool::<CodeSearchChunksTool>(
        embedding_ctx(fixture.pg.clone(), owner, registry),
        json!({ "query": query, "include_calls": false }),
    )
    .await?;
    assert_eq!(hybrid["mode"], json!("hybrid"));
    assert_eq!(hybrid["degraded_to_lexical"], json!(false));
    assert!(
        hybrid["matches"]
            .as_array()
            .expect("matches")
            .iter()
            .any(|m| m["file_path"] == "src/control.rs"),
        "the default mode must find it too; got {:?}",
        hybrid["matches"]
    );
    Ok(())
}

/// An exact path lookup is the one case where the caller has said precisely
/// what they want. Rank fusion on its own would let a chunk the embedding
/// model is confident about outrank it, so the literal arms survive fusion
/// as an absolute prefix — asserted end to end, not just over the fusion
/// function.
#[tokio::test]
async fn an_exact_path_lookup_outranks_the_semantic_favourite()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    ingest_topic_repo(&fixture, owner, &registry, &temp).await?;

    // `docs/notes.md` is off-topic for the embedding model, so the semantic
    // arm ranks `src/control.rs` first; the exact path must still win.
    let found = run_tool::<CodeSearchChunksTool>(
        embedding_ctx(fixture.pg.clone(), owner, registry),
        json!({ "query": "docs/notes.md", "include_calls": false }),
    )
    .await?;
    let matches = found["matches"].as_array().expect("matches");
    assert_eq!(
        matches[0]["file_path"], "docs/notes.md",
        "an exact path match must outrank the embedding model's favourite; got {matches:?}"
    );
    Ok(())
}

/// The contract for a deployment with no embedding model configured, which
/// is every deployment that has not set one up: `hybrid` — the default —
/// still answers, ranked lexically, and reports that it did. Nothing about
/// the default silently starts requiring an LLM.
#[tokio::test]
async fn hybrid_without_an_embedding_model_answers_lexically_and_says_so()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    ingest_topic_repo(&fixture, owner, &registry, &temp).await?;

    // `ctx`, not `embedding_ctx`: no embedding client on this engine.
    let found = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "query": "halt_iteration", "include_calls": false }),
    )
    .await?;
    assert_eq!(found["mode"], json!("hybrid"));
    assert_eq!(found["degraded_to_lexical"], json!(true));
    assert_eq!(
        found["matches"].as_array().expect("matches")[0]["file_path"],
        "src/control.rs",
        "a degraded hybrid search still answers lexically"
    );

    // Pure semantic has no other arm, so it refuses rather than quietly
    // answering a different question.
    let refused = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "query": "halt_iteration", "mode": "semantic", "include_calls": false }),
    )
    .await
    .expect_err("semantic search without an embedding model must fail");
    let message = refused.to_string();
    assert!(
        message.contains("no embedding model is configured"),
        "the error must name the cause and the way out; got {message}"
    );
    Ok(())
}

fn fact_draft(_owner: Owner, schema_id: &str, payload: &[u8]) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new(schema_id.into()),
        schema_version: SchemaVersion::new(1),
        payload: payload.to_vec(),
        rendered_text: None,
        lexical_language: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new("test/source"),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new("test/cited_blob".into()),
                schema_version: SchemaVersion::new(1),
                content_hash: blake3::hash(payload).into(),
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new("test/citation_blob".into()),
                schema_version: SchemaVersion::new(1),
            },
        }),
    }
}

async fn fact_memory(
    engine: &Engine,
    owner: Owner,
    schema_id: &str,
    payload: &[u8],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    Ok(engine
        .fact_ingest(
            &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::HostBearer),
            fact_draft(owner, schema_id, payload),
        )
        .await?
        .memory_id
        .into_inner())
}

async fn abstraction_memory(
    pool: &PgPool,
    owner: &Owner,
    schema_id: &str,
    payload: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, payload.as_bytes());
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id,
             prompt_version)
         VALUES ($1, $2, $3, $4, 1, 'Abstraction', $5,
             'AtoA', '00000000-0000-0000-0000-000000000491'::uuid,
             $1, NULL,
             'test/code-index', 'test')
         ON CONFLICT (memory_id) DO NOTHING",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(schema_id)
    .bind(payload)
    .execute(pool)
    .await?;
    Ok(memory_id)
}

/// A repo handle that names no repository is an error on every tool that
/// takes one — not silence on some of them.
///
/// `ingest_head_snapshot` always said so, because it looks the repo record
/// up for its own reasons. The read tools did not: a handle or bare UUID
/// short-circuited on *parse*, so a stale handle after `erase_repo`, a
/// typo, or another owner's id resolved happily and the reads returned
/// `matches: []`, `commits: []`, `revision: null`. None of that is
/// distinguishable from "this code is not indexed", which is the wrong
/// thing for an agent to conclude. `search_chunks` even disagreed with
/// itself — a bad display name errored while a bad id did not.
#[tokio::test]
async fn an_unknown_repo_handle_is_an_error_on_every_tool_that_takes_one()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let engine = engine_for_test(fixture.pg.clone());

    // A real repository exists, so an empty answer would be about the
    // handle rather than about an empty index.
    let real_repo = Uuid::now_v7();
    ingest_file_revision(
        fixture.pg.pool_for_tests(),
        &engine,
        owner,
        real_repo,
        "src/real.rs",
        "v1",
    )
    .await?;

    let absent = Uuid::now_v7().to_string();
    for (tool, args) in [
        (
            "search_chunks",
            json!({ "query": "real", "repo_handle": absent, "include_calls": false }),
        ),
        (
            "search_commits",
            json!({ "query": "real", "repo_handle": absent }),
        ),
        (
            "open_file_revision",
            json!({ "repo_handle": absent, "file_path": "src/real.rs" }),
        ),
    ] {
        let err = match tool {
            "search_chunks" => run_tool::<CodeSearchChunksTool>(
                ctx(fixture.pg.clone(), owner, registry.clone()),
                args,
            )
            .await
            .err(),
            "search_commits" => run_tool::<CodeSearchCommitsTool>(
                ctx(fixture.pg.clone(), owner, registry.clone()),
                args,
            )
            .await
            .err(),
            _ => run_tool::<CodeOpenFileRevisionTool>(
                ctx(fixture.pg.clone(), owner, registry.clone()),
                args,
            )
            .await
            .err(),
        };
        let err = err.unwrap_or_else(|| panic!("{tool} must reject an unknown repo handle"));
        assert!(
            err.to_string().contains("repo_handle not found"),
            "{tool} must say the handle is unknown, got: {err}"
        );
    }

    // The same handle, well-formed and real, still works.
    let found = run_tool::<CodeOpenFileRevisionTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "repo_handle": real_repo.to_string(), "file_path": "src/real.rs" }),
    )
    .await?;
    assert_eq!(found["revision"]["indexed_commit_sha"], "v1");
    Ok(())
}

/// Give `repo_id` a registry row, the way `register_repo` would.
///
/// These fixtures write sidecar rows straight to Postgres, so without this
/// they describe a repository that has chunks and no registry entry — a
/// state the tool surface cannot produce. `register_repo` creates the row,
/// `ingest_head_snapshot` refuses to run without it, and `erase_repo`
/// removes the row and tombstones the chunks together. Handle resolution
/// checks that row, so a fixture missing it is testing a shape that does
/// not occur rather than the behaviour it means to test.
async fn register_repo_row(
    pool: &PgPool,
    owner: Owner,
    repo_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    sqlx::query(
        "INSERT INTO proxima_code.repos
            (owner_kind, owner_id, repo_id, canonical_path, display_name, created_at)
         VALUES ($1, $2, $3, $4, $5, now())
         ON CONFLICT (owner_kind, owner_id, repo_id) DO NOTHING",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(repo_id)
    .bind(format!("/fixtures/{repo_id}"))
    .bind(format!("fixture-{repo_id}"))
    .execute(pool)
    .await?;
    Ok(())
}

async fn ingest_file_revision(
    pool: &PgPool,
    engine: &Engine,
    owner: Owner,
    repo_id: Uuid,
    file_path: &str,
    indexed_commit_sha: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let payload = format!("{file_path}:{indexed_commit_sha}");
    register_repo_row(pool, owner, repo_id).await?;
    let memory_id =
        fact_memory(engine, owner, FileRevisionV1::SCHEMA_ID, payload.as_bytes()).await?;
    sqlx::query(
        "INSERT INTO proxima_code.file_revision_v1
            (memory_id, repo_id, file_path, language, content_sha256,
             size_bytes, indexed_commit_sha, state)
         VALUES ($1, $2, $3, 'rust', $4, $5, $6, 'Present')",
    )
    .bind(memory_id)
    .bind(repo_id)
    .bind(file_path)
    .bind(blake3::hash(payload.as_bytes()).as_bytes().to_vec())
    .bind(i64::try_from(payload.len())?)
    .bind(indexed_commit_sha)
    .execute(pool)
    .await?;
    Ok(memory_id)
}

async fn ingest_file_revision_tombstone(
    pool: &PgPool,
    engine: &Engine,
    owner: Owner,
    repo_id: Uuid,
    file_path: &str,
    indexed_commit_sha: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let payload = format!("{file_path}:{indexed_commit_sha}:tombstone");
    let memory_id =
        fact_memory(engine, owner, FileRevisionV1::SCHEMA_ID, payload.as_bytes()).await?;
    sqlx::query(
        "INSERT INTO proxima_code.file_revision_v1
            (memory_id, repo_id, file_path, language, content_sha256,
             size_bytes, indexed_commit_sha, state)
         VALUES ($1, $2, $3, NULL, $4, 0, $5, 'Tombstone')",
    )
    .bind(memory_id)
    .bind(repo_id)
    .bind(file_path)
    .bind([0u8; 32].to_vec())
    .bind(indexed_commit_sha)
    .execute(pool)
    .await?;
    Ok(memory_id)
}

async fn ingest_code_chunk(
    pool: &PgPool,
    engine: &Engine,
    owner: Owner,
    repo_id: Uuid,
    file_path: &str,
    chunk_index: i32,
    text: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    ingest_code_chunk_with_type(
        pool,
        engine,
        owner,
        ChunkFixture {
            repo_id,
            file_path,
            chunk_index,
            text,
            chunk_type: "function",
        },
    )
    .await
}

#[derive(Debug, Clone, Copy)]
struct ChunkFixture<'a> {
    repo_id: Uuid,
    file_path: &'a str,
    chunk_index: i32,
    text: &'a str,
    chunk_type: &'a str,
}

async fn ingest_code_chunk_with_type(
    pool: &PgPool,
    engine: &Engine,
    owner: Owner,
    chunk: ChunkFixture<'_>,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let file_revision =
        ensure_present_file_revision(pool, engine, owner, chunk.repo_id, chunk.file_path).await?;
    let payload = format!("{}:{}:{}", chunk.file_path, chunk.chunk_index, chunk.text);
    let memory_id = code_chunk_memory(pool, &owner, &payload).await?;
    let line_count = i64::try_from(chunk.text.lines().count().max(1))?;
    sqlx::query(
        "INSERT INTO proxima_code.code_chunk_v1
            (memory_id, repo_id, file_path, chunk_index, text, language,
             chunk_type, byte_range_start, byte_range_end,
             line_range_start, line_range_end, state)
         VALUES ($1, $2, $3, $4, $5, 'rust',
             $6, 0, $7, 1, $8, 'Present')",
    )
    .bind(memory_id)
    .bind(chunk.repo_id)
    .bind(chunk.file_path)
    .bind(chunk.chunk_index)
    .bind(chunk.text)
    .bind(chunk.chunk_type)
    .bind(i64::try_from(chunk.text.len())?)
    .bind(line_count)
    .execute(pool)
    .await?;
    insert_derived_from_edge(pool, &owner, memory_id, file_revision).await?;
    Ok(memory_id)
}

async fn ingest_code_chunk_tombstone(
    pool: &PgPool,
    engine: &Engine,
    owner: Owner,
    repo_id: Uuid,
    file_path: &str,
    chunk_index: i32,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let file_revision =
        ensure_tombstone_file_revision(pool, engine, owner, repo_id, file_path).await?;
    let payload = format!("{file_path}:{chunk_index}:tombstone");
    let memory_id = code_chunk_memory(pool, &owner, &payload).await?;
    sqlx::query(
        "INSERT INTO proxima_code.code_chunk_v1
            (memory_id, repo_id, file_path, chunk_index, text, language,
             chunk_type, byte_range_start, byte_range_end,
             line_range_start, line_range_end, state)
         VALUES ($1, $2, $3, $4, '', NULL,
             'function', 0, 0, 1, 1, 'Tombstone')",
    )
    .bind(memory_id)
    .bind(repo_id)
    .bind(file_path)
    .bind(chunk_index)
    .execute(pool)
    .await?;
    insert_derived_from_edge(pool, &owner, memory_id, file_revision).await?;
    Ok(memory_id)
}

async fn latest_file_revision(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
    file_path: &str,
) -> Result<Option<(Uuid, FileState)>, Box<dyn std::error::Error>> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    Ok(sqlx::query_as(
        "SELECT fr.memory_id, fr.state
           FROM proxima_code.file_revision_v1 fr
           JOIN proxima_core.memories m USING (memory_id)
           JOIN proxima_core.fact_receipts r USING (receipt_id)
          WHERE m.owner_kind = $1
            AND m.owner_id IS NOT DISTINCT FROM $2
            AND fr.repo_id = $3
            AND fr.file_path = $4
          ORDER BY r.source_batch_id DESC
          LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(repo_id)
    .bind(file_path)
    .fetch_optional(pool)
    .await?)
}

async fn ensure_present_file_revision(
    pool: &PgPool,
    engine: &Engine,
    owner: Owner,
    repo_id: Uuid,
    file_path: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    if let Some((memory_id, FileState::Present)) =
        latest_file_revision(pool, &owner, repo_id, file_path).await?
    {
        return Ok(memory_id);
    }
    ingest_file_revision(
        pool,
        engine,
        owner,
        repo_id,
        file_path,
        &format!("fixture-present-{}", Uuid::now_v7()),
    )
    .await
}

async fn ensure_tombstone_file_revision(
    pool: &PgPool,
    engine: &Engine,
    owner: Owner,
    repo_id: Uuid,
    file_path: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    if let Some((memory_id, FileState::Tombstone)) =
        latest_file_revision(pool, &owner, repo_id, file_path).await?
    {
        return Ok(memory_id);
    }
    ingest_file_revision_tombstone(
        pool,
        engine,
        owner,
        repo_id,
        file_path,
        &format!("fixture-tombstone-{}", Uuid::now_v7()),
    )
    .await
}

async fn code_chunk_memory(
    pool: &PgPool,
    owner: &Owner,
    payload: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, payload.as_bytes());
    let source_batch_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.source_batches
            (id, source_id, owner_kind, owner_id, closed_at)
         VALUES ($1, 'test/code-index', $2, $3, now())",
    )
    .bind(source_batch_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id,
             prompt_version)
         VALUES ($1, $2, $3, $4, 1, 'Abstraction', $5,
             'FtoA', '00000000-0000-0000-0000-000000000491'::uuid,
             $1, $6,
             'test/code-index', 'test')
         ON CONFLICT (memory_id) DO NOTHING",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(<CodeChunkV1 as AbstractionPayload>::SCHEMA_ID)
    .bind(payload)
    .bind(source_batch_id)
    .execute(pool)
    .await?;
    Ok(memory_id)
}

async fn insert_derived_from_edge(
    pool: &PgPool,
    owner: &Owner,
    chunk_memory_id: Uuid,
    file_revision_memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class, source_kind, source_memory_id,
             target_kind, target_memory_id, authorship_kind, authorship_owner_memory_id,
             owner_kind, owner_id)
         VALUES ($1, $2, 'Provenance', 'Abstraction', $3,
             'Fact', $4, 'OperatorFtoA', $3, $5, $6)",
    )
    .bind(Uuid::now_v7())
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(chunk_memory_id)
    .bind(file_revision_memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn force_same_memory_created_at(
    pool: &PgPool,
    memory_ids: &[Uuid],
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "UPDATE proxima_core.memories
            SET created_at = '2026-01-01 00:00:00+00'::timestamptz
          WHERE memory_id = ANY($1)",
    )
    .bind(memory_ids)
    .execute(pool)
    .await?;
    Ok(())
}

async fn ingest_commit(
    pool: &PgPool,
    engine: &Engine,
    owner: Owner,
    repo_id: Uuid,
    sha: &str,
    message: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let payload = format!("{sha}:{message}");
    let memory_id = fact_memory(engine, owner, CommitV1::SCHEMA_ID, payload.as_bytes()).await?;
    let now = time::OffsetDateTime::now_utc();
    sqlx::query(
        "INSERT INTO proxima_code.commit_v1
            (memory_id, repo_id, sha, parents, author_name, author_email,
             author_time, committer_name, committer_email, committer_time, message)
         VALUES ($1, $2, $3, ARRAY[]::text[], 'Ada', 'ada@example.test',
             $4, 'Ada', 'ada@example.test', $4, $5)",
    )
    .bind(memory_id)
    .bind(repo_id)
    .bind(sha)
    .bind(now)
    .bind(message)
    .execute(pool)
    .await?;
    Ok(memory_id)
}

async fn ingest_commit_summary(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
    commit_sha: &str,
    summary: &str,
    key_files: &[&str],
    change_kind: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, $4, 1, $5, $6,
             $7, '00000000-0000-0000-0000-000000000463'::uuid,
             '00000000-0000-0000-0000-000000000464'::uuid, NULL,
             'test/0', 'test')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(proxima_code::CommitSummaryV1::schema_id().into_inner())
    .bind(proxima_core::EntityKind::Abstraction)
    .bind(summary)
    .bind(proxima_core::MemoryOperatorKind::AtoA)
    .execute(pool)
    .await?;

    let files: Vec<String> = key_files.iter().map(|file| (*file).to_string()).collect();
    sqlx::query(
        "INSERT INTO proxima_code.commit_summary_v1
            (memory_id, repo_id, commit_sha, summary, key_files, change_kind)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(memory_id)
    .bind(repo_id)
    .bind(commit_sha)
    .bind(summary)
    .bind(files)
    .bind(change_kind)
    .execute(pool)
    .await?;
    Ok(memory_id)
}

async fn ingest_calls_edge(
    pool: &PgPool,
    owner: &Owner,
    source_chunk: Uuid,
    target_chunk: Uuid,
    callee_name: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let edge_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, owner_kind, owner_id, relation, relation_class,
             source_kind, source_memory_id, target_kind, target_memory_id,
             authorship_kind)
         VALUES ($1, $2, $3, 'proxima-code/calls', 'Structural',
             'Abstraction', $4, 'Abstraction', $5,
             'Engine')",
    )
    .bind(edge_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(source_chunk)
    .bind(target_chunk)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_code.code_calls_v1
            (edge_id, callsite_byte_start, callsite_byte_end, callee_name, is_dynamic)
         VALUES ($1, 0, 1, $2, false)",
    )
    .bind(edge_id)
    .bind(callee_name)
    .execute(pool)
    .await?;
    Ok(edge_id)
}
