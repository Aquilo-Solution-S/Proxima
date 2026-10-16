//! Per-Owner embedding routing through the facade: each Owner embeds and
//! searches through its own route, a route failure stays with its owner, and
//! an Owner moves to a new model without a search gap.

use super::*;

/// Holds the facade's embedding worker for a whole test, so the test's own
/// drains are the only ones and each drain's count is exact.
///
/// A configured router starts the worker in `build()`. It reconciles before
/// its first drain, and reconcile routes every registered Owner before it
/// queues anything. [`Self::boot`] registers an Owner before `build()`, so
/// the worker's first route call always comes from that reconcile; it waits
/// here until [`Self::release`]. The test's own calls run in `block_on`,
/// outside any task, and pass straight through.
#[derive(Debug)]
struct HeldWorker {
    inner: Arc<dyn proxima_core::llm::EmbeddingRouter>,
    held: std::sync::atomic::AtomicBool,
    parked: tokio::sync::Notify,
    released: tokio::sync::Notify,
}

impl HeldWorker {
    /// Registers `owner`, boots through `start`, and returns once the worker
    /// is parked.
    async fn boot<F, Fut>(
        db_name: &str,
        owner: OwnerId,
        inner: Arc<dyn proxima_core::llm::EmbeddingRouter>,
        start: F,
    ) -> Result<(proxima::BuiltProxima, Arc<Self>), Box<dyn std::error::Error>>
    where
        F: FnOnce(Arc<dyn proxima_core::llm::EmbeddingRouter>) -> Fut,
        Fut: std::future::Future<Output = Result<proxima::BuiltProxima, proxima::ProximaError>>,
    {
        let admin = sqlx::PgPool::connect(&db_url(db_name)).await?;
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal'::proxima_core.owner_kind)",
        )
        .bind(owner.into_inner())
        .execute(&admin)
        .await?;
        admin.close().await;
        let held = Arc::new(Self {
            inner,
            held: std::sync::atomic::AtomicBool::new(false),
            parked: tokio::sync::Notify::new(),
            released: tokio::sync::Notify::new(),
        });
        let built = start(held.clone()).await?;
        tokio::time::timeout(std::time::Duration::from_mins(1), held.parked.notified())
            .await
            .map_err(|_| "the embedding worker never reconciled")?;
        Ok((built, held))
    }

    /// Lets the worker go; `shutdown()` joins it.
    fn release(&self) {
        self.released.notify_one();
    }
}

#[async_trait::async_trait]
impl proxima_core::llm::EmbeddingRouter for HeldWorker {
    async fn route(
        &self,
        owner: &OwnerId,
    ) -> Result<proxima_core::llm::EmbeddingRoute, proxima_core::llm::EmbeddingRouteError> {
        if tokio::task::try_id().is_some()
            && !self.held.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.parked.notify_one();
            self.released.notified().await;
        }
        self.inner.route(owner).await
    }
}

/// A client at a non-default width whose vectors separate two topics, so a
/// semantic search ranks correctly only by reading its own width lane.
#[derive(Debug)]
struct LaneEmbedding(proxima_core::llm::EmbeddingDim);

#[async_trait::async_trait]
impl proxima_core::llm::EmbeddingClient for LaneEmbedding {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, proxima_core::llm::LlmError> {
        let mut vector = vec![0.0; self.0.width()];
        vector[usize::from(!text.contains("alpha"))] = 1.0;
        Ok(vector)
    }

    fn model_id(&self) -> &'static str {
        "lane-embed"
    }

    fn dim(&self) -> usize {
        self.0.width()
    }
}

/// One `vector` lane and one `halfvec` lane, each end to end through the
/// facade: `core_remember` queues jobs in the client's space, the drain
/// stores vectors at that width, and `core_search_memories` embeds the query
/// in the same space and ranks through that lane.
#[tokio::test]
async fn facade_embeds_and_searches_at_a_non_default_width() {
    for dim in [
        proxima_core::llm::EmbeddingDim::D768,
        proxima_core::llm::EmbeddingDim::D3072,
    ] {
        let split_db = clone_split_core_db("proxima_core_width_lane")
            .await
            .expect("PG required for tests");
        let db_name = split_db.name().to_owned();
        let (runtime_url, platform_url) = (
            split_db.runtime_url().to_owned(),
            split_db.platform_url().to_owned(),
        );

        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = company_owner(Uuid::now_v7());
            let (built, held) = HeldWorker::boot(
                &db_name,
                owner,
                Arc::new(proxima_core::llm::SingleClientRouter::bind(Arc::new(
                    LaneEmbedding(dim),
                ))?),
                |router| {
                    Proxima::<AgentMemoryApp>::app()
                        .database_url(runtime_url.clone())
                        .platform_database_url(platform_url.clone())
                        .owner(owner)
                        .embedding_router(router)
                        .tool_scope(ToolScope::All)
                        .build()
                },
            )
            .await?;
            let tools = built.host().core_mcp_tools();
            let authz = host_authz(&owner, ToolScope::All);

            let mut handles = Vec::new();
            for topic in ["alpha", "beta"] {
                let remembered = call_test_model_tool(
                    &tools,
                    authz.clone(),
                    owner,
                    "core_remember",
                    serde_json::json!({
                        "title": format!("{topic} lane fact"),
                        "body": format!("{topic} survey notes"),
                        "idempotency_key": format!("width-lane-{topic}")
                    }),
                )
                .await?;
                handles.push(remembered["handle"].as_str().expect("handle").to_owned());
            }

            let drained = built.host().engine().drain_embedding_jobs(10).await?;
            assert_eq!((drained.processed, drained.failed), (2, 0), "{dim}");
            let admin_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
            let stored: Vec<(String, i16, i32)> = sqlx::query_as(
                "SELECT model_id, dim, vector_dims(vec)
                   FROM proxima_core.embeddings ORDER BY entity_id",
            )
            .fetch_all(&admin_pool)
            .await?;
            let width = i16::try_from(dim.width())?;
            assert_eq!(
                stored,
                vec![("lane-embed".to_owned(), width, i32::from(width)); 2],
                "vectors land in the client's space"
            );

            for (query, expected) in [("alpha", &handles[0]), ("beta", &handles[1])] {
                let found = call_test_model_tool(
                    &tools,
                    authz.clone(),
                    owner,
                    "core_search_memories",
                    serde_json::json!({
                        "query": query, "mode": "semantic", "kind": "Fact", "limit": 5
                    }),
                )
                .await?;
                assert_eq!(found["memories"][0]["memory"], **expected, "{dim}: {query}");
                assert_eq!(found["memories"].as_array().map(Vec::len), Some(2));
            }

            held.release();
            built.shutdown().await;
            Ok(())
        }
        .await;

        result.unwrap_or_else(|err| panic!("width lane {dim} end to end failed: {err}"));
    }
}

/// Records every text it embeds. Every vector carries axis 0; a text that
/// says `far` also carries axis 1, so a `needle` query scores `near` rows
/// above `far` ones.
#[derive(Debug)]
struct RecordingRouteEmbedding {
    model: &'static str,
    dim: proxima_core::llm::EmbeddingDim,
    texts: std::sync::Mutex<Vec<String>>,
}

