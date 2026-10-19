//! #488 review r1: a forgotten memory reads as absent on every read path.

use std::sync::Arc;

use proxima::Proxima;
use proxima_core::llm::BoundEmbeddingClient;
use proxima_core::test_fixtures::TestEmbeddingRouter;
use proxima_core::{OwnerId, Role, UserId};
use proxima_pg_testkit::db_url;
use uuid::Uuid;

use super::split_core_db::clone_split_core_db;
use super::{
    AgentMemoryApp, call_test_model_tool, read_test_model_resource, space_authz, test_embedding,
};

fn handles(rows: &serde_json::Value, list: &str, field: &str) -> Vec<String> {
    rows[list]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row[field].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Recall takes caller-supplied subjects as raw ids. A forgotten subject's
/// sketch is not returned, while a live subject in the same call is.
#[tokio::test]
async fn recall_through_a_forgotten_subject_returns_nothing_for_it() {
    let split_db = clone_split_core_db("proxima_recall_forgotten")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let remember = |title: &'static str| {
            call(
                "core_remember",
                serde_json::json!({"title": title, "body": title, "tags": []}),
            )
        };
        let gone = remember("recallneedle forgotten").await?["handle"]
            .as_str()
            .ok_or("handle")?
            .to_owned();
        let kept = remember("recallneedle kept").await?["handle"]
            .as_str()
            .ok_or("handle")?
            .to_owned();
        let args = serde_json::json!({"subjects": [gone.clone(), kept.clone()], "limit": 16});
        let before = call("core_recall", args.clone()).await?;
        assert!(
            handles(&before, "sketches", "handle").contains(&gone),
            "{before}"
        );

        call("core_forget", serde_json::json!({"memory": gone})).await?;

        let after = call("core_recall", args).await?;
        let recalled = handles(&after, "sketches", "handle");
        assert!(!recalled.contains(&gone), "{after}");
        assert!(
            !after.to_string().contains("recallneedle forgotten"),
            "{after}"
        );
        assert!(recalled.contains(&kept), "{after}");
        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("recall forgotten subject");
}

/// Store 0's view of store 1: every read is forwarded to store 1's served
/// tools as the same user, exactly as a host's store endpoint would answer.
struct ForwardTo {
    tools: proxima::CoreMcpTools,
    owner: OwnerId,
    authz: proxima::AuthzContext,
}

impl proxima::host::OtherStores for ForwardTo {
    fn stores<'a>(
        &'a self,
        _caller: proxima::host::StoreCaller<'a>,
    ) -> futures::future::BoxFuture<
        'a,
        Result<Vec<proxima::host::StoreInfo>, proxima::host::OtherStoresError>,
    > {
        Box::pin(async move {
            Ok(vec![proxima::host::StoreInfo {
                number: 1,
                name: "second".into(),
                description: "the store a forget happened in".into(),
                schema_ids: Vec::new(),
            }])
        })
    }

    fn search<'a>(
        &'a self,
        _caller: proxima::host::StoreCaller<'a>,
        _store: u16,
        args: &'a proxima::host::RemoteSearch,
    ) -> futures::future::BoxFuture<
        'a,
        Result<proxima::host::RemoteSearchResult, proxima::host::OtherStoresError>,
    > {
        Box::pin(async move {
            let found = call_test_model_tool(
                &self.tools,
                self.authz.clone(),
                self.owner,
                "core_search_memories",
                serde_json::json!({"query": args.query, "mode": "lexical"}),
            )
            .await
            .map_err(|err| proxima::host::OtherStoresError::Unavailable(err.to_string()))?;
            let hits = found["memories"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|hit| {
                    let handle = hit["memory"]
                        .as_str()
                        .or_else(|| hit["memory_id"].as_str())?
                        .parse()
                        .ok()?;
                    Some(proxima::host::RemoteHit {
                        handle,
                        writer: proxima_core::WriterAttribution {
                            by: self.owner.into_inner(),
                            via: None,
                        },
                        kind: proxima_core::verbs::query::EntityKind::Fact,
                        schema_id: "core/fact".into(),
                        snippet: hit.to_string(),
                        created_at: time::OffsetDateTime::UNIX_EPOCH,
                        tags: Vec::new(),
                    })
                })
                .collect();
            Ok(proxima::host::RemoteSearchResult {
                hits,
                degraded_to_lexical: false,
            })
        })
    }

    fn read<'a>(
        &'a self,
        _caller: proxima::host::StoreCaller<'a>,
        id: Uuid,
    ) -> futures::future::BoxFuture<
        'a,
        Result<proxima::host::RemoteRead, proxima::host::OtherStoresError>,
    > {
        Box::pin(async move {
            let read = read_test_model_resource(
                &self.tools,
                self.authz.clone(),
                self.owner,
                &format!("proxima://memory/{id}"),
            )
            .await;
            Ok(match read {
                Ok(memory) => proxima::host::RemoteRead::Found(proxima::host::RemoteMemory {
                    handle: id,
                    writer: proxima_core::WriterAttribution {
                        by: self.owner.into_inner(),
                        via: None,
                    },
                    kind: proxima_core::verbs::query::EntityKind::Fact,
                    schema_id: "core/fact".into(),
                    title: memory["title"].as_str().map(str::to_owned),
                    body: memory.to_string(),
                    created_at: time::OffsetDateTime::UNIX_EPOCH,
                    tags: Vec::new(),
                }),
                Err(_) => proxima::host::RemoteRead::NotFound,
            })
        })
    }
}

