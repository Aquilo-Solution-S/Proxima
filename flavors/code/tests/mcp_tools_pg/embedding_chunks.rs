use super::*;

fn vector(x: f32, y: f32) -> pgvector::Vector {
    let mut values = vec![0.0; 1024];
    values[0] = x;
    values[1] = y;
    pgvector::Vector::from(values)
}

async fn replace_chunk_vectors(
    pool: &PgPool,
    path: &str,
    vectors: &[pgvector::Vector],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_id, model_id, dim, embedding_version, chunk_ordinal, vec, owner_id)
         SELECT head.entity_id, head.model_id, head.dim, head.embedding_version + 1,
                (chunk.ordinality - 1)::integer, chunk.vec, head.owner_id
           FROM proxima_core.embedding_heads head
           JOIN proxima_code.code_chunk_v1 c ON c.t = head.entity_id
           CROSS JOIN unnest($2::vector[]) WITH ORDINALITY AS chunk(vec, ordinality)
          WHERE c.file_path = $1 AND head.model_id = 'test-topic-embed'",
    )
    .bind(path)
    .bind(vectors)
    .execute(&mut *tx)
    .await?;
    let updated = sqlx::query(
        "UPDATE proxima_core.embedding_heads head
            SET embedding_version = head.embedding_version + 1
           FROM proxima_code.code_chunk_v1 c
          WHERE c.t = head.entity_id AND c.file_path = $1
            AND head.model_id = 'test-topic-embed'",
    )
    .bind(path)
    .execute(&mut *tx)
    .await?;
    assert_eq!(updated.rows_affected(), 1, "one code memory in {path}");
    tx.commit().await
}

/// The backend must deduplicate embedding pieces before the code flavor
/// assigns semantic ranks; otherwise one long memory consumes every rank.
#[tokio::test]
async fn code_semantic_and_hybrid_rank_distinct_memories_by_best_chunk()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestDb::fresh().await;
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let temp = TempDir::new()?;
    let router = topic_router();
    ingest_embedded_repo(
        &fixture,
        owner,
        &registry,
        &temp,
        router.clone(),
        &[
            ("src/first.rs", "pub fn first() -> usize { 1 }\n"),
            ("src/second.rs", "pub fn second() -> usize { 2 }\n"),
            ("src/third.rs", "pub fn third() -> usize { 3 }\n"),
        ],
    )
    .await?;
    let pool = fixture.pg.pool_for_tests();
    replace_chunk_vectors(pool, "src/first.rs", &vec![vector(1.0, 0.0); 130]).await?;
    replace_chunk_vectors(pool, "src/second.rs", &[vector(0.0, 1.0), vector(0.8, 0.6)]).await?;
    replace_chunk_vectors(pool, "src/third.rs", &[vector(0.6, 0.8)]).await?;
    for mode in ["semantic", "hybrid"] {
        let found = run_tool::<CodeSearchChunksTool>(
            embedding_ctx(fixture.pg.clone(), owner, registry.clone(), router.clone()),
            json!({
                "query": "stop going round again",
                "mode": mode,
                "limit": 2,
                "file_class": "source",
                "include_calls": false,
                "verbose": true,
            }),
        )
        .await?;
        assert_eq!(
            match_paths(&found),
            ["src/first.rs", "src/second.rs"],
            "{mode}: {found}"
        );
        let second = &found["matches"][1];
        let similarity = second["similarity_score"]
            .as_f64()
            .expect("similarity score");
        assert!((similarity - 0.8).abs() < 0.000_001, "{found}");
        let score = second["score"].as_f64().expect("ranking score");
        let expected = if mode == "semantic" { 0.8 } else { 1.0 / 62.0 };
        assert!(
            (score - expected).abs() < 0.000_001,
            "distinct rank: {found}"
        );
        assert_eq!(found["has_more"], true, "{found}");
    }
    Ok(())
}
