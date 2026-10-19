#[path = "fixtures/split_core_db.rs"]
mod split_core_db;

use proxima_core::storage_ports::*;
use std::collections::HashSet;
use std::sync::Arc;

use proxima::flavor::{FlavorBundle, NamedMigrator};
use proxima::{
    AppInfo, AuthzContext, CoreMcpError, CoreMcpErrorKind, CoreMcpTools, CoreToolInfo, FlavorApp,
    FlavorServiceError, FlavorServices, Proxima, S3RuntimeConfig, ToolScope, company_owner,
};
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::{
    AuthPath, CitedObjectPayload, ColdObjectStore, FlavorRegistry, FlavorRegistryFrozen, GroupId,
    MemoryId, OwnerId, Relation, Replay, Role, ToolEffect, UserId, provider_safe_tool_name,
};
use proxima_pg_testkit::{db_url, split_role_urls};
use proxima_storage_pg::PgStorage;
use split_core_db::clone_split_core_db;
use uuid::Uuid;

#[cfg(feature = "openai-compat-embed")]
#[path = "core_mcp_pg/embedding_routing.rs"]
mod embedding_routing;

#[path = "core_mcp_pg/hybrid_search.rs"]
mod hybrid_search;

#[path = "core_mcp_pg/other_stores.rs"]
mod other_stores;

#[path = "core_mcp_pg/forgotten_targets.rs"]
mod forgotten_targets;

#[path = "core_mcp_pg/memory_reads_alike.rs"]
mod memory_reads_alike;

#[path = "core_mcp_pg/read_veto.rs"]
mod read_veto;

mod embedding_failure_regressions {
    use super::*;
    use proxima::host::{EmbedCaps, OpenAiCompatConfig, OpenAiCompatEmbeddingClient};
    use proxima_core::llm::{BoundEmbeddingClient, EmbeddingClient, EmbeddingDim, LlmError};
    use proxima_core::test_fixtures::TestEmbeddingRouter;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    type TestResult<T> = Result<T, Box<dyn std::error::Error>>;
    const DEADLINE: Duration = Duration::from_secs(10);
    const EMBEDDING_DIM: usize = EmbeddingDim::D1024.width();

    fn bound(client: Arc<dyn EmbeddingClient>) -> BoundEmbeddingClient {
        BoundEmbeddingClient::bind(client).expect("test clients embed in a lane")
    }

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
            "test-embed",
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
        memory: String,
        adapter: Result<Vec<f32>, LlmError>,
        lexical: serde_json::Value,
        hybrid: serde_json::Value,
        semantic: Result<serde_json::Value, CoreMcpError>,
        recall: serde_json::Value,
        subject_recall: serde_json::Value,
        calls: [usize; 6],
        healthy_semantic: serde_json::Value,
    }

    async fn prepare_fact(
        built: &proxima::BuiltProxima,
        router: &TestEmbeddingRouter,
        owner: OwnerId,
    ) -> TestResult<String> {
        let tools = built.host().core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);
        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Overflow fallback fact",
                "body": "provider overflow sentinel needle",
                "idempotency_key": "provider-overflow-fallback"
            }),
        )
        .await?;
        let memory = remembered["handle"]
            .as_str()
            .ok_or("memory handle")?
            .to_owned();
        router.set_default(bound(test_embedding()));
        ensure_fact_embedding_for_handle(built.host().engine(), &owner, &memory).await?;
        Ok(memory)
    }

    fn search_args(mode: &str) -> serde_json::Value {
        let mut args = serde_json::json!({
            "query": "sentinel needle", "kind": "Fact", "limit": 5, "mode": mode
        });
        if mode == "hybrid" {
            args["semantic_weight"] = serde_json::json!(0.7);
        }
        args
    }

    async fn exercise(
        built: &proxima::BuiltProxima,
        router: &TestEmbeddingRouter,
        owner: OwnerId,
    ) -> TestResult<Observed> {
        let tools = built.host().core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);
        let memory = prepare_fact(built, router, owner).await?;
        let endpoint = overflow_endpoint().await?;
        let adapter = endpoint.client.embed("response control").await;
        router.set_default(bound(endpoint.client.clone()));
        let mut calls = [0; 6];
        calls[0] = endpoint.calls.load(Ordering::SeqCst);

        let call = |name: &'static str, args| {
            tokio::time::timeout(
                DEADLINE,
                call_test_model_tool(&tools, authz.clone(), owner, name, args),
            )
        };
        let lexical = call("core_search_memories", search_args("lexical")).await??;
        calls[1] = endpoint.calls.load(Ordering::SeqCst);
        let hybrid = call("core_search_memories", search_args("hybrid")).await??;
        calls[2] = endpoint.calls.load(Ordering::SeqCst);
        // Each handler must receive its own malformed response, independently
        // of the preceding client's retry backoff.
        router.set_default(bound(Arc::new(endpoint.client.as_ref().clone())));
        let semantic = call("core_search_memories", search_args("semantic")).await?;
        calls[3] = endpoint.calls.load(Ordering::SeqCst);
        router.set_default(bound(Arc::new(endpoint.client.as_ref().clone())));
        let recall = call(
            "core_recall",
            serde_json::json!({"question": "sentinel needle", "kind": "Fact"}),
        )
        .await??;
        calls[4] = endpoint.calls.load(Ordering::SeqCst);
        let subject_recall = call(
            "core_recall",
            serde_json::json!({"subjects": [&memory], "kind": "Fact"}),
        )
        .await??;
        calls[5] = endpoint.calls.load(Ordering::SeqCst);

        router.set_default(bound(test_embedding()));
        let healthy = call("core_search_memories", search_args("semantic")).await??;
        Ok(Observed {
            memory,
            adapter,
            lexical,
            hybrid,
            semantic,
            recall,
            subject_recall,
            calls,
            healthy_semantic: healthy,
        })
    }

    #[tokio::test]
    async fn core_search_and_recall_degrade_after_actual_provider_response_overflow() {
        let split_db = clone_split_core_db("proxima_query_provider_overflow")
            .await
            .expect("PG fixture");
        let name = split_db.name().to_owned();
        let result: TestResult<Observed> = async {
            let (runtime_url, platform_url) = split_role_urls(&name).await?;
            let owner = company_owner(Uuid::now_v7());
            let router = Arc::new(TestEmbeddingRouter::default());
            let built = Proxima::<AgentMemoryApp>::app()
                .database_url(runtime_url)
                .store_number(0)
                .platform_database_url(platform_url)
                .owner(owner)
                .served_surface(proxima_core::ServedSurface::All)
                .embedding_router(router.clone())
                .build()
                .await?;
            built
                .feature_control(proxima::Feature::EmbeddingWorker)
                .expect("the configured router starts a worker")
                .quiesce()
                .await
                .expect("manual query cases run without background embedding calls");
            let result = exercise(&built, &router, owner).await;
            built.shutdown().await;
            result
        }
        .await;
        drop(split_db); // the fixture goes before the assertions
        let observed = result.expect("bounded handler exercise");
        eprintln!(
            "core overflow handlers: calls={:?}; fixture removed",
            observed.calls,
        );
        assert!(
            matches!(observed.adapter, Err(LlmError::Retryable { ref message, .. })
            if message == "embedding 0 has non-finite component at position 1023")
        );
        assert_eq!(observed.calls, [1, 1, 2, 3, 4, 4]);
        for (page, mode, degraded) in [
            (&observed.lexical, "lexical", false),
            (&observed.hybrid, "hybrid", true),
        ] {
            assert_eq!(page["mode"], mode);
            assert_eq!(page["degraded_to_lexical"], degraded);
            assert_eq!(page["memories"][0]["memory"], observed.memory);
            assert!(
                page["memories"][0]["lexical_score"]
                    .as_f64()
                    .unwrap_or_default()
                    > 0.0
            );
            assert_eq!(page["memories"][0]["similarity_score"], 0.0);
        }
        assert_eq!(
            observed.hybrid["memories"][0]["score"],
            observed.lexical["memories"][0]["score"]
        );
        assert!(matches!(observed.semantic, Err(CoreMcpError::Tool {
            kind: proxima_core::McpToolErrorKind::InvalidRequest, ref message,
        }) if message == "semantic search unavailable: embedding provider error"));
        for (packet, reason) in [
            (&observed.recall, "question"),
            (&observed.subject_recall, "subject"),
        ] {
            assert!(packet["sketches"].as_array().expect("sketches").iter().any(
                |row| row["handle"] == observed.memory
                    && row["kind"] == "Fact"
                    && row["reason"] == reason
                    && row["sketch"] == "Overflow fallback fact"
            ));
        }
        assert_eq!(
            observed.healthy_semantic["memories"][0]["memory"],
            observed.memory
        );
        assert!(
            observed.healthy_semantic["memories"][0]["similarity_score"]
                .as_f64()
                .unwrap_or_default()
                > 0.99
        );
    }
}

