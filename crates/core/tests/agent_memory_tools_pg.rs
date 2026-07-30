use std::sync::Arc;

mod common;

use common::{ConstantEmbedding, drop_db, fresh_pg, owner_fixture};
use proxima_core::engine::Engine;
use proxima_core::mcp::core_tools::get_memories::{GetMemoriesArgs, get_memories};
use proxima_core::mcp::core_tools::get_memory::{GetMemoryArgs, get_memory};
use proxima_core::mcp::{
    McpAuthorContext, McpToolCtx, McpToolExtensions, PrefixedUuidClass, PrefixedUuidError,
    parse_prefixed_uuid,
};
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
    let author = author_ctx();

    let remembered = call_tool(
        &pg,
        &owner,
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
            .starts_with("F:"),
        "remember mints an F:<uuid> reference, got: {remembered}"
    );

    let derived = call_tool(
        &pg,
        &owner,
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

    assert_search_since_rejects_invalid_timestamp(&pg, &owner, &frozen, author).await;

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Every surface that reports a `space` must report a key that
/// `core_memory_spaces` advertises and a write accepts. The read path used to
/// emit the literal "entry", which is not a space: an agent following the
/// documented "use a returned `space` key" loop fed it straight back into
/// `core_remember` and got `unknown memory space: entry`. Asserting membership
/// in the advertised set — rather than a literal — keeps the two surfaces tied
/// together if the key vocabulary ever changes.
#[tokio::test]
async fn a_read_reports_a_space_key_that_a_write_accepts() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
    let author = author_ctx();

    let remembered = call_tool(
        &pg,
        &owner,
        &frozen,
        author.clone(),
        "core_remember",
        json!({ "title": "Space key round trip", "body": "Read it back." }),
    )
    .await?;
    let handle = remembered["handle"].as_str().expect("handle");

    let advertised = call_tool(
        &pg,
        &owner,
        &frozen,
        author.clone(),
        "core_memory_spaces",
        json!({}),
    )
    .await?;
    let keys: Vec<String> = advertised["spaces"]
        .as_array()
        .expect("spaces")
        .iter()
        .map(|space| space["key"].as_str().expect("key").to_string())
        .collect();

    let read = read_memory_prefixed(&pg, &owner, &frozen, author.clone(), handle, false).await?;
    let reported = read["space"].as_str().expect("space").to_string();
    assert!(
        keys.contains(&reported),
        "get_memory reported space {reported:?}, which core_memory_spaces does not advertise: {keys:?}",
    );

    // `get_memories` has no tool name -- it is reached as a resource -- so it
    // is called directly, the same way `read_memory_prefixed` calls
    // `get_memory`. It carried its own copy of the sentinel.
    let batch = get_memories(
        McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            registry: frozen.clone(),
            author: author.clone(),
            caller_self_perspective: author.caller_self_perspective,
            extensions: McpToolExtensions::with(pg.pool_for_tests().clone()),
            engine: Some(engine_for_registry(&frozen, &pg)),
        },
        GetMemoriesArgs {
            memories: vec![handle.to_string()],
        },
    )
    .await?;
    let batch_space = batch.memories[0].space.clone();
    assert!(
        keys.contains(&batch_space),
        "get_memories reported space {batch_space:?}, not advertised: {keys:?}",
    );

    // The loop the server instructions actually prescribe: read a memory,
    // reuse its reported space key on the next write.
    call_tool(
        &pg,
        &owner,
        &frozen,
        author,
        "core_remember",
        json!({ "title": "Reused key", "body": "b", "space": reported }),
    )
    .await
    .map_err(|err| format!("a write rejected the space key a read reported: {err}"))?;

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn search_memories_paginates_with_opaque_cursor() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
    let author = author_ctx();

    for idx in 0..7 {
        call_tool(
            &pg,
            &owner,
            &frozen,
            author.clone(),
            "core_remember",
            json!({
                "title": format!("Cursorgrain {idx}"),
                "body": "cursorgrain page fodder",
                "idempotency_key": format!("cursor-page-{idx}"),
            }),
        )
        .await?;
    }

    let base_args = json!({
        "query": "cursorgrain",
        "mode": "lexical",
        "limit": 3,
        "include_neighbor_edges": false,
    });

    let mut seen_handles = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut args = base_args.clone();
        if let Some(token) = &cursor {
            args["cursor"] = json!(token);
        }
        let page = call_tool(
            &pg,
            &owner,
            &frozen,
            author.clone(),
            "core_search_memories",
            args,
        )
        .await?;
        let memories = page["memories"].as_array().expect("memories array");
        for memory in memories {
            seen_handles.push(memory["memory"].as_str().expect("handle").to_string());
        }
        pages += 1;
        if page["has_more"] == json!(true) {
            cursor = Some(
                page["next_cursor"]
                    .as_str()
                    .expect("has_more implies a next_cursor")
                    .to_string(),
            );
            assert!(pages <= 3, "pagination must terminate");
        } else {
            assert_eq!(
                page["next_cursor"],
                serde_json::Value::Null,
                "the final page mints no cursor"
            );
            break;
        }
    }
    assert_eq!(
        pages, 3,
        "seven results at page size three make three pages"
    );
    let mut deduped = seen_handles.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(
        deduped.len(),
        7,
        "pages must be disjoint and exhaustive, got: {seen_handles:?}"
    );

    let last_cursor = cursor.unwrap_or_default();
    assert_cursor_misuse_rejected(&pg, &owner, &frozen, author, &last_cursor).await;

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// A cursor replayed against a different query shape fails closed, and
/// garbage tokens are malformed — never a silent first page.
async fn assert_cursor_misuse_rejected(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    registry: &Arc<proxima_core::FlavorRegistryFrozen>,
    author: McpAuthorContext,
    cursor: &str,
) {
    let stale = call_tool(
        pg,
        owner,
        registry,
        author.clone(),
        "core_search_memories",
        json!({
            "query": "something else entirely",
            "mode": "lexical",
            "limit": 3,
            "include_neighbor_edges": false,
            "cursor": cursor,
        }),
    )
    .await;
    match stale {
        Err(proxima_core::McpToolError::InvalidInput(message)) => {
            assert!(
                message.contains("does not match this query"),
                "unexpected message: {message}"
            );
        }
        other => panic!("expected invalid-input for foreign cursor, got {other:?}"),
    }

    let garbled = call_tool(
        pg,
        owner,
        registry,
        author,
        "core_search_memories",
        json!({
            "query": "cursorgrain",
            "mode": "lexical",
            "limit": 3,
            "cursor": "not-a-cursor",
        }),
    )
    .await;
    match garbled {
        Err(proxima_core::McpToolError::InvalidInput(message)) => {
            assert!(message.contains("malformed cursor"), "got: {message}");
        }
        other => panic!("expected malformed-cursor error, got {other:?}"),
    }
}