impl RecordingRouteEmbedding {
    fn new(model: &'static str, dim: proxima_core::llm::EmbeddingDim) -> Arc<Self> {
        Arc::new(Self {
            model,
            dim,
            texts: std::sync::Mutex::default(),
        })
    }

    fn texts(&self) -> Vec<String> {
        self.texts.lock().expect("recorder lock").clone()
    }

    fn bound(self: &Arc<Self>) -> proxima_core::llm::BoundEmbeddingClient {
        proxima_core::llm::BoundEmbeddingClient::bind(self.clone()).expect("lane width")
    }
}

#[async_trait::async_trait]
impl proxima_core::llm::EmbeddingClient for RecordingRouteEmbedding {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, proxima_core::llm::LlmError> {
        self.texts
            .lock()
            .expect("recorder lock")
            .push(text.to_owned());
        let mut vector = vec![0.0; self.dim.width()];
        vector[0] = 1.0;
        if text.contains("far") {
            vector[1] = 1.0;
        }
        Ok(vector)
    }

    fn model_id(&self) -> &str {
        self.model
    }

    fn dim(&self) -> usize {
        self.dim.width()
    }
}

/// Memory search and recall embed their query in the route's default
/// instruction, not a flavor's; the Fact they find was embedded as it is
/// (#354).
#[tokio::test]
async fn memory_search_embeds_its_query_in_the_default_instruction() {
    use proxima_core::llm::{QueryInstruction, QueryInstructions, QueryTask, SingleClientRouter};

    let split_db = clone_split_core_db("proxima_core_query_instruction")
        .await
        .expect("PG required for tests");
    let db_name = split_db.name().to_owned();
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let recorder = RecordingRouteEmbedding::new(
            "instructed-embed",
            proxima_core::llm::EmbeddingDim::D1024,
        );
        let instructions = QueryInstructions::default()
            .with_instruction(QueryTask::DEFAULT, QueryInstruction::new("note: {query}")?)
            .with_instruction(
                QueryTask::named("code"),
                QueryInstruction::new("code: {query}")?,
            );
        let (built, held) = HeldWorker::boot(
            &db_name,
            owner,
            Arc::new(SingleClientRouter::new(
                recorder.bound().with_query_instructions(instructions),
            )),
            |router| {
                Proxima::<AgentMemoryApp>::app()
                    .database_url(runtime_url.clone())
                    .platform_database_url(platform_url.clone())
                    .owner(owner)
                    .embedding_router(router)
                    .tool_scope(ToolScope::All)
                    .build()
            },
        )
        .await?;
        let tools = built.host().core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "needle fact",
                "body": "needle survey notes",
                "idempotency_key": "query-instruction-needle"
            }),
        )
        .await?;
        built.host().engine().drain_embedding_jobs(10).await?;
        let stored = recorder.texts();
        assert!(
            !stored.is_empty() && stored.iter().all(|text| !text.starts_with("note: ")),
            "a Fact embeds as it is: {stored:?}"
        );

        let found = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_search_memories",
            serde_json::json!({ "query": "needle", "mode": "semantic", "kind": "Fact" }),
        )
        .await?;
        assert_eq!(
            found["memories"][0]["memory"], remembered["handle"],
            "{found}"
        );
        call_test_model_tool(
            &tools,
            authz,
            owner,
            "core_recall",
            serde_json::json!({ "question": "needle" }),
        )
        .await?;
        assert_eq!(
            recorder.texts()[stored.len()..],
            ["note: needle", "note: needle"]
        );

        held.release();
        built.shutdown().await;
        Ok(())
    }
    .await;

    result.unwrap_or_else(|err| panic!("query instruction end to end failed: {err}"));
}

/// A caller with a personal space and a group space, each its own data
/// owner, served by one runtime whose router the test rewires.
struct TwoOwnerFixture {
    built: proxima::BuiltProxima,
    tools: CoreMcpTools,
    authz: AuthzContext,
    personal: OwnerId,
    shared: OwnerId,
    shared_space: String,
    router: Arc<proxima_core::test_fixtures::TestEmbeddingRouter>,
    held: Arc<HeldWorker>,
    admin_pool: sqlx::PgPool,
}

impl TwoOwnerFixture {
    async fn boot(db_name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Self::boot_with_policy(db_name, proxima_core::EmbeddingRuntimePolicy::default()).await
    }

    async fn boot_with_policy(
        db_name: &str,
        policy: proxima_core::EmbeddingRuntimePolicy,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (runtime_url, platform_url) = split_role_urls(db_name).await?;
        let personal = OwnerId::from(UserId::new(Uuid::now_v7()));
        let shared = OwnerId::from(GroupId::new(Uuid::now_v7()));
        let router = Arc::new(proxima_core::test_fixtures::TestEmbeddingRouter::default());
        let (built, held) = HeldWorker::boot(db_name, personal, router.clone(), |router| {
            Proxima::<AgentMemoryApp>::app()
                .database_url(runtime_url)
                .platform_database_url(platform_url)
                .owner(personal)
                .embedding_router(router)
                .embedding_runtime_policy(policy)
                .tool_scope(ToolScope::All)
                .build()
        })
        .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(personal, vec![personal, shared], Role::admin());
        let shared_space =
            server_issued_group_space_selector(&tools, authz.clone(), personal).await;
        let admin_pool = sqlx::PgPool::connect(&db_url(db_name)).await?;
        Ok(Self {
            built,
            tools,
            authz,
            personal,
            shared,
            shared_space,
            router,
            held,
            admin_pool,
        })
    }

    async fn shutdown(self) {
        self.held.release();
        self.built.shutdown().await;
    }

    async fn remember(&self, space: &str, body: &str) -> Result<String, CoreMcpError> {
        let remembered = call_test_model_tool(
            &self.tools,
            self.authz.clone(),
            self.personal,
            "core_remember",
            serde_json::json!({"space": space, "title": "routed note", "body": body}),
        )
        .await?;
        Ok(remembered["handle"].as_str().expect("handle").to_owned())
    }