/// Acceptance item 2 of #488, end to end and without an object store:
/// after `core_forget`, lexical and semantic search, recall by question and
/// by subject, a walk, a by-id read and a batch read return nothing for the
/// memory, and another store reading it through its port sees it as
/// unavailable. Each leg is asserted before the forget too, so an empty
/// answer is about the forget and not the fixture.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one story: control, forget, every read path, both stores
async fn a_forgotten_memory_reads_as_absent_everywhere_and_unavailable_from_another_store() {
    let second_db = clone_split_core_db("proxima_forgotten_reads_b")
        .await
        .expect("PG required for tests");
    let first_db = clone_split_core_db("proxima_forgotten_reads_a")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let authz = space_authz(owner, vec![owner], Role::admin());
        // Store 1 holds the memory that is forgotten. Its Facts are admitted
        // with no embedding client, so the vector below is the only one.
        let router = Arc::new(TestEmbeddingRouter::default());
        let second = Proxima::<AgentMemoryApp>::app()
            .database_url(second_db.runtime_url().to_owned())
            .store_number(1)
            .platform_database_url(second_db.platform_url().to_owned())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .embedding_router(router.clone())
            .build()
            .await?;
        let tools = second.host().core_mcp_tools();
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let read = |uri: String| {
            let tools = &tools;
            let authz = authz.clone();
            async move { read_test_model_resource(tools, authz, owner, &uri).await }
        };
        let remember = |title: &'static str, body: &'static str| {
            call(
                "core_remember",
                serde_json::json!({"title": title, "body": body, "tags": [], "language": "english"}),
            )
        };
        let gone = remember("forgetneedle gone", "Unrelated vocabulary").await?["handle"]
            .as_str()
            .ok_or("handle")?
            .to_owned();
        let kept = remember("forgetneedle kept", "kept body").await?["handle"]
            .as_str()
            .ok_or("handle")?
            .to_owned();
        let gone_t: Uuid = gone.parse()?;
        let admin = sqlx::PgPool::connect(&db_url(second_db.name())).await?;
        let mut vector = vec![0.0_f32; 1024];
        vector[0] = 0.8;
        vector[1] = 0.6;
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_id, model_id, dim, embedding_version, vec, owner_id)
             VALUES ($1, 'test-embed', 1024, 1, $2::real[]::vector, $3)",
        )
        .bind(gone_t)
        .bind(&vector)
        .bind(owner.into_inner())
        .execute(&admin)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_heads
                (entity_id, model_id, dim, embedding_version, owner_id)
             VALUES ($1, 'test-embed', 1024, 1, $2)",
        )
        .bind(gone_t)
        .bind(owner.into_inner())
        .execute(&admin)
        .await?;
        admin.close().await;
        router.set_default(BoundEmbeddingClient::bind(test_embedding())?);

        let lexical = serde_json::json!({"query": "forgetneedle", "mode": "lexical"});
        let semantic = serde_json::json!({
            "query": "How does the checkpoint recovery commit work?",
            "mode": "hybrid", "kind": "Fact", "semantic_weight": 1.0,
        });
        let by_question = serde_json::json!({"question": "forgetneedle", "limit": 16});
        let by_subject = serde_json::json!({"subjects": [gone.clone(), kept.clone()], "limit": 16});
        let think = serde_json::json!({"seeds": [gone.clone()], "direction": "ancestors"});
        let walk = format!("proxima://memory/{gone}/lineage?direction=ancestors&depth=3");
        let by_id = format!("proxima://memory/{gone}");
        let batch = format!("proxima://memories?ids={gone},{kept}");

        // Store 0 reads store 1 through its port, and derives from the
        // memory while it is readable.
        let first = Proxima::<AgentMemoryApp>::app()
            .database_url(first_db.runtime_url().to_owned())
            .store_number(0)
            .platform_database_url(first_db.platform_url().to_owned())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .other_stores(Arc::new(ForwardTo {
                tools: tools.clone(),
                owner,
                authz: authz.clone(),
            }))
            .build()
            .await?;
        let first_tools = first.host().core_mcp_tools();
        let first_call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&first_tools, authz.clone(), owner, tool, args)
        };
        let first_read = |uri: String| {
            let tools = &first_tools;
            let authz = authz.clone();
            async move { read_test_model_resource(tools, authz, owner, &uri).await }
        };
        let local = first_call(
            "core_remember",
            serde_json::json!({"title": "local basis", "body": "local basis", "tags": []}),
        )
        .await?["handle"]
            .as_str()
            .ok_or("handle")?
            .to_owned();
        let derived = first_call(
            "core_derive",
            serde_json::json!({
                "kind": "Abstraction", "title": "over both", "body": "over both",
                "source_handles": [local, gone.clone()], "model_id": "test-model"
            }),
        )
        .await?["handle"]
            .as_str()
            .ok_or("handle")?
            .to_owned();
        let remote_walk = format!("proxima://memory/{derived}/lineage?direction=ancestors&depth=3");
        let remote_batch = format!("proxima://memories?ids={gone}");
        let remote_search = serde_json::json!({"query": "forgetneedle", "mode": "lexical", "stores": [1]});
        let remote_node = |lineage: &serde_json::Value| {
            lineage["nodes"]
                .as_array()
                .and_then(|nodes| nodes.iter().find(|node| node["memory"] == gone.as_str()))
                .cloned()
        };

        // Control: every path reaches the memory before the forget.
        let has = |value: &serde_json::Value| value.to_string().contains(&gone);
        let found = call("core_search_memories", lexical.clone()).await?;
        assert!(has(&found), "lexical control: {found}");
        let found = call("core_search_memories", semantic.clone()).await?;
        assert!(has(&found), "semantic control: {found}");
        assert_eq!(found["degraded_to_lexical"], false, "{found}");
        let found = call("core_recall", by_question.clone()).await?;
        assert!(has(&found), "recall question control: {found}");
        let found = call("core_recall", by_subject.clone()).await?;
        assert!(has(&found), "recall subject control: {found}");
        assert!(read(walk.clone()).await.is_ok(), "walk control");
        let found = call("core_think", think.clone()).await?;
        assert!(has(&found), "think control: {found}");
        assert_eq!(read(by_id.clone()).await?["memory"], gone.as_str());
        let found = read(batch.clone()).await?;
        assert_eq!(handles(&found, "memories", "memory").len(), 2, "{found}");
        let lineage = first_read(remote_walk.clone()).await?;
        let node = remote_node(&lineage).ok_or("remote node")?;
        assert!(node.get("status").is_none(), "{lineage}");
        let found = first_read(remote_batch.clone()).await?;
        assert!(handles(&found, "memories", "memory").contains(&gone), "{found}");
        let found = first_call("core_search_memories", remote_search.clone()).await?;
        assert!(has(&found), "remote search control: {found}");

        call("core_forget", serde_json::json!({"memory": gone})).await?;

        // Store 1 itself: nothing for it, and the live neighbour stays.
        let found = call("core_search_memories", lexical).await?;
        assert!(!has(&found), "lexical: {found}");
        assert!(found.to_string().contains(&kept), "{found}");
        let found = call("core_search_memories", semantic).await?;
        assert!(!has(&found), "semantic: {found}");
        let found = call("core_recall", by_question).await?;
        assert!(!has(&found), "recall question: {found}");
        let found = call("core_recall", by_subject).await?;
        assert!(!has(&found), "recall subject: {found}");
        assert!(found.to_string().contains(&kept), "{found}");
        assert!(read(walk).await.is_err(), "a walk from it finds no start");
        let think = call("core_think", think).await;
        assert!(think.as_ref().map_or(true, |page| !has(page)), "{think:?}");
        assert!(read(by_id).await.is_err(), "by id");
        let found = read(batch).await?;
        let listed = handles(&found, "memories", "memory");
        assert_eq!(listed, vec![kept.clone()], "{found}");

        // Store 0 through the port: unavailable, not readable, no hit.
        let lineage = first_read(remote_walk).await?;
        let node = remote_node(&lineage).ok_or("remote node stays named")?;
        assert_eq!(node["status"], "unavailable", "{lineage}");
        assert_eq!(node["unavailable_reason"], "not readable", "{lineage}");
        let found = first_read(remote_batch).await?;
        assert!(handles(&found, "memories", "memory").is_empty(), "{found}");
        let found = first_call("core_search_memories", remote_search).await?;
        assert!(!has(&found), "remote search: {found}");

        first.shutdown().await;
        second.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("forgotten reads");
}

