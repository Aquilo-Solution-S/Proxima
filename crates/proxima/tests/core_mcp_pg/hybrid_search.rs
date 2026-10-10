use super::*;
use proxima_core::llm::BoundEmbeddingClient;
use proxima_core::test_fixtures::TestEmbeddingRouter;

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn remember(
    tools: &CoreMcpTools,
    authz: &AuthzContext,
    owner: Owner,
    title: &str,
    body: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let result = call_test_model_tool(
        tools,
        authz.clone(),
        owner,
        "core_remember",
        serde_json::json!({"title": title, "body": body, "language": "english"}),
    )
    .await?;
    Ok(result["handle"]
        .as_str()
        .ok_or("memory handle")?
        .strip_prefix("F:")
        .ok_or("fact handle")?
        .parse()?)
}

async fn search(
    tools: &CoreMcpTools,
    authz: &AuthzContext,
    owner: Owner,
    query: &str,
    cursor: Option<&str>,
    floor: Option<f32>,
) -> Result<serde_json::Value, CoreMcpError> {
    call_test_model_tool(
        tools,
        authz.clone(),
        owner,
        "core_search_memories",
        serde_json::json!({
            "query": query, "mode": "hybrid", "kind": "Fact", "limit": 1,
            "semantic_weight": 0.75, "cursor": cursor, "min_score": floor,
        }),
    )
    .await
}

fn number(hit: &serde_json::Value, field: &str) -> f64 {
    hit[field].as_f64().expect("numeric search score")
}

#[tokio::test]
async fn served_hybrid_rescues_bilingual_queries_and_pages_with_raw_leg_floors() -> TestResult {
    let split_db = clone_split_core_db("proxima_served_hybrid_rrf").await?;
    let name = split_db.name().to_owned();
    let result: TestResult = async {
        let (runtime_url, platform_url) = split_role_urls(&name).await?;
        let owner = Owner::Personal(UserId::new(Uuid::now_v7()));
        // Admit Facts while the route has no client, so no background job
        // can replace the deliberately distinct vectors below.
        let router = Arc::new(TestEmbeddingRouter::default());
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(runtime_url)
            .platform_database_url(platform_url)
            .owner(owner)
            .tool_scope(ToolScope::All)
            .embedding_router(router.clone())
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = host_authz(&owner, ToolScope::All);
        let lexical = remember(&tools, &authz, owner, "Partial result", "checkpoint").await?;
        let semantic = remember(&tools, &authz, owner, "Concept result", "Unrelated vocabulary").await?;
        let admin = sqlx::PgPool::connect(&db_url(&name)).await?;
        sqlx::query("INSERT INTO proxima_core.lexical_languages(config) VALUES ('german') ON CONFLICT DO NOTHING")
            .execute(&admin).await?;
        let mut vector = vec![0.0_f32; 1024];
        vector[0] = 0.8;
        vector[1] = 0.6;
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_id, model_id, dim, embedding_version, vec, owner_id)
             VALUES ($1, 'test-embed', 1024, 1, $2::real[]::vector, $3)",
        )
        .bind(semantic).bind(&vector).bind(owner.stored_owner_id()).execute(&admin).await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_heads
                (entity_id, model_id, dim, embedding_version, owner_id)
             VALUES ($1, 'test-embed', 1024, 1, $2)",
        )
        .bind(semantic).bind(owner.stored_owner_id()).execute(&admin).await?;
        router.set_default(BoundEmbeddingClient::bind(test_embedding())?);

        for query in [
            "How does the checkpoint recovery commit work?",
            "Wie wird ein checkpoint recovery commit work?",
        ] {
            let first = search(&tools, &authz, owner, query, None, None).await?;
            let hit = &first["memories"][0];
            assert_eq!(hit["memory_id"], semantic.to_string());
            assert!((number(hit, "score") - 0.75 / 61.0).abs() < 0.000_001);
            assert!((number(hit, "similarity_score") - 0.8).abs() < 0.000_001);
            assert_eq!(number(hit, "lexical_score").to_bits(), 0.0_f64.to_bits());
            assert_eq!(first["has_more"], true);
            let cursor = first["next_cursor"].as_str().expect("hybrid cursor");
            let second = search(&tools, &authz, owner, query, Some(cursor), None).await?;
            let hit = &second["memories"][0];
            assert_eq!(hit["memory_id"], lexical.to_string());
            assert!((number(hit, "score") - 0.25 / 61.0).abs() < 0.000_001);
            assert!(number(hit, "lexical_score") > 0.0, "rescue term must contribute");
            assert_eq!(number(hit, "similarity_score").to_bits(), 0.0_f64.to_bits());
            assert_eq!(second["degraded_to_lexical"], false);
            assert_eq!(second["has_more"], false);
            assert!(second["next_cursor"].is_null());
            let floored = search(&tools, &authz, owner, query, None, Some(0.5)).await?;
            assert_eq!(floored["memories"].as_array().expect("hits").len(), 1);
            assert_eq!(floored["memories"][0]["memory_id"], semantic.to_string());
            assert!(number(&floored["memories"][0], "score") < 0.5);
            assert_eq!(floored["has_more"], false);
        }
        admin.close().await;
        built.shutdown().await;
        Ok(())
    }.await;
    result
}
