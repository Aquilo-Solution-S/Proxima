use proxima_code::RepoScope;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

mod common;

use common::{TestDb, test_owner as owner_fixture};
use proxima_code::mcp::{
    CodeEmitExecutionPlanTool, CodeEmitExecutionRequestTool, CodeEraseRepoTool,
    CodeGetIngestRunTool, CodeIngestHeadSnapshotTool, CodeListReposTool, CodeOpenFileRevisionTool,
    CodeRegisterRepoTool, CodeRetryExecutionRequestTool, CodeSearchChunksTool,
    CodeSearchCommitsTool, CodeStartIngestHeadSnapshotTool, CodeWorkItemBundleTool,
};
use proxima_code::testkit::register_repo;
use proxima_code::{
    CodeChunkV1, CodeFlavorStore, CommitV1, ExecutionRequestV1, ExecutionResultV1, FileRevisionV1,
    FileState,
};
use proxima_core::engine::Engine;
use proxima_core::mcp::{McpAuthorContext, McpTool, McpToolCtx, McpToolError};
use proxima_core::{
    AbstractionPayload, AuthPath, AuthzContext, FactPayload, FlavorRegistry, FlavorRegistryFrozen,
    FlavorServices, MemoryId, Owner, schema_only_key,
};
use proxima_storage_pg::PgStorage;
use serde_json::json;
use sqlx::PgPool;
use tempfile::TempDir;
use uuid::Uuid;
mod embedding_failure_regressions {
    use super::*;
    use proxima::host::{EmbedCaps, OpenAiCompatConfig, OpenAiCompatEmbeddingClient};
    use proxima_core::llm::{BoundEmbeddingClient, EmbeddingClient, EmbeddingDim, LlmError};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    type TestResult<T> = Result<T, Box<dyn std::error::Error>>;
    const DEADLINE: Duration = Duration::from_secs(10);
    const EMBEDDING_DIM: usize = EmbeddingDim::D1024.width();

    struct OverflowEndpoint {
        client: Arc<OpenAiCompatEmbeddingClient>,
        calls: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for OverflowEndpoint {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn read_request(stream: &mut TcpStream) -> TestResult<()> {
        let mut request = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).await?;
            if count == 0 || request.len() > 65_536 {
                return Err("incomplete or oversized test request".into());
            }
            request.extend_from_slice(&buffer[..count]);
            let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = std::str::from_utf8(&request[..end])?;
            if !headers.starts_with("POST /v1/embeddings HTTP/1.1\r\n") {
                return Err("unexpected embedding request path".into());
            }
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .ok_or("missing test content length")?
                .1
                .trim()
                .parse::<usize>()?;
            if request.len() >= end + 4 + length {
                return Ok(());
            }
        }
    }

    async fn overflow_endpoint() -> TestResult<OverflowEndpoint> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let mut components = vec!["0.0"; EMBEDDING_DIM];
        components[EMBEDDING_DIM - 1] = "1e39";
        let body = format!(
            r#"{{"data":[{{"index":0,"embedding":[{}]}}]}}"#,
            components.join(",")
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept test request");
                tokio::time::timeout(DEADLINE, read_request(&mut stream))
                    .await
                    .expect("bounded request")
                    .expect("complete request");
                count.fetch_add(1, Ordering::SeqCst);
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("respond");
            }
        });
        let client = Arc::new(OpenAiCompatEmbeddingClient::new(
            "test-topic-embed",
            EmbedCaps::new(u32::try_from(EMBEDDING_DIM)?, false),
            OpenAiCompatConfig::new(format!("http://{addr}/v1"), None)
                .with_timeout(Duration::from_secs(5)),
        )?);
        Ok(OverflowEndpoint {
            client,
            calls,
            task,
        })
    }

    struct Observed {
        adapter: Result<Vec<f32>, LlmError>,
        lexical: serde_json::Value,
        hybrid: serde_json::Value,
        semantic: Result<<CodeSearchChunksTool as McpTool>::Output, McpToolError>,
        calls: [usize; 4],
        healthy_semantic: serde_json::Value,
    }

    fn args(mode: &str) -> serde_json::Value {
        json!({"query": "halt_iteration", "mode": mode, "include_calls": false, "verbose": true})
    }

    async fn exercise(fixture: &TestDb) -> TestResult<Observed> {
        let owner = owner_fixture();
        let registry = registry_for_mcp();
        let temp = TempDir::new()?;
        ingest_topic_repo(fixture, owner, &registry, &temp).await?;
        let router = Arc::new(proxima_core::test_fixtures::TestEmbeddingRouter::default());
        let mut context = ctx(fixture.pg.clone(), owner, registry);
        context.engine = Some(Arc::new(
            engine_for_test(fixture.pg.clone()).with_embedding_router(router.clone()),
        ));
        let endpoint = overflow_endpoint().await?;
        let adapter = endpoint.client.embed("response control").await;
        router
            .set_default(BoundEmbeddingClient::bind(endpoint.client.clone()).expect("lane width"));
        let mut calls = [0; 4];
        calls[0] = endpoint.calls.load(Ordering::SeqCst);

        let lexical = tokio::time::timeout(
            DEADLINE,
            run_tool::<CodeSearchChunksTool>(context.clone(), args("lexical")),
        )
        .await??;
        calls[1] = endpoint.calls.load(Ordering::SeqCst);
        let hybrid = tokio::time::timeout(
            DEADLINE,
            run_tool::<CodeSearchChunksTool>(context.clone(), args("hybrid")),
        )
        .await??;
        calls[2] = endpoint.calls.load(Ordering::SeqCst);
        // Keep the typed handler error instead of boxing it in run_tool.
        let semantic = tokio::time::timeout(
            DEADLINE,
            CodeSearchChunksTool::call(context.clone(), serde_json::from_value(args("semantic"))?),
        )
        .await?;
        calls[3] = endpoint.calls.load(Ordering::SeqCst);

        router
            .set_default(BoundEmbeddingClient::bind(Arc::new(TopicEmbedding)).expect("lane width"));
        let healthy = tokio::time::timeout(
            DEADLINE,
            run_tool::<CodeSearchChunksTool>(context, args("semantic")),
        )
        .await??;
        Ok(Observed {
            adapter,
            lexical,
            hybrid,
            semantic,
            calls,
            healthy_semantic: healthy,
        })
    }

    #[tokio::test]
    async fn code_search_degrades_after_actual_provider_response_overflow() {
        let fixture = TestDb::fresh().await;
        let result = exercise(&fixture).await;
        proxima_pg_testkit::drop_db(&fixture.name)
            .await
            .expect("drop isolated fixture before assertions");
        drop(fixture);
        let observed = result.expect("bounded handler exercise");
        eprintln!(
            "code overflow handlers: calls={:?}; fixture removed",
            observed.calls,
        );
        assert!(matches!(observed.adapter, Err(LlmError::Embed(ref message))
            if message == "embedding 0 has non-finite component at position 1023"));
        assert_eq!(observed.calls, [1, 1, 2, 3]);
        for (page, mode, degraded) in [
            (&observed.lexical, "lexical", false),
            (&observed.hybrid, "hybrid", true),
        ] {
            assert_eq!(page["mode"], mode);
            assert_eq!(page["degraded_to_lexical"], degraded);
            assert_eq!(page["matches"][0]["file_path"], "src/control.rs");
            assert!(
                page["matches"][0]["lexical_score"]
                    .as_f64()
                    .unwrap_or_default()
                    > 0.0
            );
            assert_eq!(page["matches"][0]["similarity_score"], 0.0);
        }
        assert_eq!(
            observed.hybrid["matches"][0]["handle"],
            observed.lexical["matches"][0]["handle"]
        );
        assert_eq!(
            observed.hybrid["matches"][0]["score"],
            observed.lexical["matches"][0]["score"]
        );
        assert!(
            matches!(observed.semantic, Err(McpToolError::Unavailable(ref message))
            if message == "semantic chunk search unavailable: embedding provider error")
        );
        assert_eq!(
            observed.healthy_semantic["matches"][0]["file_path"],
            "src/control.rs"
        );
        assert!(
            observed.healthy_semantic["matches"][0]["similarity_score"]
                .as_f64()
                .unwrap_or_default()
                > 0.99
        );
    }
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