/// A well-formed local id that names nothing: `existing` with its random
/// tail flipped, so the store number still says "this store".
fn absent_local_id(existing: &str) -> Result<String, uuid::Error> {
    let mut bytes = *existing.parse::<Uuid>()?.as_bytes();
    for byte in &mut bytes[12..] {
        *byte ^= 0xff;
    }
    Ok(Uuid::from_bytes(bytes).to_string())
}

/// What a caller observes of a refused call, with the one id it named
/// replaced, so a forgotten id and an absent id compare equal iff the
/// caller cannot tell them apart.
fn observed(err: &proxima::CoreMcpError, id: &str) -> String {
    format!("{err:?}").replace(id, "<id>")
}

/// Pin integrity accepts any memory row since #488 S13, so the trigger no
/// longer stops a new write that names a forgotten memory. The owner scope
/// the write resolves its sources in must: deriving from, or interpreting,
/// a forgotten memory is refused exactly as naming an absent id is, also
/// when a live source is named beside it.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one fixture, every citing write
async fn a_new_write_cannot_cite_a_forgotten_memory() {
    let split_db = clone_split_core_db("proxima_cite_forgotten")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(split_db.runtime_url().to_owned())
            .store_number(0)
            .platform_database_url(split_db.platform_url().to_owned())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let handle = |made: serde_json::Value| -> Result<String, &'static str> {
            Ok(made["handle"].as_str().ok_or("handle")?.to_owned())
        };
        let remember = |title: &'static str| {
            call(
                "core_remember",
                serde_json::json!({"title": title, "body": title, "tags": []}),
            )
        };
        let derive = |kind: &'static str, sources: Vec<String>, key: String| {
            call(
                "core_derive",
                serde_json::json!({
                    "kind": kind, "title": "cites", "body": "cites", "tags": [],
                    "source_handles": sources, "model_id": "test-model",
                    "idempotency_key": key,
                }),
            )
        };
        let interpret = |subjects: Vec<String>| {
            call(
                "core_interpret",
                serde_json::json!({
                    "claim": "about it", "confidence": 70,
                    "subjects": subjects, "model_id": "test-model",
                }),
            )
        };
        let live = handle(remember("live fact").await?)?;
        let fact = handle(remember("forgotten fact").await?)?;
        let abstraction = handle(
            derive(
                "Abstraction",
                vec![live.clone()],
                "cite-forgotten-abstraction".into(),
            )
            .await?,
        )?;
        // Control: both are citable while live.
        derive(
            "Abstraction",
            vec![fact.clone()],
            "cite-control-fact".into(),
        )
        .await?;
        derive(
            "Perspective",
            vec![abstraction.clone()],
            "cite-control-abs".into(),
        )
        .await?;
        interpret(vec![fact.clone()]).await?;

        call("core_forget", serde_json::json!({"memory": fact})).await?;
        call("core_forget", serde_json::json!({"memory": abstraction})).await?;

        for (forgotten, kind) in [(&fact, "Abstraction"), (&abstraction, "Perspective")] {
            let absent = absent_local_id(forgotten)?;
            for (with_live, tag) in [(false, "alone"), (true, "beside-live")] {
                let sources = |id: &str| {
                    let mut sources = vec![id.to_owned()];
                    if with_live && kind == "Abstraction" {
                        sources.push(live.clone());
                    }
                    sources
                };
                let cited = derive(kind, sources(forgotten), format!("cite-{kind}-{tag}"))
                    .await
                    .expect_err("deriving from a forgotten memory is refused");
                let missing = derive(kind, sources(&absent), format!("absent-{kind}-{tag}"))
                    .await
                    .expect_err("deriving from an absent memory is refused");
                assert_eq!(observed(&cited, forgotten), observed(&missing, &absent));
            }
        }
        let absent = absent_local_id(&fact)?;
        let cited = interpret(vec![fact.clone(), live.clone()])
            .await
            .expect_err("interpreting a forgotten memory is refused");
        let missing = interpret(vec![absent.clone(), live.clone()])
            .await
            .expect_err("interpreting an absent memory is refused");
        assert_eq!(observed(&cited, &fact), observed(&missing, &absent));
        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("cite forgotten");
}