    async fn search(
        &self,
        mode: &str,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<serde_json::Value, CoreMcpError> {
        let mut args = serde_json::json!({
            "query": "needle", "mode": mode, "kind": "Fact", "limit": limit,
            "spaces": ["current", self.shared_space],
        });
        if let Some(cursor) = cursor {
            args["cursor"] = serde_json::json!(cursor);
        }
        call_test_model_tool(
            &self.tools,
            self.authz.clone(),
            self.personal,
            "core_search_memories",
            args,
        )
        .await
    }

    /// Every page of a search, in order.
    async fn search_all_pages(
        &self,
        mode: &str,
        limit: u32,
    ) -> Result<(Vec<String>, Vec<serde_json::Value>), Box<dyn std::error::Error>> {
        let mut handles = Vec::new();
        let mut pages = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..16 {
            let page = self.search(mode, limit, cursor.as_deref()).await?;
            handles.extend(
                page["memories"]
                    .as_array()
                    .expect("memories")
                    .iter()
                    .map(|memory| memory["memory"].as_str().expect("handle").to_owned()),
            );
            cursor = page["next_cursor"].as_str().map(str::to_owned);
            pages.push(page);
            if cursor.is_none() {
                return Ok((handles, pages));
            }
        }
        Err("search did not finish paging".into())
    }

    /// `(model_id, dim)` of stored chunks or a published head for `handle`.
    async fn stored_space(
        &self,
        handle: &str,
    ) -> Result<Option<(String, i16)>, Box<dyn std::error::Error>> {
        let memory_id = handle
            .strip_prefix("F:")
            .ok_or("fact handle")?
            .parse::<Uuid>()?;
        Ok(sqlx::query_as(
            "SELECT model_id, dim FROM proxima_core.embeddings WHERE entity_id = $1
                 UNION
                 SELECT model_id, dim FROM proxima_core.embedding_heads WHERE entity_id = $1",
        )
        .bind(memory_id)
        .fetch_optional(&self.admin_pool)
        .await?)
    }

    async fn job_statuses(&self, handle: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        Ok(self
            .jobs(handle)
            .await?
            .into_iter()
            .map(|(status, _)| status)
            .collect())
    }

    async fn jobs(
        &self,
        handle: &str,
    ) -> Result<Vec<(String, Option<String>)>, Box<dyn std::error::Error>> {
        let memory_id = handle
            .strip_prefix("F:")
            .ok_or("fact handle")?
            .parse::<Uuid>()?;
        Ok(sqlx::query_as(
            "SELECT status::text, last_error FROM proxima_core.embedding_jobs WHERE entity_id = $1",
        )
        .bind(memory_id)
        .fetch_all(&self.admin_pool)
        .await?)
    }
}

/// Two data Owners on two models: each Owner's texts and queries reach only
/// its own endpoint, its vectors land in its own space, and a search over
/// both interleaves them by rank — scores from two models are not one scale
/// — while paging through every row exactly once.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn each_owner_embeds_and_searches_through_its_own_route() {
    use proxima_core::llm::EmbeddingDim;
    let split_db = clone_split_core_db("proxima_core_route_owners")
        .await
        .expect("PG required for tests");
    let db_name = split_db.name().to_owned();
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fixture = TwoOwnerFixture::boot(&db_name).await?;
        let personal_client = RecordingRouteEmbedding::new("route-a", EmbeddingDim::D768);
        let shared_client = RecordingRouteEmbedding::new("route-b", EmbeddingDim::D1024);
        fixture
            .router
            .set_owner(fixture.personal, Some(personal_client.bound()));
        fixture
            .router
            .set_owner(fixture.shared, Some(shared_client.bound()));

        let personal_near = fixture.remember("current", "personal needle near").await?;
        let personal_far = fixture.remember("current", "personal needle far").await?;
        let shared_near = fixture
            .remember(&fixture.shared_space, "shared needle near")
            .await?;
        let shared_far = fixture
            .remember(&fixture.shared_space, "shared needle far")
            .await?;
        let drained = fixture
            .built
            .host()
            .engine()
            .drain_embedding_jobs(10)
            .await?;
        assert_eq!((drained.processed, drained.failed), (4, 0));

        let personal_texts = personal_client.texts();
        let shared_texts = shared_client.texts();
        assert_eq!(personal_texts.len(), 2, "{personal_texts:?}");
        assert_eq!(shared_texts.len(), 2, "{shared_texts:?}");
        assert!(personal_texts.iter().all(|text| text.contains("personal")));
        assert!(shared_texts.iter().all(|text| text.contains("shared")));
        for handle in [&personal_near, &personal_far] {
            assert_eq!(
                fixture.stored_space(handle).await?,
                Some(("route-a".to_owned(), 768))
            );
        }
        for handle in [&shared_near, &shared_far] {
            assert_eq!(
                fixture.stored_space(handle).await?,
                Some(("route-b".to_owned(), 1024))
            );
        }

        let whole = fixture.search("semantic", 10, None).await?;
        assert_eq!(whole["ranking"], "rank", "{whole}");
        let (visited, pages) = fixture.search_all_pages("semantic", 1).await?;
        // Rank 1 of each list, then rank 2; ties go to the later memory.
        let expected = vec![
            shared_near.clone(),
            personal_near.clone(),
            shared_far.clone(),
            personal_far.clone(),
        ];
        assert_eq!(visited, expected, "one row per page, every row once");
        assert!(pages.iter().all(|page| page["ranking"] == "rank"));
        let whole_handles: Vec<&str> = whole["memories"]
            .as_array()
            .expect("memories")
            .iter()
            .map(|memory| memory["memory"].as_str().expect("handle"))
            .collect();
        assert_eq!(whole_handles, expected, "paging matches the one-page order");

        let queries = |texts: Vec<String>| texts.iter().filter(|text| *text == "needle").count();
        assert_eq!(queries(personal_client.texts()), 1 + pages.len());
        assert_eq!(queries(shared_client.texts()), 1 + pages.len());
        assert!(
            personal_client
                .texts()
                .iter()
                .all(|text| !text.contains("shared")),
            "no shared text reaches the personal endpoint"
        );
        assert!(
            shared_client
                .texts()
                .iter()
                .all(|text| !text.contains("personal")),
            "no personal text reaches the shared endpoint"
        );

        fixture.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("per-owner routing test failed");
}

/// One Owner's refused route refuses that Owner's writes and parks its
/// queued jobs, while the other Owner keeps writing and draining. An Owner
/// routed to no client writes without queuing anything.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_route_failure_stays_with_its_owner() {
    use proxima_core::llm::EmbeddingDim;
    let split_db = clone_split_core_db("proxima_core_route_refusal")
        .await
        .expect("PG required for tests");
    let db_name = split_db.name().to_owned();
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fixture = TwoOwnerFixture::boot(&db_name).await?;
        let personal_client = RecordingRouteEmbedding::new("route-a", EmbeddingDim::D768);
        let shared_client = RecordingRouteEmbedding::new("route-b", EmbeddingDim::D1024);
        fixture
            .router
            .set_owner(fixture.personal, Some(personal_client.bound()));
        fixture
            .router
            .set_owner(fixture.shared, Some(shared_client.bound()));
        let queued = fixture
            .remember("current", "personal needle queued")
            .await?;

        fixture
            .router
            .refuse(fixture.personal, "no key for this owner");
        assert!(
            fixture
                .remember("current", "personal needle refused")
                .await
                .is_err(),
            "a write the route refuses must fail, not land unsearchable"
        );
        let shared = fixture
            .remember(&fixture.shared_space, "shared needle routed")
            .await?;
        let drained = fixture
            .built
            .host()
            .engine()
            .drain_embedding_jobs(10)
            .await?;
        assert_eq!((drained.processed, drained.failed), (1, 0));
        assert!(personal_client.texts().is_empty());
        assert_eq!(shared_client.texts().len(), 1);
        assert_eq!(fixture.job_statuses(&queued).await?, vec!["pending"]);
        assert_eq!(fixture.stored_space(&queued).await?, None);
        assert_eq!(
            fixture.stored_space(&shared).await?,
            Some(("route-b".to_owned(), 1024))
        );

        let hybrid = fixture.search("hybrid", 10, None).await?;
        assert_eq!(hybrid["degraded_to_lexical"], true, "{hybrid}");
        assert_eq!(hybrid["ranking"], "rank", "{hybrid}");
        assert_eq!(hybrid["memories"].as_array().map(Vec::len), Some(2));
        let semantic = fixture.search("semantic", 10, None).await;
        assert!(
            matches!(&semantic, Err(CoreMcpError::Tool { message, .. })
                if message.contains("no embedding client is configured for space current")),
            "{semantic:?}"
        );
        assert!(
            personal_client.texts().is_empty(),
            "a refused route sends nothing"
        );

        fixture.router.set_owner(fixture.personal, None);
        let unrouted = fixture
            .remember("current", "personal needle unrouted")
            .await?;
        assert!(fixture.job_statuses(&unrouted).await?.is_empty());

        fixture.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("route refusal isolation test failed");
}

