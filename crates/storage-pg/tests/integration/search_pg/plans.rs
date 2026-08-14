//! EXPLAIN-based plan-shape regressions for the search branches.

use super::{
    any_kind_lexical_request, drop_db, fresh_pg, insert_embedded_memory, owner_fixture,
    padded_embedding, plan_seq_scans_relation, semantic_request, vector_literal,
};

use proxima_core::verbs::query::SearchMode;
use proxima_core::{OwnerRef, UserId};
use proxima_storage_pg::{PgTuning, SemanticIndexFirst};
use uuid::Uuid;

/// The legacy configuration, exactly as the environment escape hatch
/// (`PROXIMA_PG_SEMANTIC_INDEX_FIRST=off`,
/// `PROXIMA_PG_CANDIDATE_WINDOW_DEDUP=off`) selects it.
fn legacy_tuning() -> PgTuning {
    PgTuning {
        semantic_index_first: SemanticIndexFirst::Off,
        candidate_window_dedup: false,
        ..PgTuning::default()
    }
}

/// Both semantic arms must still be servable by the HNSW index.
///
/// The default (pushdown) arm is the shipped path. The legacy arm is the
/// escape hatch, and its entire purpose is to be a working fallback — the
/// statement it emits carries a frozen comment claiming *this* test pins it
/// ("the inner scan keeps `ORDER BY <vector distance> LIMIT n` intact, which
/// is the only shape the HNSW index can serve"), a claim that stops being
/// true the moment this test EXPLAINs only the default tuning. So run both,
/// and name the arm in the assertion.
#[tokio::test]
async fn semantic_search_plan_uses_hnsw_index() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();
    insert_embedded_memory(&pg, &owner, "plan probe", [1.0, 0.0, 0.0]).await?;

    let mut req = semantic_request(&owner, padded_embedding([1.0, 0.0, 0.0]));
    // No schema filter: keeps the bind list to owner arrays + vector + model.
    req.schema_id = None;

    let (owner_kind, owner_id) = owner.columns();
    for (arm, tuning) in [
        ("default", PgTuning::default()),
        ("legacy", legacy_tuning()),
    ] {
        let sql = proxima_storage_pg::verbs::query::semantic_search_sql_for_tests(
            &req,
            &[],
            40,
            512,
            &tuning,
        )?;
        let explain_sql = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {sql}");

        let mut tx = pg.pool_for_tests().begin().await?;
        // The production session settings — the same statement `run_semantic`
        // sends under this tuning, not a restated copy of the defaults — plus
        // seqscan/sort penalized so the assertion is about capability, not
        // tiny-table costing: the only way to satisfy
        // `ORDER BY emb.vec <=> $query` without an explicit sort is the HNSW
        // scan, so if the shipped query shape can no longer be served by the
        // index (e.g. the ORDER BY expression stops matching the operator
        // class, or the pushdown owner predicate stops being a plain filter
        // the scan can carry), no planner setting can save it and this fails.
        //
        // SQL-POLICY: fixed-fragment — the audited production settings builder
        // over this deployment's own tuning integers and enum spellings.
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            proxima_storage_pg::verbs::query::set_hnsw_search_sql_for_tests(&tuning),
        ))
        .execute(&mut *tx)
        .await?;
        for setting in [
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
            "the {arm} semantic branch must scan the HNSW index; plan:\n{rendered}"
        );
    }

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
///
/// The full guard runs against the escape-hatch (legacy) configuration,
/// whose SQL is pinned byte-for-byte by the unit goldens. Under the default
/// configuration the `candidate_window_dedup` successor anti-join currently
/// makes the planner hash-build over all of `memories` on this corpus shape
/// (`m` becomes the probe side of a hash right join instead of an
/// owner-index scan), so the default arm asserts only the `goals` half.
///
/// That asymmetry is not a hole for the regression this test exists to
/// catch. The owner join itself is written by `push_read_owner_scope`,
/// which emits the same text on both arms — the `EXISTS (SELECT 1 FROM
/// unnest(...))` block is character-for-character identical in the default
/// and legacy candidate goldens — so a regression there would still fail
/// the legacy assertion above. What the default arm does not guard is
/// narrower and separate: which plan the dedup anti-join draws on a
/// sliver-owner corpus. Guarding that means asserting a costed plan choice
/// on a synthetic corpus, which breaks on an unrelated `ANALYZE`, so it
/// waits for measurement rather than a stricter assertion here.
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

    let (owner_kind, owner_id) = target.columns();
    for (arm, tuning, guard_memories) in [
        ("legacy", legacy_tuning(), true),
        ("default", PgTuning::default(), false),
    ] {
        let mut req = any_kind_lexical_request(&target, "target corpus");
        let lexical_sql =
            proxima_storage_pg::verbs::query::lexical_search_sql_for_tests(&req, &[], 40, &tuning)?;
        req.mode = SearchMode::Semantic;
        req.query_embedding = Some(padded_embedding([1.0, 0.0, 0.0]));
        req.embedding_model_id = Some("test-embed".into());
        let semantic_sql = proxima_storage_pg::verbs::query::semantic_search_sql_for_tests(
            &req,
            &[],
            40,
            512,
            &tuning,
        )?;

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
            if guard_memories {
                assert!(
                    !plan_seq_scans_relation(&root, "memories"),
                    "{arm} {label} branch must not seq-scan memories for owner scoping; \
                     plan:\n{plan:#}"
                );
            }
            // A candidate branch reads `FROM proxima_core.memories m`, so its
            // owner columns are `m`'s own. Resolving them through the
            // `memories ∪ goals` union instead put `goals` in every plan and
            // made that union the driving relation — which is what kept the
            // text and vector predicates out of reach of any index. Touching
            // `goals` at all in a memory search is the regression.
            assert!(
                !plan_seq_scans_relation(&root, "goals"),
                "{arm} {label} branch must not read goals to scope a memory's owner; \
                 plan:\n{plan:#}"
            );
        }
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}
