use std::sync::Arc;

mod common;

use common::{ConstantEmbedding, drop_db, fresh_pg, owner_fixture};
use proxima_core::engine::Engine;
use proxima_core::mcp::core_tools::get_memory::{GetMemoryArgs, get_memory};
use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolCtx, McpToolExtensions, OutputMode};
use proxima_core::{
    AgentNoteV1, AuthPath, AuthzContext, CitationMappingPayload, CitedObjectPayload, ErrorCode,
    FactPayload, FlavorRegistry, FlavorRegistryFrozen, GroupId, McpToolError, MemoryId, Owner,
    OwnerRef, OwnerRefKind, Role, SchemaId, UserId,
};
use proxima_storage_pg::sidecars::{
    PgCitationMappingSidecar, PgCitedObjectSidecar, PgSidecarFuture,
};
use proxima_storage_pg::{PgSidecarRegistry, register_core_pg_sidecars};
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct RememberTestCitedObject {
    artifact_id: String,
    locator: String,
}

impl CitedObjectPayload for RememberTestCitedObject {
    const SCHEMA_ID: &'static str = "test/remember-cited-object-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "public.remember_test_cited_object_v1"
    }

    fn idempotency_key(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.artifact_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.locator.as_bytes());
        *hasher.finalize().as_bytes()
    }
}

impl PgCitedObjectSidecar for RememberTestCitedObject {
    fn insert_cited_object_sidecar<'t>(
        &'t self,
        tx: &'t mut sqlx::PgConnection,
        cited_object_id: uuid::Uuid,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.remember_test_cited_object_v1
                    (cited_object_id, artifact_id, locator)
                 VALUES ($1, $2, $3)
                 ON CONFLICT (cited_object_id) DO NOTHING",
            )
            .bind(cited_object_id)
            .bind(&self.artifact_id)
            .bind(&self.locator)
            .execute(tx)
            .await
            .map_err(|err| proxima_core::StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct RememberTestCitationMapping {
    section: String,
    byte_start: i32,
    byte_end: i32,
}

impl CitationMappingPayload for RememberTestCitationMapping {
    const SCHEMA_ID: &'static str = "test/remember-citation-mapping-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        Some("public.remember_test_citation_mapping_v1")
    }

    fn cited_object_schema() -> SchemaId {
        RememberTestCitedObject::schema_id()
    }
}

impl PgCitationMappingSidecar for RememberTestCitationMapping {
    fn insert_citation_mapping_sidecar<'t>(
        &'t self,
        tx: &'t mut sqlx::PgConnection,
        citation_mapping_id: uuid::Uuid,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.remember_test_citation_mapping_v1
                    (citation_mapping_id, section, byte_start, byte_end)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(citation_mapping_id)
            .bind(&self.section)
            .bind(self.byte_start)
            .bind(self.byte_end)
            .execute(tx)
            .await
            .map_err(|err| proxima_core::StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

#[tokio::test]
async fn remember_then_search_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());
    let author = author_ctx();