type ResolvedAuthz = AuthzContext;

struct EmptyApp;
struct AgentMemoryApp;

fn test_embedding() -> Arc<ConstantEmbedding> {
    Arc::new(ConstantEmbedding::prefixed("test-embed", &[1.0, 0.0, 0.0]))
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct TestCitedObject {
    locator: String,
}

impl CitedObjectPayload for TestCitedObject {
    const SCHEMA_ID: &'static str = "test/facade-cited-object-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn idempotency_key(&self) -> [u8; 32] {
        let mut out = [0; 32];
        let bytes = self.locator.as_bytes();
        let len = bytes.len().min(out.len());
        out[..len].copy_from_slice(&bytes[..len]);
        out
    }
}

impl FlavorBundle for EmptyApp {
    fn register(_registry: &mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for EmptyApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "core-mcp-test",
            title: "Core MCP Test",
            version: "1",
        }
    }
}

#[derive(Debug)]
struct BootMark;

struct MarkApp;

impl FlavorBundle for MarkApp {
    fn register(_: &mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for MarkApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "mark-app",
            title: "Mark App",
            version: "1",
        }
    }

    fn services(_: &proxima::AppContext) -> Result<FlavorServices, FlavorServiceError> {
        let mut services = FlavorServices::new();
        services.try_insert(BootMark)?;
        Ok(services)
    }
}

impl FlavorBundle for AgentMemoryApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError> {
        registry.try_add_cited_object_schema::<TestCitedObject>()?;
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for AgentMemoryApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "agent-memory-core-mcp-test",
            title: "Agent Memory Core MCP Test",
            version: "1",
        }
    }
}

fn palette_of(ids: &[&str]) -> ToolScope {
    ToolScope::Palette(
        ids.iter()
            .map(|id| proxima::ScopeKey::parse(id).expect("a scope key"))
            .collect(),
    )
}

fn host_authz(owner: &OwnerId, tool_scope: ToolScope) -> ResolvedAuthz {
    let authz = AuthzContext::for_subject(owner.as_user(), AuthPath::HostBearer);
    proxima_core::test_fixtures::authenticated_context(authz.with_tool_scope(tool_scope))
}

fn verified(authz: AuthzContext) -> AuthzContext {
    proxima_core::test_fixtures::authenticated_context(authz)
}

fn space_authz(subject: OwnerId, owners: Vec<OwnerId>, group_role: Role) -> ResolvedAuthz {
    let user = subject.as_user();
    let roles = owners
        .into_iter()
        .filter(|owner| *owner != subject)
        .map(|owner| (owner, group_role));
    verified(AuthzContext::for_subject_with_role(
        user,
        roles,
        AuthPath::HostBearer,
    ))
}

async fn seed_group_membership(
    pg: &PgStorage,
    space_owner: &OwnerId,
    relation: Relation,
    subject: &OwnerId,
) {
    let group = space_owner.as_group();
    let user = subject.as_user();
    let engine = proxima_core::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    let authz = verified(AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(*space_owner, Role::admin())],
        AuthPath::HostBearer,
    ));
    let permit = engine
        .authorize_owner_write(&authz, space_owner, proxima_core::AccessKind::Goal)
        .await
        .expect("seed group membership permit");
    let membership_permit = MembershipWritePermit::new_for_tests(&permit, None, None, None);
    pg.add_group_member(
        &membership_permit,
        group,
        user.into(),
        relation,
        Uuid::now_v7(),
    )
    .await
    .expect("seed group membership");
}

async fn server_issued_group_space_selector(
    tools: &CoreMcpTools,
    authz: AuthzContext,
    owner: OwnerId,
) -> String {
    let spaces = call_test_model_tool(
        tools,
        authz,
        owner,
        "core_memory_spaces",
        serde_json::json!({}),
    )
    .await
    .expect("core_memory_spaces succeeds");
    spaces["spaces"]
        .as_array()
        .expect("spaces is an array")
        .iter()
        .filter_map(|space| space["key"].as_str())
        .find(|key| *key != "current")
        .expect("group space exists")
        .to_string()
}

async fn call_test_model_tool(
    tools: &CoreMcpTools,
    authz: AuthzContext,
    owner: OwnerId,
    name: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, CoreMcpError> {
    tools
        .call_core_tool(authz, owner, Some("test-model".to_string()), name, args)
        .await
}

async fn read_test_model_resource(
    tools: &CoreMcpTools,
    authz: AuthzContext,
    owner: OwnerId,
    uri: &str,
) -> Result<serde_json::Value, CoreMcpError> {
    tools
        .read_core_resource(authz, owner, Some("test-model".to_string()), uri)
        .await
}

#[tokio::test]
async fn core_memory_tools_route_by_explicit_space_grants() {
    let split_db = clone_split_core_db("proxima_core_memory_spaces_route")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let personal = OwnerId::from(UserId::new(Uuid::now_v7()));
        let shared = OwnerId::from(GroupId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone()).store_number(0)
        .platform_database_url(platform_url.clone())
            .owner(personal)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let scope = proxima_storage_pg::PgPlatformScope::new(
            sqlx::PgPool::connect(&platform_url).await?,
            &["proxima_core"],
        )
        .await?;
        let pg = PgStorage::connect_with_config(
            &runtime_url,
            proxima_storage_pg::PgPoolConfig::from_env()?,
            proxima_storage_pg::PgTuning::from_env()?,
            scope,
            0,
        )
        .await?;
        seed_group_membership(&pg, &shared, Relation::Viewer, &personal).await;
        let authz = space_authz(
            personal,
            vec![personal, shared],
            Role::viewer(),
        );
        let shared_space = server_issued_group_space_selector(&tools, authz.clone(), personal).await;

        let spaces = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_memory_spaces",
            serde_json::json!({}),
        )
        .await?;
        let space_keys = spaces["spaces"]
            .as_array()
            .expect("spaces is an array")
            .iter()
            .map(|space| space["key"].as_str().expect("space key").to_string())
            .collect::<Vec<_>>();
        assert_eq!(space_keys.first().map(String::as_str), Some("current"));
        assert!(space_keys.contains(&shared_space));

        call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_remember",
            serde_json::json!({"space":"current","title":"private","body":"private body","tags":[]}),
        )
        .await?;

        let denied = call_test_model_tool(
            &tools,
            authz,
            personal,
            "core_remember",
            serde_json::json!({"space":shared_space,"title":"leak","body":"should deny","tags":[]}),
        )
        .await;
        assert!(denied.is_err(), "shared write must be denied");

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("explicit memory-space routing test failed");
}