#[tokio::test]
async fn remember_enqueues_one_embedding_job_and_replay_does_not_duplicate()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
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
        &frozen,
        author.clone(),
        "core_remember",
        args.clone(),
    )
    .await?;
    let replay = call_tool(&pg, &owner, &frozen, author, "core_remember", args).await?;

    assert_eq!(first["idempotent_replay"], json!(false));
    assert_eq!(replay["idempotent_replay"], json!(true));
    assert_eq!(replay["handle"], first["handle"]);
    let memory_id = resolve_memory(first["handle"].as_str().expect("handle"))?;
    assert_eq!(
        embedding_job_count(pg.pool_for_tests(), memory_id, "test-embed").await?,
        1
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn concurrent_remembers_into_one_keyed_batch_never_collide()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
    let author = author_ctx();

    // A keyed batch id is deterministic, so concurrent core_remember calls
    // with the same key race to insert the SAME source_batches row. The
    // insert must no-op for the losers on every unique index — with the
    // primary key as the sole ON CONFLICT arbiter, losers collided on
    // `source_batches_unique_per_source` and the ingest failed spuriously
    // (observed live at 8-way ingest concurrency). Many rounds of paired
    // concurrent calls keep the speculative-insert window hot.
    for round in 0..24 {
        let calls = (0..4).map(|i| {
            let pg = pg.clone();
            let frozen = Arc::clone(&frozen);
            let author = author.clone();
            tokio::spawn(async move {
                call_tool(
                    &pg,
                    &owner,
                    &frozen,
                    author,
                    "core_remember",
                    json!({
                        "title": format!("racing exchange {round}/{i}"),
                        "body": format!("user: concurrent fact {round}/{i}"),
                        "idempotency_key": format!("race-batch-{round}/r{i}"),
                        "source_batch_key": format!("race-batch-{round}"),
                    }),
                )
                .await
            })
        });
        for handle in calls {
            let noted = handle.await.expect("task join")?;
            assert!(noted["handle"].as_str().is_some(), "remember must succeed");
        }
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn source_batch_key_groups_remembers_for_multi_fact_consolidation()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
    let author = author_ctx();

    // Two separate core_remember calls join ONE source batch via the key —
    // the enabler for multi-Fact F→A consolidation (per-call batches make
    // core_derive reject multi-Fact source sets).
    let mut handles = Vec::new();
    for (idx, body) in [
        "user: I adopted a kitten named Luna.",
        "user: Luna's vet visit is on Friday.",
    ]
    .iter()
    .enumerate()
    {
        let noted = call_tool(
            &pg,
            &owner,
            &frozen,
            author.clone(),
            "core_remember",
            json!({
                "title": format!("Session exchange {idx}"),
                "body": body,
                "idempotency_key": format!("batched-session/r{idx}"),
                "source_batch_key": "batched-session",
            }),
        )
        .await?;
        handles.push(noted["handle"].as_str().expect("handle").to_string());
    }

    let derived = call_tool(
        &pg,
        &owner,
        &frozen,
        author.clone(),
        "core_derive",
        json!({
            "kind": "Abstraction",
            "title": "Session summary",
            "body": "User adopted a kitten named Luna; vet visit Friday.",
            "source_handles": handles,
            "model_id": "test-consolidator",
            "idempotency_key": "batched-session/abs",
        }),
    )
    .await?;
    assert!(
        derived["handle"]
            .as_str()
            .expect("handle")
            .starts_with("A:"),
        "F→A consolidation over batched Facts must succeed: {derived}"
    );
    assert_eq!(
        derived["provenance_edge_handles"]
            .as_array()
            .expect("edges")
            .len(),
        2,
        "provenance must record both consolidated Facts"
    );

    // Deriving declared the observation complete: the batch is closed, so
    // later writes with the same key are rejected...
    let late = call_tool(
        &pg,
        &owner,
        &frozen,
        author.clone(),
        "core_remember",
        json!({
            "title": "Late arrival",
            "body": "user: one more thing.",
            "idempotency_key": "batched-session/r9",
            "source_batch_key": "batched-session",
        }),
    )
    .await;
    let err = format!("{late:?}");
    assert!(late.is_err(), "write into closed batch must fail: {err}");
    assert!(
        err.contains("closed source batch"),
        "error must name the closed batch: {err}"
    );

    // ...while a fresh key opens a fresh batch.
    let fresh = call_tool(
        &pg,
        &owner,
        &frozen,
        author,
        "core_remember",
        json!({
            "title": "Next session",
            "body": "user: a new conversation begins.",
            "idempotency_key": "batched-session-2/r0",
            "source_batch_key": "batched-session-2",
        }),
    )
    .await?;
    assert!(fresh["handle"].as_str().expect("handle").starts_with("F:"));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn remember_and_record_utterance_backdate_receipt_observed_at()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
    let author = author_ctx();

    // Historical import: the caller-supplied observation time lands in the
    // receipt provenance (observed_at AND occurred_at) instead of now().
    let backdate = "2023-03-22T17:47:00Z";
    let noted = call_tool(
        &pg,
        &owner,
        &frozen,
        author.clone(),
        "core_remember",
        json!({
            "title": "Historical note",
            "body": "Imported from a 2023 conversation.",
            "observed_at": backdate
        }),
    )
    .await?;
    let memory_id = resolve_memory(noted["handle"].as_str().expect("handle"))?;
    let (observed_at, occurred_at) = receipt_times(pg.pool_for_tests(), memory_id).await?;
    let expected =
        time::OffsetDateTime::parse(backdate, &time::format_description::well_known::Rfc3339)?;
    assert_eq!(observed_at, expected);
    assert_eq!(occurred_at, expected);

    let uttered = call_tool(
        &pg,
        &owner,
        &frozen,
        author.clone(),
        "core_record_utterance",
        json!({
            "speaker": "user",
            "conversation_id": "imported-2023",
            "text": "An utterance from the archive.",
            "observed_at": backdate
        }),
    )
    .await?;
    let utterance_id = resolve_memory(uttered["handle"].as_str().expect("handle"))?;
    let (observed_at, _) = receipt_times(pg.pool_for_tests(), utterance_id).await?;
    assert_eq!(observed_at, expected);

    // Omitted observed_at keeps the present-time default.
    let fresh = call_tool(
        &pg,
        &owner,
        &frozen,
        author.clone(),
        "core_remember",
        json!({
            "title": "Fresh note",
            "body": "Written just now."
        }),
    )
    .await?;
    let fresh_id = resolve_memory(fresh["handle"].as_str().expect("handle"))?;
    let (observed_at, _) = receipt_times(pg.pool_for_tests(), fresh_id).await?;
    assert!(
        (time::OffsetDateTime::now_utc() - observed_at)
            .whole_seconds()
            .abs()
            < 60,
        "omitted observed_at must default to now, got {observed_at}"
    );

    // Malformed and future timestamps are caller errors.
    for (bad, expect_in_message) in [
        ("22.03.2023", "RFC3339"),
        ("2199-01-01T00:00:00Z", "future"),
    ] {
        let rejected = call_tool(
            &pg,
            &owner,
            &frozen,
            author.clone(),
            "core_remember",
            json!({
                "title": "Bad backdate",
                "body": "Should be rejected.",
                "observed_at": bad
            }),
        )
        .await;
        match rejected {
            Err(McpToolError::InvalidInput(message)) => assert!(
                message.contains(expect_in_message),
                "error for {bad:?} must mention {expect_in_message}, got {message:?}"
            ),
            other => panic!("expected InvalidInput for observed_at {bad:?}, got {other:?}"),
        }
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

async fn receipt_times(
    pool: &sqlx::PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<(time::OffsetDateTime, time::OffsetDateTime), sqlx::Error> {
    sqlx::query_as(
        "SELECT fr.observed_at, fr.occurred_at
           FROM proxima_core.memories m
           JOIN proxima_core.fact_receipts fr ON fr.receipt_id = m.receipt_id
          WHERE m.memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_one(pool)
    .await
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // linear PG flow: ingest + supersede + heads/all assertions read best together
async fn remember_reused_idempotency_key_changed_body_creates_new_stateful_fact()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
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
        &frozen,
        author.clone(),
        "core_remember",
        base_args,
    )
    .await?;
    let second = call_tool(&pg, &owner, &frozen, author, "core_remember", changed_args).await?;

    assert_eq!(first["idempotent_replay"], json!(false));
    assert_eq!(second["idempotent_replay"], json!(false));
    assert_ne!(second["handle"], first["handle"]);

    let first_memory_id = resolve_memory(first["handle"].as_str().expect("first handle"))?;
    let second_memory_id = resolve_memory(second["handle"].as_str().expect("second handle"))?;
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

    let independent = call_tool(
        &pg,
        &owner,
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
        &frozen,
        author.clone(),
        "core_remember",
        args.clone(),
    )
    .await?;
    let replay = call_tool(&pg, &owner, &frozen, author, "core_remember", args).await?;

    assert_eq!(first["idempotent_replay"], json!(false));
    assert_eq!(replay["idempotent_replay"], json!(true));
    assert_eq!(replay["handle"], first["handle"]);

    let memory_id = resolve_memory(first["handle"].as_str().expect("handle"))?;
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
    let author = author_ctx();

    let cited = call_tool(
        &pg,
        &owner,
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

    let cited_memory_id =
        resolve_memory(cited["handle"].as_str().expect("cited handle"))?.into_inner();
    let uncited_memory_id =
        resolve_memory(uncited["handle"].as_str().expect("uncited handle"))?.into_inner();

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
    let author = author_ctx();

    let first = call_tool(
        &pg,
        &owner,
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
    let author = author_ctx();

    let engine = engine_for_registry(&frozen, &pg);
    let remembered = call_tool_with_engine(
        &pg,
        &owner,
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
    let remembered_id = resolve_memory(remembered["handle"].as_str().expect("remember handle"))?;
    engine.ensure_fact_embedding(&owner, remembered_id).await?;
    let lexical = call_tool(
        &pg,
        &owner,
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
    let fact_a = call_tool(
        &pg,
        &owner_a,
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
        &frozen,
        author_ctx(),
        "core_derive",
        shared_args(fact_a["handle"].clone()),
    )
    .await?;
    let b = call_tool(
        &pg,
        &owner_b,
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
    let author = author_ctx();

    let fact = call_tool(
        &pg,
        &owner,
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
    let memory_id = resolve_memory(&handle).expect("resolves the just-created Fact handle");

    // an ordinary outsider with zero group role cannot read it yet.
    let outsider_subject = UserId::new(Uuid::now_v7());
    let outsider_authz = AuthzContext::for_subject(outsider_subject, AuthPath::HostBearer);
    let pre_publish_read = get_memory(
        outsider_ctx(
            &pg,
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
        // Invisible entities read as not-found: existence is not
        // disclosed to non-readers, so pre-publish the outsider cannot
        // even learn the Fact exists (not-exists and not-visible are
        // deliberately indistinguishable).
        Err(McpToolError::NotFound(message)) => {
            assert!(
                message.contains(&handle),
                "not-found names the wire handle: {message}"
            );
        }
        other => panic!("expected not-found before publish, got {other:?}"),
    }

    // publish requires admin authority on the current (Group) owner.
    call_tool_as(
        &pg,
        &group_owner,
        admin_authz.clone(),
        &frozen,
        author.clone(),
        Some(engine.clone()),
        "core_publish",
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
        &frozen,
        author.clone(),
        Some(engine.clone()),
        "core_publish",
        json!({"action": "publish_to_world", "entity": handle}),
    )
    .await;
    match republish {
        Err(McpToolError::Protocol(err)) => assert_eq!(err.code, ErrorCode::Forbidden),
        other => panic!("expected forbidden re-publish, got {other:?}"),
    }

    // reads now work for the same ordinary outsider.
    let post_publish_read = get_memory(
        outsider_ctx(&pg, &frozen, author, &engine, outsider_authz),
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
    let owner = nil_owner();
    let group = GroupId::new(Uuid::now_v7());
    let member = UserId::new(Uuid::now_v7());

    // `single_owner`/`for_subject` on a Personal owner has World+Personal
    // roles only — no resolver role on `group`.
    let err = call_tool_with_engine(
        &pg,
        &owner,
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
    let owner = nil_owner();
    let group = GroupId::new(Uuid::now_v7());
    let member = UserId::new(Uuid::now_v7());

    let err = call_tool_with_engine(
        &pg,
        &owner,
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
    let owner = nil_owner();
    let group = GroupId::new(Uuid::now_v7());

    let err = call_tool_with_engine(
        &pg,
        &owner,
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
    registry: &Arc<proxima_core::FlavorRegistryFrozen>,
    author: McpAuthorContext,
    engine: &Arc<Engine>,
    authz: AuthzContext,
) -> McpToolCtx {
    let subject = authz.subject().expect("subject-bearing authz");
    McpToolCtx {
        owner: OwnerRef::Personal(subject),
        authz,
        registry: registry.clone(),
        author,
        caller_self_perspective: None,
        extensions: McpToolExtensions::with(pg.pool_for_tests().clone()),
        engine: Some(engine.clone()),
    }
}

async fn call_tool(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    registry: &Arc<proxima_core::FlavorRegistryFrozen>,
    author: McpAuthorContext,
    name: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, proxima_core::McpToolError> {
    call_tool_with_engine(
        pg,
        owner,
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
    registry: &Arc<proxima_core::FlavorRegistryFrozen>,
    author: McpAuthorContext,
) {
    let invalid_since = call_tool(
        pg,
        owner,
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
            registry: registry.clone(),
            author,
            caller_self_perspective,
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
            registry: registry.clone(),
            author,
            caller_self_perspective,
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

async fn call_tool_with_engine(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
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
            registry: registry.clone(),
            author,
            caller_self_perspective: None,
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

/// Parse a wire memory reference by its class prefix (`F:`/`A:`/`P:`),
/// mirroring the server's prefixed-id grammar on the assertion side.
fn resolve_memory(raw: &str) -> Result<MemoryId, PrefixedUuidError> {
    let class = match raw.split_once(':').map(|(prefix, _)| prefix) {
        Some("A") => PrefixedUuidClass::Abstraction,
        Some("P") => PrefixedUuidClass::Perspective,
        _ => PrefixedUuidClass::Fact,
    };
    parse_prefixed_uuid(raw, class).map(MemoryId::new)
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

/// A Fact can cite a page of an uploaded document, through the shipped
/// schemas alone.
///
/// This was impossible before `core/uploaded-blob-whole-v1` and
/// `core/uploaded-blob-page-span-v1` existed. `core/uploaded-blob-v1` was a
/// registered cited object with no registered mapping naming it, and
/// `authorize_fact_with_citation` requires the mapping's
/// `cited_object_schema()` to match — so there was no argument a caller
/// could pass that attached a Fact to an uploaded blob. Every other test in
/// this file that cites anything defines its own schemas to do it; this one
/// deliberately uses no test-local types.
#[tokio::test]
#[allow(clippy::too_many_lines)] // linear PG flow: cite, span row, read-back locator, dedupe
async fn a_fact_can_cite_a_page_of_an_uploaded_document() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let frozen = Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    let owner = nil_owner();

    let blob = json!({
        "content_hash": vec![7u8; 32],
        "bucket": "proxima-cited",
        "object_key": "objects/owner/core/uploaded-blob-v1/deadbeef",
        "sha256": vec![9u8; 32],
        "byte_len": 4_096,
        "mime": "application/pdf",
        "filename": "handbuch.pdf",
        "etag": null,
        "uploaded_at": "2026-07-27T00:00:00Z",
    });

    let cited = call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Mindestbreite einer Tür",
            "body": "Die lichte Durchgangsbreite beträgt mindestens 90 cm.",
            "tags": ["din18040"],
            "idempotency_key": "page-span-cited-note",
            "citation": {
                "object_schema_id": proxima_core::UPLOADED_BLOB_SCHEMA_ID,
                "object_schema_version": 1,
                "object_payload": blob.clone(),
                "mapping_schema_id": proxima_core::UPLOADED_BLOB_PAGE_SPAN_SCHEMA_ID,
                "mapping_schema_version": 1,
                "mapping_payload": {"page_from": 47, "page_to": 47},
            }
        }),
    )
    .await?;

    let memory_id = resolve_memory(cited["handle"].as_str().expect("handle"))?.into_inner();
    let span: (i32, i32, Option<i32>, Option<i32>) = sqlx::query_as(
        "SELECT s.page_from, s.page_to, s.char_range_start, s.char_range_end
           FROM proxima_core.memories m
           JOIN proxima_core.citation_uploaded_blob_page_span_v1 s
             ON s.citation_mapping_id = m.citation_mapping_id
          WHERE m.memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(
        span,
        (47, 47, None, None),
        "the page span did not round-trip"
    );

    // The citation read-back returns the locator, not just ids: the page
    // span and what the document IS — never bucket/object_key, which the
    // read-back policy reserves for presigned-URL surfaces.
    let read_back = call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_fact",
        json!({ "action": "citation_of_fact", "fact": cited["handle"] }),
    )
    .await?;
    let citation = &read_back["citation"];
    assert_eq!(
        citation["page_span"],
        json!({"page_from": 47, "page_to": 47}),
        "read-back page span: {citation:#}"
    );
    assert_eq!(
        citation["document"],
        json!({
            "filename": "handbuch.pdf",
            "mime": "application/pdf",
            "byte_len": 4_096,
            "sha256_hex": "09".repeat(32),
            "uploaded_at": "2026-07-27T00:00:00Z",
        }),
        "read-back document metadata: {citation:#}"
    );
    assert!(
        citation.get("bucket").is_none() && citation["document"].get("bucket").is_none(),
        "storage coordinates must never appear in read-back: {citation:#}"
    );

    // Re-ingesting the same document under a different page reuses the one
    // cited object: that is what makes a book one artefact and its pages N
    // citations, rather than N copies of the book.
    call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Bewegungsfläche vor der Tür",
            "body": "Vor der Tür ist eine Bewegungsfläche vorzusehen.",
            "tags": ["din18040"],
            "idempotency_key": "page-span-cited-note-2",
            "citation": {
                "object_schema_id": proxima_core::UPLOADED_BLOB_SCHEMA_ID,
                "object_schema_version": 1,
                "object_payload": blob,
                "mapping_schema_id": proxima_core::UPLOADED_BLOB_PAGE_SPAN_SCHEMA_ID,
                "mapping_schema_version": 1,
                "mapping_payload": {"page_from": 48, "page_to": 49,
                                    "char_range_start": 0, "char_range_end": 120},
            }
        }),
    )
    .await?;

    let objects = count_rows(pg.pool_for_tests(), "proxima_core.cited_objects").await?;
    assert_eq!(objects, 1, "the same document became two cited objects");
    let mappings = count_rows(pg.pool_for_tests(), "proxima_core.citation_mappings").await?;
    assert_eq!(mappings, 2, "each page needs its own mapping");

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// A Fact can cite an ALREADY-STORED object by `cited_object_id` — the
/// only path an MCP client has after `core_upload`, since `complete`
/// deliberately never returns `bucket`/`object_key` and the inline
/// `object_payload` requires them. The by-ref mapping lands on the same
/// object row the inline path created, and `citation_of_fact` reads it
/// back.
#[tokio::test]
#[allow(clippy::too_many_lines)] // linear PG flow: inline object, by-ref cite, read-back assertions
async fn a_fact_can_cite_an_already_stored_object_by_id() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let frozen = Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    let owner = nil_owner();

    let blob = json!({
        "content_hash": vec![7u8; 32],
        "bucket": "proxima-cited",
        "object_key": "objects/owner/core/uploaded-blob-v1/deadbeef",
        "sha256": vec![9u8; 32],
        "byte_len": 4_096,
        "mime": "application/pdf",
        "filename": "handbuch.pdf",
        "etag": null,
        "uploaded_at": "2026-07-27T00:00:00Z",
    });
    let inline = call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Türbreite (inline zitiert)",
            "body": "Die lichte Durchgangsbreite beträgt mindestens 90 cm.",
            "tags": [],
            "citation": {
                "object_schema_id": proxima_core::UPLOADED_BLOB_SCHEMA_ID,
                "object_schema_version": 1,
                "object_payload": blob,
                "mapping_schema_id": proxima_core::UPLOADED_BLOB_WHOLE_SCHEMA_ID,
                "mapping_schema_version": 1,
                "mapping_payload": {},
            }
        }),
    )
    .await?;
    let inline_citation = call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_fact",
        json!({ "action": "citation_of_fact", "fact": inline["handle"] }),
    )
    .await?;
    let cited_object_id = inline_citation["citation"]["cited_object_id"]
        .as_str()
        .expect("cited_object_id")
        .to_string();

    // By-ref cite with the `C:` prefix and a page-span mapping: no
    // object payload anywhere in the call.
    let by_ref = call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Bewegungsfläche (per Referenz zitiert)",
            "body": "Vor der Tür ist eine Bewegungsfläche vorzusehen.",
            "tags": [],
            "citation": {
                "cited_object_id": format!("C:{cited_object_id}"),
                "mapping_schema_id": proxima_core::UPLOADED_BLOB_PAGE_SPAN_SCHEMA_ID,
                "mapping_schema_version": 1,
                "mapping_payload": {"page_from": 47, "page_to": 47},
            }
        }),
    )
    .await?;
    let read_back = call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_fact",
        json!({ "action": "citation_of_fact", "fact": by_ref["handle"] }),
    )
    .await?;
    assert_eq!(
        read_back["citation"]["cited_object_id"]
            .as_str()
            .expect("by-ref cited_object_id"),
        cited_object_id,
        "the by-ref mapping must land on the referenced object"
    );
    assert_eq!(
        read_back["citation"]["mapping_schema_id"],
        json!(proxima_core::UPLOADED_BLOB_PAGE_SPAN_SCHEMA_ID)
    );

    // One artefact, two citations: by-ref must not have minted a second
    // object row, and its mapping sidecar (the page span) must exist.
    let objects = count_rows(pg.pool_for_tests(), "proxima_core.cited_objects").await?;
    assert_eq!(objects, 1, "by-ref citing duplicated the cited object");
    let mappings = count_rows(pg.pool_for_tests(), "proxima_core.citation_mappings").await?;
    assert_eq!(mappings, 2);
    let by_ref_memory = resolve_memory(by_ref["handle"].as_str().expect("handle"))?.into_inner();
    let span: (i32, i32) = sqlx::query_as(
        "SELECT s.page_from, s.page_to
           FROM proxima_core.memories m
           JOIN proxima_core.citation_uploaded_blob_page_span_v1 s
             ON s.citation_mapping_id = m.citation_mapping_id
          WHERE m.memory_id = $1",
    )
    .bind(by_ref_memory)
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(span, (47, 47), "by-ref mapping sidecar did not land");

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// A by-ref citation against an id that does not exist — or that exists
/// only under ANOTHER owner — fails with the same clean, caller-fixable
/// error, and writes nothing.
#[tokio::test]
#[allow(clippy::too_many_lines)] // linear PG flow: the missing-id and foreign-owner arms read best together
async fn by_ref_citation_rejects_missing_and_foreign_objects()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let frozen = Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    let owner = nil_owner();

    // Nonexistent id.
    let missing = uuid::Uuid::now_v7();
    let err = call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Zitat ins Leere",
            "body": "Es gibt kein Objekt.",
            "tags": [],
            "citation": {
                "cited_object_id": missing.to_string(),
                "mapping_schema_id": proxima_core::UPLOADED_BLOB_WHOLE_SCHEMA_ID,
                "mapping_schema_version": 1,
                "mapping_payload": {},
            }
        }),
    )
    .await
    .expect_err("citing a nonexistent object must fail");
    match &err {
        McpToolError::Protocol(protocol) => {
            assert_eq!(protocol.code, ErrorCode::InvalidArgument, "{err:?}");
            assert!(
                protocol.message.contains("not found for this owner"),
                "message: {}",
                protocol.message
            );
        }
        other => panic!("expected clean invalid-argument protocol error, got {other:?}"),
    }

    // Owner A stores an object; owner B tries to cite it by id. Same
    // error as nonexistent — foreign existence must not be observable.
    call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "As Dokument",
            "body": "Gehört Owner A.",
            "tags": [],
            "citation": {
                "object_schema_id": proxima_core::UPLOADED_BLOB_SCHEMA_ID,
                "object_schema_version": 1,
                "object_payload": {
                    "content_hash": vec![3u8; 32],
                    "bucket": "b", "object_key": "k",
                    "sha256": vec![4u8; 32], "byte_len": 1,
                    "mime": "application/pdf", "filename": "a.pdf",
                    "etag": null, "uploaded_at": "2026-07-27T00:00:00Z",
                },
                "mapping_schema_id": proxima_core::UPLOADED_BLOB_WHOLE_SCHEMA_ID,
                "mapping_schema_version": 1,
                "mapping_payload": {},
            }
        }),
    )
    .await?;
    let a_object: uuid::Uuid =
        sqlx::query_scalar("SELECT cited_object_id FROM proxima_core.cited_objects")
            .fetch_one(pg.pool_for_tests())
            .await?;

    let owner_b = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let err = call_tool_as(
        &pg,
        &owner_b,
        AuthzContext::single_owner(&owner_b, AuthPath::HostBearer),
        &frozen,
        author_ctx(),
        Some(engine_for_registry(&frozen, &pg)),
        "core_remember",
        json!({
            "title": "Bs Zitatversuch",
            "body": "Zeigt auf As Objekt.",
            "tags": [],
            "citation": {
                "cited_object_id": a_object.to_string(),
                "mapping_schema_id": proxima_core::UPLOADED_BLOB_WHOLE_SCHEMA_ID,
                "mapping_schema_version": 1,
                "mapping_payload": {},
            }
        }),
    )
    .await
    .expect_err("citing another owner's object must fail");
    match &err {
        McpToolError::Protocol(protocol) => {
            assert_eq!(protocol.code, ErrorCode::InvalidArgument, "{err:?}");
            assert!(
                protocol.message.contains("not found for this owner"),
                "message: {}",
                protocol.message
            );
        }
        other => panic!("expected clean invalid-argument protocol error, got {other:?}"),
    }

    // Neither failed attempt left a Fact or mapping behind for B, and A
    // keeps exactly its one mapping. Both tables matter: a regression that
    // commits the Fact before verifying the citation would keep the
    // mapping count green while leaving an uncited Fact behind.
    let mappings = count_rows(pg.pool_for_tests(), "proxima_core.citation_mappings").await?;
    assert_eq!(mappings, 1, "failed by-ref attempts must write no mapping");
    let memories = count_rows(pg.pool_for_tests(), "proxima_core.memories").await?;
    assert_eq!(memories, 1, "failed by-ref attempts must write no Fact");

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// The mapping's declared cited-object schema must match the referenced
/// object's stored schema — the same target check the inline path runs,
/// enforced against the stored row for by-ref.
#[tokio::test]
async fn by_ref_citation_rejects_a_mapping_target_mismatch()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_remember_sidecars().await;
    create_remember_citation_sidecars(pg.pool_for_tests()).await?;
    let frozen = registry_with_remember_test_citation();
    let owner = nil_owner();

    // Store an object of the TEST schema inline.
    call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Testartefakt",
            "body": "Ein Objekt mit Testschema.",
            "tags": [],
            "citation": {
                "object_schema_id": RememberTestCitedObject::SCHEMA_ID,
                "object_schema_version": 1,
                "object_payload": {"artifact_id": "art-1", "locator": "loc-1"},
                "mapping_schema_id": RememberTestCitationMapping::SCHEMA_ID,
                "mapping_schema_version": 1,
                "mapping_payload": {"section": "s", "byte_start": 0, "byte_end": 1},
            }
        }),
    )
    .await?;
    let test_object: uuid::Uuid =
        sqlx::query_scalar("SELECT cited_object_id FROM proxima_core.cited_objects")
            .fetch_one(pg.pool_for_tests())
            .await?;

    // By-ref with a mapping that targets core/uploaded-blob-v1 instead.
    let err = call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Falsches Mapping",
            "body": "Die Zuordnung passt nicht zum Objekt.",
            "tags": [],
            "citation": {
                "cited_object_id": test_object.to_string(),
                "mapping_schema_id": proxima_core::UPLOADED_BLOB_WHOLE_SCHEMA_ID,
                "mapping_schema_version": 1,
                "mapping_payload": {},
            }
        }),
    )
    .await
    .expect_err("mapping target mismatch must be rejected");
    match &err {
        McpToolError::Protocol(protocol) => {
            assert_eq!(protocol.code, ErrorCode::InvalidArgument, "{err:?}");
            assert!(
                protocol
                    .message
                    .contains(proxima_core::UPLOADED_BLOB_SCHEMA_ID)
                    && protocol
                        .message
                        .contains(RememberTestCitedObject::SCHEMA_ID),
                "message must name both schemas: {}",
                protocol.message
            );
        }
        other => panic!("expected invalid-argument protocol error, got {other:?}"),
    }

    // The mapping count alone would stay green if a regression committed
    // the Fact before the target check; the memories count pins that too.
    let mappings = count_rows(pg.pool_for_tests(), "proxima_core.citation_mappings").await?;
    assert_eq!(mappings, 1, "the mismatch must write no mapping");
    let memories = count_rows(pg.pool_for_tests(), "proxima_core.memories").await?;
    assert_eq!(memories, 1, "the mismatch must write no Fact");

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// The citation argument's arity is validated before any engine work:
/// by-ref and inline are mutually exclusive, and one of them is required.
#[tokio::test]
async fn remember_citation_arity_is_validated() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let frozen = Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    let owner = nil_owner();

    let both = call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Beides",
            "body": "Referenz und Inline zugleich.",
            "tags": [],
            "citation": {
                "cited_object_id": Uuid::now_v7().to_string(),
                "object_schema_id": proxima_core::UPLOADED_BLOB_SCHEMA_ID,
                "object_schema_version": 1,
                "object_payload": {},
                "mapping_schema_id": proxima_core::UPLOADED_BLOB_WHOLE_SCHEMA_ID,
                "mapping_schema_version": 1,
                "mapping_payload": {},
            }
        }),
    )
    .await;
    match both {
        Err(McpToolError::InvalidInput(message)) => {
            assert!(message.contains("not both"), "message: {message}");
        }
        other => panic!("expected InvalidInput for both shapes, got {other:?}"),
    }

    let neither = call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Keines",
            "body": "Weder Referenz noch Inline.",
            "tags": [],
            "citation": {
                "mapping_schema_id": proxima_core::UPLOADED_BLOB_WHOLE_SCHEMA_ID,
                "mapping_schema_version": 1,
                "mapping_payload": {},
            }
        }),
    )
    .await;
    match neither {
        Err(McpToolError::InvalidInput(message)) => {
            assert!(message.contains("cited_object_id"), "message: {message}");
        }
        other => panic!("expected InvalidInput for neither shape, got {other:?}"),
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// The sidecar's constraints, not just the Rust helper, reject a malformed
/// span. A client that writes the row by any other path gets the same
/// answer.
#[tokio::test]
async fn a_zero_page_span_is_rejected_by_storage() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let frozen = Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    let owner = nil_owner();

    let result = call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Ungültige Seitenangabe",
            "body": "Seite 0 gibt es nicht.",
            "tags": [],
            "idempotency_key": "page-span-zero",
            "citation": {
                "object_schema_id": proxima_core::UPLOADED_BLOB_SCHEMA_ID,
                "object_schema_version": 1,
                "object_payload": {
                    "content_hash": vec![1u8; 32],
                    "bucket": "b", "object_key": "k",
                    "sha256": vec![2u8; 32], "byte_len": 1,
                    "mime": "application/pdf", "filename": "x.pdf",
                    "etag": null, "uploaded_at": "2026-07-27T00:00:00Z",
                },
                "mapping_schema_id": proxima_core::UPLOADED_BLOB_PAGE_SPAN_SCHEMA_ID,
                "mapping_schema_version": 1,
                "mapping_payload": {"page_from": 0, "page_to": 0},
            }
        }),
    )
    .await;
    let err = format!("{:?}", result.as_ref().err());
    assert!(result.is_err(), "page 0 was accepted");
    assert!(
        err.contains("citation_blob_page_span_pages_chk"),
        "rejected for the wrong reason: {err}"
    );

    let mappings = count_rows(pg.pool_for_tests(), "proxima_core.citation_mappings").await?;
    assert_eq!(mappings, 0, "a rejected span left a mapping row behind");

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// `language: "auto"` on `core_remember`: the detector stamps the row and
/// its sidecar german, and the note becomes reachable through a German
/// inflection no English stemmer can conflate. The reliability gate is
/// what makes "auto" safe — measured ≥98% accurate wherever the detector
/// calls itself reliable, and falling back to the default where it does
/// not (second write below).
#[tokio::test]
async fn auto_detection_stamps_a_german_note_and_makes_it_searchable()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let frozen = Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    let owner = nil_owner();

    let noted = call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "Bauleitung beauftragt",
            "body": "Die Bauleitung wurde beauftragt, die Fluchtwege nach DIN 18040 \
                     barrierefrei zu planen und die Türbreiten zu prüfen.",
            "tags": [],
            "language": "auto",
        }),
    )
    .await?;
    let memory_id = resolve_memory(noted["handle"].as_str().expect("handle"))?.into_inner();

    let (row_language, sidecar_language): (String, String) = sqlx::query_as(
        "SELECT m.lexical_language::text, n.lexical_language::text
           FROM proxima_core.memories m
           JOIN proxima_core.agent_note_v1 n USING (memory_id)
          WHERE m.memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(row_language, "german", "auto did not detect German");
    assert_eq!(
        sidecar_language, "german",
        "the note sidecar did not mirror the detected language"
    );

    // `Bauleitungen` shares no token or substring with the note; only the
    // German stemmer reaches it.
    let page = call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_search_memories",
        json!({"query": "Bauleitungen", "mode": "lexical", "limit": 5}),
    )
    .await?;
    let memories = page["memories"].as_array().expect("memories array");
    assert!(
        memories
            .iter()
            .any(|memory| memory["memory"].as_str() == noted["handle"].as_str()),
        "the auto-stamped note is unreachable through its German inflection: {page:#}"
    );

    // No language signal → no guess: the row takes the database default.
    let defaulted = call_tool(
        &pg,
        &owner,
        &frozen,
        author_ctx(),
        "core_remember",
        json!({
            "title": "42",
            "body": "1024 2048 4096",
            "tags": [],
            "language": "auto",
        }),
    )
    .await?;
    let defaulted_id = resolve_memory(defaulted["handle"].as_str().expect("handle"))?.into_inner();
    let stamped: String = sqlx::query_scalar(
        "SELECT lexical_language::text FROM proxima_core.memories WHERE memory_id = $1",
    )
    .bind(defaulted_id)
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(
        stamped, "english",
        "an unreliable detection must fall back to the default, not guess"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Authoring surfaces strip leading and trailing whitespace before validating
/// length, and the descriptions now say so. Two consequences follow from the
/// order, and both are worth pinning: a whitespace-only value is rejected as
/// empty rather than stored blank, and a value that only exceeds the cap
/// because of trailing whitespace is accepted rather than refused.
///
/// Appended at the end of the file rather than beside the other search tests
/// to keep it off the anchor two other branches insert at.
#[tokio::test]
async fn authoring_trims_before_it_checks_length() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let registry = FlavorRegistry::new();
    let frozen = Arc::new(registry.freeze_or_panic_for_tests());
    let owner = nil_owner();
    let author = author_ctx();

    let padded = call_tool(
        &pg,
        &owner,
        &frozen,
        author.clone(),
        "core_remember",
        json!({ "title": "  Padded title  ", "body": "  padded body\n" }),
    )
    .await?;
    let read = read_memory_prefixed(
        &pg,
        &owner,
        &frozen,
        author.clone(),
        padded["handle"].as_str().expect("handle"),
        false,
    )
    .await?;
    assert_eq!(read["title"], json!("Padded title"));
    assert_eq!(read["body"], json!("padded body"));

    let blank = call_tool(
        &pg,
        &owner,
        &frozen,
        author.clone(),
        "core_remember",
        json!({ "title": "ok", "body": "   " }),
    )
    .await;
    assert!(
        blank.is_err(),
        "a whitespace-only body trims to empty and must be refused, not stored blank",
    );

    // At the cap once trimmed: the trailing newline must not push it over.
    let at_cap = call_tool(
        &pg,
        &owner,
        &frozen,
        author,
        "core_remember",
        json!({ "title": "at cap", "body": format!("{}\n", "a".repeat(20_000)) }),
    )
    .await?;
    assert!(
        at_cap["handle"]
            .as_str()
            .is_some_and(|h| h.starts_with("F:")),
        "20000 chars plus trailing whitespace trims to exactly the cap: {at_cap}",
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}
