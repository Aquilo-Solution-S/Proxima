//! EXPLAIN plan-shape guards for v0.0.8 hot reads.
//!
//! Capability check: `enable_seqscan = off` so a tiny template DB still
//! has to pick the shipped index. Not a cost/latency bench.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::verbs::query::{
    EntityKind, MemorySearchRequest, QueryRequest, SearchMode, SearchOrder, SupersessionStatus,
    TagMatch,
};
use proxima_core::verbs::schema::MemorySearchProjection;
use proxima_core::{EdgeKind, OwnerRef, SchemaId, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::fact_embeddings::claim_embedding_jobs_sql_for_tests;
use proxima_storage_pg::verbs::query::{
    ancestor_hop_sql_for_tests, descendant_hop_sql_for_tests, inbound_pin_sql_for_tests,
    lexical_sidecar_sql_for_tests, memory_page_sql_for_tests, search_admit_sql_for_tests,
    semantic_search_sql_for_tests, set_hnsw_search_sql_for_tests,
};
use uuid::Uuid;

fn note_projection() -> MemorySearchProjection {
    // The shipped declaration, not a second copy of it. This fixture used
    // to restate `core/agent-note-v1`'s columns by hand, which meant the
    // test agreed with the contract only by coincidence.
    proxima_core::FlavorRegistry::new()
        .freeze_or_panic_for_tests()
        .search_projections()
        .iter()
        .find(|projection| projection.schema_id.as_str() == "core/agent-note-v1")
        .expect("core/agent-note-v1 is a search surface")
        .clone()
}

fn search_req(owner: OwnerRef) -> MemorySearchRequest {
    MemorySearchRequest {
        owner,
        read_owners: vec![owner],
        query: "needle".into(),
        mode: SearchMode::Lexical,
        supersession: SupersessionStatus::HeadsOnly,
        limit: 8,
        kind: Some(EntityKind::Fact),
        schema_id: None,
        tags: Vec::new(),
        tag_match: TagMatch::Any,
        since: None,
        until: None,
        order: SearchOrder::Relevance,
        min_score: None,
        semantic_weight: None,
        after: None,
        query_embedding: None,
        embedding_model_id: None,
    }
}

fn embed_literal() -> String {
    format!(
        "[{}]",
        std::iter::once("1")
            .chain(std::iter::repeat_n("0", EMBEDDING_DIM - 1))
            .collect::<Vec<_>>()
            .join(",")
    )
}

async fn seed_note(
    pool: &sqlx::PgPool,
    owner: OwnerRef,
    title: &str,
    body: &str,
) -> Result<Uuid, sqlx::Error> {
    let owner_id = owner.stored_owner_id();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal')
         ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .execute(pool)
    .await?;
    let handle = Uuid::now_v7();
    let t = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'fact', 'core/agent-note-v1', $2, $3)",
    )
    .bind(handle)
    .bind(owner_id)
    .bind(t)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
         VALUES ($1, $2, 'fact', $3, 'core/agent-note-v1')",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
         VALUES ($1, $2, $3, $4, '{}')",
    )
    .bind(t)
    .bind(Uuid::now_v7())
    .bind(title)
    .bind(body)
    .execute(pool)
    .await?;
    Ok(t)
}

async fn seed_derived(
    pool: &sqlx::PgPool,
    owner: OwnerRef,
    origin: Uuid,
) -> Result<Uuid, sqlx::Error> {
    let owner_id = owner.stored_owner_id();
    let handle = Uuid::now_v7();
    let t = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'abstraction', 'core/agent-note-v1', $2, $3)",
    )
    .bind(handle)
    .bind(owner_id)
    .bind(t)
    .execute(pool)
    .await?;
    let mut hash = [0_u8; 32];
    hash[..16].copy_from_slice(t.as_bytes());
    hash[16..].copy_from_slice(t.as_bytes());
    let content_id: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/agent-note-v1', $2)
         RETURNING content_id",
    )
    .bind(owner_id)
    .bind(hash.as_slice())
    .fetch_one(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory
            (handle, t, kind, owner_id, schema_id, origins, content_id)
         VALUES ($1, $2, 'abstraction', $3, 'core/agent-note-v1', ARRAY[$4]::uuid[], $5)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .bind(origin)
    .bind(content_id)
    .execute(pool)
    .await?;
    Ok(t)
}