#[tokio::test]
async fn shared_space_include_body_uses_shared_owner() {
    let split_db = clone_split_core_db("proxima_core_memory_spaces_body")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let personal = OwnerId::from(UserId::new(Uuid::now_v7()));
        let shared = OwnerId::from(GroupId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone()).store_number(0)
        .platform_database_url(platform_url.clone())
            .owner(personal)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(
            personal,
            vec![personal, shared],
            Role::admin(),
        );
        let shared_space = server_issued_group_space_selector(&tools, authz.clone(), personal).await;

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_remember",
            serde_json::json!({"space":shared_space.clone(),"title":"shared note","body":"shared body unique needle","tags":["shared"]}),
        )
        .await?;
        let remembered_handle = remembered["handle"].as_str().expect("handle");

        let search = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_search_memories",
            serde_json::json!({
                "query": "unique needle",
                "mode": "lexical",
                "kind": "Fact",
                "spaces": [shared_space.clone()],
                "include_body": true,
                "limit": 5
            }),
        )
        .await?;
        assert_eq!(search["memories"][0]["memory"], remembered_handle);
        assert_eq!(search["memories"][0]["space"], shared_space);
        assert!(search["memories"][0]["body"].as_str().unwrap().contains("shared body"));

        // This deployment has no embedding client, so a hybrid request
        // degrades to lexical ranking. The search verb rejects a fusion
        // weight paired with a non-hybrid mode, and the caller broke no
        // such rule — they asked for hybrid. The tool must therefore drop
        // the weight along with the semantic component it was weighting,
        // rather than forward a pairing the verb refuses.
        let degraded = call_test_model_tool(
            &tools,
            authz,
            personal,
            "core_search_memories",
            serde_json::json!({
                "query": "unique needle",
                "mode": "hybrid",
                "semantic_weight": 0.7,
                "kind": "Fact",
                "spaces": [shared_space.clone()],
                "limit": 5
            }),
        )
        .await?;
        assert_eq!(
            degraded["degraded_to_lexical"], true,
            "no embedding client means hybrid ranks lexically: {degraded}"
        );
        assert_eq!(
            degraded["memories"][0]["memory"], remembered_handle,
            "the degraded search still returns the hit: {degraded}"
        );

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("shared-space search body test failed");
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // legal Fact→A setup plus cross-space A→A assertion is intentionally end-to-end
async fn cross_space_derive_succeeds_when_sources_readable() {
    let split_db = clone_split_core_db("proxima_core_memory_spaces_derive")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let personal = OwnerId::from(UserId::new(Uuid::now_v7()));
        let shared = OwnerId::from(GroupId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone()).store_number(0)
        .platform_database_url(platform_url.clone())
            .owner(personal)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(
            personal,
            vec![personal, shared],
            Role::admin(),
        );
        let shared_space = server_issued_group_space_selector(&tools, authz.clone(), personal).await;

        let personal_fact = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_remember",
            serde_json::json!({"space":"current","title":"personal fact","body":"personal source","tags":[]}),
        )
        .await?;
        let shared_fact = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_remember",
            serde_json::json!({"space":shared_space,"title":"shared fact","body":"shared source","tags":[]}),
        )
        .await?;

        let personal_abstraction = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_derive",
            serde_json::json!({
                "space": "current",
                "kind": "Abstraction",
                "title": "personal abstraction",
                "body": "personal source abstraction",
                "tags": [],
                "source_handles": [personal_fact["handle"].as_str().unwrap()],
                "model_id": "test-model"
            }),
        )
        .await?;
        let shared_abstraction = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_derive",
            serde_json::json!({
                "space": shared_space,
                "kind": "Abstraction",
                "title": "shared abstraction",
                "body": "shared source abstraction",
                "tags": [],
                "source_handles": [shared_fact["handle"].as_str().unwrap()],
                "model_id": "test-model"
            }),
        )
        .await?;

        let derived = call_test_model_tool(
            &tools,
            authz.clone(),
            personal,
            "core_derive",
            serde_json::json!({
                "space": "current",
                "kind": "Abstraction",
                "title": "cross-space pattern",
                "body": "personal and shared abstractions are readable",
                "tags": [],
                "source_handles": [personal_abstraction["handle"].as_str().unwrap(), shared_abstraction["handle"].as_str().unwrap()],
                "model_id": "test-model"
            }),
        )
        .await?;
        assert!(
            derived["handle"]
                .as_str()
                .is_some_and(|handle| Uuid::parse_str(handle).is_ok()),
            "cross-space derive must return an Abstraction handle, got {derived}"
        );
        // Two `origin` rows, reported as a count: an edge has no handle to
        // hand back, and re-running the derivation re-asserts the same rows.
        assert_eq!(derived["edge_count"], serde_json::json!(2));

        let perspective = call_test_model_tool(
            &tools, authz.clone(), personal, "core_derive",
            serde_json::json!({"kind":"Perspective", "title":"stored layer", "body":"an interpretation",
                "source_handles":[personal_abstraction["handle"]], "model_id":"test-model"}),
        ).await?;
        assert!(Uuid::parse_str(perspective["handle"].as_str().expect("bare perspective UUID")).is_ok());
        for (kind, sources, reason) in [
            ("Perspective", vec![personal_fact["handle"].clone()], "Fact"),
            ("Abstraction", vec![personal_fact["handle"].clone(), personal_abstraction["handle"].clone()], "layer"),
        ] {
            let error = call_test_model_tool(
                &tools, authz.clone(), personal, "core_derive",
                serde_json::json!({"kind":kind, "title":"refused layer", "body":"must not be written",
                    "source_handles":sources, "model_id":"test-model"}),
            ).await.expect_err("stored source layer is authoritative");
            assert!(error.to_string().contains(reason), "{error}");
        }
        let uuid = personal_fact["handle"].as_str().expect("fact UUID");
        for prefix in ["F", "A", "P", "G"] {
            let error = call_test_model_tool(
                &tools, authz.clone(), personal, "core_derive",
                serde_json::json!({"kind":"Abstraction", "title":"refused prefix", "body":"bare UUID required",
                    "source_handles":[format!("{prefix}:{uuid}")], "model_id":"test-model"}),
            ).await.expect_err("letters never enter the handle grammar");
            assert!(error.to_string().contains("UUID"), "{error}");
        }

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("cross-space derive success test failed");
}

/// The facade projects the descriptor's output schema, like the MCP handler
/// and the REST document already do. A host that bound a call's result off
/// this listing had to guess its shape.
fn assert_facade_projects_output_schema(registry: &FlavorRegistryFrozen, tool: &CoreToolInfo) {
    let descriptor = registry
        .list_mcp_tools()
        .iter()
        .find(|descriptor| provider_safe_tool_name(descriptor.name) == tool.name)
        .expect("listed tool has a registered descriptor");
    assert!(
        tool.output_schema
            .as_object()
            .is_some_and(|object| !object.is_empty()),
        "{}: a known core tool has a non-empty output schema",
        tool.name
    );
    assert_eq!(tool.output_schema, descriptor.output_schema);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn facade_lists_and_dispatches_core_mcp_tools() {
    let split_db = clone_split_core_db("proxima_core_mcp")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(runtime_url.clone()).store_number(0)
        .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();

        let listed = tools.list_core_tools();
        assert!(!listed.is_empty(), "core tool registry is non-empty");
        let retired_personality = format!("core_{}", "personality");
        assert!(
            !listed
                .iter()
                .any(|tool| tool.name == retired_personality.as_str())
        );
        let recall = listed
            .iter()
            .find(|tool| tool.name == "core_recall")
            .expect("core_recall registered");
        assert_eq!(recall.effect, Some(ToolEffect::ReadOnly));
        assert_facade_projects_output_schema(built.host().registry(), recall);
        let search = listed
            .iter()
            .find(|tool| tool.name == "core_search_memories")
            .expect("core/search_memories registered");
        assert!(
            search
                .args_schema
                .as_object()
                .is_some_and(|object| !object.is_empty()),
            "known core tool has a non-empty args schema"
        );
        assert_eq!(search.effect, Some(ToolEffect::ReadOnly));
        assert_facade_projects_output_schema(built.host().registry(), search);
        let remember = listed
            .iter()
            .find(|tool| tool.name == "core_remember")
            .expect("core/remember registered");
        assert_eq!(
            remember.effect,
            Some(ToolEffect::Additive(Replay::NonIdempotent))
        );

        let palette = tools.list_core_tools_for_scope(&palette_of(&["core_search_memories"]));
        // #489: exact caller scope admits only the named search key.
        assert_eq!(palette.len(), 1);
        assert_eq!(palette[0].name, "core_search_memories");
        let upload_palette = tools.list_core_tools_for_scope(&palette_of(&["core_upload:read_url"]));
        // The explicitly admitted read leaf still projects a read-only schema.
        let upload = upload_palette
            .iter()
            .find(|tool| tool.name == "core_upload")
            .expect("core_upload is listed for its read action");
        assert_eq!(upload.effect, Some(ToolEffect::ReadOnly));
        let upload_schema = upload.args_schema.to_string();
        assert!(upload_schema.contains("read_url"), "{upload_schema}");
        assert!(!upload_schema.contains("\"prepare\""), "{upload_schema}");
        let palette_names: HashSet<_> = palette.into_iter().map(|tool| tool.name).collect();
        assert!(palette_names.contains("core_search_memories"));
        assert!(!palette_names.contains("core_remember"));
        assert!(
            !palette_names.contains("core_membership"),
            "an owner-audience read stays with the palette"
        );
        assert!(!palette_names.contains(retired_personality.as_str()));

        let admin_authz = host_authz(&owner, ToolScope::All);
        let output = call_test_model_tool(
            &tools,
            admin_authz,
            owner,
            "core_search_memories",
            serde_json::json!({
                "query": "nothing yet",
                "mode": "lexical",
                "kind": "Fact",
                "limit": 5
            }),
        )
        .await?;
        assert!(output.get("memories").is_some(), "read tool returns JSON");

        let palette_denied = tools
            .call_core_tool(
                host_authz(&owner, palette_of(&["core_search_memories"])),
                owner,
                None,
                "core_remember",
                serde_json::json!({
                    "title": "denied",
                    "body": "palette must be a call gate",
                    "idempotency_key": "palette-denied-remember"
                }),
            )
            .await;
        assert!(
            matches!(palette_denied, Err(CoreMcpError::NotAuthorized(ref tool)) if tool == "core_remember"),
            "palette must deny the call, not only hide the tool: {palette_denied:?}"
        );

        let empty_request = built
            .host().core_mcp_tools_with_request_services(FlavorServices::default())
            .expect("empty request bag merges");
        let listed_after_merge = empty_request.list_core_tools();
        assert!(
            listed_after_merge
                .iter()
                .any(|tool| tool.name == "core_search_memories")
        );

        let retired = tools
            .call_core_tool(
                host_authz(&owner, ToolScope::All),
                owner,
                None,
                &retired_personality,
                serde_json::json!({ "action": "list" }),
            )
            .await;
        assert!(
            matches!(retired, Err(CoreMcpError::NotFound(tool)) if tool == retired_personality)
        );

        let invalid = tools
            .read_core_resource(
                host_authz(&owner, ToolScope::All),
                owner,
                None,
                "proxima://memory/not-a-memory-id",
            )
            .await
            .expect_err("invalid memory handle rejected");
        assert_eq!(invalid.kind(), CoreMcpErrorKind::InvalidInput);

        let unknown = tools
            .call_core_tool(
                host_authz(&owner, ToolScope::All),
                owner,
                None,
                "core/not_a_tool",
                serde_json::json!({}),
            )
            .await;
        assert!(matches!(unknown, Err(CoreMcpError::NotFound(tool)) if tool == "core/not_a_tool"));

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("core MCP facade integration test failed");
}

#[tokio::test]
async fn facade_reads_core_resources_with_resource_scope() {
    let split_db = clone_split_core_db("proxima_core_resource_mcp")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Resource facade",
                "body": "Read through MCP resource surface.",
                "tags": ["resource"],
                "idempotency_key": "facade-core-resource-read"
            }),
        )
        .await?;
        let memory = remembered["handle"].as_str().expect("remembered handle");

        let resource_memory = read_test_model_resource(
            &tools,
            authz.clone(),
            owner,
            &format!("proxima://memory/{memory}"),
        )
        .await?;
        assert_eq!(resource_memory["memory"], memory);

        let resource_schemas =
            read_test_model_resource(&tools, authz.clone(), owner, "proxima://schemas").await?;
        assert!(
            !resource_schemas["schemas"]
                .as_array()
                .expect("schemas")
                .is_empty()
        );

        // #489: a caller's resource keys are exact, including reads.
        let denied = read_test_model_resource(
            &tools,
            host_authz(&owner, palette_of(&["resource:schemas"])),
            owner,
            &format!("proxima://memory/{memory}"),
        )
        .await;
        assert!(
            matches!(denied, Err(CoreMcpError::NotAuthorized(_))),
            "an omitted resource is refused by the caller scope: {denied:?}"
        );
        let memory_scope = || host_authz(&owner, palette_of(&["resource:memory"]));
        let admitted = read_test_model_resource(
            &tools,
            memory_scope(),
            owner,
            &format!("proxima://memory/{memory}"),
        )
        .await?;
        assert_eq!(admitted["memory"], memory);
        let schemas =
            read_test_model_resource(&tools, memory_scope(), owner, "proxima://schemas").await;
        assert!(
            matches!(schemas, Err(CoreMcpError::NotAuthorized(_))),
            "an admitted memory key grants no schemas resource: {schemas:?}"
        );

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("core MCP resource facade integration test failed");
}