/// Owners that share one client share one scale: the page merges by score,
/// as before routing, and the query is embedded once for both.
#[tokio::test]
async fn one_client_across_owners_ranks_by_score() {
    use proxima_core::llm::EmbeddingDim;
    let split_db = clone_split_core_db("proxima_core_route_shared")
        .await
        .expect("PG required for tests");
    let db_name = split_db.name().to_owned();
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fixture = TwoOwnerFixture::boot(&db_name).await?;
        let client = RecordingRouteEmbedding::new("route-a", EmbeddingDim::D1024);
        fixture.router.set_default(client.bound());

        let first = fixture.remember("current", "personal needle near").await?;
        let second = fixture
            .remember("current", "personal needle near again")
            .await?;
        let far = fixture
            .remember(&fixture.shared_space, "shared needle far")
            .await?;
        let drained = fixture
            .built
            .host()
            .engine()
            .drain_embedding_jobs(10)
            .await?;
        assert_eq!((drained.processed, drained.failed), (3, 0));

        let page = fixture.search("semantic", 10, None).await?;
        assert_eq!(page["ranking"], "score", "{page}");
        let handles: Vec<&str> = page["memories"]
            .as_array()
            .expect("memories")
            .iter()
            .map(|memory| memory["memory"].as_str().expect("handle"))
            .collect();
        // By rank this would be [second, far, first]; by score both near
        // rows outrank the far one.
        assert_eq!(handles, [second.as_str(), first.as_str(), far.as_str()]);
        let queries = client
            .texts()
            .iter()
            .filter(|text| *text == "needle")
            .count();
        assert_eq!(queries, 1, "one shared client embeds the query once");

        fixture.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("shared-client ranking test failed");
}

impl TwoOwnerFixture {
    /// A search over the personal space only.
    async fn search_personal(&self, mode: &str) -> Result<serde_json::Value, CoreMcpError> {
        call_test_model_tool(
            &self.tools,
            self.authz.clone(),
            self.personal,
            "core_search_memories",
            serde_json::json!({
                "query": "needle", "mode": mode, "limit": 10, "spaces": ["current"],
            }),
        )
        .await
    }

    /// `(model_id, dim)` of every vector and every job for `handle`.
    #[allow(clippy::type_complexity)]
    async fn embedding_rows(
        &self,
        handle: &str,
    ) -> Result<(Vec<(String, i16)>, Vec<(String, i16)>), Box<dyn std::error::Error>> {
        let memory_id = handle
            .split_once(':')
            .ok_or("typed handle")?
            .1
            .parse::<Uuid>()?;
        let vectors = sqlx::query_as(
            "SELECT model_id, dim FROM proxima_core.embeddings
              WHERE entity_id = $1 ORDER BY model_id",
        )
        .bind(memory_id)
        .fetch_all(&self.admin_pool)
        .await?;
        let jobs = sqlx::query_as(
            "SELECT model_id, dim FROM proxima_core.embedding_jobs
              WHERE entity_id = $1 ORDER BY model_id",
        )
        .bind(memory_id)
        .fetch_all(&self.admin_pool)
        .await?;
        Ok((vectors, jobs))
    }
}

fn returned_handles(page: &serde_json::Value) -> Vec<String> {
    page["memories"]
        .as_array()
        .expect("memories")
        .iter()
        .map(|memory| memory["memory"].as_str().expect("handle").to_owned())
        .collect()
}

/// An Owner moves to a new model with no search gap: while its route is
/// moving, every write is queued for both spaces and backfill fills the new
/// one, search stays on the old space until the host flips the route and
/// then moves with it, and purge leaves only the new space.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn an_owner_moves_to_a_new_model_without_a_search_gap() {
    use proxima_core::EmbeddingSpaceRole;
    use proxima_core::llm::{EmbeddingDim, EmbeddingRoute};
    let split_db = clone_split_core_db("proxima_core_route_move")
        .await
        .expect("PG required for tests");
    let db_name = split_db.name().to_owned();
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fixture = TwoOwnerFixture::boot(&db_name).await?;
        let engine = &fixture.built.host().engine();
        let owner = fixture.personal;
        let old = RecordingRouteEmbedding::new("route-old", EmbeddingDim::D768);
        let new = RecordingRouteEmbedding::new("route-new", EmbeddingDim::D1024);
        let old_space = ("route-old".to_owned(), 768_i16);
        let new_space = ("route-new".to_owned(), 1024_i16);
        fixture.router.set_owner(owner, Some(old.bound()));

        let before = fixture
            .remember("current", "needle before the move")
            .await?;
        let drained = engine.drain_embedding_jobs(10).await?;
        assert_eq!((drained.processed, drained.failed), (1, 0));

        // Moving: new Facts and derived memories are queued for both spaces.
        fixture.router.set_route(
            owner,
            EmbeddingRoute::moving(Some(old.bound()), new.bound())?,
        );
        let during = fixture
            .remember("current", "needle during the move")
            .await?;
        assert_eq!(
            fixture.embedding_rows(&during).await?,
            (vec![], vec![new_space.clone(), old_space.clone()]),
            "a write while moving is queued for both spaces"
        );
        let derived = call_test_model_tool(
            &fixture.tools,
            fixture.authz.clone(),
            owner,
            "core_derive",
            serde_json::json!({
                "space": "current",
                "kind": "Abstraction",
                "title": "moved pattern",
                "body": "needle pattern across the move",
                "tags": [],
                "source_handles": [before, during],
                "model_id": "test-model"
            }),
        )
        .await?;
        let derived = derived["handle"].as_str().expect("handle").to_owned();
        assert_eq!(
            fixture.embedding_rows(&derived).await?,
            (vec![], vec![new_space.clone(), old_space.clone()]),
            "a derived memory queues current and next without a provider call"
        );

        engine
            .backfill_missing_embeddings(&fixture.authz, &owner, 100)
            .await?;
        let drained = engine.drain_embedding_jobs(20).await?;
        assert_eq!(drained.failed, 0);
        for handle in [&before, &during, &derived] {
            assert_eq!(
                fixture.embedding_rows(handle).await?,
                (vec![new_space.clone(), old_space.clone()], vec![]),
                "{handle} is embedded in both spaces"
            );
        }
        let coverage = engine.embedding_coverage(&owner).await?;
        let roles: Vec<_> = coverage
            .iter()
            .map(|row| (row.space.model_id().to_owned(), row.role))
            .collect();
        assert_eq!(
            roles,
            vec![
                ("route-new".to_owned(), EmbeddingSpaceRole::Next),
                ("route-old".to_owned(), EmbeddingSpaceRole::Current),
            ]
        );
        for row in &coverage {
            assert!(row.counts.embeddable >= 3, "{row:?}");
            assert_eq!(row.counts.embedded, row.counts.embeddable, "{row:?}");
            assert_eq!(row.counts.pending + row.counts.processing, 0, "{row:?}");
        }

        // Search reads the old space until the flip.
        let (old_seen, new_seen) = (old.texts().len(), new.texts().len());
        let page = fixture.search_personal("semantic").await?;
        assert_eq!(page["ranking"], "score", "{page}");
        let found = returned_handles(&page);
        for handle in [&before, &during] {
            assert!(found.contains(handle), "{page}");
        }
        assert_eq!(
            old.texts().len(),
            old_seen + 1,
            "the query embeds through current"
        );
        assert_eq!(new.texts().len(), new_seen, "next serves no query");

        // The flip: search moves with the route, with every memory already
        // embedded in the new space.
        fixture.router.set_owner(owner, Some(new.bound()));
        let (old_seen, new_seen) = (old.texts().len(), new.texts().len());
        let page = fixture.search_personal("semantic").await?;
        let found = returned_handles(&page);
        for handle in [&before, &during] {
            assert!(found.contains(handle), "{page}");
        }
        assert_eq!(
            new.texts().len(),
            new_seen + 1,
            "the query embeds through the new model"
        );
        assert_eq!(
            old.texts().len(),
            old_seen,
            "the old model sees nothing after the flip"
        );

        let coverage = engine.embedding_coverage(&owner).await?;
        let unrouted = coverage
            .iter()
            .find(|row| row.role == EmbeddingSpaceRole::Unrouted)
            .expect("the old space is left over until purged");
        assert_eq!(unrouted.space.model_id(), "route-old");
        assert!(unrouted.counts.vectors >= 3, "{unrouted:?}");

        let purged = engine.purge_embedding_spaces(&owner).await?;
        assert_eq!(purged.vectors, unrouted.counts.vectors);
        assert_eq!(purged.heads, unrouted.counts.embedded);
        let coverage = engine.embedding_coverage(&owner).await?;
        assert_eq!(coverage.len(), 1, "{coverage:?}");
        assert_eq!(coverage[0].role, EmbeddingSpaceRole::Current);
        assert_eq!(coverage[0].space.model_id(), "route-new");
        for handle in [&before, &during, &derived] {
            assert_eq!(
                fixture.embedding_rows(handle).await?,
                (vec![new_space.clone()], vec![]),
                "{handle} keeps only its new vector"
            );
        }
        let after = fixture.remember("current", "needle after the move").await?;
        assert_eq!(
            fixture.embedding_rows(&after).await?,
            (vec![], vec![new_space.clone()]),
            "after the flip a write is queued for the new space only"
        );
        Ok(())
    }
    .await;
    result.expect("model move test failed");
}