fn assert_plan_names(plan: &serde_json::Value, needle: &str) {
    let rendered = plan.to_string();
    assert!(
        rendered.contains(needle),
        "expected {needle} in plan:\n{rendered}"
    );
}

/// Tiny corpora walk `memory_t_key` and filter `origins &&`; a large
/// one can pick `memory_origins_gin`. Either is an index on `memory`.
fn assert_origin_overlap_index(plan: &serde_json::Value, label: &str) {
    let rendered = plan.to_string();
    assert!(
        rendered.contains("memory_origins_gin") || rendered.contains("memory_t_key"),
        "{label} must index memory for origins overlap; plan:\n{rendered}"
    );
}

#[tokio::test]
async fn hot_path_plans_use_expected_indexes() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let leaf = seed_note(pool, owner, "Needle title", "needle body").await?;
        let child = seed_derived(pool, owner, leaf).await?;
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_id, model_id, embedding_version, vec, owner_id)
             VALUES ($1, 'test-embed', 1, $2::vector, $3)",
        )
        .bind(leaf)
        .bind(embed_literal())
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_heads
                (entity_id, model_id, embedding_version, owner_id)
             VALUES ($1, 'test-embed', 1, $2)",
        )
        .bind(leaf)
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_jobs (entity_id, model_id, owner_id, status)
             VALUES ($1, 'test-embed', $2, 'pending')",
        )
        .bind(leaf)
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;
        // SQL-POLICY: fixed-fragment
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            "ANALYZE proxima_core.agent_note_v1;
             ANALYZE proxima_core.memory;
             ANALYZE proxima_core.memory_head;
             ANALYZE proxima_core.embeddings;
             ANALYZE proxima_core.embedding_jobs",
        ))
        .execute(pool)
        .await?;

        let owner_ids = vec![owner.stored_owner_id()];
        let req = search_req(owner);
        let like = proxima_core::verbs::query::like_pattern(&req.query);

        let mut tx = pool.begin().await?;
        sqlx::query("SET LOCAL enable_seqscan = off")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL enable_sort = off")
            .execute(&mut *tx)
            .await?;

        let lexical = lexical_sidecar_sql_for_tests(&note_projection(), &req, true, false)?;
        let lexical_explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {lexical}");
        // SQL-POLICY: PgIdent — production lexical builder
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(lexical_explain))
            .bind(&req.query)
            .bind(&like)
            .bind(20_i64)
            .bind(None::<Vec<String>>)
            .bind(None::<time::OffsetDateTime>)
            .bind(None::<time::OffsetDateTime>)
            .bind(None::<Uuid>)
            .bind(&owner_ids)
            .bind(note_projection().schema_id.as_str())
            .fetch_one(&mut *tx)
            .await?;
        let lexical_plan = plan.to_string();
        // The composite `gin(owner_id, search_tsv)` is the whole reason the
        // projection carries `owner_id`: both halves of the predicate are on
        // one relation, so one index scan answers "this owner's rows that
        // match this query". The previous shape scanned the sidecar's own
        // tsvector GIN and reached the owner through a join to `memory`,
        // which is what the projection replaced.
        assert!(
            lexical_plan.contains("core_projection_owner_tsv_gin"),
            "the ranked arm must scan the projection's composite GIN; plan:\n{lexical_plan}"
        );
        assert!(
            !lexical_plan.contains("agent_note_v1_search_tsv_gin"),
            "the per-sidecar tsvector index is gone; plan:\n{lexical_plan}"
        );

        // SQL-POLICY: fixed-fragment
        sqlx::raw_sql(sqlx::AssertSqlSafe(set_hnsw_search_sql_for_tests(
            &proxima_storage_pg::PgTuning::default(),
        )))
        .execute(&mut *tx)
        .await?;
        let semantic = semantic_search_sql_for_tests();
        let semantic_explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {semantic}");
        // SQL-POLICY: fixed-fragment
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(semantic_explain))
            .bind(&owner_ids)
            .bind("test-embed")
            .bind(embed_literal())
            .bind(20_i64)
            .bind(None::<time::OffsetDateTime>)
            .bind(None::<time::OffsetDateTime>)
            .fetch_one(&mut *tx)
            .await?;
        assert_plan_names(&plan, "idx_embeddings_vec_hnsw");

        let admit = search_admit_sql_for_tests(true);
        let admit_explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {admit}");
        // SQL-POLICY: fixed-fragment
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(admit_explain))
            .bind(vec![child])
            .bind(&owner_ids)
            .bind(None::<String>)
            .bind(None::<String>)
            .bind(None::<time::OffsetDateTime>)
            .bind(None::<time::OffsetDateTime>)
            .fetch_one(&mut *tx)
            .await?;
        assert!(
            plan.to_string().contains("memory_head") || plan.to_string().contains("memory_pkey"),
            "admit HeadsOnly must join through a memory index; plan:\n{plan}"
        );

        let mut page = QueryRequest::for_owner(owner);
        page.schema_id = Some(SchemaId::new("core/agent-note-v1".into()));
        page.entity_kind = Some(EntityKind::Fact);
        page.limit = 20;
        let heads = memory_page_sql_for_tests(&page)?;
        let heads_explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {heads}");
        // SQL-POLICY: fixed-fragment
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(heads_explain))
            .bind(&owner_ids)
            .bind("core/agent-note-v1")
            .bind("fact")
            .fetch_one(&mut *tx)
            .await?;
        let heads_plan = plan.to_string();
        assert!(
            heads_plan.contains("memory_head_owner_schema_idx")
                || heads_plan.contains("memory_head_pkey"),
            "heads page must index memory_head; plan:\n{heads_plan}"
        );

        let ancestor = ancestor_hop_sql_for_tests();
        let ancestor_explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {ancestor}");
        // SQL-POLICY: fixed-fragment
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(ancestor_explain))
            .bind(vec![child])
            .bind(&owner_ids)
            .bind(None::<Uuid>)
            .bind(None::<Uuid>)
            .bind(20_i64)
            .fetch_one(&mut *tx)
            .await?;
        assert!(
            plan.to_string().contains("memory_t_key") || plan.to_string().contains("Index Scan"),
            "ancestor hop must use memory.t; plan:\n{plan}"
        );

        let descendant = descendant_hop_sql_for_tests();
        let descendant_explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {descendant}");
        // SQL-POLICY: fixed-fragment
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(descendant_explain))
            .bind(vec![leaf])
            .bind(&owner_ids)
            .bind(None::<Uuid>)
            .bind(None::<Uuid>)
            .bind(20_i64)
            .fetch_one(&mut *tx)
            .await?;
        assert_origin_overlap_index(&plan, "descendant hop");

        let inbound = inbound_pin_sql_for_tests(true, Some(EdgeKind::Origin));
        let inbound_explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {inbound}");
        // SQL-POLICY: fixed-fragment
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(inbound_explain))
            .bind(vec![leaf])
            .bind(&owner_ids)
            .bind(None::<Uuid>)
            .bind(20_i64)
            .fetch_one(&mut *tx)
            .await?;
        assert_origin_overlap_index(&plan, "inbound origin");

        let claim = claim_embedding_jobs_sql_for_tests();
        let claim_explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {claim}");
        // SQL-POLICY: fixed-fragment
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(claim_explain))
            .bind("test-embed")
            .bind(32_i64)
            .fetch_one(&mut *tx)
            .await?;
        assert_plan_names(&plan, "embedding_jobs_pending_claim_idx");

        tx.rollback().await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("hot-path EXPLAIN failed");
}