/// #466: a Fact keeps when it occurred. The `observed_at` given to
/// `core_remember` reads back as `occurred_at` from `proxima://memory/{id}`,
/// from a search hit and on the query verb's `MemoryRow`; a Fact written
/// without one reads back its own write time; an Abstraction carries none;
/// and a time beyond the future bound is refused as invalid input.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one remembered corpus read back through every read surface
async fn a_remembered_observation_reads_back_when_it_occurred() {
    let split_db = clone_split_core_db("proxima_core_occurred_at")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let writer = proxima_core::WriterAttribution {
            by: Uuid::now_v7(),
            via: Some(Uuid::now_v7()),
        };
        let next_writer = proxima_core::WriterAttribution {
            by: Uuid::now_v7(),
            via: None,
        };
        let attested = |writer| {
            verified(
                AuthzContext::for_subject(owner.as_user(), AuthPath::HostBearer)
                    .with_attested_writer(writer)
                    .expect("unsealed host attestation"),
            )
        };
        let authz = attested(writer);
        let rfc3339 = &time::format_description::well_known::Rfc3339;
        let parse = |value: &serde_json::Value| {
            time::OffsetDateTime::parse(value.as_str().expect("an RFC 3339 string"), rfc3339)
                .expect("a valid RFC 3339 time")
        };
        let observed = time::OffsetDateTime::parse("2021-06-01T10:20:30.456789Z", rfc3339)?;

        let backdated = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Quay survey",
                "body": "occurrence needle seen last summer",
                "tags": ["occurrence"],
                "observed_at": "2021-06-01T12:20:30.456789+02:00",
                "idempotency_key": "writer-first-observation"
            }),
        )
        .await?;
        let backdated = backdated["handle"].as_str().expect("handle").to_owned();
        let replay = call_test_model_tool(
            &tools,
            attested(next_writer),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Quay survey",
                "body": "occurrence needle seen last summer",
                "tags": ["occurrence"],
                "observed_at": "2021-06-01T12:20:30.456789+02:00",
                "idempotency_key": "writer-first-observation"
            }),
        )
        .await?;
        assert_eq!(
            replay["handle"], backdated,
            "exact replay retains its first version"
        );
        let current = call_test_model_tool(
            &tools,
            attested(next_writer),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Quay survey again",
                "body": "occurrence needle seen today",
                "tags": ["occurrence"]
            }),
        )
        .await?;
        let current = current["handle"].as_str().expect("handle").to_owned();
        let memory_id = |read: &serde_json::Value| {
            let memory = read["memory"].as_str().expect("a memory UUID");
            let t = memory;
            MemoryId::new(t.parse::<Uuid>().expect("a memory id"))
        };

        // proxima://memory/{id}
        let read = read_test_model_resource(
            &tools,
            authz.clone(),
            owner,
            &format!("proxima://memory/{backdated}"),
        )
        .await?;
        let backdated_id = memory_id(&read);
        assert_eq!(parse(&read["occurred_at"]), observed);
        assert_eq!(read["by"], serde_json::json!(writer.by));
        assert_eq!(read.get("via"), Some(&serde_json::json!(writer.via)));
        let read = read_test_model_resource(
            &tools,
            authz.clone(),
            owner,
            &format!("proxima://memory/{current}"),
        )
        .await?;
        let current_id = memory_id(&read);
        assert_eq!(read["by"], serde_json::json!(next_writer.by));
        assert_eq!(read.get("via"), Some(&serde_json::Value::Null));
        assert_eq!(
            parse(&read["occurred_at"]),
            current_id.created_at(),
            "a Fact written without a time occurred at its write time"
        );

        // core_search_memories
        let found = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_search_memories",
            serde_json::json!({ "query": "occurrence needle", "kind": "Fact", "limit": 5 }),
        )
        .await?;
        let hit = found["memories"]
            .as_array()
            .expect("memories array")
            .iter()
            .find(|hit| hit["memory_id"] == backdated_id.into_inner().to_string())
            .unwrap_or_else(|| panic!("the backdated Fact is found: {found}"));
        assert_eq!(parse(&hit["occurred_at"]), observed);
        assert_eq!(hit["by"], serde_json::json!(writer.by));
        assert_eq!(hit.get("via"), Some(&serde_json::json!(writer.via)));

        // An Abstraction is a conclusion, not an observation.
        let derived = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_derive",
            serde_json::json!({
                "kind": "Abstraction",
                "title": "Quay pattern",
                "body": "the quay keeps being surveyed",
                "tags": [],
                "source_handles": [backdated, current],
                "model_id": "test-model"
            }),
        )
        .await?;
        let derived = derived["handle"].as_str().expect("handle").to_owned();
        let read = read_test_model_resource(
            &tools,
            authz.clone(),
            owner,
            &format!("proxima://memory/{derived}"),
        )
        .await?;
        let derived_id = memory_id(&read);
        assert_eq!(read["by"], serde_json::json!(writer.by));
        assert_eq!(read.get("via"), Some(&serde_json::json!(writer.via)));
        assert!(read.get("occurred_at").is_none(), "{read}");

        // The query verb's `MemoryRow`.
        let rows = built
            .host()
            .engine()
            .query(
                &verified(AuthzContext::for_subject(
                    owner.as_user(),
                    AuthPath::HostBearer,
                )),
                &proxima_core::verbs::query::QueryRequest::readable(),
            )
            .await?
            .memories;
        let writers: std::collections::HashMap<_, _> =
            rows.iter().map(|row| (row.id, row.writer)).collect();
        assert_eq!(
            writers,
            std::collections::HashMap::from([
                (backdated_id, writer),
                (current_id, next_writer),
                (derived_id, writer),
            ])
        );
        let occurred: std::collections::HashMap<_, _> =
            rows.iter().map(|row| (row.id, row.occurred_at)).collect();
        assert_eq!(
            occurred,
            std::collections::HashMap::from([
                (backdated_id, Some(observed)),
                (current_id, Some(current_id.created_at())),
                (derived_id, None),
            ])
        );

        // One bound: beyond five minutes ahead is invalid input.
        let future =
            (time::OffsetDateTime::now_utc() + time::Duration::minutes(10)).format(rfc3339)?;
        let refused = call_test_model_tool(
            &tools,
            authz,
            owner,
            "core_remember",
            serde_json::json!({
                "title": "From the future",
                "body": "not yet observed",
                "tags": [],
                "observed_at": future
            }),
        )
        .await
        .expect_err("an occurrence beyond the future bound is refused");
        assert_eq!(refused.kind(), CoreMcpErrorKind::InvalidInput);

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("core occurrence read-back integration test failed");
}