/// "A replay answers, it does not revive." Replaying the write that made a
/// derived memory, after that memory was forgotten, answers with its handle
/// and `forgotten: true` and writes nothing: no new version, no search hit.
/// The same holds for an interpretation and for an episode's derive.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one fixture, three replaying writes
async fn replaying_a_derived_write_after_forget_answers_forgotten_and_does_not_revive() {
    let split_db = clone_split_core_db("proxima_derive_replay_forgotten")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(split_db.runtime_url().to_owned())
            .store_number(0)
            .platform_database_url(split_db.platform_url().to_owned())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let memory_rows = || async {
            let admin = sqlx::PgPool::connect(&db_url(split_db.name())).await?;
            // Every episode call records its own write-act Fact; count the
            // memories a replay could revive.
            let rows: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM proxima_core.memory WHERE schema_id <> 'core/write-act-v1'",
            )
                .fetch_one(&admin)
                .await?;
            admin.close().await;
            Ok::<i64, Box<dyn std::error::Error>>(rows)
        };
        let source = call(
            "core_remember",
            serde_json::json!({"title": "replay source", "body": "replay source", "tags": []}),
        )
        .await?["handle"]
            .as_str()
            .ok_or("handle")?
            .to_owned();
        let derive_args = serde_json::json!({
            "kind": "Abstraction", "title": "replayneedle derived", "body": "replayneedle derived",
            "tags": [], "source_handles": [source.clone()], "model_id": "test-model",
        });
        let interpret_args = serde_json::json!({
            "claim": "replayneedle stance", "confidence": 70,
            "subjects": [source.clone()], "model_id": "test-model",
        });
        let episode_args = serde_json::json!({
            "remember": [{"title": "episode fact", "body": "episode fact", "idempotency_key": "ep-fact"}],
            "derive": {"title": "replayneedle episode", "body": "replayneedle episode",
                "source_handles": ["remember:0"], "model_id": "test-model",
                "idempotency_key": "ep-derive"},
        });
        let derived = call("core_derive", derive_args.clone()).await?;
        let stance = call("core_interpret", interpret_args.clone()).await?;
        let episode = call("core_episode_commit", episode_args.clone()).await?;
        let derived = derived["handle"].as_str().ok_or("handle")?.to_owned();
        let stance = stance["handle"].as_str().ok_or("handle")?.to_owned();
        let episode_derived = episode["derived"].as_str().ok_or("derived")?.to_owned();
        // Control: a replay of a live write answers without `forgotten`.
        let replay = call("core_derive", derive_args.clone()).await?;
        assert_eq!(replay["handle"], derived.as_str(), "{replay}");
        assert_eq!(replay["idempotent_replay"], true, "{replay}");
        assert!(replay.get("forgotten").is_none(), "{replay}");

        for handle in [&derived, &stance, &episode_derived] {
            call("core_forget", serde_json::json!({"memory": handle})).await?;
        }
        let rows = memory_rows().await?;

        let replay = call("core_derive", derive_args).await?;
        assert_eq!(replay["handle"], derived.as_str(), "derive: {replay}");
        assert_eq!(replay["idempotent_replay"], true, "derive: {replay}");
        assert_eq!(replay["forgotten"], true, "derive: {replay}");
        let replay = call("core_interpret", interpret_args).await?;
        assert_eq!(replay["handle"], stance.as_str(), "interpret: {replay}");
        assert_eq!(replay["idempotent_replay"], true, "interpret: {replay}");
        assert_eq!(replay["forgotten"], true, "interpret: {replay}");
        let replay = call("core_episode_commit", episode_args).await?;
        assert_eq!(replay["derived"], episode_derived.as_str(), "episode: {replay}");

        assert_eq!(memory_rows().await?, rows, "a replay wrote a memory row");
        let found = call(
            "core_search_memories",
            serde_json::json!({"query": "replayneedle", "mode": "lexical"}),
        )
        .await?;
        assert!(
            found["memories"].as_array().is_none_or(Vec::is_empty),
            "{found}"
        );
        for handle in [&derived, &stance, &episode_derived] {
            let read = read_test_model_resource(
                &tools,
                authz.clone(),
                owner,
                &format!("proxima://memory/{handle}"),
            )
            .await;
            assert!(read.is_err(), "{handle} stays forgotten: {read:?}");
        }
        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("derived replay after forget");
}

/// Goal evidence and a goal's target Perspective that name a forgotten
/// memory are refused exactly as an unknown id is: `entry not found`.
#[tokio::test]
async fn goal_evidence_naming_a_forgotten_memory_is_refused_as_absent() {
    let split_db = clone_split_core_db("proxima_goal_forgotten_evidence")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(split_db.runtime_url().to_owned())
            .store_number(0)
            .platform_database_url(split_db.platform_url().to_owned())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let handle = |made: serde_json::Value| -> Result<String, &'static str> {
            Ok(made["handle"].as_str().ok_or("handle")?.to_owned())
        };
        let fact = handle(
            call(
                "core_remember",
                serde_json::json!({"title": "goal fact", "body": "goal fact", "tags": []}),
            )
            .await?,
        )?;
        let derive = |title: &'static str| {
            call(
                "core_derive",
                serde_json::json!({
                    "kind": "Abstraction", "title": title, "body": title, "tags": [],
                    "source_handles": [fact.clone()], "model_id": "test-model",
                }),
            )
        };
        let evidence = handle(derive("goal evidence").await?)?;
        let gone = handle(derive("forgotten evidence").await?)?;
        let perspective = |claim: &'static str| {
            call(
                "core_interpret",
                serde_json::json!({"claim": claim, "confidence": 80, "subjects": [fact.clone()]}),
            )
        };
        let target = handle(perspective("goal target").await?)?;
        let gone_target = handle(perspective("forgotten target").await?)?;
        let set_goal = |evidence: String, target: String, key: String| {
            call(
                "core_goal",
                serde_json::json!({
                    "action": "set", "schema_id": "core/simple-text-v1",
                    "title": "goal", "text": "goal", "body": {},
                    "evidence": [evidence], "target_perspective": target,
                    "idempotency_key": key,
                }),
            )
        };
        // Control: both are accepted while live.
        set_goal(gone.clone(), gone_target.clone(), "goal-control".into()).await?;
        call("core_forget", serde_json::json!({"memory": gone})).await?;
        call("core_forget", serde_json::json!({"memory": gone_target})).await?;

        let absent = absent_local_id(&gone)?;
        let cited = set_goal(gone.clone(), target.clone(), "goal-gone-evidence".into())
            .await
            .expect_err("forgotten evidence is refused");
        let missing = set_goal(
            absent.clone(),
            target.clone(),
            "goal-absent-evidence".into(),
        )
        .await
        .expect_err("absent evidence is refused");
        assert_eq!(observed(&cited, &gone), observed(&missing, &absent));
        assert!(
            format!("{cited:?}").contains("entry not found"),
            "{cited:?}"
        );

        let absent = absent_local_id(&gone_target)?;
        let cited = set_goal(
            evidence.clone(),
            gone_target.clone(),
            "goal-gone-target".into(),
        )
        .await
        .expect_err("a forgotten target is refused");
        let missing = set_goal(
            evidence.clone(),
            absent.clone(),
            "goal-absent-target".into(),
        )
        .await
        .expect_err("an absent target is refused");
        assert_eq!(observed(&cited, &gone_target), observed(&missing, &absent));
        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("goal evidence forgotten");
}

