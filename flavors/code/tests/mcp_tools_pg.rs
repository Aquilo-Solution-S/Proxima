use std::sync::Arc;
use std::time::Duration;

use proxima_code::mcp::{CodeOpenFileRevisionTool, CodeSearchChunksTool, CodeSearchCommitsTool};
use proxima_code::{CodeChunkV1, CommitV1, FileRevisionV1, register_repo};
use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::engine::Engine;
use proxima_core::mcp::{HandleTable, McpAuthorContext, McpTool, McpToolCtx, OutputMode};
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{
    AbstractionPayload, FactPayload, FlavorRegistry, FlavorRegistryFrozen, OrgId, Owner, Principal,
    SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use serde_json::json;
use sqlx::{Connection, Executor, PgConnection, PgPool};
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/proxima";

#[tokio::test]
async fn search_chunks_returns_only_head_per_nk() -> Result<(), Box<dyn std::error::Error>> {
    let Some(fixture) = TestDb::fresh().await? else {
        return Ok(());
    };
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone(), owner.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    ingest_code_chunk(
        fixture.pg.pool(),
        &engine,
        owner.clone(),
        repo_id,
        "src/atlas.rs",
        0,
        "fn atlas_edges_v1() {}",
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;
    ingest_code_chunk(
        fixture.pg.pool(),
        &engine,
        owner.clone(),
        repo_id,
        "src/atlas.rs",
        0,
        "fn atlas_edges_v2() {}",
    )
    .await?;

    let result = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.pool().clone(), owner, registry),
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
    let Some(fixture) = TestDb::fresh().await? else {
        return Ok(());
    };
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone(), owner.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    ingest_code_chunk(
        fixture.pg.pool(),
        &engine,
        owner.clone(),
        repo_id,
        "src/atlas.rs",
        0,
        "fn atlas_edges_v1() {}",
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;
    ingest_code_chunk_tombstone(
        fixture.pg.pool(),
        &engine,
        owner.clone(),
        repo_id,
        "src/atlas.rs",
        0,
    )
    .await?;

    let result = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.pool().clone(), owner, registry),
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
async fn search_chunks_includes_calls_edges_when_present() -> Result<(), Box<dyn std::error::Error>>
{
    let Some(fixture) = TestDb::fresh().await? else {
        return Ok(());
    };
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone(), owner.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    let source_chunk = ingest_code_chunk(
        fixture.pg.pool(),
        &engine,
        owner.clone(),
        repo_id,
        "src/a.rs",
        0,
        "fn a() { b(); }",
    )
    .await?;
    let target_chunk = ingest_code_chunk(
        fixture.pg.pool(),
        &engine,
        owner.clone(),
        repo_id,
        "src/b.rs",
        0,
        "fn b() {}",
    )
    .await?;
    ingest_calls_edge(fixture.pg.pool(), &owner, source_chunk, target_chunk, "b").await?;

    let result = run_tool::<CodeSearchChunksTool>(
        ctx(fixture.pg.pool().clone(), owner, registry),
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
    let Some(fixture) = TestDb::fresh().await? else {
        return Ok(());
    };
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone(), owner.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    ingest_code_chunk_with_type(
        fixture.pg.pool(),
        &engine,
        owner.clone(),
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
        fixture.pg.pool(),
        &engine,
        owner.clone(),
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
        ctx(fixture.pg.pool().clone(), owner, registry),
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
    let Some(fixture) = TestDb::fresh().await? else {
        return Ok(());
    };
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone(), owner.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    ingest_file_revision(
        fixture.pg.pool(),
        &engine,
        owner.clone(),
        repo_id,
        "src/atlas.rs",
        "v1",
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;
    ingest_file_revision(
        fixture.pg.pool(),
        &engine,
        owner.clone(),
        repo_id,
        "src/atlas.rs",
        "v2",
    )
    .await?;
    ingest_code_chunk(
        fixture.pg.pool(),
        &engine,
        owner.clone(),
        repo_id,
        "src/atlas.rs",
        0,
        "fn a() {\n    call();\n}",
    )
    .await?;
    ingest_code_chunk(
        fixture.pg.pool(),
        &engine,
        owner.clone(),
        repo_id,
        "src/atlas.rs",
        1,
        "fn b() {}",
    )
    .await?;

    let test_ctx = ctx(fixture.pg.pool().clone(), owner.clone(), registry);
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
        ctx(fixture.pg.pool().clone(), owner.clone(), registry_for_mcp()),
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
        ctx(fixture.pg.pool().clone(), owner, registry_for_mcp()),
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
async fn open_file_revision_accepts_raw_repo_uuid() -> Result<(), Box<dyn std::error::Error>> {
    let Some(fixture) = TestDb::fresh().await? else {
        return Ok(());
    };
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone(), owner.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    ingest_file_revision(
        fixture.pg.pool(),
        &engine,
        owner.clone(),
        repo_id,
        "src/raw.rs",
        "v1",
    )
    .await?;

    let result = run_tool::<CodeOpenFileRevisionTool>(
        ctx(fixture.pg.pool().clone(), owner, registry),
        json!({ "repo_handle": repo_id.to_string(), "file_path": "src/raw.rs" }),
    )
    .await?;

    assert_eq!(result["revision"]["indexed_commit_sha"], "v1");
    Ok(())
}

#[tokio::test]
async fn open_file_revision_accepts_unambiguous_repo_display_name()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(fixture) = TestDb::fresh().await? else {
        return Ok(());
    };
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone(), owner.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();
    register_repo(
        fixture.pg.pool(),
        &owner,
        repo_id,
        "/tmp/proxima-mcp-display",
        "Proxima",
    )
    .await?;

    ingest_file_revision(
        fixture.pg.pool(),
        &engine,
        owner.clone(),
        repo_id,
        "src/atlas.rs",
        "v1",
    )
    .await?;

    let result = run_tool::<CodeOpenFileRevisionTool>(
        ctx(fixture.pg.pool().clone(), owner, registry),
        json!({ "repo_handle": "proxima", "file_path": "src/atlas.rs" }),
    )
    .await?;

    assert_eq!(result["revision"]["indexed_commit_sha"], "v1");
    assert_eq!(result["revision"]["repo_handle"], "R1");
    Ok(())
}

#[tokio::test]
async fn search_commits_unions_commit_and_summary_legs() -> Result<(), Box<dyn std::error::Error>> {
    let Some(fixture) = TestDb::fresh().await? else {
        return Ok(());
    };
    let owner = owner_fixture();
    let engine = engine_for_test(fixture.pg.clone(), owner.clone());
    let registry = registry_for_mcp();
    let repo_id = Uuid::now_v7();

    ingest_commit(
        fixture.pg.pool(),
        &engine,
        owner.clone(),
        repo_id,
        "deadbeef",
        "fix atlas edges",
    )
    .await?;
    ingest_commit_summary(
        fixture.pg.pool(),
        &owner,
        repo_id,
        "deadbeef",
        "Hardens the atlas edge cap.",
        &["src/atlas.rs"],
        "Refactor",
    )
    .await?;

    let result = run_tool::<CodeSearchCommitsTool>(
        ctx(fixture.pg.pool().clone(), owner, registry),
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

async fn run_tool<T: McpTool>(
    ctx: McpToolCtx,
    args: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let typed: T::Args = serde_json::from_value(args)?;
    let output = T::call(ctx, typed).await?;
    Ok(serde_json::to_value(output)?)
}

fn ctx(pool: PgPool, owner: Owner, registry: Arc<FlavorRegistryFrozen>) -> McpToolCtx {
    McpToolCtx {
        pool,
        owner,
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
        engine: None,
    }
}

#[derive(Debug)]
struct TestDb {
    name: String,
    pg: PgStorage,
}

impl TestDb {
    async fn fresh() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let name = format!("proxima_test_{}", Uuid::now_v7().simple());
        if create_db(&name).await.is_err() {
            panic!("PG required for tests but admin connect failed");
        }
        let setup: Result<PgStorage, Box<dyn std::error::Error>> = async {
            let pg = PgStorage::connect(&format!("postgres://proxima:proxima@localhost/{name}")).await?;
            pg.run_migrations().await?;
            proxima_code::migrator().run(pg.pool()).await?;
            Ok(pg)
        }
        .await;

        match setup {
            Ok(pg) => Ok(Some(Self { name, pg })),
            Err(error) => {
                let _ = drop_db(&name).await;
                Err(error)
            }
        }
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let name = self.name.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("drop runtime");
            runtime.block_on(async {
                let _ = drop_db(&name).await;
            });
        })
        .join()
        .expect("drop db thread");
    }
}

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

fn owner_fixture() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

fn registry_for_mcp() -> Arc<FlavorRegistryFrozen> {
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry);
    Arc::new(registry.freeze())
}

fn registry_for_engine() -> FlavorRegistryFrozen {
    let mut flavor = FlavorRegistry::new();
    proxima_code::register(&mut flavor);
    let mut schemas = flavor.freeze().list();
    schemas.push(SchemaInfo {
        schema_id: SchemaId::new("test/cited_blob".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::CitedObject,
        filter_keys: vec![],
        sidecar_table: None,
        natural_key_columns: vec![],
        tombstone: None,
        cbor_encoder: None,
    });
    schemas.push(SchemaInfo {
        schema_id: SchemaId::new("test/citation_blob".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::CitationMapping,
        filter_keys: vec![],
        sidecar_table: None,
        natural_key_columns: vec![],
        tombstone: None,
        cbor_encoder: None,
    });
    FlavorRegistryFrozen::with_schemas(schemas)
}

fn engine_for_test(pg: PgStorage, owner: Owner) -> Engine {
    let principal = owner.principal.clone();
    let storage: Arc<dyn Storage> = Arc::new(pg);
    Engine::new(
        registry_for_engine(),
        MemoryStore::new(),
        Box::new(NoAuth::new(principal, owner)),
    )
    .with_storage(storage)
}

fn fact_draft(owner: Owner, schema_id: &str, payload: &[u8]) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner,
        schema_id: SchemaId::new(schema_id.into()),
        schema_version: SchemaVersion::new(1),
        payload: payload.to_vec(),
        observed_at: now,
        occurred_at: now,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new("test/cited_blob".into()),
            schema_version: SchemaVersion::new(1),
            content_hash: blake3::hash(payload).into(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new("test/citation_blob".into()),
            schema_version: SchemaVersion::new(1),
        },
    }
}

async fn fact_memory(
    engine: &Engine,
    owner: Owner,
    schema_id: &str,
    payload: &[u8],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    Ok(engine
        .event_ingest(&Credentials::None, fact_draft(owner, schema_id, payload))
        .await?
        .memory_id
        .into_inner())
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
    let payload = format!("{}:{}:{}", chunk.file_path, chunk.chunk_index, chunk.text);
    let memory_id = fact_memory(engine, owner, CodeChunkV1::SCHEMA_ID, payload.as_bytes()).await?;
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
    let payload = format!("{file_path}:{chunk_index}:tombstone");
    let memory_id = fact_memory(engine, owner, CodeChunkV1::SCHEMA_ID, payload.as_bytes()).await?;
    sqlx::query(
        "INSERT INTO proxima_code.code_chunk_v1
            (memory_id, repo_id, file_path, chunk_index, text, language,
             chunk_type, byte_range_start, byte_range_end,
             line_range_start, line_range_end, state)
         VALUES ($1, $2, $3, $4, '', 'rust',
             'function', 0, 0, 1, 1, 'Tombstone')",
    )
    .bind(memory_id)
    .bind(repo_id)
    .bind(file_path)
    .bind(chunk_index)
    .execute(pool)
    .await?;
    Ok(memory_id)
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
    let (owner_kind, owner_id) = owner_principal(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id, prompt_version,
             personality_instance_id)
         VALUES ($1, $2, $3, $4, $5, 1, $6, $7,
             $8, 'test/0', 'test', $9)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner.org_id.into_inner())
    .bind(proxima_code::CommitSummaryV1::schema_id().into_inner())
    .bind(proxima_core::EntityKind::Abstraction)
    .bind(summary)
    .bind(proxima_core::MemoryOperatorKind::FtoA)
    .bind(Uuid::nil())
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
    let (owner_kind, owner_id) = owner_principal(owner);
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, target_kind, target_memory_id,
             authorship_kind, owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1, 'proxima-code/calls', 'Structural',
             'Fact', $2, 'Fact', $3,
             'EventSource', $4, $5, $6)",
    )
    .bind(edge_id)
    .bind(source_chunk)
    .bind(target_chunk)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(owner.org_id.into_inner())
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

fn owner_principal(owner: &Owner) -> (proxima_core::OwnerPrincipalKind, Uuid) {
    let kind = proxima_core::OwnerPrincipalKind::of(&owner.principal);
    let principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    (kind, principal_id)
}