async fn assert_exact_caller_reads(
    tools: &CoreMcpTools,
    owner: OwnerId,
    served: &proxima_core::ServedSurface,
    mine: &str,
    theirs: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let exact =
        |keys: &[&str]| host_authz(&owner, palette_of(keys)).with_served_surface(served.clone());
    let omitted_search = call_test_model_tool(
        tools,
        exact(&["flavor_command"]),
        owner,
        "core_search_memories",
        serde_json::json!({"query":"palette","mode":"lexical","limit":10}),
    )
    .await;
    assert!(matches!(
        omitted_search,
        Err(CoreMcpError::NotAuthorized(_))
    ));
    let omitted_memory = read_test_model_resource(
        tools,
        exact(&["flavor_command"]),
        owner,
        &format!("proxima://memory/{mine}"),
    )
    .await;
    assert!(matches!(
        omitted_memory,
        Err(CoreMcpError::NotAuthorized(_))
    ));
    let requested = || exact(&["core_search_memories", "resource:memory"]);
    let search = call_test_model_tool(
        tools,
        requested(),
        owner,
        "core_search_memories",
        serde_json::json!({"query":"palette","mode":"lexical","limit":10}),
    )
    .await?;
    assert!(
        search["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["memory"] == mine)
    );
    assert!(
        !search["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["memory"] == theirs)
    );
    let memory = read_test_model_resource(
        tools,
        requested(),
        owner,
        &format!("proxima://memory/{mine}"),
    )
    .await?;
    assert_eq!(memory["memory"], mine);
    let schemas = read_test_model_resource(tools, requested(), owner, "proxima://schemas").await;
    assert!(matches!(schemas, Err(CoreMcpError::NotAuthorized(_))));
    Ok(())
}

/// #462 and #489: a deployment's flavor-only surface keeps the owner's reads,
/// while the caller's keys independently restrict those reads. Owner RLS
/// continues to hide other owners' memories.
#[tokio::test]
async fn a_flavor_only_palette_reads_the_owners_memory_and_refuses_writes() {
    let split_db = clone_split_core_db("proxima_core_palette_reads")
        .await
        .expect("PG required for tests");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let other = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(split_db.runtime_url())
            .store_number(0)
            .platform_database_url(split_db.platform_url())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();

        let remember = |label: &'static str| {
            serde_json::json!({
                "title": format!("Palette read {label}"),
                "body": "A deployment keeps its reads while a caller selects exact keys.",
                "tags": ["palette"],
                "idempotency_key": format!("palette-read-{label}")
            })
        };
        let mine = call_test_model_tool(
            &tools,
            host_authz(&owner, ToolScope::All),
            owner,
            "core_remember",
            remember("mine"),
        )
        .await?;
        let mine = mine["handle"].as_str().expect("handle").to_owned();
        let theirs = call_test_model_tool(
            &tools,
            host_authz(&other, ToolScope::All),
            other,
            "core_remember",
            remember("theirs"),
        )
        .await?;
        let theirs = theirs["handle"].as_str().expect("handle").to_owned();

        let served = proxima_core::ServedSurface::Palette(vec![
            proxima_core::ScopeKey::parse("flavor_command").unwrap(),
        ]);
        let flavor_only = || host_authz(&owner, ToolScope::All).with_served_surface(served.clone());

        let found = call_test_model_tool(
            &tools,
            flavor_only(),
            owner,
            "core_search_memories",
            serde_json::json!({ "query": "palette", "mode": "lexical", "limit": 10 }),
        )
        .await?;
        let found: Vec<&str> = found["memories"]
            .as_array()
            .expect("memories")
            .iter()
            .filter_map(|hit| hit["memory"].as_str())
            .collect();
        assert!(found.contains(&mine.as_str()), "{found:?}");
        assert!(!found.contains(&theirs.as_str()), "{found:?}");

        let read = read_test_model_resource(
            &tools,
            flavor_only(),
            owner,
            &format!("proxima://memory/{mine}"),
        )
        .await?;
        assert_eq!(read["memory"], mine.as_str());

        let write = call_test_model_tool(
            &tools,
            flavor_only(),
            owner,
            "core_remember",
            remember("refused"),
        )
        .await;
        assert!(
            matches!(write, Err(CoreMcpError::NotAuthorized(ref name)) if name == "core_remember"),
            "a write outside the palette is refused: {write:?}"
        );

        let foreign = read_test_model_resource(
            &tools,
            flavor_only(),
            owner,
            &format!("proxima://memory/{theirs}"),
        )
        .await;
        // Refused by owner authorization, which hides it, not by the palette.
        assert!(
            matches!(&foreign, Err(err) if err.kind() == CoreMcpErrorKind::NotFound),
            "owner authorization still refuses another owner's memory: {foreign:?}"
        );

        assert_exact_caller_reads(&tools, owner, &served, &mine, &theirs).await?;

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("palette read integration test failed");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn facade_core_recall_returns_cue_packet_and_rejects_empty_cue() {
    let split_db = clone_split_core_db("proxima_core_recall_mcp")
        .await
        .expect("PG required for tests");
    let db_name = split_db.name().to_owned();
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let admin_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        let empty = tools
            .call_core_tool(
                authz.clone(),
                owner,
                Some("test-model".to_string()),
                "core_recall",
                serde_json::json!({}),
            )
            .await;
        assert!(
            empty.is_err(),
            "empty cue must fail: Self is not parameterless"
        );

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Cue fact",
                "body": "recall-subject-needle unique observation",
                "tags": ["recall"],
                "idempotency_key": "facade-core-recall-fact"
            }),
        )
        .await?;
        let fact = remembered["handle"].as_str().expect("fact handle");
        let fact_t = fact.parse::<Uuid>()?;
        let stored_sketch: String =
            sqlx::query_scalar("SELECT text FROM proxima_core.sketch WHERE t = $1")
                .bind(fact_t)
                .fetch_one(&admin_pool)
                .await?;
        assert_eq!(stored_sketch, "Cue fact");

        let derived = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_derive",
            serde_json::json!({
                "kind": "Abstraction",
                "title": "Recall pattern",
                "body": "recall-subject-needle repeats as a pattern",
                "tags": ["recall"],
                "source_handles": [fact],
                "model_id": "test-model",
                "idempotency_key": "facade-core-recall-abstraction"
            }),
        )
        .await?;
        let abstraction = derived["handle"].as_str().expect("abstraction handle");

        let interpreted = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_interpret",
            serde_json::json!({
                "claim": "the recall-subject-needle observation is a stance I hold",
                "confidence": 80,
                "subjects": [fact]
            }),
        )
        .await?;
        let perspective = interpreted["handle"].as_str().expect("perspective handle");

        let goal = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_goal",
            serde_json::json!({
                "action": "set",
                "schema_id": "core/simple-text-v1",
                "title": "Act on the cue stance",
                "text": "Pursue the interpretation",
                "body": {},
                "evidence": [abstraction],
                "target_perspective": perspective,
                "idempotency_key": "facade-core-recall-goal"
            }),
        )
        .await?;
        let goal_handle = goal["handle"].as_str().expect("goal handle");

        let packet = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_recall",
            serde_json::json!({
                "subjects": [fact],
                "kind": "Perspective",
                "limit": 2
            }),
        )
        .await?;
        let sketches = packet["sketches"]
            .as_array()
            .expect("sketches")
            .iter()
            .map(|row| {
                (
                    row["handle"].as_str().unwrap_or_default().to_string(),
                    row["kind"].as_str().unwrap_or_default().to_string(),
                    row["reason"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect::<Vec<_>>();
        assert!(
            packet["sketches"]
                .as_array()
                .expect("sketches")
                .iter()
                .any(|row| {
                    row["handle"].as_str() == Some(perspective)
                        && row["kind"].as_str() == Some("Perspective")
                        && row["reason"].as_str() == Some("cue_touch")
                        && row["sketch"]
                            .as_str()
                            .is_some_and(|sketch| sketch.contains("stance I hold"))
                }),
            "cue-touch Perspective missing or sketch empty: {packet}"
        );
        assert!(
            sketches
                .iter()
                .any(|(handle, kind, reason)| handle == goal_handle
                    && kind == "Goal"
                    && reason == "assigned_goal"),
            "assigned Active Goal missing from Self packet: {packet}"
        );

        let goal_t = goal_handle.parse::<Uuid>()?;
        sqlx::query("DELETE FROM proxima_core.sketch WHERE t = $1")
            .bind(goal_t)
            .execute(&admin_pool)
            .await?;
        let without_goal = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_recall",
            serde_json::json!({
                "subjects": [fact],
                "kind": "Perspective",
                "limit": 8
            }),
        )
        .await?;
        assert!(
            without_goal["sketches"]
                .as_array()
                .expect("sketches")
                .iter()
                .all(|row| row["handle"].as_str() != Some(goal_handle)),
            "assigned Goal without a persisted sketch must be omitted: {without_goal}"
        );

        let by_question = call_test_model_tool(
            &tools,
            authz,
            owner,
            "core_recall",
            serde_json::json!({
                "question": "recall-subject-needle",
                "limit": 16
            }),
        )
        .await?;
        let question_hits = by_question["sketches"]
            .as_array()
            .expect("sketches")
            .iter()
            .filter_map(|row| row["handle"].as_str())
            .collect::<Vec<_>>();
        assert!(
            question_hits.contains(&fact),
            "question cue must find the Fact: {by_question}"
        );

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("core recall MCP facade integration test failed");
}

/// The `PayloadOnly` arm, end to end: an interpretation's ancestors are its
/// SUBJECTS.
///
/// `core/interpretation-v1` writes `derived_from: &[]` on both of its ingest
/// paths — an interpretation is not made FROM its subjects, it is made ABOUT
/// them — so `memory.origins` is empty and a walk that expanded `origins[]`
/// and nothing else would stop dead at every interpretation. The subjects
/// live in `interpretation_v1.subject_memory_ids`, which is exactly what
/// `Provenance::PayloadOnly { subject_columns }` declares.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn facade_core_think_reaches_an_interpretations_subject_through_its_payload() {
    let split_db = clone_split_core_db("proxima_core_think_payload")
        .await
        .expect("PG required for tests");
    let db_name = split_db.name().to_owned();
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let admin_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        let subject = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Interpretation subject",
                "body": "the thing being interpreted",
                "tags": ["provenance"],
                "idempotency_key": "facade-provenance-subject"
            }),
        )
        .await?;
        let subject_handle = subject["handle"].as_str().expect("subject").to_owned();

        let interpretation = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_interpret",
            serde_json::json!({
                "claim": "this is what the subject means",
                "confidence": 90,
                "subjects": [subject_handle.clone()],
                "model_id": "test-model"
            }),
        )
        .await?;
        let interpretation_handle = interpretation["handle"]
            .as_str()
            .expect("interpretation")
            .to_owned();

        // The premise, asked of the row itself: the interpretation carries
        // NO origins, and the subject is in the payload column the
        // declaration names. If either becomes false the declaration is
        // wrong, not this test.
        let interpretation_t = interpretation_handle.parse::<Uuid>()?;
        let origins: i64 = sqlx::query_scalar(
            "SELECT cardinality(origins)::bigint FROM proxima_core.memory WHERE t = $1",
        )
        .bind(interpretation_t)
        .fetch_one(&admin_pool)
        .await?;
        assert_eq!(
            origins, 0,
            "an interpretation is made ABOUT its subjects, not FROM them"
        );
        let subjects: i64 = sqlx::query_scalar(
            "SELECT cardinality(subject_memory_ids)::bigint
               FROM proxima_core.interpretation_v1 WHERE t = $1",
        )
        .bind(interpretation_t)
        .fetch_one(&admin_pool)
        .await?;
        assert_eq!(subjects, 1, "the subject lives in the declared column");

        let page = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_think",
            serde_json::json!({
                "seeds": [interpretation_handle],
                "direction": "ancestors",
                "depth": 2,
                "limit": 16
            }),
        )
        .await?;
        let handles = page["visits"]
            .as_array()
            .expect("visits")
            .iter()
            .filter_map(|visit| visit["handle"].as_str())
            .collect::<Vec<_>>();
        assert!(
            handles.contains(&subject_handle.as_str()),
            "the subject is reached through the declared subject_columns, not \
             through origins: {page}"
        );

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("core_think payload-provenance integration test failed");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn facade_core_think_pages_ancestors_from_a_derivation() {
    let split_db = clone_split_core_db("proxima_core_think_mcp")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        let empty = tools
            .call_core_tool(
                authz.clone(),
                owner,
                Some("test-model".to_string()),
                "core_think",
                serde_json::json!({ "seeds": [] }),
            )
            .await;
        assert!(empty.is_err(), "think requires a seed");

        let fact = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Think source",
                "body": "think-ancestor-needle observation",
                "tags": ["think"],
                "idempotency_key": "facade-core-think-fact"
            }),
        )
        .await?;
        let fact_handle = fact["handle"].as_str().expect("fact");

        let derived = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_derive",
            serde_json::json!({
                "kind": "Abstraction",
                "title": "Think pattern",
                "body": "think-ancestor-needle is a pattern",
                "tags": ["think"],
                "source_handles": [fact_handle],
                "model_id": "test-model",
                "idempotency_key": "facade-core-think-abstraction"
            }),
        )
        .await?;
        let abstraction = derived["handle"].as_str().expect("abstraction");

        let page = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_think",
            serde_json::json!({
                "seeds": [abstraction],
                "direction": "ancestors",
                "depth": 3,
                "limit": 16
            }),
        )
        .await?;
        assert_eq!(page["direction"], "ancestors");
        let handles = page["visits"]
            .as_array()
            .expect("visits")
            .iter()
            .filter_map(|visit| visit["handle"].as_str())
            .collect::<Vec<_>>();
        assert!(
            handles.contains(&fact_handle),
            "ancestor page must include the source Fact: {page}"
        );

        let down = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_think",
            serde_json::json!({
                "seeds": [fact_handle],
                "direction": "descendants",
                "depth": 3,
                "limit": 16
            }),
        )
        .await?;
        let down_handles = down["visits"]
            .as_array()
            .expect("visits")
            .iter()
            .filter_map(|visit| visit["handle"].as_str())
            .collect::<Vec<_>>();
        assert!(
            down_handles.contains(&abstraction),
            "descendant page must include the Abstraction: {down}"
        );

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("core think MCP facade integration test failed");
}