/// #513: Goal evidence may be a Fact, so a forgotten Fact is refused on set,
/// modify and decompose exactly as an unknown id is (`entry not found`),
/// each after a live-Fact control that is accepted.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one fixture, three goal writes
async fn goal_evidence_naming_a_forgotten_fact_is_refused_as_absent() {
    let split_db = clone_split_core_db("proxima_goal_forgotten_fact")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(split_db.runtime_url().to_owned())
            .store_number(0)
            .platform_database_url(split_db.platform_url().to_owned())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let handle = |made: serde_json::Value| -> Result<String, &'static str> {
            Ok(made["handle"].as_str().ok_or("handle")?.to_owned())
        };
        let remember = |title: &'static str| {
            call(
                "core_remember",
                serde_json::json!({"title": title, "body": title, "tags": []}),
            )
        };
        let base = handle(remember("ff base").await?)?;
        let abs = handle(
            call(
                "core_derive",
                serde_json::json!({
                    "kind": "Abstraction", "title": "ff abs", "body": "ff abs", "tags": [],
                    "source_handles": [base.clone()], "model_id": "test-model",
                }),
            )
            .await?,
        )?;
        let target = handle(
            call(
                "core_interpret",
                serde_json::json!({"claim": "ff target", "confidence": 80, "subjects": [base.clone()]}),
            )
            .await?,
        )?;
        let set_goal = |evidence: String, key: &'static str| {
            call(
                "core_goal",
                serde_json::json!({
                    "action": "set", "schema_id": "core/simple-text-v1",
                    "title": key, "text": key, "body": {},
                    "evidence": [evidence], "target_perspective": target.clone(),
                    "idempotency_key": key,
                }),
            )
        };
        let modify_goal = |goal: String, evidence: String, key: &'static str| {
            call(
                "core_goal",
                serde_json::json!({
                    "action": "modify", "goal": goal, "schema_id": "core/simple-text-v1",
                    "title": key, "text": key, "body": {},
                    "evidence": [evidence], "idempotency_key": key,
                }),
            )
        };
        let decompose_goal = |parent: String, evidence: String, key: &'static str| {
            call(
                "core_goal",
                serde_json::json!({
                    "action": "decompose", "parent_goal": parent,
                    "children": [{"schema_id": "core/simple-text-v1", "title": key,
                        "text": key, "body": {}, "evidence": [evidence]}],
                    "target_perspective": target.clone(), "idempotency_key": key,
                }),
            )
        };
        let parity = |cited: proxima::CoreMcpError,
                      missing: proxima::CoreMcpError,
                      gone: &str,
                      absent: &str,
                      what: &str| {
            assert_eq!(observed(&cited, gone), observed(&missing, absent), "{what}");
            assert!(format!("{cited:?}").contains("entry not found"), "{what}: {cited:?}");
        };

        // set
        let gone = handle(remember("ff set").await?)?;
        set_goal(gone.clone(), "ff-set-control").await?;
        call("core_forget", serde_json::json!({"memory": gone})).await?;
        let absent = absent_local_id(&gone)?;
        let cited = set_goal(gone.clone(), "ff-set-gone").await.expect_err("set gone");
        let missing = set_goal(absent.clone(), "ff-set-absent").await.expect_err("set absent");
        parity(cited, missing, &gone, &absent, "set");

        // modify
        let gone = handle(remember("ff modify").await?)?;
        let head = handle(set_goal(abs.clone(), "ff-mod-head").await?)?;
        let head = handle(modify_goal(head, gone.clone(), "ff-mod-control").await?)?;
        call("core_forget", serde_json::json!({"memory": gone})).await?;
        let absent = absent_local_id(&gone)?;
        let cited = modify_goal(head.clone(), gone.clone(), "ff-mod-gone")
            .await
            .expect_err("modify gone");
        let missing = modify_goal(head.clone(), absent.clone(), "ff-mod-absent")
            .await
            .expect_err("modify absent");
        parity(cited, missing, &gone, &absent, "modify");

        // decompose
        let gone = handle(remember("ff decompose").await?)?;
        let parent = handle(set_goal(abs.clone(), "ff-dec-parent").await?)?;
        decompose_goal(parent.clone(), gone.clone(), "ff-dec-control").await?;
        call("core_forget", serde_json::json!({"memory": gone})).await?;
        let absent = absent_local_id(&gone)?;
        let cited = decompose_goal(parent.clone(), gone.clone(), "ff-dec-gone")
            .await
            .expect_err("decompose gone");
        let missing = decompose_goal(parent.clone(), absent.clone(), "ff-dec-absent")
            .await
            .expect_err("decompose absent");
        parity(cited, missing, &gone, &absent, "decompose");
        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("goal evidence forgotten Fact");
}

/// #488 review r3: a replay matches any version of its series, not only the
/// head. With v1 (sources [A]) and v2 (sources [B]) under one key, forgetting
/// v2 moves the head back to v1; replaying v2's exact write still answers
/// v2 with `forgotten: true` and writes nothing. The same holds for an
/// episode's derive. (An interpretation's handle folds in its subjects, so
/// two versions of one interpretation series always carry equal pins.)
#[tokio::test]
#[allow(clippy::too_many_lines)] // one fixture, two replaying writes
async fn replaying_a_forgotten_older_version_answers_forgotten_and_does_not_revive() {
    let split_db = clone_split_core_db("proxima_derive_replay_version")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(split_db.runtime_url().to_owned())
            .store_number(0)
            .platform_database_url(split_db.platform_url().to_owned())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let memory_rows = || async {
            let admin = sqlx::PgPool::connect(&db_url(split_db.name())).await?;
            let rows: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM proxima_core.memory WHERE schema_id <> 'core/write-act-v1'",
            )
            .fetch_one(&admin)
            .await?;
            admin.close().await;
            Ok::<i64, Box<dyn std::error::Error>>(rows)
        };
        let mut sources = Vec::new();
        for body in ["versource alpha", "versource beta"] {
            let handle = call(
                "core_remember",
                serde_json::json!({"title": body, "body": body, "tags": []}),
            )
            .await?["handle"]
                .as_str()
                .ok_or("handle")?
                .to_owned();
            sources.push(handle);
        }
        let derive = |source: &str| {
            serde_json::json!({
                "kind": "Abstraction", "title": "probevtwo secret", "body": "probevtwo secret",
                "tags": [], "source_handles": [source], "model_id": "test-model",
                "idempotency_key": "probe-version-key",
            })
        };
        let episode = |body: &str, key: &str| {
            serde_json::json!({
                "remember": [{"title": body, "body": body, "idempotency_key": key}],
                "derive": {"title": "probevepisode secret", "body": "probevepisode secret",
                    "source_handles": ["remember:0"], "model_id": "test-model",
                    "idempotency_key": "probe-episode-key"},
            })
        };
        let v1 = call("core_derive", derive(&sources[0])).await?;
        let v2 = call("core_derive", derive(&sources[1])).await?;
        let v2_handle = v2["handle"].as_str().ok_or("handle")?.to_owned();
        assert_ne!(v1["handle"], v2["handle"], "v2 is a new version: {v1} {v2}");
        let e1 = call("core_episode_commit", episode("epver one", "epver-1")).await?;
        let e2 = call("core_episode_commit", episode("epver two", "epver-2")).await?;
        let e2_derived = e2["derived"].as_str().ok_or("derived")?.to_owned();
        assert_ne!(
            e1["derived"], e2["derived"],
            "episode v2 is a new version: {e1} {e2}"
        );

        for handle in [&v2_handle, &e2_derived] {
            call("core_forget", serde_json::json!({"memory": handle})).await?;
        }
        let rows = memory_rows().await?;

        let replay = call("core_derive", derive(&sources[1])).await?;
        assert_eq!(replay["handle"], v2_handle.as_str(), "derive: {replay}");
        assert_eq!(replay["idempotent_replay"], true, "derive: {replay}");
        assert_eq!(replay["forgotten"], true, "derive: {replay}");
        let replay = call("core_episode_commit", episode("epver two", "epver-2")).await?;
        assert_eq!(replay["derived"], e2_derived.as_str(), "episode: {replay}");

        assert_eq!(memory_rows().await?, rows, "a replay wrote a memory row");
        for needle in ["probevtwo", "probevepisode"] {
            let found = call(
                "core_search_memories",
                serde_json::json!({"query": needle, "mode": "lexical"}),
            )
            .await?;
            let hits = handles(&found, "memories", "handle");
            assert!(
                !hits.contains(&v2_handle) && !hits.contains(&e2_derived),
                "{needle}: a forgotten version is searchable again: {found}"
            );
            assert!(hits.len() <= 1, "{needle}: only v1 may be found: {found}");
        }
        for handle in [&v2_handle, &e2_derived] {
            let read = read_test_model_resource(
                &tools,
                authz.clone(),
                owner,
                &format!("proxima://memory/{handle}"),
            )
            .await;
            assert!(read.is_err(), "{handle} stays forgotten: {read:?}");
        }
        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("replay of a forgotten older version");
}

