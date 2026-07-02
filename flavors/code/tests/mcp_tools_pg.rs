use std::sync::Arc;
use std::time::Duration;

mod common;

use common::{TestDb, test_owner as owner_fixture};
use proxima_code::mcp::{
    CodeEmitExecutionPlanTool, CodeIngestHeadSnapshotTool, CodeListReposTool,
    CodeOpenFileRevisionTool, CodeRegisterRepoTool, CodeRetryExecutionRequestTool,
    CodeSearchChunksTool, CodeSearchCommitsTool,
};
use proxima_code::testkit::register_repo;
use proxima_code::{
    CodeChunkV1, CodeFlavorStore, CommitV1, ExecutionRequestV1, FileRevisionV1, FileState,
};
use proxima_core::engine::Engine;
use proxima_core::mcp::{
    HandleTable, McpAuthorContext, McpTool, McpToolCtx, McpToolError, McpToolExtensions, OutputMode,
};
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
    assert_eq!(result["repo"]["repo_handle"], "R1");
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
    assert_eq!(result["revision"]["repo_handle"], "R1");
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

    let master_token = Uuid::now_v7();
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
            master_token,
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
            Uuid::now_v7(),
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

    let master_token = Uuid::now_v7();
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
        master_token,
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
        handles: Some(Arc::new(HandleTable::new())),
        mode: OutputMode::Handles,
        registry,
        author: McpAuthorContext {
            model_id: "test/0".into(),
            client_name: "test".into(),
            client_version: "0".into(),
            caller_self_perspective: None,
        },
        caller_self_perspective: None,
        master_token_id: None,
        extensions: McpToolExtensions::with(store),
        engine: Some(engine),
    }
}

/// Master-token shell-author context: `PrefixedIds` wire ids, no handle
/// table, a master-token id, and a `caller_self_perspective` — the shape
/// `McpToolHost` builds for `code_retry_execution_request` callers.
fn shell_ctx(
    pg: PgStorage,
    owner: Owner,
    registry: Arc<FlavorRegistryFrozen>,
    master_token_id: Uuid,
    caller_self_perspective: MemoryId,
) -> McpToolCtx {
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
    let engine = Arc::new(engine_for_test(pg));
    McpToolCtx {
        owner,
        authz,
        handles: None,
        mode: OutputMode::PrefixedIds,
        registry,
        author: McpAuthorContext {
            model_id: "test/0".into(),
            client_name: "test".into(),
            client_version: "0".into(),
            caller_self_perspective: Some(caller_self_perspective),
        },
        caller_self_perspective: Some(caller_self_perspective),
        master_token_id: Some(master_token_id),
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
    run_git(repo, &["init"])?;
    let file_path = repo.join(relative_path);
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&file_path, contents)?;
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

fn fact_draft(_owner: Owner, schema_id: &str, payload: &[u8]) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new(schema_id.into()),
        schema_version: SchemaVersion::new(1),
        payload: payload.to_vec(),
        rendered_text: None,
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

async fn ingest_file_revision(
    pool: &PgPool,
    engine: &Engine,
    owner: Owner,
    repo_id: Uuid,
    file_path: &str,
    indexed_commit_sha: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let payload = format!("{file_path}:{indexed_commit_sha}");
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