/// A transferred memory is embedded under its new Owner's route: its vector
/// in a space the destination does not route goes with the transfer, and it
/// is queued for the destination's space. A destination the host cannot
/// route refuses the transfer.
#[tokio::test]
async fn a_transferred_memory_moves_to_the_destinations_space() {
    use proxima_core::llm::EmbeddingDim;
    let split_db = clone_split_core_db("proxima_core_route_transfer")
        .await
        .expect("PG required for tests");
    let db_name = split_db.name().to_owned();
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fixture = TwoOwnerFixture::boot(&db_name).await?;
        let engine = &fixture.built.host().engine();
        let personal_client = RecordingRouteEmbedding::new("route-a", EmbeddingDim::D768);
        let shared_client = RecordingRouteEmbedding::new("route-b", EmbeddingDim::D1024);
        fixture
            .router
            .set_owner(fixture.personal, Some(personal_client.bound()));
        fixture
            .router
            .refuse(fixture.shared, "tenant config unavailable");

        let moved = fixture
            .remember("current", "needle that changes hands")
            .await?;
        engine.drain_embedding_jobs(10).await?;
        let personal_space = ("route-a".to_owned(), 768_i16);
        let shared_space = ("route-b".to_owned(), 1024_i16);
        assert_eq!(
            fixture.embedding_rows(&moved).await?,
            (vec![personal_space.clone()], vec![])
        );
        let entity = proxima_core::EntityId::Memory(MemoryId::new(
            moved
                .split_once(':')
                .ok_or("typed handle")?
                .1
                .parse::<Uuid>()?,
        ));

        let refused = engine
            .transfer_to_owner(&fixture.authz, entity, fixture.shared)
            .await
            .expect_err("an unroutable destination refuses the transfer");
        assert!(
            refused.to_string().contains("no embedding route"),
            "{refused}"
        );
        assert_eq!(
            fixture.embedding_rows(&moved).await?,
            (vec![personal_space.clone()], vec![]),
            "a refused transfer moves nothing"
        );

        fixture
            .router
            .set_owner(fixture.shared, Some(shared_client.bound()));
        engine
            .transfer_to_owner(&fixture.authz, entity, fixture.shared)
            .await?;
        assert_eq!(
            fixture.embedding_rows(&moved).await?,
            (vec![], vec![shared_space.clone()]),
            "the source's vector goes; the destination's space is queued"
        );
        let job_owner: Uuid = sqlx::query_scalar(
            "SELECT owner_id FROM proxima_core.embedding_jobs WHERE model_id = 'route-b'",
        )
        .fetch_one(&fixture.admin_pool)
        .await?;
        assert_eq!(job_owner, fixture.shared.into_inner());

        let drained = engine.drain_embedding_jobs(10).await?;
        assert_eq!((drained.processed, drained.failed), (1, 0));
        assert!(
            shared_client
                .texts()
                .iter()
                .any(|text| text.contains("changes hands")),
            "the destination's endpoint embeds the moved text"
        );
        assert_eq!(
            fixture.embedding_rows(&moved).await?,
            (vec![shared_space], vec![])
        );
        Ok(())
    }
    .await;
    result.expect("transfer retarget test failed");
}

/// A purge for an Owner the host cannot route deletes nothing.
#[tokio::test]
async fn an_unroutable_owner_is_never_purged() {
    use proxima_core::llm::EmbeddingDim;
    let split_db = clone_split_core_db("proxima_core_route_purge_refusal")
        .await
        .expect("PG required for tests");
    let db_name = split_db.name().to_owned();
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fixture = TwoOwnerFixture::boot(&db_name).await?;
        let engine = &fixture.built.host().engine();
        let owner = fixture.personal;
        let client = RecordingRouteEmbedding::new("route-a", EmbeddingDim::D768);
        fixture.router.set_owner(owner, Some(client.bound()));
        let kept = fixture.remember("current", "needle kept").await?;
        engine.drain_embedding_jobs(10).await?;

        fixture.router.refuse(owner, "tenant config unavailable");
        let err = engine
            .purge_embedding_spaces(&owner)
            .await
            .expect_err("a route error purges nothing");
        assert!(
            err.to_string().contains("tenant config unavailable"),
            "{err}"
        );
        assert_eq!(
            fixture.embedding_rows(&kept).await?.0,
            vec![("route-a".to_owned(), 768)]
        );
        Ok(())
    }
    .await;
    result.expect("purge refusal test failed");
}

#[derive(Debug, Clone, Copy)]
enum RoutedRefusal {
    Configuration(proxima_core::llm::EmbeddingConfigurationFault),
    Input {
        first: proxima_core::llm::EmbeddingInputRefusal,
        later: proxima_core::llm::EmbeddingInputRefusal,
    },
}