/// #488 review r3: a same-key remember with a changed body after forget
/// continues the forgotten note's series, as a changed derive does. The
/// forgotten version stays history, and restoring it never leaves two live
/// series for one note key: one handle, one live head, one search hit.
#[tokio::test]
async fn a_changed_remember_after_forget_continues_the_series_and_restore_keeps_one_head() {
    let split_db = clone_split_core_db("proxima_remember_forgotten_series")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(split_db.runtime_url().to_owned())
            .store_number(0)
            .platform_database_url(split_db.platform_url().to_owned())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let note = |body: &str| {
            serde_json::json!({"title": "notex", "body": body, "tags": [],
                "idempotency_key": "notex-key"})
        };
        let series = || async {
            let admin = sqlx::PgPool::connect(&db_url(split_db.name())).await?;
            let handles: i64 = sqlx::query_scalar(
                "SELECT count(DISTINCT handle) FROM proxima_core.memory WHERE schema_id = $1",
            )
            .bind(<proxima_core::AgentNoteV1 as proxima_core::FactPayload>::SCHEMA_ID)
            .fetch_one(&admin)
            .await?;
            let live_heads: Vec<Uuid> = sqlx::query_scalar(
                "SELECT h.t FROM proxima_core.memory_head h
                   JOIN proxima_core.memory m ON m.t = h.t
                  WHERE m.schema_id = $1 AND m.forgotten_at IS NULL",
            )
            .bind(<proxima_core::AgentNoteV1 as proxima_core::FactPayload>::SCHEMA_ID)
            .fetch_all(&admin)
            .await?;
            admin.close().await;
            Ok::<(i64, Vec<Uuid>), Box<dyn std::error::Error>>((handles, live_heads))
        };
        let first = call("core_remember", note("notex notefirst")).await?;
        let first = first["handle"].as_str().ok_or("handle")?.to_owned();
        call("core_forget", serde_json::json!({"memory": first})).await?;
        let second = call("core_remember", note("notex notesecond")).await?;
        assert_eq!(second["idempotent_replay"], false, "{second}");
        let second = second["handle"].as_str().ok_or("handle")?.to_owned();
        assert_eq!(
            series().await?.0,
            1,
            "a changed remember after forget minted a new series"
        );

        call(
            "core_forget",
            serde_json::json!({"memory": first, "action": "restore"}),
        )
        .await?;
        let (handles, live_heads) = series().await?;
        assert_eq!(handles, 1, "one note key, one series");
        assert_eq!(
            live_heads,
            vec![Uuid::parse_str(&second)?],
            "restore must not move the head back or add a second live head"
        );

        let third = call("core_remember", note("notex notethird")).await?;
        let third = third["handle"].as_str().ok_or("handle")?.to_owned();
        let (handles, live_heads) = series().await?;
        assert_eq!(handles, 1, "the next write appends to the one series");
        assert_eq!(live_heads, vec![Uuid::parse_str(&third)?]);
        let found = call(
            "core_search_memories",
            serde_json::json!({"query": "notex", "mode": "lexical"}),
        )
        .await?;
        assert_eq!(handles_of(&found), vec![third.clone()], "{found}");
        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("changed remember after forget, then restore");
}

fn handles_of(found: &serde_json::Value) -> Vec<String> {
    handles(found, "memories", "memory")
}

/// #488 review r3: every replay surface says when the memory it answers is
/// forgotten, as `core_remember` and `core_derive` do: `core_record_utterance`
/// carries `forgotten: true`, and `core_episode_commit` lists the replayed
/// handles that are forgotten. (`core_upload` complete is pinned by a unit
/// test: completing an upload needs an object store.)
#[tokio::test]
async fn utterance_and_episode_replays_say_the_memory_is_forgotten() {
    let split_db = clone_split_core_db("proxima_replay_says_forgotten")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(split_db.runtime_url().to_owned())
            .store_number(0)
            .platform_database_url(split_db.platform_url().to_owned())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let utterance = serde_json::json!({"speaker": "user", "conversation_id": "conv-r3",
            "text": "utterneedle words", "idempotency_key": "utter-r3"});
        let source = call(
            "core_remember",
            serde_json::json!({"title": "episode source", "body": "episode source", "tags": []}),
        )
        .await?["handle"]
            .as_str()
            .ok_or("handle")?
            .to_owned();
        let episode = serde_json::json!({
            "derive": {"title": "episayneedle", "body": "episayneedle",
                "source_handles": [source], "model_id": "test-model",
                "idempotency_key": "episay-derive"},
        });
        let said = call("core_record_utterance", utterance.clone()).await?;
        let said_handle = said["handle"].as_str().ok_or("handle")?.to_owned();
        let committed = call("core_episode_commit", episode.clone()).await?;
        let derived = committed["derived"].as_str().ok_or("derived")?.to_owned();
        // Control: a live replay says nothing about forgetting.
        let replay = call("core_record_utterance", utterance.clone()).await?;
        assert!(replay.get("forgotten").is_none(), "{replay}");
        let replay = call("core_episode_commit", episode.clone()).await?;
        assert!(replay.get("forgotten").is_none(), "{replay}");

        for handle in [&said_handle, &derived] {
            call("core_forget", serde_json::json!({"memory": handle})).await?;
        }
        let replay = call("core_record_utterance", utterance).await?;
        assert_eq!(replay["handle"], said_handle.as_str(), "{replay}");
        assert_eq!(replay["idempotent_replay"], true, "{replay}");
        assert_eq!(replay["forgotten"], true, "{replay}");
        let replay = call("core_episode_commit", episode).await?;
        assert_eq!(replay["derived"], derived.as_str(), "{replay}");
        assert_eq!(
            replay["forgotten"],
            serde_json::json!([derived]),
            "{replay}"
        );
        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("replays say forgotten");
}