#[tokio::test]
async fn facade_core_episode_commit_binds_only_listed_members() {
    let split_db = clone_split_core_db("proxima_core_episode_mcp")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        let committed = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            serde_json::json!({
                "remember": [
                    {"title": "Episode a", "body": "first bound observation", "tags": ["ep"]},
                    {"title": "Episode b", "body": "second bound observation", "tags": ["ep"]}
                ],
                "bind": ["remember:0", "remember:1"]
            }),
        )
        .await?;
        let a = committed["remembered"][0].as_str().expect("a");
        let b = committed["remembered"][1].as_str().expect("b");
        assert_eq!(committed["bound"].as_array().map(Vec::len), Some(2));

        let siblings = call_test_model_tool(
            &tools,
            authz,
            owner,
            "core_think",
            serde_json::json!({
                "seeds": [a],
                "direction": "episode_siblings",
                "limit": 8
            }),
        )
        .await?;
        let handles = siblings["visits"]
            .as_array()
            .expect("visits")
            .iter()
            .filter_map(|visit| visit["handle"].as_str())
            .collect::<Vec<_>>();
        assert!(
            handles.contains(&b),
            "bound sibling must appear: {siblings}"
        );

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("core episode commit MCP facade integration test failed");
}

/// A bound principal, as the OIDC subject map produces for a configured
/// runner.
fn trusted_host_authz(owner: &OwnerId, trusted_model_id: &str) -> ResolvedAuthz {
    host_authz(owner, ToolScope::All)
        .with_trusted_model_id(trusted_model_id)
        .expect("a well-formed runner id binds")
}