#[derive(Debug)]
struct RefusingRouteEmbedding {
    refusal: RoutedRefusal,
    lengths: std::sync::Mutex<Vec<usize>>,
}

impl RefusingRouteEmbedding {
    fn new(refusal: RoutedRefusal) -> Arc<Self> {
        Arc::new(Self {
            refusal,
            lengths: std::sync::Mutex::default(),
        })
    }

    fn lengths(&self) -> Vec<usize> {
        self.lengths.lock().expect("refusal recorder lock").clone()
    }

    fn bound(self: &Arc<Self>) -> proxima_core::llm::BoundEmbeddingClient {
        proxima_core::llm::BoundEmbeddingClient::bind(self.clone()).expect("lane width")
    }
}

#[async_trait::async_trait]
impl proxima_core::llm::EmbeddingClient for RefusingRouteEmbedding {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, proxima_core::llm::LlmError> {
        use proxima_core::llm::LlmError;
        let mut lengths = self.lengths.lock().expect("refusal recorder lock");
        let first_call = lengths.is_empty();
        lengths.push(text.len());
        Err(match self.refusal {
            RoutedRefusal::Configuration(fault) => LlmError::Configuration(fault),
            RoutedRefusal::Input { first, later } => {
                LlmError::InputRefused(if first_call { first } else { later })
            }
        })
    }

    fn model_id(&self) -> &'static str {
        "route-failure"
    }

    fn dim(&self) -> usize {
        proxima_core::llm::EmbeddingDim::D1024.width()
    }
}

async fn shared_configuration_fault_case(
    fixture: &TwoOwnerFixture,
    fault: proxima_core::llm::EmbeddingConfigurationFault,
) -> Result<(), Box<dyn std::error::Error>> {
    use proxima_core::llm::EmbeddingDim;
    let engine = fixture.built.host().engine();
    let refused = RefusingRouteEmbedding::new(RoutedRefusal::Configuration(fault));
    for owner in [fixture.personal, fixture.shared] {
        fixture.router.set_owner(owner, Some(refused.bound()));
    }
    let personal = fixture
        .remember("current", "needle configuration one")
        .await?;
    let another = fixture
        .remember("current", "needle configuration two")
        .await?;
    let shared = fixture
        .remember(&fixture.shared_space, "needle configuration shared")
        .await?;
    assert!(
        refused.lengths().is_empty(),
        "admission must not call the client"
    );
    let first = engine.drain_embedding_jobs(10).await?;
    assert_eq!(first.failed, 0, "configuration faults keep jobs retryable");
    assert_eq!(first.configuration_faults, vec![fault]);
    assert_eq!(engine.embedding_configuration_faults(), vec![fault]);
    assert_eq!(
        refused.lengths().len(),
        1,
        "already-claimed owners share one pause"
    );
    for handle in [&personal, &another, &shared] {
        assert_eq!(fixture.job_statuses(handle).await?, ["pending"]);
        assert_eq!(fixture.stored_space(handle).await?, None);
    }
    let (left, right) = tokio::join!(
        engine.drain_embedding_jobs(10),
        engine.drain_embedding_jobs(10)
    );
    for paused in [left?, right?] {
        assert_eq!(paused.processed, 0);
        assert_eq!(paused.configuration_faults, vec![fault]);
    }
    assert_eq!(refused.lengths().len(), 1, "paused drains must not probe");

    // Rebinding one Owner resumes that identical space without clearing the
    // fault still used by the other Owner.
    let healthy = RecordingRouteEmbedding::new("route-failure", EmbeddingDim::D1024);
    fixture
        .router
        .set_owner(fixture.personal, Some(healthy.bound()));
    let resumed = engine.drain_embedding_jobs(10).await?;
    assert_eq!((resumed.processed, resumed.failed), (2, 0));
    assert_eq!(resumed.configuration_faults, vec![fault]);
    assert_eq!(healthy.texts().len(), 2);
    assert_eq!(fixture.job_statuses(&shared).await?, ["pending"]);
    assert_eq!(engine.embedding_configuration_faults(), vec![fault]);
    fixture
        .router
        .set_owner(fixture.shared, Some(healthy.bound()));
    let resumed = engine.drain_embedding_jobs(10).await?;
    assert_eq!((resumed.processed, resumed.failed), (1, 0));
    assert!(resumed.configuration_faults.is_empty());
    assert!(engine.embedding_configuration_faults().is_empty());
    for handle in [&personal, &another, &shared] {
        assert!(fixture.job_statuses(handle).await?.is_empty());
        assert_eq!(
            fixture.stored_space(handle).await?,
            Some(("route-failure".to_owned(), 1024))
        );
    }

    // A still-live original Arc carries its pause if the host selects it again.
    fixture
        .router
        .set_owner(fixture.personal, Some(refused.bound()));
    let returned = fixture
        .remember("current", "needle old client returned")
        .await?;
    let paused = engine.drain_embedding_jobs(1).await?;
    assert_eq!(paused.processed, 0);
    assert_eq!(paused.configuration_faults, vec![fault]);
    assert_eq!(refused.lengths().len(), 1);
    assert_eq!(fixture.job_statuses(&returned).await?, ["pending"]);
    fixture
        .router
        .set_owner(fixture.personal, Some(healthy.bound()));
    let resumed = engine.drain_embedding_jobs(1).await?;
    assert_eq!((resumed.processed, resumed.failed), (1, 0));
    assert!(engine.embedding_configuration_faults().is_empty());
    Ok(())
}