/// #488 review r3: an episode whose remembered Fact was forgotten answers
/// its identical replay as a replay (the remember slot answers forgotten and
/// the derive built on it replays), instead of refusing its own derive. A
/// new derive over that slot is still refused.
#[tokio::test]
async fn an_episode_replay_over_a_forgotten_remembered_fact_answers_as_a_replay() {
    let split_db = clone_split_core_db("proxima_episode_forgotten_slot")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(split_db.runtime_url().to_owned())
            .store_number(0)
            .platform_database_url(split_db.platform_url().to_owned())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let episode = serde_json::json!({
            "remember": [{"title": "epslot fact", "body": "epslot fact", "idempotency_key": "ep-f"}],
            "derive": {"title": "epslot derived", "body": "epslot derived",
                "source_handles": ["remember:0"], "model_id": "test-model",
                "idempotency_key": "ep-d"},
        });
        let committed = call("core_episode_commit", episode.clone()).await?;
        let fact = committed["remembered"][0].as_str().ok_or("fact")?.to_owned();
        let derived = committed["derived"].as_str().ok_or("derived")?.to_owned();
        let standalone_args = serde_json::json!({
            "kind": "Abstraction", "title": "epslot standalone", "body": "epslot standalone",
            "tags": [], "source_handles": [fact.clone()], "model_id": "test-model",
        });
        let standalone = call("core_derive", standalone_args.clone()).await?;
        call("core_forget", serde_json::json!({"memory": fact})).await?;

        let replay = call("core_episode_commit", episode.clone()).await?;
        assert_eq!(replay["remembered"], serde_json::json!([fact]), "{replay}");
        assert_eq!(replay["derived"], derived.as_str(), "{replay}");
        assert_eq!(replay["forgotten"], serde_json::json!([fact]), "{replay}");
        // A standalone derive names its source by a caller-supplied id, and
        // a forgotten id reads as absent there (#488 r1), replay or not. The
        // episode's slot is its own item, not a caller-supplied id.
        let refused = call("core_derive", standalone_args)
            .await
            .expect_err("a forgotten source id reads as absent");
        assert!(refused.to_string().contains("entry not found"), "{refused}");
        assert!(standalone["handle"].is_string());

        // A new derive over the forgotten Fact is still refused.
        let fresh = serde_json::json!({
            "remember": [{"title": "epslot fact", "body": "epslot fact", "idempotency_key": "ep-f"}],
            "derive": {"title": "epslot other", "body": "epslot other",
                "source_handles": ["remember:0"], "model_id": "test-model",
                "idempotency_key": "ep-d-other"},
        });
        let refused = call("core_episode_commit", fresh).await;
        assert!(refused.is_err(), "a new derive cited a forgotten Fact: {refused:?}");
        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("episode replay over a forgotten remembered Fact");
}

/// #488 review r4: a slot that replayed a forgotten memory answers only its
/// own replay. A NEW stance in the same episode naming that slot
/// (`remember:N`, `derive` or `stance:N`) is refused as `entry not found`
/// and no live row comes to pin the forgotten memory. An identical replay of
/// an episode whose stance subject was forgotten afterwards still answers.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one fixture, four episodes
async fn a_new_stance_on_a_forgotten_replay_slot_is_refused() {
    let split_db = clone_split_core_db("proxima_episode_forgotten_stance")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(split_db.runtime_url().to_owned())
            .store_number(0)
            .platform_database_url(split_db.platform_url().to_owned())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let live_pins = |target: String| {
            let url = db_url(split_db.name());
            async move {
                let admin = sqlx::PgPool::connect(&url).await?;
                let rows: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM proxima_core.memory
                      WHERE forgotten_at IS NULL
                        AND ($1 = ANY(refs) OR $1 = ANY(origins) OR $1 = ANY(goal_refs))",
                )
                .bind(Uuid::parse_str(&target)?)
                .fetch_one(&admin)
                .await?;
                admin.close().await;
                Ok::<i64, Box<dyn std::error::Error>>(rows)
            }
        };
        let remember = |key: &str| serde_json::json!({"title": key, "body": key, "idempotency_key": key});
        let derive = |key: &str| {
            serde_json::json!({"title": key, "body": key, "source_handles": ["remember:0"],
                "model_id": "test-model", "idempotency_key": key})
        };
        let stance = |claim: &str, subject: &str| {
            serde_json::json!({"claim": claim, "confidence": 70, "subjects": [subject],
                "model_id": "test-model"})
        };
        let refused = |result: Result<serde_json::Value, proxima::CoreMcpError>, what: &str| {
            match result {
                Ok(value) => panic!("{what}: a new stance pinned a forgotten slot: {value}"),
                Err(err) => assert!(err.to_string().contains("entry not found"), "{what}: {err}"),
            }
        };

        // remember:N
        let first = call("core_episode_commit", serde_json::json!({"remember": [remember("r4-f")]})).await?;
        let fact = first["remembered"][0].as_str().ok_or("fact")?.to_owned();
        call("core_forget", serde_json::json!({"memory": fact})).await?;
        refused(
            call("core_episode_commit", serde_json::json!({
                "remember": [remember("r4-f")],
                "stance": [stance("rfour new claim fact", "remember:0")],
            }))
            .await,
            "remember:0",
        );
        assert_eq!(live_pins(fact.clone()).await?, 0, "a live row pins the forgotten Fact");

        // derive
        let first = call("core_episode_commit", serde_json::json!({
            "remember": [remember("r4-b")], "derive": derive("r4-d"),
        }))
        .await?;
        let derived = first["derived"].as_str().ok_or("derived")?.to_owned();
        call("core_forget", serde_json::json!({"memory": derived})).await?;
        refused(
            call("core_episode_commit", serde_json::json!({
                "remember": [remember("r4-b")], "derive": derive("r4-d"),
                "stance": [stance("rfour new claim derive", "derive")],
            }))
            .await,
            "derive",
        );
        assert_eq!(live_pins(derived.clone()).await?, 0, "a live row pins the forgotten derive");

        // stance:N
        let first = call("core_episode_commit", serde_json::json!({
            "remember": [remember("r4-c")], "stance": [stance("rfour old stance", "remember:0")],
        }))
        .await?;
        let old_stance = first["stances"][0].as_str().ok_or("stance")?.to_owned();
        call("core_forget", serde_json::json!({"memory": old_stance})).await?;
        refused(
            call("core_episode_commit", serde_json::json!({
                "remember": [remember("r4-c")],
                "stance": [stance("rfour old stance", "remember:0"),
                    stance("rfour new claim stance", "stance:0")],
            }))
            .await,
            "stance:0",
        );
        assert_eq!(live_pins(old_stance.clone()).await?, 0, "a live row pins the forgotten stance");

        // An identical replay whose stance subject was forgotten afterwards
        // still answers, with the forgotten Fact listed.
        let episode = serde_json::json!({
            "remember": [remember("r4-e")], "stance": [stance("rfour replayed stance", "remember:0")],
        });
        let first = call("core_episode_commit", episode.clone()).await?;
        let fact = first["remembered"][0].as_str().ok_or("fact")?.to_owned();
        call("core_forget", serde_json::json!({"memory": fact})).await?;
        let replay = call("core_episode_commit", episode).await?;
        assert_eq!(replay["stances"], first["stances"], "{replay}");
        assert_eq!(replay["forgotten"], serde_json::json!([fact]), "{replay}");
        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("new stance on a forgotten replay slot");
}