/// Regression: `core_episode_commit` carries `model_id` *per item*
/// (`derive.model_id`, `stance[].model_id`). The transport edges only ever
/// see a top-level field, so neither `author_from_args` nor
/// `reject_reserved_arguments` can see these — before the rule moved into
/// core's `operator_label`, a caller string landed in
/// `AgentDerivationV1.model_id` / `InterpretationV1.model_id` and in the
/// `{model_id}:{hex}` idempotency key while the token said otherwise.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn facade_core_episode_commit_refuses_nested_model_id_against_the_bound_identity() {
    let split_db = clone_split_core_db("proxima_core_episode_trusted")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = trusted_host_authz(&owner, "acme/runner-v3");

        let episode = |derive_label: Option<&str>, stance_label: Option<&str>| {
            let mut derive = serde_json::json!({
                "title": "Episode pattern",
                "body": "the two observations form a pattern",
                "source_handles": ["remember:0", "remember:1"],
            });
            if let Some(label) = derive_label {
                derive["model_id"] = serde_json::json!(label);
            }
            let mut stance = serde_json::json!({
                "claim": "the pattern is the stance I hold in this episode",
                "confidence": 80,
                "subjects": ["remember:0", "derive"],
            });
            if let Some(label) = stance_label {
                stance["model_id"] = serde_json::json!(label);
            }
            serde_json::json!({
                "remember": [
                    {"title": "Bound fact", "body": "first observation", "tags": ["ep"]},
                    {"title": "Other fact", "body": "second observation", "tags": ["ep"]}
                ],
                "derive": derive,
                "stance": [stance],
            })
        };

        // A nested claim that contradicts the token is refused, from either slot.
        for (derive_label, stance_label) in [
            (Some("anthropic/claude-opus-5"), None),
            (None, Some("openai/gpt-9")),
        ] {
            let err = tools
                .call_core_tool(
                    authz.clone(),
                    owner,
                    None,
                    "core_episode_commit",
                    episode(derive_label, stance_label),
                )
                .await
                .expect_err("a nested model_id may not relabel an authenticated runner");
            assert_eq!(err.kind(), CoreMcpErrorKind::InvalidInput);
            assert!(
                err.to_string()
                    .contains("authenticated token already binds"),
                "{err}"
            );
        }

        // Agreeing is accepted, and an omitted one records the bound identity.
        for (derive_label, stance_label) in [
            (Some("acme/runner-v3"), Some("acme/runner-v3")),
            (None, None),
        ] {
            let committed = tools
                .call_core_tool(
                    authz.clone(),
                    owner,
                    None,
                    "core_episode_commit",
                    episode(derive_label, stance_label),
                )
                .await?;
            let derived = committed["derived"].as_str().expect("derived");
            let stance = committed["stances"][0].as_str().expect("stance");

            for handle in [derived, stance] {
                // No host label: the shared precedence applies to resource
                // reads through this API too, so passing one that disagrees
                // with the token would be refused here as well.
                let memory = tools
                    .read_core_resource(
                        authz.clone(),
                        owner,
                        None,
                        &format!("proxima://memory/{handle}"),
                    )
                    .await?;
                assert_eq!(
                    memory["payload"]["model_id"], "acme/runner-v3",
                    "the persisted operator label is the bound identity, not a caller string: \
                     {memory}"
                );
            }
        }

        Ok(())
    }
    .await;

    result.expect("episode nested model_id binding test failed");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn facade_core_episode_commit_bound_replay_fails() {
    let split_db = clone_split_core_db("proxima_core_episode_replay")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            serde_json::json!({
                "remember": [{
                    "title": "Replay seed",
                    "body": "seed observation",
                    "tags": ["ep"],
                    "idempotency_key": "episode-replay-seed"
                }]
            }),
        )
        .await?;
        let remember_err = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            serde_json::json!({
                "remember": [{
                    "title": "Replay seed",
                    "body": "seed observation",
                    "tags": ["ep"],
                    "idempotency_key": "episode-replay-seed"
                }],
                "bind": ["remember:0"]
            }),
        )
        .await
        .expect_err("bound remember replay must fail");
        assert!(
            remember_err.to_string().contains("replayed"),
            "remember H9: {remember_err}"
        );

        let derive_unbound = serde_json::json!({
            "remember": [{
                "title": "Derive source",
                "body": "fresh fact for derive replay",
                "tags": ["ep"],
                "idempotency_key": "episode-replay-derive-src"
            }],
            "derive": {
                "title": "Replay pattern",
                "body": "same pattern body",
                "source_handles": ["remember:0"],
                "model_id": "test-model",
                "idempotency_key": "episode-replay-derive"
            }
        });
        call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            derive_unbound.clone(),
        )
        .await?;
        let mut derive_bound = derive_unbound;
        derive_bound["bind"] = serde_json::json!(["derive"]);
        let derive_err = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            derive_bound,
        )
        .await
        .expect_err("bound derive replay must fail");
        assert!(
            derive_err.to_string().contains("replayed")
                || derive_err.to_string().contains("changed declared refs"),
            "derive H9: {derive_err}"
        );

        let stance_unbound = serde_json::json!({
            "remember": [{
                "title": "Stance source",
                "body": "fresh fact for stance replay",
                "tags": ["ep"],
                "idempotency_key": "episode-replay-stance-src"
            }],
            "stance": [{
                "claim": "same stance claim about the replay fact",
                "confidence": 80,
                "subjects": ["remember:0"]
            }]
        });
        call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            stance_unbound.clone(),
        )
        .await?;
        let mut stance_bound = stance_unbound;
        stance_bound["bind"] = serde_json::json!(["stance:0"]);
        let stance_err = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            stance_bound,
        )
        .await
        .expect_err("bound stance replay must fail");
        assert!(
            stance_err.to_string().contains("replayed")
                || stance_err.to_string().contains("changed declared refs"),
            "stance H9: {stance_err}"
        );

        let goal_unbound = serde_json::json!({
            "remember": [{
                "title": "Goal source fact",
                "body": "fresh fact for goal replay",
                "tags": ["ep"],
                "idempotency_key": "episode-replay-goal-src"
            }],
            "derive": {
                "title": "Goal evidence",
                "body": "abstraction for the replayed goal",
                "source_handles": ["remember:0"],
                "model_id": "test-model",
                "idempotency_key": "episode-replay-goal-abs"
            },
            "stance": [{
                "claim": "assignment stance for the replayed goal",
                "confidence": 80,
                "subjects": ["derive"]
            }],
            "goal": [{
                "schema_id": "core/simple-text-v1",
                "title": "Replay goal",
                "text": "same goal text",
                "body": {},
                "evidence": ["derive"],
                "target_perspective": "stance:0",
                "idempotency_key": "episode-replay-goal"
            }]
        });
        call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_episode_commit",
            goal_unbound.clone(),
        )
        .await?;
        let mut goal_bound = goal_unbound;
        goal_bound["bind"] = serde_json::json!(["goal:0"]);
        let goal_err =
            call_test_model_tool(&tools, authz, owner, "core_episode_commit", goal_bound)
                .await
                .expect_err("bound goal replay must fail");
        assert!(
            goal_err.to_string().contains("replayed"),
            "goal H9: {goal_err}"
        );

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("episode bound replay test failed");
}

#[tokio::test]
async fn facade_core_search_memories_degrades_to_lexical_without_embed_client() {
    // Finding A: a deployment with NO embedding client must not hard-fail on the
    // DEFAULT (Hybrid) search — it degrades to lexical (selfdoc's promise). Only
    // an EXPLICIT semantic search errors when embeddings are unavailable.
    let split_db = clone_split_core_db("proxima_core_search_degrade")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        // No .embed_client(...) — engine.embed_client() is None.
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Degrade path",
                "body": "lexical needle without embeddings",
                "tags": ["retrieval"],
                "idempotency_key": "facade-core-search-degrade"
            }),
        )
        .await?;

        // DEFAULT mode (omitted → Hybrid) must SUCCEED, return the fact via
        // lexical ranking, and flag the degrade — not error.
        let default_search = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_search_memories",
            serde_json::json!({
                "query": "lexical needle",
                "kind": "Fact",
                "limit": 5
            }),
        )
        .await?;
        assert_eq!(
            default_search["degraded_to_lexical"].as_bool(),
            Some(true),
            "default Hybrid search must flag degrade when no embed client is configured"
        );
        assert!(
            !default_search["memories"]
                .as_array()
                .expect("memories array")
                .is_empty(),
            "degraded search must still return lexical matches"
        );

        // EXPLICIT semantic must still hard-error when embeddings are unavailable.
        let semantic = call_test_model_tool(
            &tools,
            authz,
            owner,
            "core_search_memories",
            serde_json::json!({
                "query": "lexical needle",
                "mode": "semantic",
                "kind": "Fact",
                "limit": 5
            }),
        )
        .await;
        assert!(
            semantic.is_err(),
            "explicit semantic search must error without an embedding client"
        );

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("core search degrade integration test failed");
}