    let remembered = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "core_remember",
        json!({
            "title": "Atlas edges",
            "body": "Edges must be loaded from the visible node set.",
            "tags": ["atlas"],
            "idempotency_key": "tools-smoke-atlas-edge-loading"
        }),
    )
    .await?;
    assert!(
        remembered["handle"]
            .as_str()
            .expect("handle")
            .starts_with('F'),
        "remember mints a Fact handle, got: {remembered}"
    );

    let derived = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "core_derive",
        json!({
            "kind": "Abstraction",
            "title": "Atlas edge summary",
            "body": "Visible node set edges should surface beside search results.",
            "tags": ["atlas-derived"],
            "source_handles": [remembered["handle"].clone()],
            "model_id": "codex-test",
            "idempotency_key": "tools-smoke-atlas-edge-derived"
        }),
    )
    .await?;

    let searched = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "core_search_memories",
        json!({
            "query": "atlas edges",
            "mode": "lexical",
            "limit": 5,
            "kind": "Fact",
            "tags": ["atlas"],
            "tag_match": "all",
            "since": "1970-01-01T00:00:00Z",
            "order": "recency"
        }),
    )
    .await?;
    assert_eq!(
        searched["memories"][0]["memory"], remembered["handle"],
        "search should reuse the session handle"
    );
    assert_eq!(searched["memories"][0]["tags"], json!(["atlas"]));
    let created_at = searched["memories"][0]["created_at"]
        .as_str()
        .expect("created_at");
    time::OffsetDateTime::parse(created_at, &Rfc3339)?;
    assert_eq!(
        searched["neighbor_edges"][0]["target"], remembered["handle"],
        "search should include neighbor edges touching matched memories"
    );
    assert_eq!(searched["neighbor_edges"][0]["source"], derived["handle"]);

    assert_search_since_rejects_invalid_timestamp(&pg, &owner, &handles, &frozen, author).await;

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn remember_enqueues_one_embedding_job_and_replay_does_not_duplicate()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());
    let author = author_ctx();
    let args = json!({
        "title": "Embedding job",
        "body": "This Fact needs async embedding.",
        "tags": ["embedding"],
        "idempotency_key": "remember-embedding-job-replay"
    });

    let first = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "core_remember",
        args.clone(),
    )
    .await?;
    let replay = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author,
        "core_remember",
        args,
    )
    .await?;

    assert_eq!(first["idempotent_replay"], json!(false));
    assert_eq!(replay["idempotent_replay"], json!(true));
    assert_eq!(replay["handle"], first["handle"]);
    let memory_id = handles.resolve_memory(first["handle"].as_str().expect("handle"))?;
    assert_eq!(
        embedding_job_count(pg.pool_for_tests(), memory_id, "test-embed").await?,
        1
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // linear PG flow: ingest + supersede + heads/all assertions read best together
async fn remember_reused_idempotency_key_changed_body_creates_new_stateful_fact()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());
    let author = author_ctx();
    let base_args = json!({
        "title": "Stateful remember",
        "body": "First body.",
        "tags": ["remember", "stateful"],
        "idempotency_key": "remember-stateful-changed-body"
    });
    let changed_args = json!({
        "title": "Stateful remember",
        "body": "Second body.",
        "tags": ["remember", "stateful"],
        "idempotency_key": "remember-stateful-changed-body"
    });

    let first = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "core_remember",
        base_args,
    )
    .await?;
    let second = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author,
        "core_remember",
        changed_args,
    )
    .await?;

    assert_eq!(first["idempotent_replay"], json!(false));
    assert_eq!(second["idempotent_replay"], json!(false));
    assert_ne!(second["handle"], first["handle"]);

    let first_memory_id =
        handles.resolve_memory(first["handle"].as_str().expect("first handle"))?;
    let second_memory_id =
        handles.resolve_memory(second["handle"].as_str().expect("second handle"))?;
    assert_ne!(second_memory_id, first_memory_id);

    let first_note_id = agent_note_id(pg.pool_for_tests(), first_memory_id).await?;
    let second_note_id = agent_note_id(pg.pool_for_tests(), second_memory_id).await?;
    assert_eq!(second_note_id, first_note_id);
    assert_eq!(
        agent_note_fact_count(pg.pool_for_tests(), first_note_id).await?,
        2
    );
    assert_eq!(
        agent_note_current_memory_id(pg.pool_for_tests(), first_note_id).await?,
        second_memory_id.into_inner()
    );
    assert_eq!(
        supersedes_edge_count_between(pg.pool_for_tests(), first_memory_id, second_memory_id)
            .await?,
        0
    );

    let default_search = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author_ctx(),
        "core_search_memories",
        json!({
            "query": "Stateful remember",
            "mode": "lexical",
            "limit": 5,
            "include_neighbor_edges": false
        }),
    )
    .await?;
    let default_memories = default_search["memories"]
        .as_array()
        .expect("default memories");
    assert_eq!(default_memories.len(), 1, "{default_search:#}");
    assert_eq!(default_memories[0]["memory"], second["handle"]);
    assert!(
        default_memories[0].get("body").is_none(),
        "body omitted by default: {default_search:#}"
    );

    let hydrated_search = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author_ctx(),
        "core_search_memories",
        json!({
            "query": "Stateful remember",
            "mode": "lexical",
            "limit": 5,
            "include_neighbor_edges": false,
            "include_body": true
        }),
    )
    .await?;
    assert_eq!(
        hydrated_search["memories"][0]["body"],
        json!("Second body.")
    );

    let full_history_search = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author_ctx(),
        "core_search_memories",
        json!({
            "query": "Stateful remember",
            "mode": "lexical",
            "limit": 5,
            "supersession": "all",
            "include_neighbor_edges": false
        }),
    )
    .await?;
    let history_handles: Vec<_> = full_history_search["memories"]
        .as_array()
        .expect("history memories")
        .iter()
        .map(|row| row["memory"].clone())
        .collect();
    assert_eq!(history_handles.len(), 2, "{full_history_search:#}");
    assert!(history_handles.contains(&first["handle"]));
    assert!(history_handles.contains(&second["handle"]));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn search_memories_heads_filter_runs_before_limit() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());

    let independent = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Prefilter independent",
            "body": "prefilter needle independent head",
            "tags": ["prefilter"],
            "idempotency_key": "search-prefilter-independent"
        }),
    )
    .await?;
    let mut chain_head = serde_json::Value::Null;
    for idx in 0..10 {
        chain_head = call_tool(
            &pg,
            &owner,
            &handles,
            &frozen,
            author_ctx(),
            "core_remember",
            json!({
                "title": "Prefilter chain",
                "body": format!("prefilter needle chain version {idx}"),
                "tags": ["prefilter"],
                "idempotency_key": "search-prefilter-chain"
            }),
        )
        .await?;
    }

    let search = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author_ctx(),
        "core_search_memories",
        json!({
            "query": "prefilter needle",
            "mode": "lexical",
            "limit": 2,
            "include_neighbor_edges": false
        }),
    )
    .await?;
    let handles: Vec<_> = search["memories"]
        .as_array()
        .expect("memories")
        .iter()
        .map(|row| row["memory"].clone())
        .collect();
    assert_eq!(handles.len(), 2, "{search:#}");
    assert!(handles.contains(&independent["handle"]), "{search:#}");
    assert!(handles.contains(&chain_head["handle"]), "{search:#}");

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn remember_reused_idempotency_key_identical_content_is_idempotent_replay()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());
    let author = author_ctx();
    let args = json!({
        "title": "Identical remember",
        "body": "Same body.",
        "tags": ["remember", "stateful"],
        "idempotency_key": "remember-stateful-identical-content"
    });

    let first = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "core_remember",
        args.clone(),
    )
    .await?;
    let replay = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author,
        "core_remember",
        args,
    )
    .await?;

    assert_eq!(first["idempotent_replay"], json!(false));
    assert_eq!(replay["idempotent_replay"], json!(true));
    assert_eq!(replay["handle"], first["handle"]);

    let memory_id = handles.resolve_memory(first["handle"].as_str().expect("handle"))?;
    let note_id = agent_note_id(pg.pool_for_tests(), memory_id).await?;
    assert_eq!(
        agent_note_fact_count(pg.pool_for_tests(), note_id).await?,
        1
    );
    assert_eq!(
        agent_note_current_memory_id(pg.pool_for_tests(), note_id).await?,
        memory_id.into_inner()
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "PG integration fixture validates cited and uncited remember rows in one transaction shape"
)]
async fn remember_cited_and_uncited_persist_citation_rows() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg_with_remember_sidecars().await;
    create_remember_citation_sidecars(pg.pool_for_tests()).await?;

    let frozen = registry_with_remember_test_citation();
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());
    let author = author_ctx();

    let cited = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "core_remember",
        json!({
            "title": "Cited remembered note",
            "body": "This note cites a typed test artifact.",
            "tags": ["citation"],
            "idempotency_key": "remember-cited-test-note",
            "citation": {
                "object_schema_id": RememberTestCitedObject::SCHEMA_ID,
                "object_schema_version": RememberTestCitedObject::SCHEMA_VERSION,
                "object_payload": {
                    "artifact_id": "sharepoint-test-item",
                    "locator": "https://example.invalid/sites/test/doc"
                },
                "mapping_schema_id": RememberTestCitationMapping::SCHEMA_ID,
                "mapping_schema_version": RememberTestCitationMapping::SCHEMA_VERSION,
                "mapping_payload": {
                    "section": "body",
                    "byte_start": 0,
                    "byte_end": 12
                }
            }
        }),
    )
    .await?;
    let uncited = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author,
        "core_remember",
        json!({
            "title": "Uncited remembered note",
            "body": "This note has no citation.",
            "tags": ["citation"],
            "idempotency_key": "remember-uncited-test-note"
        }),
    )
    .await?;

    let cited_memory_id = handles
        .resolve_memory(cited["handle"].as_str().expect("cited handle"))?
        .into_inner();
    let uncited_memory_id = handles
        .resolve_memory(uncited["handle"].as_str().expect("uncited handle"))?
        .into_inner();

    let cited_row: (Option<uuid::Uuid>,) = sqlx::query_as(
        "SELECT citation_mapping_id
         FROM proxima_core.memories
         WHERE memory_id = $1",
    )
    .bind(cited_memory_id)
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert!(
        cited_row.0.is_some(),
        "cited remember must attach citation_mapping_id"
    );

    let uncited_row: (Option<uuid::Uuid>,) = sqlx::query_as(
        "SELECT citation_mapping_id
         FROM proxima_core.memories
         WHERE memory_id = $1",
    )
    .bind(uncited_memory_id)
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert!(
        uncited_row.0.is_none(),
        "uncited remember must remain a plain Fact"
    );

    let cited_sidecar_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.agent_note_v1 WHERE memory_id = $1")
            .bind(cited_memory_id)
            .fetch_one(pg.pool_for_tests())
            .await?;
    let uncited_sidecar_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.agent_note_v1 WHERE memory_id = $1")
            .bind(uncited_memory_id)
            .fetch_one(pg.pool_for_tests())
            .await?;
    assert_eq!(cited_sidecar_count, 1);
    assert_eq!(uncited_sidecar_count, 1);
    assert_eq!(
        count_rows(pg.pool_for_tests(), "proxima_core.cited_objects").await?,
        1
    );
    assert_eq!(
        count_rows(pg.pool_for_tests(), "public.remember_test_cited_object_v1").await?,
        1
    );
    assert_eq!(
        count_rows(pg.pool_for_tests(), "proxima_core.citation_mappings").await?,
        1
    );
    assert_eq!(
        count_rows(
            pg.pool_for_tests(),
            "public.remember_test_citation_mapping_v1"
        )
        .await?,
        1
    );
    assert_eq!(
        embedding_job_count(
            pg.pool_for_tests(),
            MemoryId::new(cited_memory_id),
            "test-embed"
        )
        .await?,
        1
    );
    assert_eq!(
        embedding_job_count(
            pg.pool_for_tests(),
            MemoryId::new(uncited_memory_id),
            "test-embed"
        )
        .await?,
        1
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn link_rejects_direct_fact_to_fact_interpretation() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());
    let author = author_ctx();

    let first = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "core_remember",
        json!({
            "title": "First fact",
            "body": "A remembered observation.",
            "idempotency_key": "link-fact-a"
        }),
    )
    .await?;
    let second = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "core_remember",
        json!({
            "title": "Second fact",
            "body": "Another remembered observation.",
            "idempotency_key": "link-fact-b"
        }),
    )
    .await?;

    let link = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author,
        "core_link",
        json!({
            "source": first["handle"],
            "target": second["handle"],
            "reason": "semantic direct Fact-to-Fact interpretation"
        }),
    )
    .await;

    // A Fact cannot be a link source: rejected up front at source-class
    // validation (strict layering) with a clear caller-facing InvalidInput,
    // before reaching the central relation-mask check.
    match link {
        Err(McpToolError::InvalidInput(msg)) => {
            assert!(
                msg.contains("Fact cannot be a link source"),
                "unexpected message: {msg}"
            );
        }
        other => panic!("expected Fact-source rejection (InvalidInput), got {other:?}"),
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn search_memories_hybrid_returns_embedding_only_match()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen_inner = registry.freeze_or_panic_for_tests();
    let frozen = Arc::new(frozen_inner.clone());
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());
    let author = author_ctx();

    let engine = engine_for_registry(&frozen, &pg);
    let remembered = call_tool_with_engine(
        &pg,
        &owner,
        &handles,
        &frozen,
        author.clone(),
        Some(engine.clone()),
        "core_remember",
        json!({
            "title": "Operational note",
            "body": "This body deliberately omits the query token.",
            "tags": ["hybrid"],
            "idempotency_key": "tools-smoke-hybrid-embedding-only"
        }),
    )
    .await?;
    let remembered_id =
        handles.resolve_memory(remembered["handle"].as_str().expect("remember handle"))?;
    engine.ensure_fact_embedding(&owner, remembered_id).await?;
    let lexical = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "core_search_memories",
        json!({"query": "galaxy", "mode": "lexical", "limit": 5}),
    )
    .await?;
    assert!(lexical["memories"].as_array().expect("memories").is_empty());

    let hybrid = call_tool_with_engine(
        &pg,
        &owner,
        &handles,
        &frozen,
        author,
        Some(engine),
        "core_search_memories",
        json!({"query": "galaxy", "mode": "hybrid", "limit": 5}),
    )
    .await?;
    assert_eq!(hybrid["memories"][0]["memory"], remembered["handle"]);
    assert_eq!(hybrid["memories"][0]["tags"], json!(["hybrid"]));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn prefixed_search_and_open_keep_company_shared_visibility()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
    let caller_self_perspective = MemoryId::new(uuid::Uuid::now_v7());
    let authored_handle = call_tool_prefixed(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Company shared author",
            "body": "Company shared alpha needle.",
            "tags": ["company-shared"],
            "idempotency_key": "company-shared-alpha"
        }),
    )
    .await?["handle"]
        .as_str()
        .expect("remember handle")
        .to_string();
    let nil_handle = call_tool_prefixed(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Nil author",
            "body": "Company shared beta needle.",
            "tags": ["company-shared"],
            "idempotency_key": "company-shared-beta"
        }),
    )
    .await?["handle"]
        .as_str()
        .expect("remember handle")
        .to_string();

    let search = call_tool_prefixed(
        &pg,
        &owner,
        &frozen,
        author_ctx().with_self_perspective(caller_self_perspective),
        "core_search_memories",
        json!({"query": "alpha needle", "mode": "lexical", "limit": 5}),
    )
    .await?;
    assert_eq!(search["memories"][0]["memory"], authored_handle);
    assert!(
        search["memories"][0]
            .get("authoring_personality_instance_id")
            .is_none()
    );

    let opened = read_memory_prefixed(
        &pg,
        &owner,
        &frozen,
        author_ctx().with_self_perspective(caller_self_perspective),
        &authored_handle,
        false,
    )
    .await?;
    assert!(opened.get("authoring_personality_instance_id").is_none());

    let nil_opened = read_memory_prefixed(
        &pg,
        &owner,
        &frozen,
        author_ctx().with_self_perspective(caller_self_perspective),
        &nil_handle,
        false,
    )
    .await?;
    assert!(
        nil_opened
            .get("authoring_personality_instance_id")
            .is_none()
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // end-to-end idempotency dimensions need distinct owner/kind fixtures
async fn derive_scopes_idempotency_by_owner_and_kind() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let frozen_b = frozen.clone();
    let owner_a = nil_owner();
    let owner_b: Owner = OwnerRef::Personal(UserId::new(uuid::Uuid::from_u128(1)));

    let handles_a = Arc::new(HandleTable::new());
    let handles_b = Arc::new(HandleTable::new());
    let fact_a = call_tool(
        &pg,
        &owner_a,
        &handles_a,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Shared idempotency source A",
            "body": "Owner A source fact.",
            "idempotency_key": "shared-key-source-a",
        }),
    )
    .await?;
    let fact_b = call_tool(
        &pg,
        &owner_b,
        &handles_b,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Shared idempotency source B",
            "body": "Owner B source fact.",
            "idempotency_key": "shared-key-source-b",
        }),
    )
    .await?;
    let shared_args = |source_handle: serde_json::Value| {
        json!({
            "kind": "Abstraction",
            "title": "Shared idempotency",
            "body": "Same body, same key, different owner.",
            "source_handles": [source_handle],
            "model_id": "codex-test",
            "idempotency_key": "shared-key-collision",
        })
    };

    let a = call_tool(
        &pg,
        &owner_a,
        &handles_a,
        &frozen,
        author_ctx(),
        "core_derive",
        shared_args(fact_a["handle"].clone()),
    )
    .await?;
    let b = call_tool(
        &pg,
        &owner_b,
        &handles_b,
        &frozen,
        author_ctx(),
        "core_derive",
        shared_args(fact_b["handle"].clone()),
    )
    .await?;

    let distinct_owner_memories: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT memory_id) FROM proxima_core.agent_derivation_v1
         WHERE idempotency_key = 'shared-key-collision'",
    )
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(
        distinct_owner_memories, 2,
        "owner-a and owner-b must not collide"
    );
    assert_eq!(a["idempotent_replay"], json!(false));
    assert_eq!(b["idempotent_replay"], json!(false));

    let kind_fact = call_tool(
        &pg,
        &owner_a,
        &handles_a,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Kind dimension source",
            "body": "Source fact for kind dimension test.",
            "idempotency_key": "kind-key-source",
        }),
    )
    .await?;
    let abstraction = call_tool(
        &pg,
        &owner_a,
        &handles_a,
        &frozen,
        author_ctx(),
        "core_derive",
        json!({
            "kind": "Abstraction",
            "title": "Same key, A vs P",
            "body": "kind dimension test.",
            "source_handles": [kind_fact["handle"].clone()],
            "model_id": "codex-test",
            "idempotency_key": "kind-key-collision",
        }),
    )
    .await?;
    let perspective = call_tool(
        &pg,
        &owner_a,
        &handles_a,
        &frozen_b,
        author_ctx(),
        "core_derive",
        json!({
            "kind": "Perspective",
            "title": "Same key, A vs P",
            "body": "kind dimension test.",
            "source_handles": [abstraction["handle"].clone()],
            "model_id": "codex-test",
            "idempotency_key": "kind-key-collision",
        }),
    )
    .await?;
    let distinct_kind_memories: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT memory_id) FROM proxima_core.agent_derivation_v1
         WHERE idempotency_key = 'kind-key-collision'",
    )
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(
        distinct_kind_memories, 2,
        "kind dimension must split memory_id"
    );
    assert_eq!(abstraction["idempotent_replay"], json!(false));
    assert_eq!(perspective["idempotent_replay"], json!(false));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn derive_rejects_upward_provenance() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
    let handles = Arc::new(HandleTable::new());
    let author = author_ctx();

    let fact = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "core_remember",
        json!({
            "title": "Layering fact",
            "body": "Fact source for legal F→A then A→P setup.",
            "idempotency_key": "derive-layer-test-fact"
        }),
    )
    .await?;
    let abstraction = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "core_derive",
        json!({
            "kind": "Abstraction",
            "title": "Layering abstraction",
            "body": "Abstraction source for legal A→P setup.",
            "source_handles": [fact["handle"].clone()],
            "model_id": "codex-test",
            "idempotency_key": "derive-layer-test-abstraction"
        }),
    )
    .await?;
    let perspective = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author.clone(),
        "core_derive",
        json!({
            "kind": "Perspective",
            "title": "Top-layer perspective",
            "body": "A perspective with abstraction provenance, used as a layering pivot.",
            "source_handles": [abstraction["handle"].clone()],
            "model_id": "codex-test",
            "idempotency_key": "derive-layer-test-perspective"
        }),
    )
    .await?;
    let perspective_handle = perspective["handle"].as_str().expect("handle").to_string();

    let upward = call_tool(
        &pg,
        &owner,
        &handles,
        &frozen,
        author,
        "core_derive",
        json!({
            "kind": "Abstraction",
            "title": "Should fail",
            "body": "Trying to derive an Abstraction from a Perspective is upward.",
            "model_id": "codex-test",
            "source_handles": [perspective_handle],
            "idempotency_key": "derive-layer-test-upward"
        }),
    )
    .await;

    match upward {
        Err(McpToolError::LayeringViolation(_)) => {}
        other => panic!("expected LayeringViolation, got {other:?}"),
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // linear PG flow: author + pre-publish deny + publish + owner-column assert + re-publish deny + post-publish read read best together
async fn publish_to_world_transfers_owner_denies_rewrite_and_allows_ordinary_reads()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let handles = Arc::new(HandleTable::new());
    let author = author_ctx();
    let engine = engine_for_registry(&frozen, &pg);

    let group = GroupId::new(Uuid::now_v7());
    let group_owner = OwnerRef::Group(group);
    let admin_subject = UserId::new(Uuid::now_v7());
    let admin_authz = AuthzContext::for_subject_with_role(
        admin_subject,
        [(group_owner, Role::admin())],
        AuthPath::HostBearer,
    );

    // author a Fact under the Group.
    let remembered = call_tool_as(
        &pg,
        &group_owner,
        admin_authz.clone(),
        &handles,
        &frozen,
        author.clone(),
        Some(engine.clone()),
        "core_remember",
        json!({
            "title": "Group catalog fact",
            "body": "A Fact authored under a Group, destined for a deliberate publish.",
            "tags": [],
            "space": format!("group:{}", group.into_inner()),
            "idempotency_key": "publish-smoke-fact"
        }),
    )
    .await?;
    let handle = remembered["handle"].as_str().expect("handle").to_string();
    let memory_id = handles
        .resolve_memory(&handle)
        .expect("resolves the just-created Fact handle");

    // an ordinary outsider with zero group role cannot read it yet.
    let outsider_subject = UserId::new(Uuid::now_v7());
    let outsider_authz = AuthzContext::for_subject(outsider_subject, AuthPath::HostBearer);
    let pre_publish_read = get_memory(
        outsider_ctx(
            &pg,
            &handles,
            &frozen,
            author.clone(),
            &engine,
            outsider_authz.clone(),
        ),
        GetMemoryArgs {
            memory: handle.clone(),
            expand_neighbors: false,
            space: None,
        },
    )
    .await;
    match pre_publish_read {
        Err(McpToolError::Protocol(err)) => assert_eq!(err.code, ErrorCode::Forbidden),
        other => panic!("expected forbidden before publish, got {other:?}"),
    }

    // publish requires admin authority on the current (Group) owner.
    call_tool_as(
        &pg,
        &group_owner,
        admin_authz.clone(),
        &handles,
        &frozen,
        author.clone(),
        Some(engine.clone()),
        "core_membership",
        json!({"action": "publish_to_world", "entity": handle}),
    )
    .await?;

    // owner columns encode World.
    let (owner_kind, owner_id): (OwnerRefKind, Option<Uuid>) = sqlx::query_as(
        "SELECT owner_kind, owner_id FROM proxima_core.memories WHERE memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(owner_kind, OwnerRefKind::World);
    assert_eq!(owner_id, None);

    // writes are denied: re-publishing an already-World entity is Forbidden,
    // proving World is never granted write authority by this verb.
    let republish = call_tool_as(
        &pg,
        &group_owner,
        admin_authz.clone(),
        &handles,
        &frozen,
        author.clone(),
        Some(engine.clone()),
        "core_membership",
        json!({"action": "publish_to_world", "entity": handle}),
    )
    .await;
    match republish {
        Err(McpToolError::Protocol(err)) => assert_eq!(err.code, ErrorCode::Forbidden),
        other => panic!("expected forbidden re-publish, got {other:?}"),
    }

    // reads now work for the same ordinary outsider.
    let post_publish_read = get_memory(
        outsider_ctx(&pg, &handles, &frozen, author, &engine, outsider_authz),
        GetMemoryArgs {
            memory: handle,
            expand_neighbors: false,
            space: None,
        },
    )
    .await?;
    assert!(
        post_publish_read
            .text
            .as_deref()
            .is_some_and(|text| text.contains("Group catalog fact")),
        "post-publish read returns the same Fact content: {post_publish_read:?}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn core_membership_add_member_denies_missing_resolver_role()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let handles = Arc::new(HandleTable::new());
    let owner = nil_owner();
    let group = GroupId::new(Uuid::now_v7());
    let member = UserId::new(Uuid::now_v7());

    // `single_owner`/`for_subject` on a Personal owner has World+Personal
    // roles only — no resolver role on `group`.
    let err = call_tool_with_engine(
        &pg,
        &owner,
        &handles,
        &frozen,
        author_ctx(),
        Some(engine_for_registry(&frozen, &pg)),
        "core_membership",
        json!({
            "action": "add_member",
            "group": format!("group:{}", group.into_inner()),
            "member": member.into_inner().to_string(),
            "relation": "viewer"
        }),
    )
    .await
    .expect_err("add_member without a resolver role on the group must be denied");
    match err {
        McpToolError::Protocol(err) => assert_eq!(err.code, ErrorCode::Forbidden),
        other => panic!("expected forbidden protocol error, got {other:?}"),
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn core_membership_remove_member_denies_missing_resolver_role()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let handles = Arc::new(HandleTable::new());
    let owner = nil_owner();
    let group = GroupId::new(Uuid::now_v7());
    let member = UserId::new(Uuid::now_v7());

    let err = call_tool_with_engine(
        &pg,
        &owner,
        &handles,
        &frozen,
        author_ctx(),
        Some(engine_for_registry(&frozen, &pg)),
        "core_membership",
        json!({
            "action": "remove_member",
            "group": format!("group:{}", group.into_inner()),
            "member": member.into_inner().to_string(),
        }),
    )
    .await
    .expect_err("remove_member without a resolver role on the group must be denied");
    match err {
        McpToolError::Protocol(err) => assert_eq!(err.code, ErrorCode::Forbidden),
        other => panic!("expected forbidden protocol error, got {other:?}"),
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn core_membership_list_members_denies_missing_resolver_role()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let handles = Arc::new(HandleTable::new());
    let owner = nil_owner();
    let group = GroupId::new(Uuid::now_v7());

    let err = call_tool_with_engine(
        &pg,
        &owner,
        &handles,
        &frozen,
        author_ctx(),
        Some(engine_for_registry(&frozen, &pg)),
        "core_membership",
        json!({
            "action": "list_members",
            "group": format!("group:{}", group.into_inner()),
        }),
    )
    .await
    .expect_err("list_members without a resolver role on the group must be denied");
    match err {
        McpToolError::Protocol(err) => assert_eq!(err.code, ErrorCode::Forbidden),
        other => panic!("expected forbidden protocol error, got {other:?}"),
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

fn outsider_ctx(
    pg: &proxima_storage_pg::PgStorage,
    handles: &Arc<HandleTable>,
    registry: &Arc<proxima_core::FlavorRegistryFrozen>,
    author: McpAuthorContext,
    engine: &Arc<Engine>,
    authz: AuthzContext,
) -> McpToolCtx {
    let subject = authz.subject().expect("subject-bearing authz");
    McpToolCtx {
        owner: OwnerRef::Personal(subject),
        authz,
        handles: Some(handles.clone()),
        mode: OutputMode::Handles,
        registry: registry.clone(),
        author,
        caller_self_perspective: None,
        master_token_id: None,
        extensions: McpToolExtensions::with(pg.pool_for_tests().clone()),
        engine: Some(engine.clone()),
    }
}

async fn call_tool(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    handles: &Arc<HandleTable>,
    registry: &Arc<proxima_core::FlavorRegistryFrozen>,
    author: McpAuthorContext,
    name: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, proxima_core::McpToolError> {
    call_tool_with_engine(
        pg,
        owner,
        handles,
        registry,
        author,
        Some(engine_for_registry(registry, pg)),
        name,
        args,
    )
    .await
}

async fn assert_search_since_rejects_invalid_timestamp(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    handles: &Arc<HandleTable>,
    registry: &Arc<proxima_core::FlavorRegistryFrozen>,
    author: McpAuthorContext,
) {
    let invalid_since = call_tool(
        pg,
        owner,
        handles,
        registry,
        author,
        "core_search_memories",
        json!({
            "query": "atlas edges",
            "since": "not-a-timestamp"
        }),
    )
    .await;
    match invalid_since {
        Err(McpToolError::InvalidInput(message)) => assert!(message.contains("since")),
        other => panic!("expected InvalidInput for invalid since, got {other:?}"),
    }
}

async fn call_tool_prefixed(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    registry: &Arc<proxima_core::FlavorRegistryFrozen>,
    author: McpAuthorContext,
    name: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, proxima_core::McpToolError> {
    let descriptor = registry
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == name)
        .expect("registered tool");
    let caller_self_perspective = author.caller_self_perspective;
    (descriptor.call)(
        McpToolCtx {
            owner: *owner,
            authz: AuthzContext::single_owner(owner, AuthPath::HostBearer),
            handles: None,
            mode: OutputMode::PrefixedIds,
            registry: registry.clone(),
            author,
            caller_self_perspective,
            master_token_id: None,
            extensions: McpToolExtensions::with(pg.pool_for_tests().clone()),
            engine: Some(engine_for_registry(registry, pg)),
        },
        args,
    )
    .await
}

async fn read_memory_prefixed(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    registry: &Arc<proxima_core::FlavorRegistryFrozen>,
    author: McpAuthorContext,
    memory: &str,
    expand_neighbors: bool,
) -> Result<serde_json::Value, proxima_core::McpToolError> {
    let caller_self_perspective = author.caller_self_perspective;
    let output = get_memory(
        McpToolCtx {
            owner: *owner,
            authz: AuthzContext::single_owner(owner, AuthPath::HostBearer),
            handles: None,
            mode: OutputMode::PrefixedIds,
            registry: registry.clone(),
            author,
            caller_self_perspective,
            master_token_id: None,
            extensions: McpToolExtensions::with(pg.pool_for_tests().clone()),
            engine: Some(engine_for_registry(registry, pg)),
        },
        GetMemoryArgs {
            memory: memory.to_string(),
            expand_neighbors,
            space: None,
        },
    )
    .await?;
    serde_json::to_value(output)
        .map_err(|err| proxima_core::McpToolError::Other(format!("serialize memory read: {err}")))
}

#[allow(clippy::too_many_arguments)]
async fn call_tool_with_engine(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    handles: &Arc<HandleTable>,
    registry: &Arc<proxima_core::FlavorRegistryFrozen>,
    author: McpAuthorContext,
    engine: Option<Arc<Engine>>,
    name: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, proxima_core::McpToolError> {
    call_tool_as(
        pg,
        owner,
        AuthzContext::single_owner(owner, AuthPath::HostBearer),
        handles,
        registry,
        author,
        engine,
        name,
        args,
    )
    .await
}

/// Generalization of [`call_tool_with_engine`] that takes an explicit
/// `AuthzContext` instead of deriving one via `single_owner` — needed for
/// group-owned spaces (`single_owner` only mints Personal-owner contexts)
/// and for contexts deliberately built via `for_subject`/`for_subject_with_role`
/// to exercise the server-resolved authz path directly.
#[allow(clippy::too_many_arguments)]
async fn call_tool_as(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    authz: AuthzContext,
    handles: &Arc<HandleTable>,
    registry: &Arc<proxima_core::FlavorRegistryFrozen>,
    author: McpAuthorContext,
    engine: Option<Arc<Engine>>,
    name: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, proxima_core::McpToolError> {
    let descriptor = registry
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == name)
        .expect("registered tool");
    (descriptor.call)(
        McpToolCtx {
            owner: *owner,
            authz,
            handles: Some(handles.clone()),
            mode: OutputMode::Handles,
            registry: registry.clone(),
            author,
            caller_self_perspective: None,
            master_token_id: None,
            extensions: McpToolExtensions::with(pg.pool_for_tests().clone()),
            engine,
        },
        args,
    )
    .await
}

fn nil_owner() -> Owner {
    owner_fixture()
}

fn author_ctx() -> McpAuthorContext {
    McpAuthorContext {
        model_id: "codex-test".into(),
        client_name: "codex".into(),
        client_version: "1".into(),
        caller_self_perspective: None,
    }
}

trait AuthorCtxExt {
    fn with_self_perspective(self, memory_id: MemoryId) -> Self;
}

impl AuthorCtxExt for McpAuthorContext {
    fn with_self_perspective(mut self, memory_id: MemoryId) -> Self {
        self.caller_self_perspective = Some(memory_id);
        self
    }
}

fn engine_for_registry(
    registry: &Arc<FlavorRegistryFrozen>,
    pg: &proxima_storage_pg::PgStorage,
) -> Arc<Engine> {
    Arc::new(
        Engine::new((**registry).clone())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports())
            .with_embed(Arc::new(ConstantEmbedding::prefixed(
                "test-embed",
                &[1.0, 0.0, 0.0],
            ))),
    )
}

fn registry_with_remember_test_citation() -> Arc<FlavorRegistryFrozen> {
    let mut registry = FlavorRegistry::new();
    registry.add_cited_object_schema_or_panic_for_tests::<RememberTestCitedObject>();
    registry.add_citation_mapping_schema_or_panic_for_tests::<RememberTestCitationMapping>();
    Arc::new(registry.freeze_or_panic_for_tests())
}

async fn fresh_pg_with_remember_sidecars() -> (proxima_storage_pg::PgStorage, String) {
    let (pg, db_name) = fresh_pg().await;
    let registry = registry_with_remember_test_citation();
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    sidecars.add_cited_object::<RememberTestCitedObject>();
    sidecars.add_citation_mapping::<RememberTestCitationMapping>();
    let sidecars = sidecars
        .freeze_against(registry.schemas())
        .expect("remember test PG sidecars match schemas");
    (pg.with_sidecars(sidecars), db_name)
}

async fn create_remember_citation_sidecars(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE public.remember_test_cited_object_v1 (
            cited_object_id uuid PRIMARY KEY,
            artifact_id text NOT NULL,
            locator text NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE public.remember_test_citation_mapping_v1 (
            citation_mapping_id uuid PRIMARY KEY,
            section text NOT NULL,
            byte_start integer NOT NULL,
            byte_end integer NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn count_rows(pool: &sqlx::PgPool, table: &str) -> Result<i64, sqlx::Error> {
    let sql = format!("SELECT count(*) FROM {table}");
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(pool)
        .await
}

async fn agent_note_id(pool: &sqlx::PgPool, memory_id: MemoryId) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT note_id
           FROM proxima_core.agent_note_v1
          WHERE memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_one(pool)
    .await
}

async fn agent_note_fact_count(pool: &sqlx::PgPool, note_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.memories m
           JOIN proxima_core.agent_note_v1 n USING (memory_id)
          WHERE m.schema_id = $1
            AND m.schema_version = $2
            AND n.note_id = $3",
    )
    .bind(AgentNoteV1::SCHEMA_ID)
    .bind(i32::try_from(AgentNoteV1::SCHEMA_VERSION).expect("schema version fits i32"))
    .bind(note_id)
    .fetch_one(pool)
    .await
}

async fn agent_note_current_memory_id(
    pool: &sqlx::PgPool,
    note_id: Uuid,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT current_memory_id
           FROM proxima_core.fact_entities
          WHERE schema_id = $1
            AND schema_version = $2
            AND natural_key = ARRAY[$3]::text[]",
    )
    .bind(AgentNoteV1::SCHEMA_ID)
    .bind(i32::try_from(AgentNoteV1::SCHEMA_VERSION).expect("schema version fits i32"))
    .bind(note_id.to_string())
    .fetch_one(pool)
    .await
}

async fn supersedes_edge_count_between(
    pool: &sqlx::PgPool,
    first: MemoryId,
    second: MemoryId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.edges
          WHERE relation = 'core/supersedes'
            AND (
                (source_memory_id = $1 AND target_memory_id = $2)
                OR (source_memory_id = $2 AND target_memory_id = $1)
            )",
    )
    .bind(first.into_inner())
    .bind(second.into_inner())
    .fetch_one(pool)
    .await
}

async fn embedding_job_count(
    pool: &sqlx::PgPool,
    memory_id: MemoryId,
    model_id: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.embedding_jobs
          WHERE entity_kind = 'Fact'
            AND entity_id = $1
            AND model_id = $2
            AND status = 'pending'",
    )
    .bind(memory_id.into_inner())
    .bind(model_id)
    .fetch_one(pool)
    .await
}