#[tokio::test]
async fn configuration_faults_pause_shared_clients_until_their_bindings_change() {
    use proxima_core::llm::EmbeddingConfigurationFault;
    let split_db = clone_split_core_db("proxima_core_route_configuration")
        .await
        .expect("PG required for tests");
    let fixture = TwoOwnerFixture::boot(split_db.name())
        .await
        .expect("routing fixture");
    for fault in [
        EmbeddingConfigurationFault::BadRequest,
        EmbeddingConfigurationFault::Unauthorized,
        EmbeddingConfigurationFault::Forbidden,
        EmbeddingConfigurationFault::NotFound,
    ] {
        shared_configuration_fault_case(&fixture, fault)
            .await
            .expect("shared client configuration case");
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn paused_jobs_allow_limit_one_progress_in_other_owner_and_next_spaces() {
    use proxima_core::llm::{EmbeddingConfigurationFault, EmbeddingDim, EmbeddingRoute};
    let split_db = clone_split_core_db("proxima_core_route_paused_progress")
        .await
        .expect("PG required for tests");
    let fixture = TwoOwnerFixture::boot(split_db.name())
        .await
        .expect("routing fixture");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let engine = fixture.built.host().engine();
        let fault = EmbeddingConfigurationFault::Forbidden;
        let refused = RefusingRouteEmbedding::new(RoutedRefusal::Configuration(fault));
        fixture
            .router
            .set_owner(fixture.personal, Some(refused.bound()));
        let oldest = fixture
            .remember("current", "needle oldest paused job")
            .await?;
        let paused = engine.drain_embedding_jobs(1).await?;
        assert_eq!(paused.configuration_faults, vec![fault]);
        assert_eq!(fixture.job_statuses(&oldest).await?, ["pending"]);

        let healthy = RecordingRouteEmbedding::new("route-next", EmbeddingDim::D768);
        fixture.router.set_route(
            fixture.personal,
            EmbeddingRoute::moving(Some(refused.bound()), healthy.bound())?,
        );
        fixture
            .router
            .set_owner(fixture.shared, Some(healthy.bound()));
        let moving = fixture
            .remember("current", "needle healthy next space")
            .await?;
        let shared = fixture
            .remember(&fixture.shared_space, "needle later healthy owner")
            .await?;
        for _ in 0..2 {
            let drained = engine.drain_embedding_jobs(1).await?;
            assert_eq!((drained.processed, drained.failed), (1, 0));
            assert_eq!(drained.configuration_faults, vec![fault]);
        }
        assert_eq!(healthy.texts().len(), 2);
        assert_eq!(refused.lengths().len(), 1);
        assert_eq!(
            fixture.embedding_rows(&moving).await?,
            (
                vec![("route-next".to_owned(), 768)],
                vec![("route-failure".to_owned(), 1024)]
            )
        );
        assert_eq!(
            fixture.stored_space(&shared).await?,
            Some(("route-next".to_owned(), 768))
        );
        for handle in [&oldest, &moving] {
            assert_eq!(fixture.job_statuses(handle).await?, ["pending"]);
        }
        assert_eq!(engine.embedding_configuration_faults(), vec![fault]);
        let replacement = RecordingRouteEmbedding::new("route-failure", EmbeddingDim::D1024);
        fixture
            .router
            .set_owner(fixture.personal, Some(replacement.bound()));
        let resumed = engine.drain_embedding_jobs(10).await?;
        assert_eq!((resumed.processed, resumed.failed), (2, 0));
        assert!(resumed.configuration_faults.is_empty());
        assert!(engine.embedding_configuration_faults().is_empty());
        Ok(())
    }
    .await;
    fixture.shutdown().await;
    result.expect("paused queue progress");
}

async fn terminal_input_refusal_case(
    fixture: &TwoOwnerFixture,
    later: proxima_core::llm::EmbeddingInputRefusal,
) -> Result<(), Box<dyn std::error::Error>> {
    use proxima_core::llm::{EmbeddingInputRefusal, MIN_EMBED_INPUT_CAP_CHARS};
    let refused = RefusingRouteEmbedding::new(RoutedRefusal::Input {
        first: EmbeddingInputRefusal::PayloadTooLarge,
        later,
    });
    fixture
        .router
        .set_owner(fixture.personal, Some(refused.bound()));
    let handle = fixture
        .remember("current", &"x".repeat(2 * MIN_EMBED_INPUT_CAP_CHARS))
        .await?;
    let engine = fixture.built.host().engine();
    let drained = engine.drain_embedding_jobs(1).await?;
    assert_eq!(drained.failed, 1);
    assert!(drained.configuration_faults.is_empty());
    let jobs = fixture.jobs(&handle).await?;
    assert_eq!(jobs.len(), 1);
    let (status, last_error) = &jobs[0];
    assert_eq!(status, "failed_permanent");
    assert!(
        last_error.as_deref().is_some_and(
            |error| error.contains(&format!("HTTP {}", later.status().expect("HTTP refusal")))
        ),
        "terminal job must record its final refusal: {last_error:?}"
    );
    let lengths = refused.lengths();
    assert!(
        lengths.len() > 2,
        "long input must pass through chunk isolation: {lengths:?}"
    );
    assert!(
        lengths.last() < lengths.first(),
        "the final refusal belongs to a smaller segment: {lengths:?}"
    );
    assert_eq!(fixture.stored_space(&handle).await?, None);
    engine
        .reconcile_embeddings(
            proxima_core::EmbeddingReconcileScope::MissingOnly,
            Some(100),
        )
        .await?;
    assert_eq!(fixture.job_statuses(&handle).await?, ["failed_permanent"]);
    let calls = refused.lengths().len();
    let drained = engine.drain_embedding_jobs(1).await?;
    assert_eq!(drained.processed, 0);
    assert_eq!(
        refused.lengths().len(),
        calls,
        "terminal input must not be retried"
    );
    Ok(())
}

#[tokio::test]
async fn terminal_jobs_record_the_final_typed_input_refusal() {
    use proxima_core::llm::EmbeddingInputRefusal;
    let split_db = clone_split_core_db("proxima_core_route_input_status")
        .await
        .expect("PG required for tests");
    let fixture = TwoOwnerFixture::boot(split_db.name())
        .await
        .expect("routing fixture");
    for later in [
        EmbeddingInputRefusal::PayloadTooLarge,
        EmbeddingInputRefusal::UnprocessableContent,
    ] {
        terminal_input_refusal_case(&fixture, later)
            .await
            .expect("terminal input status");
    }
    fixture.shutdown().await;
}

#[derive(Debug, Clone, Copy)]
enum RetryFailure {
    Http(u16),
    Network,
    Timeout,
}

impl RetryFailure {
    fn error(self) -> proxima_core::llm::LlmError {
        use proxima_core::llm::LlmError;
        match self {
            Self::Http(status) => LlmError::Retryable {
                status: Some(status),
                retry_after: None,
                message: format!("HTTP {status} temporary failure"),
            },
            Self::Network => LlmError::retryable("network unavailable"),
            Self::Timeout => LlmError::retryable("request timed out"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum RetryStage {
    Batch,
    IsolatedItem,
    LaterChunk,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RetryCalls {
    batches: Vec<Vec<usize>>,
    singles: Vec<usize>,
    accepted_singles: usize,
}

#[derive(Debug)]
struct RetryRouteEmbedding {
    stage: RetryStage,
    failure: RetryFailure,
    calls: std::sync::Mutex<RetryCalls>,
}

impl RetryRouteEmbedding {
    fn new(stage: RetryStage, failure: RetryFailure) -> Arc<Self> {
        Arc::new(Self {
            stage,
            failure,
            calls: std::sync::Mutex::default(),
        })
    }

    fn calls(&self) -> RetryCalls {
        self.calls.lock().expect("retry recorder lock").clone()
    }

    fn bound(self: &Arc<Self>) -> proxima_core::llm::BoundEmbeddingClient {
        proxima_core::llm::BoundEmbeddingClient::bind(self.clone()).expect("lane width")
    }

    fn assert_failed_attempt(&self) {
        use proxima_core::llm::CHUNKED_EMBED_MIN_BYTES;
        let calls = self.calls();
        assert_eq!(
            calls.batches.len(),
            1,
            "one actual batch, including shared owners: {calls:?}"
        );
        match self.stage {
            RetryStage::Batch => {
                assert_eq!(calls.batches[0].len(), 2);
                assert!(
                    calls.singles.is_empty(),
                    "a transient batch must never probe or isolate: {calls:?}"
                );
                assert_eq!(calls.accepted_singles, 0);
            }
            RetryStage::IsolatedItem => {
                assert_eq!(calls.batches[0].len(), 2);
                assert_eq!(
                    calls.singles,
                    [calls.batches[0][0]],
                    "only the first full item is tried; no probe/split: {calls:?}"
                );
                assert_eq!(calls.accepted_singles, 0);
            }
            RetryStage::LaterChunk => {
                assert_eq!(calls.batches[0].len(), 1);
                assert_eq!(
                    calls.singles.len(),
                    2,
                    "no full-text replay or probe: {calls:?}"
                );
                let full = calls.batches[0][0];
                assert_eq!(
                    calls.singles.iter().sum::<usize>(),
                    full,
                    "two halves cover the entire input"
                );
                assert!(
                    calls
                        .singles
                        .iter()
                        .all(|length| *length >= CHUNKED_EMBED_MIN_BYTES && *length < full)
                );
                assert_eq!(
                    calls.accepted_singles, 1,
                    "a chunk succeeded before the transient failure"
                );
            }
        }
    }
}

#[async_trait::async_trait]
impl proxima_core::llm::EmbeddingClient for RetryRouteEmbedding {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, proxima_core::llm::LlmError> {
        let mut calls = self.calls.lock().expect("retry recorder lock");
        calls.singles.push(text.len());
        let accepted = text == "ok"
            || matches!(self.stage, RetryStage::LaterChunk) && calls.singles.len() == 1;
        if accepted {
            calls.accepted_singles += 1;
            let mut vector = vec![0.0; proxima_core::llm::EmbeddingDim::D1024.width()];
            vector[0] = 1.0;
            return Ok(vector);
        }
        Err(self.failure.error())
    }

    async fn embed_many(
        &self,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>, proxima_core::llm::LlmError> {
        use proxima_core::llm::{EmbeddingInputRefusal, LlmError};
        self.calls
            .lock()
            .expect("retry recorder lock")
            .batches
            .push(texts.iter().map(String::len).collect());
        Err(match self.stage {
            RetryStage::Batch => self.failure.error(),
            RetryStage::IsolatedItem | RetryStage::LaterChunk => {
                LlmError::InputRefused(EmbeddingInputRefusal::PayloadTooLarge)
            }
        })
    }

    fn model_id(&self) -> &'static str {
        "route-retry"
    }
    fn dim(&self) -> usize {
        proxima_core::llm::EmbeddingDim::D1024.width()
    }
}

async fn remember_retry_case(
    fixture: &TwoOwnerFixture,
    stage: RetryStage,
    failure: RetryFailure,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let marker = format!("needle retry {stage:?} {failure:?}");
    match stage {
        RetryStage::Batch | RetryStage::IsolatedItem => Ok(vec![
            fixture
                .remember("current", &format!("{marker} first"))
                .await?,
            fixture
                .remember("current", &format!("{marker} second"))
                .await?,
            fixture
                .remember(&fixture.shared_space, &format!("{marker} shared"))
                .await?,
        ]),
        RetryStage::LaterChunk => {
            let mut body = "x".repeat(2 * proxima_core::llm::MIN_EMBED_INPUT_CAP_CHARS);
            body.replace_range(..marker.len(), &marker);
            Ok(vec![fixture.remember("current", &body).await?])
        }
    }
}

async fn assert_retry_jobs_pending(
    fixture: &TwoOwnerFixture,
    handles: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    for handle in handles {
        let jobs = fixture.jobs(handle).await?;
        assert_eq!(jobs.len(), 1, "one retained job for {handle}");
        assert_eq!(
            jobs[0].0, "pending",
            "transient failure keeps {handle} pending"
        );
        assert_eq!(
            fixture.stored_space(handle).await?,
            None,
            "no partial chunk or embedding head for {handle}"
        );
    }
    Ok(())
}

async fn retryable_job_case(
    fixture: &TwoOwnerFixture,
    stage: RetryStage,
    failure: RetryFailure,
) -> Result<(), Box<dyn std::error::Error>> {
    use proxima_core::llm::EmbeddingDim;
    let client = RetryRouteEmbedding::new(stage, failure);
    for owner in [fixture.personal, fixture.shared] {
        fixture.router.set_owner(owner, Some(client.bound()));
    }
    let handles = remember_retry_case(fixture, stage, failure).await?;
    assert_eq!(
        client.calls(),
        RetryCalls::default(),
        "admission never calls the provider"
    );
    let engine = fixture.built.host().engine();
    let first = engine.drain_embedding_jobs(10).await?;
    assert_eq!((first.processed, first.failed), (0, 0));
    assert!(first.configuration_faults.is_empty());
    client.assert_failed_attempt();
    assert_retry_jobs_pending(fixture, &handles).await?;

    let before = client.calls();
    let blocked = engine.drain_embedding_jobs(10).await?;
    assert_eq!((blocked.processed, blocked.failed), (0, 0));
    assert!(blocked.configuration_faults.is_empty());
    assert_eq!(
        client.calls(),
        before,
        "shared backoff makes no provider calls or probes"
    );
    assert_retry_jobs_pending(fixture, &handles).await?;

    let healthy = RecordingRouteEmbedding::new("route-retry", EmbeddingDim::D1024);
    for owner in [fixture.personal, fixture.shared] {
        fixture.router.set_owner(owner, Some(healthy.bound()));
    }
    let recovered = engine.drain_embedding_jobs(10).await?;
    assert_eq!((recovered.processed, recovered.failed), (handles.len(), 0));
    assert!(recovered.configuration_faults.is_empty());
    assert_eq!(healthy.texts().len(), handles.len());
    assert_eq!(
        client.calls(),
        before,
        "recovery uses a new original client"
    );
    for handle in handles {
        assert!(
            fixture.jobs(&handle).await?.is_empty(),
            "all retained work is drained before the next case"
        );
        assert_eq!(
            fixture.stored_space(&handle).await?,
            Some(("route-retry".to_owned(), 1024))
        );
    }
    Ok(())
}

/// Retry timing is covered with injected time in core. This test keeps its
/// own retry interval long while real PG proves durable retention and rebind.
/// One fixture covers all 24 cases and drains each before admitting the next.
#[tokio::test]
async fn retryable_failures_retain_jobs_without_partial_embeddings() {
    use proxima_core::llm::EmbeddingRuntimePolicy;
    let split_db = clone_split_core_db("proxima_core_route_retry_retention")
        .await
        .expect("PG required for tests");
    let default = EmbeddingRuntimePolicy::default();
    let policy = EmbeddingRuntimePolicy::new(
        default.request_timeout(),
        default.batch_size(),
        std::time::Duration::from_hours(1),
        default.stale_claim_timeout(),
    )
    .expect("valid isolated retry policy");
    let fixture = TwoOwnerFixture::boot_with_policy(split_db.name(), policy)
        .await
        .expect("routing fixture");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        for stage in [
            RetryStage::Batch,
            RetryStage::IsolatedItem,
            RetryStage::LaterChunk,
        ] {
            for failure in [
                RetryFailure::Http(408),
                RetryFailure::Http(429),
                RetryFailure::Http(500),
                RetryFailure::Http(502),
                RetryFailure::Http(503),
                RetryFailure::Http(504),
                RetryFailure::Network,
                RetryFailure::Timeout,
            ] {
                retryable_job_case(&fixture, stage, failure)
                    .await
                    .map_err(|error| format!("{stage:?}/{failure:?}: {error}"))?;
            }
        }
        Ok(())
    }
    .await;
    fixture.shutdown().await;
    result.expect("retryable durable retention table");
}