#[tokio::test]
async fn facade_core_citation_readback_is_owner_scoped() {
    let split_db = clone_split_core_db("proxima_core_citation_mcp")
        .await
        .expect("PG required for tests");
    let db_name = split_db.name().to_owned();
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let other_owner = company_owner(Uuid::now_v7());
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone()).store_number(0)
        .platform_database_url(platform_url.clone())
            .owner(owner)
            .embed_client(test_embedding())
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let admin_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let tools = built.host().core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Cited retrieval",
                "body": "Fact cites external object.",
                "tags": ["citation"],
                "idempotency_key": "facade-core-citation-memory",
                "citation": {
                    "object_schema_id": TestCitedObject::SCHEMA_ID,
                    "object_schema_version": TestCitedObject::SCHEMA_VERSION,
                    "object_payload": { "locator": "artifact://facade/core-citation" }
                }
            }),
        )
        .await?;
        let memory = remembered["handle"].as_str().expect("remembered handle");
        let fact_id = memory
            .parse::<Uuid>()?;
        let cited_object_id: Uuid = sqlx::query_scalar(
            "SELECT blob_id
               FROM proxima_core.memory
              WHERE t = $1",
        )
        .bind(fact_id)
        .fetch_one(&admin_pool)
        .await?;

        let citing = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_fact",
            serde_json::json!({ "action": "facts_citing_object", "cited_object_id": cited_object_id.to_string() }),
        )
        .await?;
        assert_eq!(citing["facts"][0]["memory"], memory);

        let citation = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_fact",
            serde_json::json!({ "action": "citation_of_fact", "fact": memory }),
        )
        .await?;
        assert_eq!(
            citation["citation"]["cited_object_id"],
            cited_object_id.to_string()
        );
        assert_eq!(
            citation["citation"]["cited_object_schema_id"],
            TestCitedObject::SCHEMA_ID
        );
        // A citation is exactly the cited object and its schema; nothing
        // else rides on the wire.
        let mut citation_keys: Vec<&str> = citation["citation"]
            .as_object()
            .expect("citation object")
            .keys()
            .map(String::as_str)
            .collect();
        citation_keys.sort_unstable();
        assert_eq!(citation_keys, ["cited_object_id", "cited_object_schema_id"]);

        let cross_owner = call_test_model_tool(
            &tools,
            host_authz(&other_owner, ToolScope::All),
            other_owner,
            "core_fact",
            serde_json::json!({ "action": "facts_citing_object", "cited_object_id": cited_object_id.to_string() }),
        )
        .await?;
        assert!(cross_owner["facts"].as_array().expect("facts").is_empty());

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("core citation MCP facade integration test failed");
}

async fn ensure_fact_embedding_for_handle(
    engine: &proxima_core::Engine,
    owner: &OwnerId,
    handle: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let memory_id = handle.parse::<Uuid>()?;
    engine
        .ensure_fact_embedding(owner, MemoryId::new(memory_id))
        .await?;
    Ok(())
}

/// The authorized flavor-read facade
/// (`proxima::flavor::authorized_memory_ids` and friends) routes candidate
/// filtering through `Engine::query`, which resolves the CALLER'S whole
/// read-owner set — not owner-equality against the request's owner scope.
/// A memory transferred into a group must therefore surface for a group
/// member who has no relationship at all to the original owner. This is the
/// read half of the raw-PgPool boundary breach fix: a flavor's own
/// owner-equality-only candidate SQL would have hidden it.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn facade_authorized_read_surfaces_group_transferred_fact_to_group_member() {
    let split_db = clone_split_core_db("proxima_authorized_read_transfer")
        .await
        .expect("PG required for tests");
    let db_name = split_db.name().to_owned();
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let admin_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let author = UserId::new(Uuid::now_v7());
        let owner = OwnerId::from(author);
        let group = GroupId::new(Uuid::now_v7());
        let group_owner = OwnerId::from(group);
        let member = UserId::new(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(runtime_url.clone()).store_number(0)
        .platform_database_url(platform_url.clone())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);

        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "Transfer candidate",
                "body": "group-visible body unique needle",
                "tags": [],
                "idempotency_key": "facade-authorized-read-owner-transfer"
            }),
        )
        .await?;
        let handle = remembered["handle"].as_str().expect("remembered handle");
        let memory_id = handle
            .parse::<Uuid>()?;

        // Admin on BOTH sides: the author's own personal owner (source) and
        // the destination group (receiving-side consent).
        let transfer_authz = verified(AuthzContext::for_subject_with_role(
            author,
            [(group_owner, Role::admin())],
            AuthPath::HostBearer,
        ));
        built
            .host().engine()
            .transfer_to_owner(
                &transfer_authz,
                proxima_core::EntityId::Memory(MemoryId::new(memory_id)),
                group_owner,
            )
            .await?;
        let transferred_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(memory_id)
                .fetch_one(&admin_pool)
                .await?;
        assert_eq!(
            transferred_owner,
            group_owner.into_inner(),
            "a transfer keeps the same t and moves owner to the destination"
        );
        let still_private: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory
              WHERE t = $1 AND owner_id = $2",
        )
        .bind(memory_id)
        .bind(owner.into_inner())
        .fetch_one(&admin_pool)
        .await?;
        assert_eq!(still_private, 0);

        // A caller with no relationship to `owner` — not a co-member of any
        // of the author's other groups, no share, nothing — but a viewer of
        // the destination group. The request's owner scope is the caller's
        // OWN personal owner, so only the read-set union can surface the row.
        let member_authz = verified(AuthzContext::for_subject_with_role(
            member,
            [(group_owner, Role::viewer())],
            AuthPath::HostBearer,
        ));
        let visible = proxima::flavor::authorized_memory_ids(
            built.host().engine(),
            &member_authz,
            &[memory_id],
            proxima_core::verbs::query::EntityKind::Fact,
            None,
            10,
        )
        .await?;
        assert_eq!(
            visible,
            vec![MemoryId::new(memory_id)],
            "a group-transferred Fact must surface through the authorized-read facade for a group member"
        );

        // The same caller without the group role sees nothing: the read set,
        // not the transfer, is what grants visibility.
        let stranger = UserId::new(Uuid::now_v7());
        let stranger_authz = verified(AuthzContext::for_subject(stranger, AuthPath::HostBearer));
        let hidden = proxima::flavor::authorized_memory_ids(
            built.host().engine(),
            &stranger_authz,
            &[memory_id],
            proxima_core::verbs::query::EntityKind::Fact,
            None,
            10,
        )
        .await?;
        assert!(
            hidden.is_empty(),
            "a transfer is not a publish: a caller outside the destination group sees nothing"
        );

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("authorized-read owner-transfer visibility test failed");
}

#[tokio::test]
async fn core_forget_marks_a_remembered_fact() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIMA_S3_* unset");
        return;
    }
    let split_db = clone_split_core_db("proxima_core_forget")
        .await
        .expect("PG required for tests");
    let db_name = split_db.name().to_owned();
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let admin_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .s3(S3RuntimeConfig {
                force_path_style: true,
                ..S3RuntimeConfig::from_env()?
            })
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let listed = tools.list_core_tools();
        assert!(
            listed.iter().any(|tool| tool.name == "core_forget"),
            "served catalog must include core_forget: {listed:?}"
        );
        assert!(
            listed.iter().all(|tool| !tool.name.contains("open_batch")),
            "open_batch must stay deleted"
        );

        let authz = host_authz(&owner, ToolScope::All);
        let remembered = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_remember",
            serde_json::json!({
                "title": "forget me",
                "body": "hot row that must cool",
                "tags": []
            }),
        )
        .await?;
        let handle = remembered["handle"].as_str().expect("handle").to_string();
        let t = handle.parse::<Uuid>()?;

        let forgotten = call_test_model_tool(
            &tools,
            authz.clone(),
            owner,
            "core_forget",
            serde_json::json!({ "memory": handle }),
        )
        .await?;
        assert_eq!(forgotten["ok"], serde_json::json!(true));

        let (rows, forgotten): (i64, i64) = sqlx::query_as(
            "SELECT count(*)::bigint, count(forgotten_at)::bigint
               FROM proxima_core.memory WHERE t = $1",
        )
        .bind(t)
        .fetch_one(&admin_pool)
        .await?;
        assert_eq!((rows, forgotten), (1, 1), "forget keeps the row, marked");
        let cooled: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(t)
                .fetch_one(&admin_pool)
                .await?;
        assert_eq!(cooled, 0, "forget writes no cooled stub");

        let cold = built
            .host()
            .blobs()
            .as_ref()
            .expect("configured S3 fixture")
            .cold_store();
        let cold_key = proxima_core::cold_object_key(t);
        assert!(
            cold.get(&cold_key).await.is_err(),
            "forget writes no cold object"
        );

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("core_forget must drive shipped forget_memory");
}

#[tokio::test]
async fn request_services_reject_duplicate_boot_type() {
    let split_db = clone_split_core_db("proxima_request_services")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<MarkApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .allow_insecure_single_owner()
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let mut request = FlavorServices::new();
        request.try_insert(BootMark)?;
        let err = built
            .host()
            .core_mcp_tools_with_request_services(request)
            .expect_err("boot Marker + request Marker");
        assert!(
            matches!(err, FlavorServiceError::DuplicateService { .. }),
            "{err:?}"
        );
        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("duplicate request service must fail");
}