/// A repo's ingest scope decides what gets indexed, is reported rather
/// than silent, and is re-appliable: narrowing it tombstones what left.
#[tokio::test]
async fn an_ingest_scope_excludes_fixtures_and_tombstones_what_leaves_it()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    init_git_repo_with_files(
        temp.path(),
        &[
            ("src/lib.rs", "pub fn scope_kept_marker() -> u64 { 1 }\n"),
            (
                "fixtures/plugins/a.rs",
                "pub fn scope_dropped_marker() -> u64 { 2 }\n",
            ),
        ],
    )?;
    let path = temp.path().to_string_lossy().into_owned();

    // Registered unscoped first, so the assertions below compare against
    // a real indexed state rather than against nothing.
    let registered = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": path, "display_name": "Scope Repo" }),
    )
    .await?;
    let repo_handle = registered["repo"]["repo_id"]
        .as_str()
        .expect("repo_id")
        .to_string();
    assert_eq!(
        registered["repo"]["exclude_globs"].as_array().map(Vec::len),
        Some(0),
        "an unscoped repo reports empty lists, not null: {registered}"
    );

    let wide = run_tool::<CodeIngestHeadSnapshotTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": repo_handle }),
    )
    .await?;
    assert_eq!(wide["report"]["files_present_emitted"], 2);
    assert_eq!(
        wide["report"]["files_excluded"], 0,
        "no scope excludes nothing: {wide}"
    );
    let both = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "query": "scope_dropped_marker", "repo_handle": repo_handle, "limit": 10 }),
    )
    .await?;
    assert!(
        match_paths(&both).contains(&"fixtures/plugins/a.rs".to_string()),
        "the fixture must be indexed before the scope removes it: {both}"
    );

    // Re-registering with a scope is how an existing repo is narrowed:
    // `display_name` is ignored on replay, scope is not.
    let rescoped = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": path, "exclude_globs": ["**/fixtures/**"] }),
    )
    .await?;
    assert_eq!(rescoped["created"], false);
    assert_eq!(
        rescoped["repo"]["exclude_globs"][0], "**/fixtures/**",
        "the scope reads back exactly as written: {rescoped}"
    );

    let narrowed = run_tool::<CodeIngestHeadSnapshotTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": repo_handle }),
    )
    .await?;
    assert_eq!(
        narrowed["report"]["files_excluded"], 1,
        "the excluded file is counted, not silently dropped: {narrowed}"
    );
    assert_eq!(
        narrowed["report"]["files_tombstoned"], 1,
        "a path that leaves scope is tombstoned like a deleted one: {narrowed}"
    );

    let dropped = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "query": "scope_dropped_marker", "repo_handle": repo_handle, "limit": 10 }),
    )
    .await?;
    assert!(
        !match_paths(&dropped).contains(&"fixtures/plugins/a.rs".to_string()),
        "the out-of-scope chunk must be gone from search: {dropped}"
    );
    let kept = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "query": "scope_kept_marker", "repo_handle": repo_handle, "limit": 10 }),
    )
    .await?;
    assert!(
        match_paths(&kept).contains(&"src/lib.rs".to_string()),
        "the file beside the fixture is untouched: {kept}"
    );

    // A malformed glob is a rejected call, not a repo whose every future
    // ingest fails on a pattern nobody can now change.
    let bad = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "path": path, "exclude_globs": ["src/["] }),
    )
    .await
    .expect_err("a malformed glob must be rejected");
    assert!(
        bad.to_string().contains("invalid ingest scope"),
        "unexpected error: {bad}"
    );
    Ok(())
}

/// `repo_handle` on the read tools takes a repository's display name, path
/// or directory name as well as its handle, case-insensitively, and refuses a
/// name two repositories share (#356).
#[tokio::test]
async fn read_tools_resolve_a_repository_by_name_or_path() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let parent = TempDir::new()?;
    let mut repos = Vec::new();
    for (group, display_name, marker) in [
        ("alpha", "Alpha Service", "quartzalphamarker"),
        ("beta", "Beta Service", "zirconbetamarker"),
    ] {
        let root = parent.path().join(group).join("shared");
        std::fs::create_dir_all(&root)?;
        let text = format!("pub fn {marker}() -> u8 {{ 1 }}\n");
        init_git_repo_with_files(&root, &[("src/lib.rs", text.as_str())])?;
        let registered = run_tool::<CodeRegisterRepoTool>(
            ctx(fixture.pg.clone(), owner, registry.clone()),
            json!({ "path": root.to_string_lossy(), "display_name": display_name }),
        )
        .await?;
        let handle = registered["repo"]["repo_handle"]
            .as_str()
            .expect("repo_handle")
            .to_string();
        run_tool::<CodeIngestHeadSnapshotTool>(
            ctx(fixture.pg.clone(), owner, registry.clone()),
            json!({ "repo_handle": handle }),
        )
        .await?;
        repos.push(registered["repo"].clone());
    }
    let search = |query: &'static str, repo: String| {
        run_tool::<CodeSearchChunksTool>(
            ctx(fixture.pg.clone(), owner, registry.clone()),
            json!({ "query": query, "repo_handle": repo, "mode": "lexical", "include_calls": false }),
        )
    };

    let by_name = search("quartzalphamarker", "alpha SERVICE".into()).await?;
    assert_eq!(match_paths(&by_name), ["src/lib.rs"], "{by_name}");
    let other_repo = search("zirconbetamarker", "Alpha Service".into()).await?;
    assert!(match_paths(&other_repo).is_empty(), "{other_repo}");

    let beta_path = repos[1]["canonical_path"].as_str().expect("canonical_path");
    let opened = run_tool::<CodeOpenFileRevisionTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": beta_path, "file_path": "src/lib.rs" }),
    )
    .await?;
    assert_eq!(
        opened["revision"]["repo_handle"], repos[1]["repo_handle"],
        "{opened}"
    );

    let shared = search("quartzalphamarker", "Shared".into())
        .await
        .expect_err("a directory name two repositories share must be refused");
    assert!(
        shared.to_string().contains("matched multiple repos"),
        "unexpected error: {shared}"
    );
    Ok(())
}

/// A scope narrowed and widened back at the same HEAD brings back the files
/// it tombstoned, and narrowing it again removes them again. Each widened or
/// narrowed pass re-reports a revision the series already holds; it has to
/// head the path again rather than replay behind the later one (#357).
#[tokio::test]
async fn widening_a_scope_restores_what_narrowing_tombstoned()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    init_git_repo_with_files(
        temp.path(),
        &[
            ("src/lib.rs", "pub fn widen_kept_marker() -> u64 { 1 }\n"),
            (
                "fixtures/plugins/a.rs",
                "pub fn widen_dropped_marker() -> u64 { 2 }\n",
            ),
        ],
    )?;
    let path = temp.path().to_string_lossy().into_owned();
    let register = |scope: serde_json::Value| {
        let mut args = json!({ "path": path, "display_name": "Widen Repo" });
        if let (Some(args), Some(scope)) = (args.as_object_mut(), scope.as_object()) {
            args.extend(scope.clone());
        }
        run_tool::<CodeRegisterRepoTool>(ctx(fixture.pg.clone(), owner, registry.clone()), args)
    };
    let repo = register(json!({})).await?["repo"]["repo_id"]
        .as_str()
        .expect("repo_id")
        .to_string();
    let ingest = || {
        run_tool::<CodeIngestHeadSnapshotTool>(
            ctx(fixture.pg.clone(), owner, registry.clone()),
            json!({ "repo_handle": repo }),
        )
    };
    let dropped_file_is_live = || async {
        let found = run_tool::<CodeSearchChunksTool>(
            ctx(fixture.pg.clone(), owner, registry.clone()),
            json!({ "query": "widen_dropped_marker", "repo_handle": repo, "mode": "lexical" }),
        )
        .await?;
        Ok::<_, Box<dyn std::error::Error>>(
            match_paths(&found).contains(&"fixtures/plugins/a.rs".to_string()),
        )
    };

    ingest().await?;
    assert!(dropped_file_is_live().await?);

    register(json!({ "exclude_globs": ["**/fixtures/**"] })).await?;
    let narrowed = ingest().await?;
    assert_eq!(narrowed["report"]["files_tombstoned"], 1, "{narrowed}");
    assert!(!dropped_file_is_live().await?);

    // Omitting both lists keeps the stored scope; `[]` clears it.
    let kept = register(json!({})).await?;
    assert_eq!(
        kept["repo"]["exclude_globs"],
        json!(["**/fixtures/**"]),
        "{kept}"
    );
    register(json!({ "include_globs": [], "exclude_globs": [] })).await?;
    let widened = ingest().await?;
    assert_eq!(widened["report"]["files_present_emitted"], 1, "{widened}");
    assert_eq!(widened["report"]["chunks_emitted"], 1, "{widened}");
    assert!(
        dropped_file_is_live().await?,
        "widening the scope must make the tombstoned file searchable again: {widened}"
    );

    register(json!({ "exclude_globs": ["**/fixtures/**"] })).await?;
    let narrowed_again = ingest().await?;
    assert_eq!(
        narrowed_again["report"]["files_tombstoned"], 1,
        "{narrowed_again}"
    );
    assert!(!dropped_file_is_live().await?);
    Ok(())
}

/// Checking out a commit indexed before makes its content the searchable
/// head again, including a file the branch in between added, and it does so
/// on every round trip (#357).
#[tokio::test]
async fn returning_to_an_indexed_commit_restores_its_content()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    init_git_repo_with_files(
        temp.path(),
        &[("src/a.rs", "pub fn alphamainzebra() -> u8 { 1 }\n")],
    )?;
    run_git(temp.path(), &["checkout", "-q", "-b", "base"])?;
    run_git(temp.path(), &["checkout", "-q", "-b", "feature"])?;
    std::fs::write(
        temp.path().join("src/a.rs"),
        "pub fn betafeaturequokka() -> u8 { 2 }\n",
    )?;
    std::fs::write(
        temp.path().join("src/b.rs"),
        "pub fn gammabranchonlyotter() -> u8 { 3 }\n",
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
            "-q",
            "-m",
            "feature revision",
        ],
    )?;
    run_git(temp.path(), &["checkout", "-q", "base"])?;

    let registered = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "Round Trip" }),
    )
    .await?;
    let repo = registered["repo"]["repo_id"]
        .as_str()
        .expect("repo_id")
        .to_string();
    let checkout_and_ingest = |branch: &'static str| {
        let registry = registry.clone();
        let pg = fixture.pg.clone();
        let repo = repo.clone();
        let root = temp.path().to_path_buf();
        async move {
            run_git(&root, &["checkout", "-q", branch])?;
            run_tool::<CodeIngestHeadSnapshotTool>(
                ctx(pg, owner, registry),
                json!({ "repo_handle": repo }),
            )
            .await
        }
    };
    let live = || async {
        let mut markers = Vec::new();
        for marker in [
            "alphamainzebra",
            "betafeaturequokka",
            "gammabranchonlyotter",
        ] {
            let found = run_tool::<CodeSearchChunksTool>(
                ctx(fixture.pg.clone(), owner, registry.clone()),
                json!({ "query": marker, "repo_handle": repo, "mode": "lexical", "include_calls": false }),
            )
            .await?;
            if !match_paths(&found).is_empty() {
                markers.push(marker);
            }
        }
        Ok::<_, Box<dyn std::error::Error>>(markers)
    };

    checkout_and_ingest("base").await?;
    assert_eq!(live().await?, ["alphamainzebra"]);
    for round in 1..=2 {
        checkout_and_ingest("feature").await?;
        assert_eq!(
            live().await?,
            ["betafeaturequokka", "gammabranchonlyotter"],
            "round {round}: the branch's content must be live"
        );
        let back = checkout_and_ingest("base").await?;
        assert_eq!(
            back["report"]["files_present_emitted"], 1,
            "round {round}: {back}"
        );
        assert_eq!(
            back["report"]["files_tombstoned"], 1,
            "round {round}: {back}"
        );
        assert_eq!(
            live().await?,
            ["alphamainzebra"],
            "round {round}: the checked-out commit's content must be live again"
        );
    }
    Ok(())
}

/// Erasure is the supported way to re-index a repository from scratch, which
/// is what a chunker or render upgrade needs: a HEAD snapshot re-derives only
/// files whose content moved, so files that never change cannot be
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
    assert!(receipt["memories_deleted"].as_u64().expect("count") >= 1);
    assert_eq!(
        receipt["cold_objects_pending"].as_u64().expect("count"),
        0,
        "nothing in this fixture was ever cooled"
    );

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

/// A match has to carry enough of its chunk to answer with, and
/// truncation must be flagged in the response.
#[tokio::test]
async fn search_chunks_returns_whole_chunks_and_flags_truncation()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;

    // One function long enough that a small snippet would hide the answer
    // at the end, still inside MAX_CHUNK_CHARS so it stays a single chunk.
    let mut body = String::from("pub fn proxima_long_marker() -> u32 {\n");
    for i in 0..40 {
        writeln!(body, "    let filler_{i} = {i}; // padding line").expect("write to String");
    }
    body.push_str("    9_753\n}\n");
    assert!(
        body.len() > 480,
        "fixture must be longer than a small snippet"
    );
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

/// A match carries what an agent reads, cites and opens. A full-text match
/// points at the line that shares the query's words, `context_lines` returns
/// the numbered lines around it, a scoped search names its repository once,
/// and `verbose` adds the diagnostics back.
#[tokio::test]
async fn search_chunks_returns_lean_matches_that_point_at_a_line()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    init_git_repo_with_commit(
        temp.path(),
        "src/drain.rs",
        "pub fn drain_queue(queue: &mut Vec<u32>) -> usize {\n    let batch: Vec<u32> = queue.drain(..).collect();\n    let size = batch.len();\n    // retry a failed batch with backoff\n    resend(&batch);\n    size\n}\n",
    )?;
    let registered = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "Drain Repo" }),
    )
    .await?;
    let repo_handle = registered["repo"]["repo_handle"]
        .as_str()
        .expect("repo_handle")
        .to_owned();
    run_tool::<CodeIngestHeadSnapshotTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": repo_handle }),
    )
    .await?;
    let search = |extra: serde_json::Value| {
        let mut args = json!({
            "query": "where are failed batches retried",
            "mode": "lexical",
            "include_calls": false,
        });
        for (key, value) in extra.as_object().expect("object") {
            args[key] = value.clone();
        }
        run_tool::<CodeSearchChunksTool>(ctx(fixture.pg.clone(), owner, registry.clone()), args)
    };

    let scoped = search(json!({ "repo_handle": "Drain Repo" })).await?;
    assert_eq!(scoped["repo_handle"], repo_handle.as_str(), "{scoped}");
    let top = &scoped["matches"][0];
    assert_eq!(top["file_path"], "src/drain.rs");
    assert_eq!(top["matched_line"], 4, "{top}");
    let keys: Vec<&str> = top
        .as_object()
        .expect("match object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "handle",
            "file_path",
            "chunk_type",
            "line_range",
            "snippet",
            "snippet_truncated",
            "matched_line",
            "score"
        ]
    );

    let windowed = search(json!({ "repo_handle": "Drain Repo", "context_lines": 1 })).await?;
    let top = &windowed["matches"][0];
    assert_eq!(
        top["snippet"],
        "3:     let size = batch.len();\n4:     // retry a failed batch with backoff\n5:     resend(&batch);"
    );
    assert_eq!(top["snippet_truncated"], true);

    let unscoped = search(json!({ "verbose": true })).await?;
    assert!(unscoped.get("repo_handle").is_none(), "{unscoped}");
    let top = &unscoped["matches"][0];
    assert_eq!(top["repo_handle"], repo_handle.as_str());
    assert_eq!(top["match_kind"], "full_text");
    assert_eq!(
        top["matched_excerpt"],
        "// retry a failed batch with backoff"
    );
    assert_eq!(top["language"], "rust");
    assert!(top["lexical_score"].as_f64().unwrap_or_default() > 0.0);
    assert_eq!(top["similarity_score"], 0.0);
    for key in ["chunk_index", "byte_range"] {
        assert!(top.get(key).is_some(), "verbose match lacks {key}: {top}");
    }
    Ok(())
}

/// `lines` padding statements, so each function outgrows the merge target
/// and chunks on its own.
fn padded_body(indent: &str, lines: usize, statement: &str) -> String {
    let mut body = String::new();
    for line in 0..lines {
        body.push_str(indent);
        body.push_str(&statement.replace("{n}", &line.to_string()));
        body.push('\n');
    }
    body
}

/// One caller→callee pair per grammar, both bodies padded past the merge
/// target so each function chunks on its own. Only a callee pads with
/// `checksum`, so that word finds the callee chunk alone. `caller_note`
/// words the caller's padding, so a second call re-writes the file.
fn call_sources(caller_note: &str) -> [(&'static str, String); 3] {
    [
        (
            "src/store.rs",
            format!(
                "fn open_store() {{\n{}    build_index();\n}}\n\nfn build_index() {{\n{}}}\n",
                padded_body(
                    "    ",
                    40,
                    &format!("let value_{{n}} = {{n}}; // {caller_note} statement here")
                ),
                padded_body(
                    "    ",
                    40,
                    "let digest_{n} = {n}; // checksum statement here"
                ),
            ),
        ),
        (
            "pkg/config.py",
            format!(
                "def load_config(path):\n{}    return parse_config(path)\n\n\ndef parse_config(path):\n{}    return path\n",
                padded_body(
                    "    ",
                    40,
                    &format!("total_{{n}} = len(path) + {{n}}  # {caller_note} statement")
                ),
                padded_body(
                    "    ",
                    40,
                    "digest_{n} = len(path) + {n}  # checksum statement"
                ),
            ),
        ),
        // Go opens on a `package` clause, which merges into a following
        // function still under the merge target, so Go pads further.
        (
            "cmd/main.go",
            format!(
                "package main\n\nfunc startServer() {{\n{}\tbindListener()\n}}\n\nfunc bindListener() {{\n{}}}\n",
                padded_body(
                    "\t",
                    50,
                    &format!("value{{n}} := {{n}} // {caller_note} statement here")
                ),
                padded_body("\t", 50, "digest{n} := {n} // checksum statement here"),
            ),
        ),
    ]
}

/// Rust, Python and Go connect a caller's chunk to its callee's, and a
/// search reaching either end returns the call with its site. The index
/// names a callee by series handle; the edge must still point at the
/// callee's current chunk, and a caller edit must not leave its superseded
/// revision behind as a second caller.
#[tokio::test]
async fn calls_connect_their_chunks_in_every_grammar() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    let sources = call_sources("padding");
    let files = sources
        .iter()
        .map(|(path, source)| (*path, source.as_str()))
        .collect::<Vec<_>>();
    init_git_repo_with_files(temp.path(), &files)?;
    let registered = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "Calls Repo" }),
    )
    .await?;
    let repo = registered["repo"]["repo_id"].as_str().expect("repo_id");
    for revision in ["initial", "caller edited"] {
        if revision != "initial" {
            for (path, source) in call_sources("revised") {
                std::fs::write(temp.path().join(path), source)?;
            }
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
                    "edit the callers",
                ],
            )?;
        }
        run_tool::<CodeIngestHeadSnapshotTool>(
            ctx(fixture.pg.clone(), owner, registry.clone()),
            json!({ "repo_handle": repo }),
        )
        .await?;
        for (caller, callee, language) in [
            ("open_store", "build_index", "rust"),
            ("load_config", "parse_config", "python"),
            ("startServer", "bindListener", "go"),
        ] {
            let context = format!("{language}, {revision}");
            let search = |query: &str| {
                run_tool::<CodeSearchChunksTool>(
                    ctx(fixture.pg.clone(), owner, registry.clone()),
                    json!({
                        "query": query,
                        "repo_handle": repo,
                        "mode": "lexical",
                        "language": language,
                        "verbose": true,
                    }),
                )
            };
            let outbound = search(caller).await?;
            let source_chunk = function_chunk(&outbound, caller, &context);
            let inbound = search("checksum").await?;
            let target_chunk = function_chunk(&inbound, callee, &context);

            let out = edges_where(&outbound, "source", &source_chunk["handle"]);
            assert_eq!(out.len(), 1, "{context}: one callee: {outbound}");
            assert_eq!(
                out[0]["target"], target_chunk["handle"],
                "{context}: the call points at the callee's current chunk: {outbound}"
            );
            assert_eq!(out[0]["sites"][0]["callee_name"], callee, "{}", out[0]);
            assert_eq!(out[0]["sites"][0]["is_dynamic"], false, "{}", out[0]);

            let into = edges_where(&inbound, "target", &target_chunk["handle"]);
            assert_eq!(
                into.len(),
                1,
                "{context}: one caller, not one per caller revision: {inbound}"
            );
            assert_eq!(
                into[0]["source"], source_chunk["handle"],
                "{context}: {inbound}"
            );
            assert_eq!(into[0]["sites"][0]["callee_name"], callee, "{}", into[0]);
        }
    }
    Ok(())
}

/// The call-pair cap keeps the connections of the best-ranked page chunks,
/// from either end, whatever their ids sort as.
#[tokio::test]
async fn the_call_pair_cap_follows_page_rank() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    let sources = call_sources("padding");
    let files = sources
        .iter()
        .map(|(path, source)| (*path, source.as_str()))
        .collect::<Vec<_>>();
    init_git_repo_with_files(temp.path(), &files)?;
    let registered = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "Rank Repo" }),
    )
    .await?;
    let repo = registered["repo"]["repo_id"].as_str().expect("repo_id");
    run_tool::<CodeIngestHeadSnapshotTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": repo }),
    )
    .await?;

    // Each file's caller and callee, callers in descending id order: ranked
    // that way, id order would keep the last pair, not the first.
    let ends: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT caller.t, callee.t
           FROM proxima_code.code_chunk_v1 caller
           JOIN proxima_code.code_chunk_v1 callee ON callee.file_path = caller.file_path
          WHERE caller.text ~ '^(fn open_store|def load_config|func startServer)'
            AND callee.text ~ '^(fn build_index|def parse_config|func bindListener)'
          ORDER BY caller.t DESC",
    )
    .fetch_all(fixture.pg.pool_for_tests())
    .await?;
    assert_eq!(ends.len(), 3, "one caller and one callee per file");
    let schema = <CodeChunkV1 as AbstractionPayload>::schema_id();
    let caller_page = ends.iter().map(|(caller, _)| *caller).collect::<Vec<_>>();
    let target_page = ends.iter().map(|(_, callee)| *callee).collect::<Vec<_>>();
    for (end, page) in [("caller", &caller_page), ("callee", &target_page)] {
        let kept = proxima_storage_pg::query::head_chunk_call_pairs(
            fixture.pg.pool_for_tests(),
            &schema,
            page,
            1,
        )
        .await?;
        assert_eq!(kept.len(), 1, "{end}: {kept:?}");
        assert_eq!(
            (kept[0].caller_t, kept[0].callee_t),
            (ends[0].0, Some(ends[0].1)),
            "{end}: the cap keeps the first-ranked chunk's pair: {kept:?}"
        );
    }
    Ok(())
}

/// The match whose snippet opens on `name`'s definition, asserted to be a
/// `function` chunk.
fn function_chunk(found: &serde_json::Value, name: &str, context: &str) -> serde_json::Value {
    let chunk = found["matches"]
        .as_array()
        .expect("matches")
        .iter()
        .find(|m| {
            let snippet = m["snippet"].as_str().unwrap_or_default();
            ["fn ", "def ", "func "]
                .iter()
                .any(|keyword| snippet.starts_with(&format!("{keyword}{name}(")))
        })
        .unwrap_or_else(|| panic!("{context}: no chunk opens on {name}: {found}"))
        .clone();
    assert_eq!(chunk["chunk_type"], "function", "{context}: {chunk}");
    chunk
}

/// The `calls_edges` whose `end` is `handle`.
fn edges_where<'a>(
    found: &'a serde_json::Value,
    end: &str,
    handle: &serde_json::Value,
) -> Vec<&'a serde_json::Value> {
    found["calls_edges"]
        .as_array()
        .expect("calls_edges")
        .iter()
        .filter(|edge| &edge[end] == handle)
        .collect()
}

/// Python chunks carry a language label, or `language: "python"` can never
/// select them. An unknown label is refused rather than answered empty.
#[tokio::test]
async fn the_language_filter_finds_python() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    init_git_repo_with_files(
        temp.path(),
        &[
            (
                "pkg/client.py",
                "def language_label_marker():\n    return 1\n",
            ),
            (
                "src/lib.rs",
                "pub fn language_label_marker() -> u64 { 1 }\n",
            ),
        ],
    )?;
    let registered = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "Label Repo" }),
    )
    .await?;
    let repo = registered["repo"]["repo_id"].as_str().expect("repo_id");
    run_tool::<CodeIngestHeadSnapshotTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": repo }),
    )
    .await?;

    let search = |language: &str| {
        json!({
            "query": "language_label_marker",
            "repo_handle": repo,
            "mode": "lexical",
            "language": language,
            "include_calls": false,
            "verbose": true,
        })
    };
    let python = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        search("python"),
    )
    .await?;
    assert_eq!(match_paths(&python), ["pkg/client.py"], "{python}");
    assert_eq!(python["matches"][0]["language"], "python");
    let rust = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        search("rust"),
    )
    .await?;
    assert_eq!(match_paths(&rust), ["src/lib.rs"], "{rust}");

    let refused = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry),
        search("Python"),
    )
    .await
    .expect_err("an unknown language label must be refused");
    assert!(
        refused.to_string().contains("language must be one of"),
        "unexpected error: {refused}"
    );
    Ok(())
}

/// A caller and its callee, each padded past the merge target so both
/// chunk on their own and the call is an edge between two chunks.
fn rust_call_pair(outer: &str, inner: &str, marker: &str) -> String {
    format!(
        "pub fn {outer}() -> u32 {{\n{}    {inner}()\n}}\n\npub fn {inner}() -> u32 {{\n{}    3\n}}\n",
        padded_body("    ", 40, &format!("let step_{{n}} = {{n}}; // {marker}")),
        padded_body("    ", 40, "let base_{n} = {n}; // base statement"),
    )
}

/// Every class is ingested and searchable. Lockfiles embed their header
/// only, non-source files declare no calls, the report counts each class,
/// a `file_class` filter narrows every mode, and hybrid ranks every source
/// match above every other one while lexical keeps its own order (#360).
#[tokio::test]
async fn file_classes_stay_searchable_and_rank_after_source()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    let source = rust_call_pair("retry_entry", "retry_base", "retry_budget");
    let generated = rust_call_pair("gen_entry", "gen_base", "generated");
    init_git_repo_with_files(
        temp.path(),
        &[
            (
                ".gitattributes",
                "third_party/** linguist-vendored\n*.gen.rs linguist-generated\n",
            ),
            ("src/retry.rs", source.as_str()),
            ("src/api.gen.rs", generated.as_str()),
            (
                "third_party/retry_budget/lib.rs",
                "pub fn retry_budget() -> u32 { 5 }\n",
            ),
            (
                "Cargo.lock",
                "[[package]]\nname = \"retry_budget\"\nversion = \"0.3.1\"\n",
            ),
            (
                "gen/client.go",
                "// Code generated by protoc-gen-go. DO NOT EDIT.\n\npackage gen\n\n\
                 func RetryBudget() int { return 3 } // retry_budget\n",
            ),
            (
                "web/__snapshots__/view.test.ts.snap",
                "exports[`view 1`] = `retry_budget`;\n",
            ),
        ],
    )?;
    let registered = run_tool::<CodeRegisterRepoTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "Class Repo" }),
    )
    .await?;
    let repo = registered["repo"]["repo_id"].as_str().expect("repo_id");
    let ingested = run_tool::<CodeIngestHeadSnapshotTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": repo }),
    )
    .await?;
    assert_eq!(
        ingested["report"]["files_by_class"],
        json!({ "source": 2, "generated": 3, "vendored": 1, "lockfile": 1 }),
        "{ingested}"
    );
    let by_class = &ingested["report"]["chunks_by_class"];
    assert_eq!(
        ["source", "generated", "vendored", "lockfile"]
            .iter()
            .map(|class| by_class[class].as_u64().expect("count"))
            .sum::<u64>(),
        ingested["report"]["chunks_emitted"]
            .as_u64()
            .expect("chunks_emitted"),
        "{ingested}"
    );
    let run = run_tool::<CodeGetIngestRunTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "run_id": ingested["run_id"] }),
    )
    .await?;
    assert_eq!(
        run["run"]["files_by_class"], ingested["report"]["files_by_class"],
        "the persisted run keeps the report's file counts: {run}"
    );
    assert_eq!(
        run["run"]["chunks_by_class"], ingested["report"]["chunks_by_class"],
        "the persisted run keeps the report's chunk counts: {run}"
    );

    let pool = fixture.pg.pool_for_tests();
    let classes: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT file_path, COALESCE(file_class::text, 'source'), embed_text
           FROM proxima_code.code_chunk_v1
          WHERE state = 'Present'
          ORDER BY file_path, chunk_index",
    )
    .fetch_all(pool)
    .await?;
    let class_of = |path: &str| -> String {
        classes
            .iter()
            .find(|(file_path, ..)| file_path == path)
            .map_or_else(
                || panic!("{path} has a chunk: {classes:?}"),
                |(_, class, _)| class.clone(),
            )
    };
    for (path, class) in [
        ("src/retry.rs", "source"),
        ("src/api.gen.rs", "generated"),
        ("third_party/retry_budget/lib.rs", "vendored"),
        ("Cargo.lock", "lockfile"),
        ("gen/client.go", "generated"),
        ("web/__snapshots__/view.test.ts.snap", "generated"),
    ] {
        assert_eq!(class_of(path), class, "{path}");
    }
    for (path, _, embed_text) in &classes {
        if path == "Cargo.lock" {
            assert!(
                embed_text.starts_with("(lockfile) Cargo.lock:") && !embed_text.contains('\n'),
                "a lockfile embeds its header only: {embed_text:?}"
            );
        } else {
            assert!(
                embed_text.starts_with(&format!("{path}:")) && embed_text.contains('\n'),
                "every other class embeds header and body: {embed_text:?}"
            );
        }
    }
    let callers: Vec<(String, i64)> = sqlx::query_as(
        "SELECT COALESCE(c.file_class::text, 'source'), count(*)::bigint
           FROM proxima_code.code_chunk_call_v1 e
           JOIN proxima_code.code_chunk_v1 c ON c.t = e.caller_memory_id
          GROUP BY 1",
    )
    .fetch_all(pool)
    .await?;
    assert_eq!(
        callers,
        [("source".to_string(), 1)],
        "the source pair declares its call; the generated one, shaped the same, does not"
    );

    let search = |args: serde_json::Value| {
        let mut request = json!({
            "query": "retry_budget",
            "repo_handle": repo,
            "include_calls": false,
            "limit": 50,
        });
        for (key, value) in args.as_object().expect("object") {
            request[key] = value.clone();
        }
        run_tool::<CodeSearchChunksTool>(ctx(fixture.pg.clone(), owner, registry.clone()), request)
    };
    let classes_in = |found: &serde_json::Value| -> Vec<String> {
        found["matches"]
            .as_array()
            .expect("matches")
            .iter()
            .map(|m| m["file_class"].as_str().unwrap_or("source").to_string())
            .collect()
    };

    // Lexical keeps its own order: the vendored path match outscores.
    let lexical = search(json!({ "mode": "lexical" })).await?;
    assert_eq!(
        match_paths(&lexical)[0],
        "third_party/retry_budget/lib.rs",
        "{lexical}"
    );
    assert_eq!(lexical["matches"][0]["file_class"], "vendored", "{lexical}");

    // Hybrid — lexical here, with no embedding model — puts source first.
    let hybrid = search(json!({})).await?;
    assert_eq!(hybrid["degraded_to_lexical"], json!(true), "{hybrid}");
    assert_eq!(match_paths(&hybrid)[0], "src/retry.rs", "{hybrid}");
    assert!(
        hybrid["matches"][0].get("file_class").is_none(),
        "a source match carries no file_class: {hybrid}"
    );
    let hybrid_classes = classes_in(&hybrid);
    let first_other = hybrid_classes
        .iter()
        .position(|class| class != "source")
        .expect("non-source matches follow");
    assert!(
        hybrid_classes[first_other..]
            .iter()
            .all(|class| class != "source"),
        "every source match ranks above every other one: {hybrid_classes:?}"
    );
    assert_eq!(
        hybrid_classes.len(),
        classes_in(&lexical).len(),
        "tiering reorders, it drops nothing"
    );

    // A class filter narrows every mode, and a hybrid search asking for a
    // class ranks as its arms do.
    for mode in ["lexical", "hybrid"] {
        let lockfiles = search(json!({ "mode": mode, "file_class": "lockfile" })).await?;
        assert_eq!(
            match_paths(&lockfiles),
            ["Cargo.lock"],
            "{mode}: {lockfiles}"
        );
        assert_eq!(classes_in(&lockfiles), ["lockfile"]);
        let sources = search(json!({ "mode": mode, "file_class": "source" })).await?;
        assert!(
            classes_in(&sources).iter().all(|class| class == "source")
                && !sources["matches"].as_array().expect("matches").is_empty(),
            "{mode}: {sources}"
        );
    }
    let generated = search(json!({ "mode": "lexical", "file_class": "generated" })).await?;
    let mut generated_paths = match_paths(&generated);
    generated_paths.sort();
    assert_eq!(
        generated_paths,
        ["gen/client.go", "web/__snapshots__/view.test.ts.snap"],
        "{generated}"
    );

    let refused = search(json!({ "file_class": "Lockfile" }))
        .await
        .expect_err("an unknown class must be refused");
    assert!(
        refused
            .to_string()
            .contains("expected one of `source`, `generated`, `vendored`, `lockfile`"),
        "the refusal names the classes: {refused}"
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
/// That aborts the entire `ingest_head_snapshot`. The file is skipped as
/// binary — like any other binary file — and every other file in the tree
/// still indexes.
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

    // Nul in the path must not fail the ingest.
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

#[tokio::test]
async fn search_chunks_returns_only_head_per_nk() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    ingest_code_chunk(
        fixture.pg.pool_for_tests(),
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
async fn search_chunks_pages_with_cursor_not_raised_limit() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();
    for (index, (path, text)) in [
        ("src/a.rs", "fn pageable_unique_token_0() {}"),
        ("src/b.rs", "fn pageable_unique_token_1() {}"),
        ("src/c.rs", "fn pageable_unique_token_2() {}"),
    ]
    .into_iter()
    .enumerate()
    {
        ingest_code_chunk_with_type(
            fixture.pg.pool_for_tests(),
            owner,
            ChunkFixture {
                repo_id,
                file_path: path,
                chunk_index: i32::try_from(index)?,
                text,
                chunk_type: "function",
            },
        )
        .await?;
    }

    let first = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({
            "query": "pageable_unique_token",
            "limit": 2,
            "include_calls": false,
        }),
    )
    .await?;
    let first_matches = first["matches"].as_array().expect("matches");
    assert_eq!(first_matches.len(), 2);
    assert_eq!(first["has_more"], json!(true));
    let token = first["next_cursor"]
        .as_str()
        .expect("next_cursor")
        .to_string();

    let second = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({
            "query": "pageable_unique_token",
            "limit": 2,
            "cursor": token,
            "include_calls": false,
        }),
    )
    .await?;
    let second_matches = second["matches"].as_array().expect("matches");
    assert_eq!(second_matches.len(), 1);
    assert_eq!(second["has_more"], json!(false));
    assert_eq!(second["next_cursor"], serde_json::Value::Null);

    let first_paths: Vec<&str> = first_matches
        .iter()
        .map(|row| row["file_path"].as_str().expect("path"))
        .collect();
    let second_path = second_matches[0]["file_path"].as_str().expect("path");
    assert!(
        !first_paths.contains(&second_path),
        "pages must not overlap: {first_paths:?} vs {second_path}"
    );

    let rebound = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({
            "query": "a different query",
            "limit": 2,
            "cursor": token,
            "include_calls": false,
        }),
    )
    .await
    .expect_err("cursor must fail closed on a different query");
    let message = rebound.to_string();
    assert!(
        message.contains("cursor does not match"),
        "mismatch must name the fingerprint bind: {message}"
    );
    Ok(())
}

#[tokio::test]
async fn search_commits_stems_english_and_likes_sha_prefix()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    ingest_commit(
        fixture.pg.pool_for_tests(),
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

    let stemmed = run_tool::<CodeSearchCommitsTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "query": "edges", "limit": 10 }),
    )
    .await?;
    assert!(
        !stemmed["commits"].as_array().expect("commits").is_empty(),
        "english stem of edges must hit the commit message"
    );
    assert!(
        !stemmed["summaries"]
            .as_array()
            .expect("summaries")
            .is_empty(),
        "english stem of edges must hit the summary"
    );

    let prefix = run_tool::<CodeSearchCommitsTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "query": "deadbe", "limit": 10 }),
    )
    .await?;
    assert!(
        !prefix["commits"].as_array().expect("commits").is_empty(),
        "SHA prefix is a GIN miss and must LIKE"
    );
    assert!(
        !prefix["summaries"]
            .as_array()
            .expect("summaries")
            .is_empty(),
        "commit_sha prefix must LIKE on the summary leg"
    );
    Ok(())
}

/// The whole of what `proxima-code/has-acceptance-criteria` became: the
/// criteria Fact names the request it is the bar for, and the `reference`
/// index row falls out of that field. Nobody writes an edge, and the
/// request's author is a column.
#[tokio::test]
async fn emit_execution_request_grounds_and_attaches_acceptance_criteria()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();
    register_repo(
        fixture.pg.pool_for_tests(),
        None,
        &owner,
        repo_id,
        "/tmp/proxima-criteria",
        "Criteria Repo",
        &RepoScope::default(),
    )
    .await?;
    let planner_root = seed_perspective(&fixture.pg, &owner, "Planner Root").await?;
    let goal_activated = seed_active_goal_activation(&fixture.pg, &owner, planner_root).await?;

    let output = run_tool::<CodeEmitExecutionRequestTool>(
        shell_ctx(
            fixture.pg.clone(),
            owner,
            registry,
            MemoryId::new(planner_root),
        ),
        json!({
            "repo_handle": repo_id.to_string(),
            "title": "Harden the chunker",
            "instructions": "Split on syntax, not on bytes.",
            "idempotency_key": "criteria-1",
            "goal_activated_memory": format!("F:{goal_activated}"),
            "evidence": [],
            "acceptance_criteria": [{
                "key": "build",
                "description": "cargo build succeeds",
                "required": true,
                "verifier_kind": "command",
                "verifier_spec": { "command": ["cargo", "build"] }
            }]
        }),
    )
    .await?;

    let request_id: Uuid = output["handle"]
        .as_str()
        .expect("handle")
        .strip_prefix("F:")
        .expect("fact prefix")
        .parse()?;
    let criteria_id: Uuid = output["acceptance_criteria_handle"]
        .as_str()
        .expect("acceptance criteria handle")
        .strip_prefix("F:")
        .expect("fact prefix")
        .parse()?;

    let work_item: Uuid = sqlx::query_scalar(
        "SELECT work_item_memory_id
           FROM proxima_code.acceptance_criteria_v1
          WHERE t = $1",
    )
    .bind(criteria_id)
    .fetch_one(fixture.pg.pool_for_tests())
    .await?;
    assert_eq!(work_item, request_id);

    // Facts pin via refs (origins stay empty).
    let request_refs: Vec<Uuid> =
        sqlx::query_scalar("SELECT unnest(refs) FROM proxima_core.memory WHERE t = $1")
            .bind(request_id)
            .fetch_all(fixture.pg.pool_for_tests())
            .await?;
    assert!(
        request_refs.contains(&goal_activated),
        "request must pin the activation Fact; got {request_refs:?}"
    );
    assert_eq!(output["origin_count"], serde_json::json!(1));
    Ok(())
}

#[tokio::test]
async fn retry_execution_request_uses_owner_write_authority()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();

    let shell_self = seed_perspective(&fixture.pg, &owner, "Shell author").await?;
    let repo_id = Uuid::now_v7();
    let prior = ingest_execution_request_fixture(
        fixture.pg.pool_for_tests(),
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
        None,
        &owner,
        repo_id,
        "/tmp/proxima-plan-proof",
        "Plan Proof Repo",
        &RepoScope::default(),
    )
    .await?;
    let shell_self = seed_perspective(&fixture.pg, &owner, "Planner Root").await?;
    let goal_activated = seed_active_goal_activation(&fixture.pg, &owner, shell_self).await?;
    let plan_source =
        abstraction_memory(fixture.pg.pool_for_tests(), &owner, "planning context").await?;

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
    let origins: Vec<Uuid> =
        sqlx::query_scalar("SELECT unnest(origins) FROM proxima_core.memory WHERE t = $1")
            .bind(plan_id)
            .fetch_all(fixture.pg.pool_for_tests())
            .await?;
    assert_eq!(origins, vec![plan_source]);

    let references: Vec<Uuid> =
        sqlx::query_scalar("SELECT unnest(refs) FROM proxima_core.memory WHERE t = $1")
            .bind(plan_id)
            .fetch_all(fixture.pg.pool_for_tests())
            .await?;
    assert!(references.contains(&goal_activated), "{references:?}");

    // The plan names the request Fact each item became, and that is where
    // the plan→item connection now comes from.
    let item_request: Uuid = sqlx::query_scalar(
        "SELECT request_memory_id
           FROM proxima_code.execution_plan_item_v1
          WHERE plan_memory_id = $1",
    )
    .bind(plan_id)
    .fetch_one(fixture.pg.pool_for_tests())
    .await?;
    assert!(references.contains(&item_request), "{references:?}");
    let item_handle = output["items"][0]["handle"]
        .as_str()
        .expect("item handle")
        .strip_prefix("F:")
        .expect("fact prefix")
        .to_string();
    assert_eq!(item_request.to_string(), item_handle);

    // One origin plus one reference per payload target.
    assert_eq!(
        output["plan_edge_count"],
        serde_json::json!(1 + references.len())
    );

    Ok(())
}

#[tokio::test]
async fn retry_execution_request_rejects_unknown_target_perspective()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();

    let shell_self = seed_perspective(&fixture.pg, &owner, "Shell author").await?;

    let repo_id = Uuid::now_v7();
    let prior =
        ingest_execution_request_fixture(fixture.pg.pool_for_tests(), owner, repo_id, "prior")
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

/// Every paged read in this flavor rejects `limit: 0` with the same error.
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

/// The same rule, one layer down: a cap on *returned text* is a page
/// bound too, and zero is the same nonsense.
///
/// `search_chunks` already refused `snippet_max_chars: 0`, and so does
/// `core_search_memories` for `body_max_chars`. `open_file_revision`
/// accepted `max_text_bytes: 0` and answered `text: ""` on every chunk —
/// the empty-page shape the sibling rule exists to prevent, and worse
/// here because passing the cap at all turns text *on* (`want_text` keys
/// off `max_text_bytes.is_some()`), so the caller asked for text, was
/// given text, and the text was blank.
#[tokio::test]
async fn every_text_cap_rejects_zero_the_same_way() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();

    let snippet = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "query": "anything", "snippet_max_chars": 0 }),
    )
    .await
    .expect_err("search_chunks must reject snippet_max_chars: 0");
    assert!(
        snippet.to_string().contains("must be at least 1"),
        "got {snippet}"
    );

    let bytes = run_tool::<CodeOpenFileRevisionTool>(
        ctx(fixture.pg.clone(), owner, registry.clone()),
        json!({ "repo_handle": Uuid::now_v7().to_string(), "file_path": "src/x.rs", "max_text_bytes": 0 }),
    )
    .await
    .expect_err("open_file_revision must reject max_text_bytes: 0");
    assert!(
        bytes.to_string().contains("max_text_bytes must be >= 1"),
        "got {bytes}"
    );
    // The cap is checked before the repo handle is resolved, so the caller
    // is told what is actually wrong with the request rather than being
    // sent chasing a handle that was never the problem.
    assert!(
        !bytes.to_string().contains("repo_handle"),
        "the zero cap must be named first: {bytes}"
    );
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
            trusted_model_id: None,
            client_name: "test".into(),
            client_version: "0".into(),
            caller_self_perspective: None,
        },
        caller_self_perspective: None,
        services: FlavorServices::with(store),
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
            trusted_model_id: None,
            client_name: "test".into(),
            client_version: "0".into(),
            caller_self_perspective: Some(caller_self_perspective),
        },
        caller_self_perspective: Some(caller_self_perspective),
        services: FlavorServices::with(store),
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
    let _ = common::seed_goal(
        pg.pool_for_tests(),
        owner,
        "core/simple-text-v1",
        "Goal",
        &format!("goal-{goal_id}"),
        Some(goal_id),
        Some(self_id),
    )
    .await?;
    let _ = common::seed_memory(
        pg.pool_for_tests(),
        owner,
        GoalActivationFixture::SCHEMA_ID,
        "fact",
        Some(memory_id),
        None,
        &[],
    )
    .await?;
    Ok(memory_id)
}

async fn seed_perspective(
    pg: &PgStorage,
    owner: &Owner,
    label: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    // The stamp and the rows it promises land in one transaction: a
    // memory row that names a sidecar table it has no row in is refused
    // at COMMIT.
    let mut stamped = pg.pool_for_tests().begin().await?;
    let _ = common::seed_memory_with_sidecars_in_tx(
        &mut stamped,
        owner,
        "core/interpretation-v1",
        "perspective",
        Some(memory_id),
        None,
        &[],
        &["proxima_core.interpretation_v1"],
    )
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.interpretation_v1
            (t, claim, confidence, model_id, client_name, client_version)
         VALUES ($1, $2, 100, 'test-model', 'test', '1')",
    )
    .bind(memory_id)
    .bind(label)
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;
    Ok(memory_id)
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct GoalActivationFixture;

impl FactPayload for GoalActivationFixture {
    const SCHEMA_ID: &'static str = "test/goal-activated-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        schema_only_key(Self::SCHEMA_ID, Self::SCHEMA_VERSION)
    }

    fn render(&self) -> String {
        "goal activated".to_owned()
    }
}

/// Mint a prior execution-request Fact + sidecar row that a retry targets.
async fn ingest_execution_request_fixture(
    pool: &PgPool,
    owner: Owner,
    repo_id: Uuid,
    request_key: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    // A work item is filed under a repository, and every write that names
    // one is fenced on its registration now, so the fixture registers it.
    register_repo_row(pool, owner, repo_id).await?;
    // The stamp and the row it promises land in one transaction: a memory row
    // that names a sidecar table it has no row in is refused at COMMIT.
    let mut stamped = pool.begin().await?;
    let memory_id = fact_memory_in_tx(
        &mut stamped,
        owner,
        ExecutionRequestV1::SCHEMA_ID,
        &["proxima_code.work_requested_v1"],
    )
    .await?;
    sqlx::query(
        "INSERT INTO proxima_code.work_requested_v1
            (t, repo_id, title, instructions, request_key)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(memory_id)
    .bind(repo_id)
    .bind("Prior execution request")
    .bind("Implement the prior request; this run is being retried.")
    .bind(request_key)
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;
    Ok(memory_id)
}

/// A foreign owner cannot inject a result row into another owner's bundle.
///
/// `proxima_code.execution_result_v1` carries no `owner_id`, its FK reaches any
/// `proxima_core.memory(t)`, and `ExecutionResultV1` declares no `references()`
/// — its only fence is `CODE_REPO_SCOPE` on its OWN `repo_id`. So owner B can
/// lawfully admit an `execution-result-v1` Fact under B's registered repo that
/// names owner A's work item, and the sidecar accepts it. The bundle is what
/// has to refuse: every sidecar hit is a CANDIDATE that must be admitted
/// through `memory` before it is rendered as A's own result.
#[tokio::test]
async fn work_item_bundle_admits_results_before_rendering_them()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner_a = owner_fixture();
    let owner_b = owner_fixture();
    let registry = registry_for_mcp();
    let pool = fixture.pg.pool_for_tests();

    let repo_a = Uuid::now_v7();
    let work_item = ingest_execution_request_fixture(pool, owner_a, repo_a, "bundle-authz").await?;

    // Owner A's own result, under A's repo: admitted, and must survive.
    let own = ingest_execution_result_fixture(pool, owner_a, repo_a, work_item, "A ran it").await?;

    // Owner B's, under B's OWN repo — which is the whole of what the repo
    // fence checks — but pointed at A's work item.
    let repo_b = Uuid::now_v7();
    let foreign =
        ingest_execution_result_fixture(pool, owner_b, repo_b, work_item, "B injected").await?;

    let shell_self = seed_perspective(&fixture.pg, &owner_a, "Shell author").await?;
    let ctx = shell_ctx(
        fixture.pg.clone(),
        owner_a,
        registry,
        MemoryId::new(shell_self),
    );
    let args: <CodeWorkItemBundleTool as McpTool>::Args =
        serde_json::from_value(json!({ "handle": format!("F:{work_item}") }))?;
    let output = serde_json::to_value(CodeWorkItemBundleTool::call(ctx, args).await?)?;

    let handles: Vec<String> = output["result_handles"]
        .as_array()
        .expect("result_handles array")
        .iter()
        .map(|row| row["handle"].as_str().expect("handle").to_string())
        .collect();
    assert!(
        handles.contains(&format!("F:{own}")),
        "owner A's own result must still render: {handles:?}"
    );
    assert!(
        !handles.contains(&format!("F:{foreign}")),
        "foreign-owner result leaked into the bundle: {handles:?}"
    );
    Ok(())
}

async fn ingest_execution_result_fixture(
    pool: &PgPool,
    owner: Owner,
    repo_id: Uuid,
    work_requested_memory_id: Uuid,
    summary: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    register_repo_row(pool, owner, repo_id).await?;
    let mut stamped = pool.begin().await?;
    let memory_id = fact_memory_in_tx(
        &mut stamped,
        owner,
        ExecutionResultV1::SCHEMA_ID,
        &["proxima_code.execution_result_v1"],
    )
    .await?;
    sqlx::query(
        "INSERT INTO proxima_code.execution_result_v1
            (t, work_requested_memory_id, repo_id, status, summary, artifact_refs, log_excerpt)
         VALUES ($1, $2, $3, 'succeeded', $4, ARRAY[]::text[], NULL)",
    )
    .bind(memory_id)
    .bind(work_requested_memory_id)
    .bind(repo_id)
    .bind(summary)
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;
    Ok(memory_id)
}

fn registry_for_mcp() -> Arc<FlavorRegistryFrozen> {
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry).unwrap();
    registry.add_fact_schema_or_panic_for_tests::<GoalActivationFixture>();
    Arc::new(registry.freeze_or_panic_for_tests())
}

fn registry_for_engine() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry).expect("code schema registration");
    registry.add_fact_schema_or_panic_for_tests::<GoalActivationFixture>();
    registry
        .try_add_opaque_schema(
            proxima_core::SchemaId::new(common::TEST_CITED_BLOB_SCHEMA_ID.into()),
            proxima_core::SchemaVersion::new(1),
            proxima_core::verbs::schema::PayloadKind::CitedObject,
        )
        .expect("test cited-object registration");
    registry
        .try_add_opaque_schema(
            proxima_core::SchemaId::new(common::TEST_CITATION_BLOB_SCHEMA_ID.into()),
            proxima_core::SchemaVersion::new(1),
            proxima_core::verbs::schema::PayloadKind::CitationMapping,
        )
        .expect("test citation-mapping registration");
    registry.freeze_or_panic_for_tests()
}

/// The `file_path` of every match, in rank order.
///
/// Asserting on paths rather than on a count: the lexical rescue band
/// returns anything sharing a content word, so two files that both say
/// `marker` both come back and a count assertion would be testing the
/// ranker rather than the scope.
fn match_paths(result: &serde_json::Value) -> Vec<String> {
    result["matches"]
        .as_array()
        .expect("matches")
        .iter()
        .map(|m| m["file_path"].as_str().expect("file_path").to_string())
        .collect()
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

/// Every Owner embedded by [`TopicEmbedding`].
fn topic_router() -> Arc<proxima_core::llm::SingleClientRouter> {
    Arc::new(
        proxima_core::llm::SingleClientRouter::bind(Arc::new(TopicEmbedding)).expect("lane width"),
    )
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
        let mut embedding = vec![0.0; proxima_core::llm::EmbeddingDim::D1024.width()];
        let on_topic = TOPIC_MARKERS.iter().any(|marker| text.contains(marker));
        embedding[usize::from(!on_topic)] = 1.0;
        Ok(embedding)
    }

    fn model_id(&self) -> &'static str {
        "test-topic-embed"
    }

    fn dim(&self) -> usize {
        proxima_core::llm::EmbeddingDim::D1024.width()
    }
}

/// `ctx`, but the engine embeds through `router`, so chunks are embedded
/// on ingest and the semantic arm has something to search.
fn embedding_ctx(
    pg: PgStorage,
    owner: Owner,
    registry: Arc<FlavorRegistryFrozen>,
    router: Arc<dyn proxima_core::llm::EmbeddingRouter>,
) -> McpToolCtx {
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let store = CodeFlavorStore::from_backend_pool_for_tests(pg.pool_for_tests().clone());
    let engine = Arc::new(engine_for_test(pg).with_embedding_router(router));
    McpToolCtx {
        owner,
        authz,
        registry,
        author: McpAuthorContext {
            model_id: "test/0".into(),
            trusted_model_id: None,
            client_name: "test".into(),
            client_version: "0".into(),
            caller_self_perspective: None,
        },
        caller_self_perspective: None,
        services: FlavorServices::with(store),
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
    ingest_topic_repo_with(fixture, owner, registry, temp, topic_router()).await
}

/// [`ingest_topic_repo`] embedding through `router`.
async fn ingest_topic_repo_with(
    fixture: &TestDb,
    owner: Owner,
    registry: &Arc<FlavorRegistryFrozen>,
    temp: &TempDir,
    router: Arc<dyn proxima_core::llm::EmbeddingRouter>,
) -> Result<String, Box<dyn std::error::Error>> {
    ingest_embedded_repo(
        fixture,
        owner,
        registry,
        temp,
        router,
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
    )
    .await
}

/// Register and ingest a repo of `files`, embedded through `router`.
async fn ingest_embedded_repo(
    fixture: &TestDb,
    owner: Owner,
    registry: &Arc<FlavorRegistryFrozen>,
    temp: &TempDir,
    router: Arc<dyn proxima_core::llm::EmbeddingRouter>,
    files: &[(&str, &str)],
) -> Result<String, Box<dyn std::error::Error>> {
    init_git_repo_with_files(temp.path(), files)?;
    let registered = run_tool::<CodeRegisterRepoTool>(
        embedding_ctx(fixture.pg.clone(), owner, registry.clone(), router.clone()),
        json!({ "path": temp.path().to_string_lossy(), "display_name": "Topic Repo" }),
    )
    .await?;
    let repo_handle = registered["repo"]["repo_id"]
        .as_str()
        .expect("repo_id")
        .to_string();
    run_tool::<CodeIngestHeadSnapshotTool>(
        embedding_ctx(fixture.pg.clone(), owner, registry.clone(), router.clone()),
        json!({ "repo_handle": repo_handle }),
    )
    .await?;

    // Ingest enqueues embedding_jobs when the engine has a client. Drain
    // claims those jobs; backfill is residue for heads written without a
    // model. Between ingest and this drain the repo is lexical-only.
    let engine = engine_for_test(fixture.pg.clone()).with_embedding_router(router);
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let _ = engine
        .backfill_missing_embeddings(&authz, &owner, 1_000)
        .await;
    let _ = engine.drain_embedding_jobs(1_000).await;
    Ok(repo_handle)
}

/// [`TopicEmbedding`] that keeps every text it is sent.
#[derive(Debug, Default)]
struct RecordingTopicEmbedding(std::sync::Mutex<Vec<String>>);

#[async_trait::async_trait]
impl proxima_core::llm::EmbeddingClient for RecordingTopicEmbedding {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, proxima_core::llm::LlmError> {
        self.0.lock().expect("recording").push(text.to_owned());
        TopicEmbedding.embed(text).await
    }

    fn model_id(&self) -> &'static str {
        TopicEmbedding.model_id()
    }

    fn dim(&self) -> usize {
        TopicEmbedding.dim()
    }
}

/// Code search embeds its query in the route's code instruction, not the
/// default one, while the chunks it ranks were embedded as they are (#354).
#[tokio::test]
async fn code_search_embeds_its_query_in_the_code_instruction()
-> Result<(), Box<dyn std::error::Error>> {
    use proxima_code::mcp::search_chunks::CODE_QUERY_TASK;
    use proxima_core::llm::{
        BoundEmbeddingClient, QueryInstruction, QueryInstructions, QueryTask, SingleClientRouter,
    };

    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    let recording = Arc::new(RecordingTopicEmbedding::default());
    let bound = BoundEmbeddingClient::bind(recording.clone())?.with_query_instructions(
        QueryInstructions::default()
            .with_instruction(QueryTask::DEFAULT, QueryInstruction::new("note: {query}")?)
            .with_instruction(CODE_QUERY_TASK, QueryInstruction::new("code: {query}")?),
    );
    let router = Arc::new(SingleClientRouter::new(bound));
    ingest_topic_repo_with(&fixture, owner, &registry, &temp, router.clone()).await?;
    let stored: Vec<String> = recording.0.lock().expect("recording").drain(..).collect();
    assert!(
        stored
            .iter()
            .any(|text| text.contains("halt_iteration") && !text.starts_with("code: ")),
        "chunks embed as they are: {stored:?}"
    );

    let found = run_tool::<CodeSearchChunksTool>(
        embedding_ctx(fixture.pg.clone(), owner, registry.clone(), router),
        json!({
            "query": "stop going round again",
            "mode": "semantic",
            "include_calls": false,
            "verbose": true,
        }),
    )
    .await?;
    assert_eq!(match_paths(&found)[0], "src/control.rs", "{found}");
    assert_eq!(
        found["matches"][0]["similarity_score"],
        json!(1.0),
        "{found}"
    );
    assert_eq!(
        *recording.0.lock().expect("recording"),
        ["code: stop going round again"]
    );
    Ok(())
}

/// A route's code-search weight decides a hybrid ranking the two arms
/// disagree on, and a call's `semantic_weight` overrides it. `notes.md`
/// shares a word with the query, so lexical ranks it first; `control.rs`
/// only means it, so semantic ranks it first; neither contains the query,
/// so no literal bonus settles it (#355).
#[tokio::test]
async fn a_routes_code_weight_decides_hybrid_ranking_and_a_call_overrides_it()
-> Result<(), Box<dyn std::error::Error>> {
    use proxima_code::mcp::search_chunks::CODE_QUERY_TASK;
    use proxima_core::llm::{BoundEmbeddingClient, SemanticWeight, SingleClientRouter};

    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    let semantic_leaning = Arc::new(SingleClientRouter::new(
        BoundEmbeddingClient::bind(Arc::new(TopicEmbedding))?
            .with_semantic_weight(CODE_QUERY_TASK, SemanticWeight::new(1.0)?),
    ));
    ingest_embedded_repo(
        &fixture,
        owner,
        &registry,
        &temp,
        semantic_leaning.clone(),
        &[
            (
                "docs/notes.md",
                "# Notes\n\nEvery release goes round the team for review.\n",
            ),
            (
                "src/control.rs",
                "pub fn halt_iteration(count: usize) -> bool {\n\
                 \x20   count > 3\n\
                 }\n",
            ),
        ],
    )
    .await?;
    let first = |router: Arc<dyn proxima_core::llm::EmbeddingRouter>, weight: Option<f32>| {
        let context = embedding_ctx(fixture.pg.clone(), owner, registry.clone(), router);
        async move {
            let found = run_tool::<CodeSearchChunksTool>(
                context,
                json!({
                    "query": "stop going round again",
                    "mode": "hybrid",
                    "semantic_weight": weight,
                    "include_calls": false,
                }),
            )
            .await?;
            Ok::<_, Box<dyn std::error::Error>>(match_paths(&found).remove(0))
        }
    };

    assert_eq!(first(topic_router(), None).await?, "docs/notes.md");
    assert_eq!(
        first(semantic_leaning.clone(), None).await?,
        "src/control.rs"
    );
    assert_eq!(first(semantic_leaning, Some(0.0)).await?, "docs/notes.md");
    assert_eq!(first(topic_router(), Some(1.0)).await?, "src/control.rs");
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

async fn fact_memory_in_tx(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    owner: Owner,
    schema_id: &str,
    sidecars: &[&str],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    fact_memory_on_handle_in_tx(tx, owner, schema_id, None, sidecars).await
}

/// Seeded, not ingested: `Engine::fact_ingest` writes no flavor sidecar and
/// so stamps none, and these fixtures write theirs by hand — which without
/// the stamp is a row `sidecar_tables` cannot reach.
async fn fact_memory_on_handle_in_tx(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    owner: Owner,
    schema_id: &str,
    handle: Option<Uuid>,
    sidecars: &[&str],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let (_, t) = common::seed_memory_with_sidecars_in_tx(
        tx,
        &owner,
        schema_id,
        "fact",
        None,
        handle,
        &[],
        sidecars,
    )
    .await?;
    Ok(t)
}

async fn abstraction_memory(
    pool: &PgPool,
    owner: &Owner,
    payload: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let t = Uuid::new_v5(&Uuid::NAMESPACE_OID, payload.as_bytes());
    // The stamp and the rows it promises land in one transaction: a
    // memory row that names a sidecar table it has no row in is refused
    // at COMMIT.
    let mut stamped = pool.begin().await?;
    let _ = common::seed_memory_with_sidecars_in_tx(
        &mut stamped,
        owner,
        "core/agent-derivation-v1",
        "abstraction",
        Some(t),
        None,
        &[],
        &["proxima_core.agent_derivation_v1"],
    )
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.agent_derivation_v1
            (t, title, body, tags, model_id, client_name, client_version)
         VALUES ($1, 'planning context', $2, ARRAY[]::text[], 'test-model', 'test', '1')",
    )
    .bind(t)
    .bind(payload)
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;
    Ok(t)
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

    // A real repository exists, so an empty answer would be about the
    // handle rather than about an empty index.
    let real_repo = Uuid::now_v7();
    ingest_file_revision(
        fixture.pg.pool_for_tests(),
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
    owner: Owner,
    repo_id: Uuid,
    file_path: &str,
    indexed_commit_sha: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let payload = format!("{file_path}:{indexed_commit_sha}");
    register_repo_row(pool, owner, repo_id).await?;
    let handle = existing_file_revision_handle(pool, &owner, repo_id, file_path).await?;
    // The stamp and the row it promises land in one transaction: a memory row
    // that names a sidecar table it has no row in is refused at COMMIT.
    let mut stamped = pool.begin().await?;
    let memory_id = fact_memory_on_handle_in_tx(
        &mut stamped,
        owner,
        FileRevisionV1::SCHEMA_ID,
        handle,
        &["proxima_code.file_revision_v1"],
    )
    .await?;
    sqlx::query(
        "INSERT INTO proxima_code.file_revision_v1
            (t, repo_id, file_path, language, content_sha256,
             size_bytes, indexed_commit_sha, state)
         VALUES ($1, $2, $3, 'rust', $4, $5, $6, 'Present')",
    )
    .bind(memory_id)
    .bind(repo_id)
    .bind(file_path)
    .bind(blake3::hash(payload.as_bytes()).as_bytes().to_vec())
    .bind(i64::try_from(payload.len())?)
    .bind(indexed_commit_sha)
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;
    Ok(memory_id)
}

async fn ingest_code_chunk(
    pool: &PgPool,
    owner: Owner,
    repo_id: Uuid,
    file_path: &str,
    chunk_index: i32,
    text: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    ingest_code_chunk_with_type(
        pool,
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
    owner: Owner,
    chunk: ChunkFixture<'_>,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let file_revision =
        ensure_present_file_revision(pool, owner, chunk.repo_id, chunk.file_path).await?;
    let handle = Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!(
            "{}:{}:{}",
            chunk.repo_id, chunk.file_path, chunk.chunk_index
        )
        .as_bytes(),
    );
    // The stamp and the row it promises land in one transaction: a memory row
    // that names a sidecar table it has no row in is refused at COMMIT.
    let mut stamped = pool.begin().await?;
    let memory_id = code_chunk_memory_in_tx(&mut stamped, &owner, handle, &[file_revision]).await?;
    let line_count = i64::try_from(chunk.text.lines().count().max(1))?;
    sqlx::query(
        "INSERT INTO proxima_code.code_chunk_v1
            (t, repo_id, file_path, chunk_index, text, language,
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
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;
    // Hand-seeded sidecar, hand-kept projection. Without this the chunk is
    // invisible to the ranked arm, and every assertion below was in fact
    // being served by the substring fallback — which is exactly the state
    // an owner-blind, projection-blind fallback can hide.
    common::project_code(
        pool,
        memory_id,
        <proxima_code::CodeChunkV1 as AbstractionPayload>::SCHEMA_ID,
        None,
    )
    .await?;
    insert_origin_edge(pool, &owner, memory_id, file_revision).await?;
    Ok(memory_id)
}

async fn existing_file_revision_handle(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
    file_path: &str,
) -> Result<Option<Uuid>, Box<dyn std::error::Error>> {
    Ok(sqlx::query_scalar(
        "SELECT h.handle
           FROM proxima_core.memory_head h
           JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
           JOIN proxima_code.file_revision_v1 fr ON fr.t = m.t
          WHERE m.owner_id = $1
            AND fr.repo_id = $2
            AND fr.file_path = $3",
    )
    .bind(owner.stored_owner_id())
    .bind(repo_id)
    .bind(file_path)
    .fetch_optional(pool)
    .await?)
}

async fn latest_file_revision(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
    file_path: &str,
) -> Result<Option<(Uuid, FileState)>, Box<dyn std::error::Error>> {
    Ok(sqlx::query_as(
        "SELECT fr.t, fr.state
           FROM proxima_core.memory_head h
           JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
           JOIN proxima_code.file_revision_v1 fr ON fr.t = m.t
          WHERE m.owner_id = $1
            AND fr.repo_id = $2
            AND fr.file_path = $3",
    )
    .bind(owner.stored_owner_id())
    .bind(repo_id)
    .bind(file_path)
    .fetch_optional(pool)
    .await?)
}

async fn ensure_present_file_revision(
    pool: &PgPool,
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
        owner,
        repo_id,
        file_path,
        &format!("fixture-present-{}", Uuid::now_v7()),
    )
    .await
}

async fn code_chunk_memory_in_tx(
    stamped: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    owner: &Owner,
    handle: Uuid,
    origins: &[Uuid],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let t = Uuid::now_v7();
    let _ = common::seed_memory_with_sidecars_in_tx(
        stamped,
        owner,
        <CodeChunkV1 as AbstractionPayload>::SCHEMA_ID,
        "abstraction",
        Some(t),
        Some(handle),
        origins,
        &[<CodeChunkV1 as AbstractionPayload>::sidecar_table()],
    )
    .await?;
    Ok(t)
}

#[allow(clippy::unused_async)]
async fn insert_origin_edge(
    _pool: &PgPool,
    _owner: &Owner,
    _chunk_memory_id: Uuid,
    _file_revision_memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

#[allow(dead_code, clippy::unused_async)]
async fn force_same_memory_created_at(
    _pool: &PgPool,
    _memory_ids: &[Uuid],
) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

async fn ingest_commit(
    pool: &PgPool,
    owner: Owner,
    repo_id: Uuid,
    sha: &str,
    message: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    // The stamp and the row it promises land in one transaction: see above.
    let mut stamped = pool.begin().await?;
    let memory_id = fact_memory_in_tx(
        &mut stamped,
        owner,
        CommitV1::SCHEMA_ID,
        &["proxima_code.commit_v1"],
    )
    .await?;
    let now = time::OffsetDateTime::now_utc();
    sqlx::query(
        "INSERT INTO proxima_code.commit_v1
            (t, repo_id, sha, parents, author_name, author_email,
             author_time, committer_name, committer_email, committer_time, message)
         VALUES ($1, $2, $3, ARRAY[]::text[], 'Ada', 'ada@example.test',
             $4, 'Ada', 'ada@example.test', $4, $5)",
    )
    .bind(memory_id)
    .bind(repo_id)
    .bind(sha)
    .bind(now)
    .bind(message)
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;
    common::project_code(
        pool,
        memory_id,
        <proxima_code::CommitV1 as proxima_core::FactPayload>::SCHEMA_ID,
        None,
    )
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
    // The stamp and the row it promises land in one transaction: see above.
    let mut stamped = pool.begin().await?;
    let (_, memory_id) = common::seed_memory_with_sidecars_in_tx(
        &mut stamped,
        owner,
        <proxima_code::CommitSummaryV1 as AbstractionPayload>::SCHEMA_ID,
        "abstraction",
        None,
        None,
        &[],
        &[<proxima_code::CommitSummaryV1 as AbstractionPayload>::sidecar_table()],
    )
    .await?;

    let files: Vec<String> = key_files.iter().map(|file| (*file).to_string()).collect();
    sqlx::query(
        "INSERT INTO proxima_code.commit_summary_v1
            (t, repo_id, commit_sha, summary, key_files, change_kind)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(memory_id)
    .bind(repo_id)
    .bind(commit_sha)
    .bind(summary)
    .bind(files)
    .bind(change_kind)
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;
    // Hand-seeded sidecar, hand-kept projection: the read path ranks on the
    // projection now, so a fixture that skips it is invisible to search.
    common::project_code(
        pool,
        memory_id,
        <proxima_code::CommitSummaryV1 as AbstractionPayload>::SCHEMA_ID,
        None,
    )
    .await?;
    Ok(memory_id)
}

// ── Provenance, spent by the lineage walk ───────────────────────────────

/// The handles `core_think` visited, in order.
fn think_handles(page: &serde_json::Value) -> Vec<String> {
    page["visits"]
        .as_array()
        .expect("visits")
        .iter()
        .map(|visit| visit["handle"].as_str().expect("handle").to_owned())
        .collect()
}

/// `Provenance::None` means the walk does not read `origins`, and the only
/// way to see that is a node that HAS origins.
///
/// The arm is `Some(Provenance::None) => {}`, sitting next to
/// `None | Some(OriginEdges) => ancestors.extend(node.origins)`. Replacing
/// the empty block with the extend killed no test in the tree, because every
/// `None`-declared schema that reaches this walk is one whose rows never
/// carry origins: Facts are blocked by `memory_fact_origins_chk` and Goals
/// are not memory rows at all. The declaration was therefore true by
/// accident of the data rather than by anything the walk does.
///
/// `code/commit-summary-v1` is an Abstraction — so origins are permitted —
/// and declares `Provenance::None`. The origins are written straight into
/// `memory.origins` here, because no verb would put them there.
///
/// The control is what makes the assertion mean anything: an
/// `agent-derivation-v1` Abstraction with the SAME origin, declared
/// `OriginEdges`, must be reached. Without it, "the fact is absent" would be
/// equally consistent with a broken harness.
#[tokio::test]
async fn the_walk_does_not_read_origins_a_schema_declares_it_does_not_use()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let pool = fixture.pg.pool_for_tests();

    let (_, origin_t) =
        common::seed_memory(pool, &owner, "core/test-fact-v1", "fact", None, None, &[]).await?;

    // Declares Provenance::None, and carries an origin anyway.
    let (_, silent_t) = common::seed_memory(
        pool,
        &owner,
        "proxima-code/commit-summary-v1",
        "abstraction",
        None,
        None,
        &[origin_t],
    )
    .await?;

    // The control: same origin, a schema that declares OriginEdges.
    let (_, speaking_t) = common::seed_memory(
        pool,
        &owner,
        "core/agent-derivation-v1",
        "abstraction",
        None,
        None,
        &[origin_t],
    )
    .await?;

    let origin_handle = format!("F:{origin_t}");

    let control = run_tool::<proxima_core::mcp::core_tools::ThinkTool>(
        ctx(fixture.pg.clone(), owner, Arc::clone(&registry)),
        json!({ "seeds": [format!("A:{speaking_t}")], "direction": "ancestors", "depth": 2 }),
    )
    .await?;
    assert!(
        think_handles(&control).contains(&origin_handle),
        "an OriginEdges schema's origins ARE the lineage, so the harness can \
         see them: {:?}",
        think_handles(&control)
    );

    let walked = run_tool::<proxima_core::mcp::core_tools::ThinkTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "seeds": [format!("A:{silent_t}")], "direction": "ancestors", "depth": 2 }),
    )
    .await?;
    let handles = think_handles(&walked);
    assert!(
        !handles.contains(&origin_handle),
        "proxima-code/commit-summary-v1 declares Provenance::None, so the walk \
         must not treat its origins array as lineage; got {handles:?}"
    );
    Ok(())
}

/// `work-assignment-v1` grounds through two scalar payload columns, and the
/// walk reaches both.
///
/// The declaration changed from `None` to `PayloadOnly` in this branch on
/// the strength of a comment at the write site. Nothing exercised it: the
/// only behavioural coverage of the `PayloadOnly` arm is core's
/// `interpretation-v1`, which grounds through ONE ARRAY column. Two scalar
/// columns is the other shape, and a `payload_subjects` that handled arrays
/// and not scalars would have passed every test in the tree.
///
/// The assignment's own `origins` array is also asserted absent. `PayloadOnly`
/// means the walk does not read origins, and `seed_memory` gives every
/// non-Fact an origin, so that array is populated and must be ignored — the
/// same property as the test above, from the other arm.
#[tokio::test]
async fn a_work_assignment_walk_reaches_both_subjects_its_payload_names()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let pool = fixture.pg.pool_for_tests();

    let (_, work_item_t) =
        common::seed_memory(pool, &owner, "core/test-fact-v1", "fact", None, None, &[]).await?;
    // The stamp and the rows it promises land in one transaction: a
    // memory row that names a sidecar table it has no row in is refused
    // at COMMIT.
    let mut stamped = pool.begin().await?;
    let (_, target_t) = common::seed_memory_with_sidecars_in_tx(
        &mut stamped,
        &owner,
        "core/interpretation-v1",
        "perspective",
        None,
        None,
        &[],
        &["proxima_core.interpretation_v1"],
    )
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.interpretation_v1
            (t, claim, confidence, model_id, client_name, client_version)
         VALUES ($1, 'the assignment target', 100, 'test-model', 'test', '1')",
    )
    .bind(target_t)
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;

    // The stamp and the rows it promises land in one transaction: a
    // memory row that names a sidecar table it has no row in is refused
    // at COMMIT.
    let mut stamped = pool.begin().await?;
    let (_, assignment_t) = common::seed_memory_with_sidecars_in_tx(
        &mut stamped,
        &owner,
        "proxima-code/work-assignment-v1",
        "perspective",
        None,
        None,
        &[],
        &["proxima_code.work_assignment_v1"],
    )
    .await?;
    sqlx::query(
        "INSERT INTO proxima_code.work_assignment_v1
            (t, repo_id, target_perspective_memory_id, work_item_memory_id, reason)
         VALUES ($1, $2, $3, $4, 'a fixture')",
    )
    .bind(assignment_t)
    .bind(Uuid::now_v7())
    .bind(target_t)
    .bind(work_item_t)
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;

    let assignment_origins: Vec<Uuid> =
        sqlx::query_scalar("SELECT origins FROM proxima_core.memory WHERE t = $1")
            .bind(assignment_t)
            .fetch_one(pool)
            .await?;
    assert!(
        !assignment_origins.is_empty(),
        "the origins array has to be non-empty for the second assertion below \
         to be about anything"
    );

    let walked = run_tool::<proxima_core::mcp::core_tools::ThinkTool>(
        ctx(fixture.pg.clone(), owner, registry),
        json!({ "seeds": [format!("P:{assignment_t}")], "direction": "ancestors", "depth": 2 }),
    )
    .await?;
    let handles = think_handles(&walked);

    assert!(
        handles.contains(&format!("F:{work_item_t}")),
        "the walk reaches the work item the assignment's payload names: {handles:?}"
    );
    assert!(
        handles.contains(&format!("P:{target_t}")),
        "and the target perspective, which is the second declared subject and \
         the other endpoint kind: {handles:?}"
    );
    for origin in &assignment_origins {
        assert!(
            !handles.contains(&format!("A:{origin}")),
            "PayloadOnly means the origins array is not lineage: {handles:?}"
        );
    }
    Ok(())
}

/// Poll `proxima-code_get_ingest_run` until `run_id` is terminal.
async fn await_terminal_run(
    fixture: &TestDb,
    owner: Owner,
    registry: &Arc<FlavorRegistryFrozen>,
    run_id: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let deadline = tokio::time::Instant::now() + Duration::from_mins(1);
    loop {
        let read = run_tool::<CodeGetIngestRunTool>(
            ctx(fixture.pg.clone(), owner, registry.clone()),
            json!({ "run_id": run_id }),
        )
        .await?;
        if matches!(read["run"]["status"].as_str(), Some("succeeded" | "failed")) {
            return Ok(read["run"].clone());
        }
        if tokio::time::Instant::now() > deadline {
            return Err(format!("run {run_id} never finished: {read}").into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A HEAD ingest is a tracked run (issue #347): a synchronous call longer
/// than the MCP session idle timeout lost its result, so the async tool
/// answers at once and the run is read back; the synchronous tool drives a
/// run too, so its result survives a lost response. One active run per
/// owner and repository; a run whose driver stopped reads failed and is
/// retired by the next start.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_head_ingest_is_a_run_started_polled_and_recovered()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let call_ctx = || ctx(fixture.pg.clone(), owner, registry.clone());
    let temp = TempDir::new()?;
    init_git_repo_with_files(
        temp.path(),
        &[
            ("src/lib.rs", "pub fn run_marker() -> u64 { 1 }\n"),
            ("src/two.rs", "pub fn second_marker() -> u64 { 2 }\n"),
        ],
    )?;
    let registered = run_tool::<CodeRegisterRepoTool>(
        call_ctx(),
        json!({ "path": temp.path().to_string_lossy() }),
    )
    .await?;
    let repo_handle = registered["repo"]["repo_handle"]
        .as_str()
        .ok_or("repo_handle")?
        .to_owned();
    let repo_id = Uuid::parse_str(registered["repo"]["repo_id"].as_str().ok_or("repo_id")?)?;

    // The synchronous tool drives a run and names it; the run carries its
    // report, so a caller whose response was lost reads the same numbers.
    let synchronous =
        run_tool::<CodeIngestHeadSnapshotTool>(call_ctx(), json!({ "repo_handle": repo_handle }))
            .await?;
    let sync_run = synchronous["run_id"].as_str().ok_or("run_id")?;
    let recovered =
        run_tool::<CodeGetIngestRunTool>(call_ctx(), json!({ "repo_handle": repo_handle })).await?;
    let run = &recovered["run"];
    assert_eq!(run["run_id"], sync_run, "{recovered}");
    assert_eq!(
        (&run["status"], &run["stage"]),
        (&json!("succeeded"), &json!("done"))
    );
    assert_eq!(run["files_emitted"], 2, "{recovered}");
    assert_eq!(
        run["chunks_emitted"],
        synchronous["report"]["chunks_emitted"]
    );
    assert!(run["finished_at"].is_string(), "{recovered}");

    // The async tool answers before the ingest and the run finishes behind it.
    std::fs::write(
        temp.path().join("src/two.rs"),
        "pub fn second_marker() -> u64 { 3 }\n",
    )?;
    run_git(temp.path(), &["add", "."])?;
    run_git(
        temp.path(),
        &[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-m",
            "edit",
        ],
    )?;
    let started = run_tool::<CodeStartIngestHeadSnapshotTool>(
        call_ctx(),
        json!({ "repo_handle": repo_handle }),
    )
    .await?;
    assert_eq!(started["started"], true, "{started}");
    let async_run = started["run"]["run_id"]
        .as_str()
        .ok_or("run_id")?
        .to_owned();
    let finished = await_terminal_run(&fixture, owner, &registry, &async_run).await?;
    assert_eq!(finished["status"], "succeeded", "{finished}");
    assert_eq!(
        finished["files_emitted"], 1,
        "only the edited file moved: {finished}"
    );

    // Another owner cannot read it.
    let stranger = run_tool::<CodeGetIngestRunTool>(
        ctx(fixture.pg.clone(), owner_fixture(), registry.clone()),
        json!({ "run_id": async_run }),
    )
    .await;
    assert!(stranger.is_err(), "another owner's run is not found");

    // While a run is active, a start returns it and the synchronous tool
    // refuses, naming it.
    let pool = fixture.pg.pool_for_tests();
    let active = proxima_code::testkit::start_run(pool, None, &owner, repo_id).await?;
    let again = run_tool::<CodeStartIngestHeadSnapshotTool>(
        call_ctx(),
        json!({ "repo_handle": repo_handle }),
    )
    .await?;
    assert_eq!(again["started"], false, "{again}");
    assert_eq!(again["run"]["run_id"], active.run_id.to_string());
    let refused =
        run_tool::<CodeIngestHeadSnapshotTool>(call_ctx(), json!({ "repo_handle": repo_handle }))
            .await
            .expect_err("an active run blocks the synchronous ingest");
    assert!(
        refused.to_string().contains(&active.run_id.to_string()),
        "{refused}"
    );

    // Its driver never heartbeats: after five minutes it reads failed, and
    // the next start retires it and runs.
    sqlx::query(
        "UPDATE proxima_code.repo_ingestion_runs \
            SET updated_at = now() - interval '6 minutes' WHERE run_id = $1",
    )
    .bind(active.run_id)
    .execute(pool)
    .await?;
    let stale = run_tool::<CodeGetIngestRunTool>(
        call_ctx(),
        json!({ "run_id": active.run_id.to_string() }),
    )
    .await?;
    assert_eq!(stale["run"]["status"], "failed", "{stale}");
    assert!(
        stale["run"]["error_message"]
            .as_str()
            .is_some_and(|message| message.contains("abandoned")),
        "{stale}"
    );
    let restarted = run_tool::<CodeStartIngestHeadSnapshotTool>(
        call_ctx(),
        json!({ "repo_handle": repo_handle }),
    )
    .await?;
    assert_eq!(restarted["started"], true, "{restarted}");
    let (status, finished_at): (String, Option<time::OffsetDateTime>) = sqlx::query_as(
        "SELECT status::text, finished_at FROM proxima_code.repo_ingestion_runs WHERE run_id = $1",
    )
    .bind(active.run_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(
        status, "failed",
        "the stale run was retired, not left active"
    );
    assert!(finished_at.is_some());
    let restarted_run = restarted["run"]["run_id"]
        .as_str()
        .ok_or("run_id")?
        .to_owned();
    await_terminal_run(&fixture, owner, &registry, &restarted_run).await?;

    // A failing ingest records its error on the run.
    std::fs::remove_dir_all(temp.path().join(".git"))?;
    let doomed = run_tool::<CodeStartIngestHeadSnapshotTool>(
        call_ctx(),
        json!({ "repo_handle": repo_handle }),
    )
    .await?;
    let doomed_run = doomed["run"]["run_id"].as_str().ok_or("run_id")?.to_owned();
    let failed = await_terminal_run(&fixture, owner, &registry, &doomed_run).await?;
    assert_eq!(failed["status"], "failed", "{failed}");
    assert!(failed["error_message"].is_string(), "{failed}");
    Ok(())
}