/// #488 follow-up: a new Goal in an episode whose evidence (`derive`) or
/// target (`stance:N`) names a slot that replayed a forgotten memory is
/// refused like a new stance or derive on that slot: `entry not found`, not
/// "evidence does not exist". No live row comes to pin the forgotten memory.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one fixture, three slot kinds and controls
async fn a_new_goal_on_a_forgotten_replay_slot_says_entry_not_found() {
    let split_db = clone_split_core_db("proxima_episode_forgotten_goal")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(split_db.runtime_url().to_owned())
            .store_number(0)
            .platform_database_url(split_db.platform_url().to_owned())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        let call = |tool: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), owner, tool, args)
        };
        let live_pins = |target: String| {
            let url = db_url(split_db.name());
            async move {
                let admin = sqlx::PgPool::connect(&url).await?;
                let rows: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM proxima_core.memory
                      WHERE forgotten_at IS NULL
                        AND ($1 = ANY(refs) OR $1 = ANY(origins) OR $1 = ANY(goal_refs))",
                )
                .bind(Uuid::parse_str(&target)?)
                .fetch_one(&admin)
                .await?;
                admin.close().await;
                Ok::<i64, Box<dyn std::error::Error>>(rows)
            }
        };
        let base = |tag: &str| {
            serde_json::json!({
                "remember": [{"title": tag, "body": tag, "idempotency_key": format!("{tag}-f")}],
                "derive": {"title": format!("{tag} derived"), "body": format!("{tag} derived"),
                    "source_handles": ["remember:0"], "model_id": "test-model",
                    "idempotency_key": format!("{tag}-d")},
                "stance": [{"claim": format!("{tag} stance"), "confidence": 70,
                    "subjects": ["remember:0"], "model_id": "test-model"}],
            })
        };
        let with_goal = |tag: &str| {
            let mut episode = base(tag);
            episode["goal"] = serde_json::json!([{
                "schema_id": "core/simple-text-v1", "title": "goal", "text": "goal", "body": {},
                "evidence": ["derive"], "target_perspective": "stance:0",
                "idempotency_key": format!("{tag}-g"),
            }]);
            episode
        };
        let refused =
            |result: Result<serde_json::Value, proxima::CoreMcpError>, what: &str| match result {
                Ok(value) => panic!("{what}: a new goal pinned a forgotten slot: {value}"),
                Err(err) => assert!(err.to_string().contains("entry not found"), "{what}: {err}"),
            };

        // Evidence `derive` replays a forgotten Abstraction.
        let first = call("core_episode_commit", base("fu-ev")).await?;
        let derived = first["derived"].as_str().ok_or("derived")?.to_owned();
        call("core_forget", serde_json::json!({"memory": derived})).await?;
        refused(
            call("core_episode_commit", with_goal("fu-ev")).await,
            "evidence derive",
        );
        assert_eq!(
            live_pins(derived).await?,
            0,
            "a live row pins the forgotten derive"
        );

        // Target `stance:0` replays a forgotten Perspective.
        let first = call("core_episode_commit", base("fu-tg")).await?;
        let stance = first["stances"][0].as_str().ok_or("stance")?.to_owned();
        call("core_forget", serde_json::json!({"memory": stance})).await?;
        refused(
            call("core_episode_commit", with_goal("fu-tg")).await,
            "target stance:0",
        );
        assert_eq!(
            live_pins(stance).await?,
            0,
            "a live row pins the forgotten stance"
        );

        // #513: evidence `remember:0` replays a forgotten Fact.
        let with_goal_on = |tag: &str, evidence: &str| {
            let mut episode = with_goal(tag);
            episode["goal"][0]["evidence"] = serde_json::json!([evidence]);
            episode
        };
        let first = call("core_episode_commit", base("fu-rf")).await?;
        let fact = first["remembered"][0].as_str().ok_or("fact")?.to_owned();
        call("core_forget", serde_json::json!({"memory": fact})).await?;
        // The base episode's own derive and stance were committed over the
        // Fact before it was forgotten; the refused goal adds no pin.
        let pins_before = live_pins(fact.clone()).await?;
        refused(
            call("core_episode_commit", with_goal_on("fu-rf", "remember:0")).await,
            "evidence remember:0",
        );
        assert_eq!(
            live_pins(fact.clone()).await?,
            pins_before,
            "the refused goal pinned the forgotten Fact"
        );
        let admin = sqlx::PgPool::connect(&db_url(split_db.name())).await?;
        let goal_pins: i64 =
            sqlx::query_scalar("SELECT count(*) FROM proxima_core.goal WHERE $1 = ANY(evidence_t)")
                .bind(Uuid::parse_str(&fact)?)
                .fetch_one(&admin)
                .await?;
        admin.close().await;
        assert_eq!(goal_pins, 0, "a goal row cites the forgotten Fact");

        // Control: the same goal over live slots is accepted.
        let made = call("core_episode_commit", with_goal("fu-ok")).await?;
        assert_eq!(made["goals"].as_array().map(Vec::len), Some(1), "{made}");
        let made = call(
            "core_episode_commit",
            with_goal_on("fu-rf-ok", "remember:0"),
        )
        .await?;
        assert_eq!(made["goals"].as_array().map(Vec::len), Some(1), "{made}");
        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("new goal on a forgotten replay slot");
}
