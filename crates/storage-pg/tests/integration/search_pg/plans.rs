//! EXPLAIN-based plan-shape regressions for the search branches.

use super::{
    any_kind_lexical_request, drop_db, fresh_pg, insert_embedded_memory, owner_fixture,
    padded_embedding, plan_seq_scans_relation, semantic_request, vector_literal,
};

use proxima_core::verbs::query::SearchMode;
use proxima_core::{OwnerRef, UserId};
use proxima_storage_pg::PgTuning;
use uuid::Uuid;

#[tokio::test]
async fn semantic_search_plan_uses_hnsw_index() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();
    insert_embedded_memory(&pg, &owner, "plan probe", [1.0, 0.0, 0.0]).await?;

    let mut req = semantic_request(&owner, padded_embedding([1.0, 0.0, 0.0]));
    // No schema filter: keeps the bind list to owner arrays + vector + model.
    req.schema_id = None;
    let sql = proxima_storage_pg::verbs::query::semantic_search_sql_for_tests(
        &req,
        &[],
        40,
        512,
        &PgTuning::default(),
    )?;
    let explain_sql = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {sql}");

    let (owner_kind, owner_id) = owner.columns();
    let mut tx = pg.pool_for_tests().begin().await?;
    // The production session settings, plus seqscan/sort penalized so the
    // assertion is about capability, not tiny-table costing: the only way
    // to satisfy `ORDER BY emb.vec <=> $query` without an explicit sort is
    // the HNSW scan, so if the shipped query shape can no longer be served
    // by the index (e.g. the ORDER BY expression stops matching the
    // operator class), no planner setting can save it and this fails.
    for setting in [
        "SET LOCAL hnsw.ef_search = 100",
        "SET LOCAL hnsw.iterative_scan = relaxed_order",
        "SET LOCAL enable_seqscan = off",
        "SET LOCAL enable_sort = off",
    ] {
        // SQL-POLICY: fixed-fragment
        sqlx::query(setting).execute(&mut *tx).await?;
    }
    // SQL-POLICY: fixed-fragment — EXPLAIN prefix over the audited
    // production builder's parameterized SQL; only bound values vary.
    let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(explain_sql.as_str()))
        .bind(vec![owner_kind])
        .bind(vec![owner_id])
        .bind(vector_literal(&padded_embedding([1.0, 0.0, 0.0])))
        .bind("test-embed")
        .fetch_one(&mut *tx)
        .await?;
    tx.rollback().await?;

    let rendered = plan.to_string();
    assert!(
        rendered.contains("idx_embeddings_vec_hnsw"),
        "the semantic branch must scan the HNSW index; plan:\n{rendered}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Owner-scoped candidate enumeration must ride the `(owner_kind,
/// owner_id)` index prefixes under DEFAULT planner costing. The previous
/// `owner_id IS NOT DISTINCT FROM s.id` join had no index path at all, so
/// on a corpus where one owner holds a sliver of the table every branch
/// seq-scanned all of `memories` per owner check (measured: 150k rows
/// scanned to find ~300). Regression check: seed a large foreign corpus,
/// ANALYZE, and require that neither search branch plans a seq scan over
/// `memories`.
#[tokio::test]
async fn search_branches_enumerate_candidates_via_owner_index()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let target = owner_fixture();
    let crowd = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let (crowd_kind, crowd_id) = crowd.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, model_id, prompt_version)
         SELECT gen_random_uuid(), $1, $2, 'test/search-abstraction-v1', 1,
                'Abstraction', 'crowd filler row ' || g, 'AtoA',
                '00000000-0000-0000-0000-000000000327'::uuid,
                '00000000-0000-0000-0000-000000000328'::uuid,
                'test-model', 'test-v1'
           FROM generate_series(1, 20000) g",
    )
    .bind(crowd_kind)
    .bind(crowd_id)
    .execute(pg.pool_for_tests())
    .await?;
    for idx in 0..50 {
        insert_embedded_memory(
            &pg,
            &target,
            &format!("target corpus row {idx}"),
            [1.0, 0.0, 0.0],
        )
        .await?;
    }
    sqlx::query("ANALYZE proxima_core.memories")
        .execute(pg.pool_for_tests())
        .await?;

    let mut req = any_kind_lexical_request(&target, "target corpus");
    let lexical_sql = proxima_storage_pg::verbs::query::lexical_search_sql_for_tests(
        &req,
        &[],
        40,
        &PgTuning::default(),
    )?;
    req.mode = SearchMode::Semantic;
    req.query_embedding = Some(padded_embedding([1.0, 0.0, 0.0]));
    req.embedding_model_id = Some("test-embed".into());
    let semantic_sql = proxima_storage_pg::verbs::query::semantic_search_sql_for_tests(
        &req,
        &[],
        40,
        512,
        &PgTuning::default(),
    )?;

    let (owner_kind, owner_id) = target.columns();
    // SQL-POLICY: fixed-fragment — EXPLAIN prefix over the audited
    // production builders' parameterized SQL; only bound values vary.
    let lexical_plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "EXPLAIN (FORMAT JSON, COSTS OFF) {lexical_sql}"
    )))
    .bind(vec![owner_kind])
    .bind(vec![owner_id])
    .bind("target corpus")
    .fetch_one(pg.pool_for_tests())
    .await?;
    // SQL-POLICY: fixed-fragment — EXPLAIN prefix over the audited
    // production builders' parameterized SQL; only bound values vary.
    let semantic_plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "EXPLAIN (FORMAT JSON, COSTS OFF) {semantic_sql}"
    )))
    .bind(vec![owner_kind])
    .bind(vec![owner_id])
    .bind(vector_literal(&padded_embedding([1.0, 0.0, 0.0])))
    .bind("test-embed")
    .fetch_one(pg.pool_for_tests())
    .await?;

    for (label, plan) in [("lexical", &lexical_plan), ("semantic", &semantic_plan)] {
        let root = plan
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("Plan"))
            .cloned()
            .expect("EXPLAIN JSON has a root Plan");
        assert!(
            !plan_seq_scans_relation(&root, "memories"),
            "{label} branch must not seq-scan memories for owner scoping; plan:\n{plan:#}"
        );
        // A candidate branch reads `FROM proxima_core.memories m`, so its
        // owner columns are `m`'s own. Resolving them through the
        // `memories ∪ goals` union instead put `goals` in every plan and
        // made that union the driving relation — which is what kept the
        // text and vector predicates out of reach of any index. Touching
        // `goals` at all in a memory search is the regression.
        assert!(
            !plan_seq_scans_relation(&root, "goals"),
            "{label} branch must not read goals to scope a memory's owner; plan:\n{plan:#}"
        );
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}
