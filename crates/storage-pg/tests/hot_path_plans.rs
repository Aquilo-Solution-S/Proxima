//! EXPLAIN plan-shape guards for the hot reads.
//!
//! Capability check: `enable_seqscan = off` so a tiny template DB still
//! has to pick the shipped index. Not a cost/latency bench.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::EmbeddingDim;
use proxima_core::storage_ports::PinTargetKind;
use proxima_core::verbs::query::SidecarAtom;
use proxima_core::verbs::query::{
    EntityKind, MemorySearchRequest, QueryRequest, SearchMode, SearchOrder, SupersessionStatus,
    TagMatch,
};
use proxima_core::verbs::schema::MemorySearchProjection;
use proxima_core::{EdgeKind, OwnerId, SchemaId, UserId};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::export_statement_sql_for_tests;
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::verbs::fact_embeddings::{
    claim_embedding_jobs_sql_for_tests, reclaim_stale_embedding_jobs_sql_for_tests,
};
use proxima_storage_pg::verbs::query::{
    ancestor_hop_sql_for_tests, descendant_hop_sql_for_tests, inbound_pin_sql_for_tests,
    memory_page_sql_for_tests, owned_head_handle_sql_for_tests, ranked_projection_sql_for_tests,
    search_admit_sql_for_tests, semantic_candidate_admission_sql_for_tests,
    set_hnsw_search_sql_for_tests, substring_sql_for_tests,
};
use proxima_storage_pg::verbs::{
    fresh_backlog_estimate_sql_for_tests, prune_published_sql_for_tests,
};
use uuid::Uuid;

fn note_projection() -> MemorySearchProjection {
    // The shipped declaration, not a second copy of it: a hand-restated column
    // list agrees with the contract only by coincidence.
    proxima_core::FlavorRegistry::new()
        .freeze_or_panic_for_tests()
        .search_projections()
        .iter()
        .find(|projection| projection.schema_id.as_str() == "core/agent-note-v1")
        .expect("core/agent-note-v1 is a search surface")
        .clone()
}

fn search_req(owner: OwnerId) -> MemorySearchRequest {
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
        semantic: None,
    }
}

fn unit_values(dim: EmbeddingDim) -> Vec<f32> {
    let mut values = vec![0.0; dim.width()];
    values[0] = 1.0;
    values
}

async fn seed_note(
    pool: &sqlx::PgPool,
    owner: OwnerId,
    title: &str,
    body: &str,
) -> Result<Uuid, sqlx::Error> {
    let owner_id = owner.into_inner();
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
    // The stamp and the row it promises land in one transaction: a memory row
    // that names a sidecar table it has no row in is refused at COMMIT.
    let mut stamped = pool.begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory
             (handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via)
         VALUES ($1, $2, 'fact', $3, 'core/agent-note-v1',
                 ARRAY['proxima_core.agent_note_v1'], uuid_extract_timestamp($2), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .execute(&mut *stamped)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
         VALUES ($1, $2, $3, $4, '{}')",
    )
    .bind(t)
    .bind(Uuid::now_v7())
    .bind(title)
    .bind(body)
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;
    let schema = proxima_core::FLAVOR_0
        .schemas
        .iter()
        .find(|schema| schema.schema_id().as_str() == "core/agent-note-v1")
        .expect("core note schema");
    let projection =
        proxima_storage_pg::projection::projection_insert_sql(&proxima_core::FLAVOR_0, schema)
            .expect("core note projection");
    // SQL-POLICY: generated — the production projection renderer over the core contract.
    sqlx::query(sqlx::AssertSqlSafe(projection))
        .bind(t)
        .bind(None::<&str>)
        .bind(schema.schema_id().as_str())
        .execute(pool)
        .await?;
    Ok(t)
}

async fn seed_derived(
    pool: &sqlx::PgPool,
    owner: OwnerId,
    origin: Uuid,
) -> Result<Uuid, sqlx::Error> {
    let owner_id = owner.into_inner();
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
            (handle, t, kind, owner_id, schema_id, origins, content_id, \"by\", via)
         VALUES ($1, $2, 'abstraction', $3, 'core/agent-note-v1', ARRAY[$4]::uuid[], $5, '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
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

/// How many decoy notes the plan pin needs: enough that walking them all
/// is visibly the worse plan.
const CORPUS_ROWS: i32 = 4_000;

/// Seed `CORPUS_ROWS` notes for `owner` containing `term`, each with its
/// admission and projection row.
///
/// Three statements, not one: `memory_align_head` is a BEFORE INSERT trigger
/// that reads `memory_head` back, and every CTE of one statement reads the
/// same snapshot, so heads written in a sibling CTE are invisible to it.
/// `agent_note_v1_declared_by_memory` reads `memory` back the same way, so
/// the admissions need a statement of their own too. The projection rows
/// ride along with the sidecars, because referential integrity is an AFTER
/// trigger.
async fn seed_projection_corpus(
    pool: &sqlx::PgPool,
    owner: OwnerId,
    term: &str,
) -> Result<(), sqlx::Error> {
    let owner_id = owner.into_inner();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         SELECT uuidv7(), 'fact', 'core/agent-note-v1', $1, uuidv7()
           FROM generate_series(1, $2)",
    )
    .bind(owner_id)
    .bind(CORPUS_ROWS)
    .execute(pool)
    .await?;
    // The stamp and the rows it promises land in one transaction.
    let mut stamped = pool.begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory
             (handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via)
         SELECT h.handle, h.t, 'fact', $1, 'core/agent-note-v1',
                ARRAY['proxima_core.agent_note_v1'], uuid_extract_timestamp(h.t), '00000000-0000-0000-0000-000000000509'::uuid, NULL
           FROM proxima_core.memory_head h
          WHERE h.owner_id = $1
            AND NOT EXISTS (SELECT 1 FROM proxima_core.memory m WHERE m.t = h.t)",
    )
    .bind(owner_id)
    .execute(&mut *stamped)
    .await?;
    sqlx::query(
        "WITH ids AS MATERIALIZED (
             SELECT m.t, row_number() OVER (ORDER BY m.t) AS n
               FROM proxima_core.memory m
              WHERE m.owner_id = $1
                AND m.sidecar_tables @> ARRAY['proxima_core.agent_note_v1']
                AND NOT EXISTS (
                        SELECT 1 FROM proxima_core.agent_note_v1 s WHERE s.t = m.t)
         ), notes AS (
             INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             SELECT t, uuidv7(), $2 || ' ' || n, $2 || ' body ' || n, '{}' FROM ids
             RETURNING t
         )
         INSERT INTO proxima_core.projection (memory_id, schema_id, owner_id, search_tsv)
         SELECT t, 'core/agent-note-v1', $1,
                to_tsvector('english', $2 || ' title ' || $2 || ' body ' || n)
           FROM ids",
    )
    .bind(owner_id)
    .bind(term)
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await
}

/// The first plan node scanning `relation`, or `None`.
fn scan_of(plan: &serde_json::Value, relation: &str) -> Option<serde_json::Value> {
    match plan {
        serde_json::Value::Array(items) => items.iter().find_map(|item| scan_of(item, relation)),
        serde_json::Value::Object(node) => {
            if node
                .get("Relation Name")
                .and_then(serde_json::Value::as_str)
                == Some(relation)
            {
                return Some(plan.clone());
            }
            node.values().find_map(|value| scan_of(value, relation))
        }
        _ => None,
    }
}

/// Whether the plan scans `relation` sequentially anywhere.
fn seq_scans(plan: &serde_json::Value, relation: &str) -> bool {
    match plan {
        serde_json::Value::Array(items) => items.iter().any(|item| seq_scans(item, relation)),
        serde_json::Value::Object(node) => {
            let text = |key: &str| node.get(key).and_then(serde_json::Value::as_str);
            (text("Node Type") == Some("Seq Scan") && text("Relation Name") == Some(relation))
                || node.values().any(|value| seq_scans(value, relation))
        }
        _ => false,
    }
}

/// Everything one plan node says it narrows on, whichever way the planner
/// chose to spell it. The declaration's claim is that the owner reaches
/// candidate selection at all — `MemoryFirstNestedLoop` explicitly does NOT
/// claim the composite index, which is the probe-measured point of it — so
/// an `Index Cond` and a `Filter` both satisfy it and the pin must not
/// prefer one.
fn predicates(node: &serde_json::Value) -> String {
    ["Index Cond", "Filter", "Recheck Cond"]
        .iter()
        .filter_map(|key| node.get(*key).and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `Index Cond` of the first scan of `index` in an
/// `EXPLAIN (FORMAT JSON)` tree, or `None` when the plan never reaches it.
fn gin_index_cond(plan: &serde_json::Value, index: &str) -> Option<String> {
    match plan {
        serde_json::Value::Array(items) => {
            items.iter().find_map(|item| gin_index_cond(item, index))
        }
        serde_json::Value::Object(node) => {
            if node.get("Index Name").and_then(serde_json::Value::as_str) == Some(index) {
                return Some(
                    node.get("Index Cond")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                );
            }
            node.values().find_map(|value| gin_index_cond(value, index))
        }
        _ => None,
    }
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
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let leaf = seed_note(pool, owner, "Needle title", "needle body").await?;
        let child = seed_derived(pool, owner, leaf).await?;
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_id, model_id, dim, embedding_version, vec, owner_id)
             VALUES ($1, 'test-embed', 1024, 1, $2, $3)",
        )
        .bind(leaf)
        .bind(pgvector::Vector::from(unit_values(EmbeddingDim::D1024)))
        .bind(owner.into_inner())
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_heads
                (entity_id, model_id, dim, embedding_version, owner_id)
             VALUES ($1, 'test-embed', 1024, 1, $2)",
        )
        .bind(leaf)
        .bind(owner.into_inner())
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_jobs (entity_id, model_id, dim, owner_id, status)
             VALUES ($1, 'test-embed', 1024, $2, 'pending')",
        )
        .bind(leaf)
        .bind(owner.into_inner())
        .execute(pool)
        .await?;
        // Both predicates must be selective: our large corpus does not
        // match the word, while the neighbour's large corpus does. An
        // owner-only scan through the tag GIN would visit our whole corpus.
        seed_projection_corpus(pool, owner, "haystack").await?;
        let neighbour = OwnerId::from(UserId::new(Uuid::now_v7()));
        seed_projection_corpus(pool, neighbour, "needle").await?;
        // Flush GIN pending lists before comparing settled read paths.
        sqlx::query("VACUUM (ANALYZE) proxima_core.projection")
            .execute(pool)
            .await?;
        // SQL-POLICY: fixed-fragment
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            "ANALYZE proxima_core.agent_note_v1;
             ANALYZE proxima_core.projection;
             ANALYZE proxima_core.memory;
             ANALYZE proxima_core.memory_head;
             ANALYZE proxima_core.embeddings;
             ANALYZE proxima_core.embedding_jobs",
        ))
        .execute(pool)
        .await?;

        let owner_ids = vec![owner.into_inner()];
        let req = search_req(owner);
        let like = proxima_core::verbs::query::like_pattern(&req.query);
        let projection = note_projection();
        let schema_ids = vec![projection.schema_id.as_str()];

        let mut tx = pool.begin().await?;
        sqlx::query("SET LOCAL enable_seqscan = off")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL enable_sort = off")
            .execute(&mut *tx)
            .await?;

        // ONE statement for the flavor. `schema_id` is a row predicate now
        // (`= ANY($8)`), and the LIKE pattern is not bound at all — which
        // renumbered every placeholder that followed it.
        let lexical = ranked_projection_sql_for_tests(&[&projection], &req, true)?;
        let lexical_explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {lexical}");
        // SQL-POLICY: PgIdent — production lexical builder
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(lexical_explain))
            .bind(&req.query)
            .bind(20_i64)
            .bind(None::<Vec<String>>)
            .bind(None::<time::OffsetDateTime>)
            .bind(None::<time::OffsetDateTime>)
            .bind(None::<Uuid>)
            .bind(&owner_ids)
            .bind(&schema_ids)
            .fetch_one(&mut *tx)
            .await?;
        let lexical_plan = plan.to_string();
        // The composite `gin(owner_id, search_tsv)` is the whole reason the
        // projection carries `owner_id`: both halves of the predicate are on
        // one relation, so one index scan answers "this owner's rows that
        // match this query" — where a sidecar-local tsvector GIN would have to
        // reach the owner through a join to `memory`.
        // Naming the index is not enough. A multicolumn GIN is searchable
        // on any SUBSET of its columns, so `core_projection_owner_tsv_gin`
        // shows up in the plan even with no owner predicate at all — as a
        // tsvector-only scan that walks every owner's postings and filters
        // afterwards. What has to hold is that `owner_id` is an INDEX
        // condition, which is true only while the owner is a predicate on
        // `p` itself rather than reached through a join.
        let index_cond = gin_index_cond(&plan, "core_projection_owner_tsv_gin").unwrap_or_else(
            || panic!("the ranked arm must reach the projection's composite GIN; plan:\n{lexical_plan}"),
        );
        assert!(
            index_cond.contains("owner_id"),
            "the composite GIN must index on owner_id, not filter on it afterwards; \
             index cond was `{index_cond}`; plan:\n{lexical_plan}"
        );
        assert!(
            index_cond.contains("search_tsv") && index_cond.contains("@@"),
            "the composite GIN must apply the text condition; \
             index cond was `{index_cond}`; plan:\n{lexical_plan}"
        );
        assert!(
            !lexical_plan.contains("agent_note_v1_search_tsv_gin"),
            "the per-sidecar tsvector index is gone; plan:\n{lexical_plan}"
        );

        // The admit-side restriction reaches both `memory` and `memory_head`,
        // and on a corpus this size it reaches them by index.  `HeadsOnly` is
        // the request shape above and the tool default, and putting the
        // restriction on the candidate side is only worth doing if reaching
        // the head is cheap.
        //
        // What this pin does NOT assert is the join STRATEGY. Rejecting
        // `Hash Join` outright, on the ground that it "would build over every
        // head in the deployment", forbids the plan that wins at scale: at
        // 25 000 heads the hash join runs in 51.5 ms against the nested loop's
        // 85.8 ms. The nested loop is right for the small corpus here and for
        // the sparse case the overfetch window is designed around; the hash
        // join is right once the candidate set is large. Both are correct plans
        // over the same indexes, and choosing between them is the planner's job
        // with statistics this test does not have. So: assert the index names
        // when the shape IS a nested loop, and let the planner have the other
        // shape.
        //
        // The cost the restriction actually adds is documented on
        // `admit_side_restriction`: one probe per MATCHING row rather than
        // per returned candidate, measured at +51% on 50 000 matching rows.
        let nested_loop = lexical_plan.contains("\"Node Type\":\"Nested Loop\"");
        let head = scan_of(&plan, "memory_head")
            .unwrap_or_else(|| panic!("HeadsOnly must reach memory_head; plan:\n{lexical_plan}"));
        let memory = scan_of(&plan, "memory")
            .unwrap_or_else(|| panic!("the admit-side restriction joins memory; plan:\n{lexical_plan}"));
        if nested_loop {
            assert_eq!(
                head.get("Index Name").and_then(serde_json::Value::as_str),
                Some("memory_head_pkey"),
                "the head lookup rides the handle primary key; plan:\n{lexical_plan}"
            );
            assert_eq!(
                memory.get("Index Name").and_then(serde_json::Value::as_str),
                Some("memory_t_key"),
                "…and reaches the memory row on its unique `t`; plan:\n{lexical_plan}"
            );
        }
        // There is deliberately no `else` arm. "Neither table may be read by a
        // sequential scan under any join strategy" is false by the same
        // measurement — the hash plan reads BOTH by seq scan (Hash Join over
        // Seq Scan memory + Seq Scan memory_head, with the projection on a
        // bitmap scan; measured at 55.1 ms). Such an arm would be green only
        // because this corpus plans a nested loop, and dead code asserting
        // something the plan it permits would fail. Two tables in the plan, and
        // the index names when the shape is a nested loop, is what this test
        // actually knows.

        // The substring arm plans as the nested loop it DECLARES.
        //
        // `MemoryFirstNestedLoop` is a claim about a plan: drive
        // `proxima_core.memory` on the owner index, probe the sidecar by
        // `t`, filter `LIKE` on an already-fetched row. The alternative — a
        // sidecar-first scan — is what the declaration's `why` records as a
        // probe-measured regression, and it is what a missing owner
        // predicate would silently produce. No trigram index exists on a
        // core sidecar, deliberately: an indexed sidecar-first scan would be a
        // candidate source carrying no `owner_id`.
        let substring = substring_sql_for_tests(&[&projection], &req)?;
        let substring_explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {substring}");
        // SQL-POLICY: PgIdent — production substring builder
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(substring_explain))
            .bind(&like)
            .bind(20_i64)
            .bind(None::<Vec<String>>)
            .bind(None::<time::OffsetDateTime>)
            .bind(None::<time::OffsetDateTime>)
            .bind(None::<Uuid>)
            .bind(&owner_ids)
            .bind(&schema_ids)
            .fetch_one(&mut *tx)
            .await?;
        let substring_plan = plan.to_string();
        assert!(
            substring_plan.contains("\"Node Type\":\"Nested Loop\""),
            "MemoryFirstNestedLoop is a claim about a plan; plan:\n{substring_plan}"
        );
        assert!(
            !substring_plan.contains("\"Node Type\":\"Seq Scan\""),
            "no sequential scan of a sidecar in the substring arm; plan:\n{substring_plan}"
        );
        let memory_scan = scan_of(&plan, "memory")
            .unwrap_or_else(|| panic!("the arm must drive from memory; plan:\n{substring_plan}"));
        assert!(
            predicates(&memory_scan).contains("owner_id"),
            "the owner must narrow candidates on `memory`, not only at admit; \
             memory node was `{memory_scan}`"
        );
        assert!(
            predicates(&memory_scan).contains("schema_id"),
            "the narrowed schema set must reach the scan; memory node was `{memory_scan}`"
        );
        let sidecar_scan = scan_of(&plan, "agent_note_v1").unwrap_or_else(|| {
            panic!("the arm must probe the sidecar; plan:\n{substring_plan}")
        });
        assert_eq!(
            sidecar_scan.get("Index Name").and_then(|v| v.as_str()),
            Some("agent_note_v1_pkey"),
            "the sidecar is PROBED by t, not scanned — that is the second half of \
             `MemoryFirstNestedLoop`; sidecar node was `{sidecar_scan}`"
        );

        // SQL-POLICY: fixed-fragment
        sqlx::raw_sql(sqlx::AssertSqlSafe(set_hnsw_search_sql_for_tests(
            &proxima_storage_pg::PgTuning::default(),
        )))
        .execute(&mut *tx)
        .await?;
        // Every width lane is served by its own partial HNSW index: the
        // statement spells the lane's expression and a literal `dim = N`,
        // so the planner proves the index predicate under any plan.
        for dim in EmbeddingDim::ALL {
            let semantic = proxima_storage_pg::vector_index::candidate_scan_sql_for_tests(dim);
            let semantic_explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {semantic}");
            // SQL-POLICY: PgIdent — production semantic builder
            let lane_plan: serde_json::Value =
                sqlx::query_scalar(sqlx::AssertSqlSafe(semantic_explain))
                    .bind(&owner_ids)
                    .bind("test-embed")
                    .bind(pgvector::Vector::from(unit_values(dim)))
                    .bind(20_i64)
                    .fetch_one(&mut *tx)
                    .await?;
            assert_plan_names(&lane_plan, &format!("embeddings_hnsw_d{}", dim.width()));
        }
        let semantic_admission = semantic_candidate_admission_sql_for_tests(
            &req,
            std::slice::from_ref(&projection),
        )?;
        let explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {semantic_admission}");
        // SQL-POLICY: PgIdent — production candidate admission builder.
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(explain))
            .bind(vec![leaf])
            .bind(&owner_ids)
            .bind(Some("fact"))
            .bind(None::<&str>)
            .bind(None::<time::OffsetDateTime>)
            .bind(None::<time::OffsetDateTime>)
            .bind(&schema_ids)
            .fetch_one(&mut *tx)
            .await?;
        let projection_scan = scan_of(&plan, "projection")
            .unwrap_or_else(|| panic!("semantic admission must probe participating schemas; plan:\n{plan}"));
        assert!(
            predicates(&projection_scan).contains("schema_id"),
            "the participating schema set must narrow semantic admission; plan:\n{plan}"
        );

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

        let mut page = QueryRequest::readable();
        page.schema_id = Some(SchemaId::new("core/agent-note-v1".into()));
        page.entity_kind = Some(EntityKind::Fact);
        page.limit = 20;
        let heads = memory_page_sql_for_tests(&page)?;
        let heads_explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {heads}");
        // SQL-POLICY: fixed-fragment
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(heads_explain))
            .bind(&owner_ids)
            .bind("core/agent-note-v1")
            .fetch_one(&mut *tx)
            .await?;
        let heads_plan = plan.to_string();
        // Any index ON `memory_head`, not a named one. Which of
        // `memory_head_owner_schema_idx`, `memory_head_owner_kind_idx` and
        // `memory_head_pkey` the planner picks is a function of the
        // corpus's shape, and this assertion is about the access path, not
        // about the planner's choice among equally indexed ones. Naming a
        // subset makes the pin fail as soon as the fixture grows.
        assert!(
            heads_plan.contains("\"Index Name\":\"memory_head"),
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

        let inbound = inbound_pin_sql_for_tests(
            SupersessionStatus::HeadsOnly,
            Some(EdgeKind::Origin),
            PinTargetKind::Memory,
        );
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
            .bind(32_i64)
            .bind(&owner_ids)
            .bind(Vec::<Uuid>::new())
            .bind(Vec::<String>::new())
            .bind(Vec::<i16>::new())
            .fetch_one(&mut *tx)
            .await?;
        assert_plan_names(&plan, "embedding_jobs_pending_claim_idx");

        // The natural-key admit. `core/agent-note-v1` is the only core
        // contract declaring `natural_key_columns`, and every admission of a
        // note resolves its series through this statement before it writes.
        // It is a WRITE-path read, which is why it never showed up here
        // before: the file guards hot reads, and this one hides inside an
        // ingest.
        //
        // Without `idx_agent_note_v1_nk` the sidecar is reachable only by its
        // primary key, so the planner inverts the join — walk the owner's
        // `memory`, probe `agent_note_v1` by `t`, filter `note_id` after the
        // fact — and the cost of admitting one note grows with everything the
        // owner has ever stored.
        let note_key = Uuid::now_v7();
        let nk_sql = owned_head_handle_sql_for_tests(
            owner,
            &projection.schema_id,
            "proxima_core.agent_note_v1",
            "t",
            &[("note_id", SidecarAtom::Uuid(note_key))],
        )?;
        let nk_explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {nk_sql}");
        // SQL-POLICY: PgIdent — production series-handle builder
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(nk_explain))
            .bind(owner.into_inner())
            .bind(projection.schema_id.as_str())
            .bind(note_key)
            .fetch_one(&mut *tx)
            .await?;
        let nk_plan = plan.to_string();
        assert_plan_names(&plan, "idx_agent_note_v1_nk");
        // Naming the index is not enough: it has to carry `note_id` as the
        // index condition. A plan that reaches the index and then filters is
        // the same walk wearing a different name.
        let nk_scan = scan_of(&plan, "agent_note_v1").unwrap_or_else(|| {
            panic!("the natural-key admit must touch agent_note_v1; plan:\n{nk_plan}")
        });
        let cond = nk_scan
            .get("Index Cond")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(
            cond.contains("note_id"),
            "the natural key must be the index condition, not a post-filter; \
             cond was `{cond}`, filter was `{}`; plan:\n{nk_plan}",
            predicates(&nk_scan)
        );

        tx.rollback().await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("hot-path EXPLAIN failed");
}

/// The table's primary-key columns in index order, read the way the export
/// reads them.
async fn primary_key(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT a.attname::text
           FROM pg_index i
          CROSS JOIN LATERAL unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord)
           JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
          WHERE i.indrelid = $1::regclass
            AND i.indisprimary
          ORDER BY k.ord",
    )
    .bind(table)
    .fetch_all(&mut **tx)
    .await
}

/// Owner export narrows on the owner through an index (#490).
///
/// `IS NOT DISTINCT FROM` is not an indexable operator, so the export used
/// to read every owner's `memory` rows and filter them: the cost of one
/// small owner's export grew with the whole store. Planned as the export
/// runs it — the platform role in the platform scope, with real statistics
/// and `enable_seqscan` left on — for a surface carrying its own owner
/// column (`memory`) and for a keyed sidecar reaching its owner through
/// `memory` (`agent_note_v1`).
#[tokio::test]
async fn owner_export_reaches_its_owner_through_an_index() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        seed_note(pool, owner, "small owner", "one note").await?;
        let neighbour = OwnerId::from(UserId::new(Uuid::now_v7()));
        seed_projection_corpus(pool, neighbour, "neighbour").await?;
        // SQL-POLICY: fixed-fragment
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            "ANALYZE proxima_core.memory; ANALYZE proxima_core.agent_note_v1",
        ))
        .execute(pool)
        .await?;

        let mut tx = pg.platform_scope_for_host().begin().await?;
        for table in ["proxima_core.memory", "proxima_core.agent_note_v1"] {
            let surface = proxima_core::FLAVOR_0
                .all_surfaces()
                .find(|surface| surface.table == table)
                .unwrap_or_else(|| panic!("flavor #0 declares {table}"));
            let primary_key = primary_key(&mut tx, table).await?;
            let sql = export_statement_sql_for_tests(&surface, &primary_key)?
                .unwrap_or_else(|| panic!("{table} is exported"));
            let explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {sql}");
            // SQL-POLICY: PgIdent — production export builder
            let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(explain))
                .bind(owner.into_inner())
                .fetch_one(&mut *tx)
                .await?;
            let rendered = plan.to_string();
            let memory = scan_of(&plan, "memory").unwrap_or_else(|| {
                panic!("the {table} export must reach memory; plan:\n{rendered}")
            });
            let node = memory
                .get("Node Type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let cond = memory
                .get("Index Cond")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            assert!(
                node != "Seq Scan" && cond.contains("owner_id"),
                "the {table} export must narrow memory on owner_id by index, not \
                 walk every owner (node `{node}`, cond `{cond}`, filter `{}`); plan:\n{rendered}",
                predicates(&memory)
            );
        }
        // `=` and `IS NOT DISTINCT FROM` agree only because neither side is
        // ever NULL: `$1` is an `OwnerId`, and every column the filter reads
        // must be NOT NULL in the catalog.
        for surface in proxima_core::FLAVOR_0.all_surfaces() {
            if !surface.export.carries_rows() {
                continue;
            }
            let (table, column) = match (surface.owner_column, surface.key.home()) {
                (Some(column), _) => (surface.table, column),
                (None, Some((home, _, _))) => (home, "owner_id"),
                (None, None) => continue,
            };
            let not_null: bool = sqlx::query_scalar(
                "SELECT attnotnull FROM pg_attribute
                  WHERE attrelid = $1::regclass AND attname = $2 AND NOT attisdropped",
            )
            .bind(table)
            .bind(column)
            .fetch_one(&mut *tx)
            .await?;
            assert!(
                not_null,
                "{}: export filters {table}.{column} with `=`, so it must be NOT NULL",
                surface.table
            );
        }
        tx.rollback().await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owner export EXPLAIN failed");
}

/// The retention prune reads the delivered records it deletes, through
/// `publication_outbox_published_idx` (#491).
///
/// Without an index on `published_at` each batch scanned the whole outbox:
/// one pass cost the table, not the batch. Planned with real statistics and
/// `enable_seqscan` left on, over a backlog of pending records beside a pile
/// of old delivered ones.
#[tokio::test]
async fn retention_prune_reads_the_published_index() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')",
        )
        .bind(owner.into_inner())
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.publication_outbox
                 (t, owner_id, schema_id, schema_version, event_type, event_id,
                  envelope, envelope_digest, state, published_at)
             SELECT uuidv7(), $1, 'core/agent-note-v1', 1, 'type', 'event-' || n,
                    '\\x00'::bytea, decode(repeat('00', 32), 'hex'),
                    CASE WHEN n % 4 = 0 THEN 'pending' ELSE 'published' END::proxima_core.publication_record_state,
                    CASE WHEN n % 4 = 0 THEN NULL ELSE now() - make_interval(secs => 100000 - n) END
               FROM generate_series(1, 40000) AS n",
        )
        .bind(owner.into_inner())
        .execute(pool)
        .await?;
        // SQL-POLICY: fixed-fragment
        sqlx::raw_sql(sqlx::AssertSqlSafe("ANALYZE proxima_core.publication_outbox"))
            .execute(pool)
            .await?;

        let mut tx = pg.platform_scope_for_host().begin().await?;
        let explain = format!(
            "EXPLAIN (FORMAT JSON, COSTS OFF) {}",
            prune_published_sql_for_tests()
        );
        // SQL-POLICY: fixed-fragment — the production prune statement.
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(explain))
            .bind(3600.0_f64)
            .bind(1000_i64)
            .fetch_one(&mut *tx)
            .await?;
        let rendered = plan.to_string();
        assert!(
            rendered.contains("publication_outbox_published_idx"),
            "the prune must read the published index; plan:\n{rendered}"
        );
        assert!(
            !rendered.contains("\"Node Type\":\"Seq Scan\""),
            "the prune must not scan the outbox; plan:\n{rendered}"
        );
        tx.rollback().await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("retention prune EXPLAIN failed");
}

/// The stale-claim reclaim reads the jobs in flight, not the queue (#491).
///
/// A queue of 60,000 jobs, a few hundred of them `processing`: the reclaim
/// statement reaches those through `embedding_jobs_processing_idx` with
/// `enable_seqscan` left on.
#[tokio::test]
async fn the_stale_claim_reclaim_reads_the_processing_index() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner.into_inner())
            .execute(pool)
            .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_jobs
                 (entity_id, model_id, owner_id, status, claimed_at, claim_token, dim)
             SELECT uuidv7(), 'model', $1,
                    CASE WHEN n % 200 = 0 THEN 'processing' ELSE 'pending' END
                        ::proxima_core.embedding_job_status,
                    CASE WHEN n % 200 = 0 THEN now() - interval '1 hour' END,
                    CASE WHEN n % 200 = 0 THEN uuidv7() END,
                    384
               FROM generate_series(1, 60000) AS n",
        )
        .bind(owner.into_inner())
        .execute(pool)
        .await?;
        // SQL-POLICY: fixed-fragment
        sqlx::raw_sql(sqlx::AssertSqlSafe("ANALYZE proxima_core.embedding_jobs"))
            .execute(pool)
            .await?;

        let mut tx = pg.platform_scope_for_host().begin().await?;
        let explain = format!(
            "EXPLAIN (FORMAT JSON, COSTS OFF) {}",
            reclaim_stale_embedding_jobs_sql_for_tests()
        );
        // SQL-POLICY: fixed-fragment — the production reclaim statement.
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(explain))
            .bind(900_i64)
            .fetch_one(&mut *tx)
            .await?;
        let rendered = plan.to_string();
        assert!(
            rendered.contains("embedding_jobs_processing_idx"),
            "the reclaim must read the processing index; plan:\n{rendered}"
        );
        assert!(
            !seq_scans(&plan, "embedding_jobs"),
            "the reclaim must not scan the queue; plan:\n{rendered}"
        );
        tx.rollback().await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("stale-claim reclaim EXPLAIN failed");
}

/// A write's capacity check reads one row however long the backlog is (#491).
///
/// With 50,000 undelivered records and a fresh estimate, the statement that
/// admits a write touches `publication_backlog` alone and returns that single
/// row. The backlog is never read: the exact probe it replaces read up to
/// `max_pending` index entries of it.
#[tokio::test]
async fn a_capacity_check_reads_one_row_whatever_the_backlog() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner.into_inner())
            .execute(pool)
            .await?;
        sqlx::query(
            "INSERT INTO proxima_core.publication_outbox
                 (t, owner_id, schema_id, schema_version, event_type, event_id,
                  envelope, envelope_digest)
             SELECT uuidv7(), $1, 'core/agent-note-v1', 1, 'type', 'event-' || n,
                    '\\x00'::bytea, decode(repeat('00', 32), 'hex')
               FROM generate_series(1, 50000) AS n",
        )
        .bind(owner.into_inner())
        .execute(pool)
        .await?;
        sqlx::query(
            "UPDATE proxima_core.publication_backlog SET pending = 50000, measured_at = now()",
        )
        .execute(pool)
        .await?;
        // SQL-POLICY: fixed-fragment
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            "ANALYZE proxima_core.publication_outbox; ANALYZE proxima_core.publication_backlog",
        ))
        .execute(pool)
        .await?;

        let explain = format!(
            "EXPLAIN (ANALYZE, FORMAT JSON) {}",
            fresh_backlog_estimate_sql_for_tests()
        );
        // SQL-POLICY: fixed-fragment — the production capacity-check read.
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(explain))
            .bind(30.0_f64)
            .fetch_one(pool)
            .await?;
        let rendered = plan.to_string();
        assert!(
            scan_of(&plan, "publication_backlog").is_some(),
            "the check reads the estimate; plan:\n{rendered}"
        );
        assert!(
            scan_of(&plan, "publication_outbox").is_none(),
            "the check must not read the backlog; plan:\n{rendered}"
        );
        let rows = plan[0]["Plan"]["Actual Rows"].as_f64().unwrap_or(f64::MAX);
        assert!(rows <= 1.0, "one row, not {rows}; plan:\n{rendered}");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("capacity check EXPLAIN failed");
}

/// An erase reads the uploads of its blobs by blob, not by scanning the store's
/// uploads (#3596).
///
/// 30,000 uploads of other owners beside the erased owner's one blob: the
/// fence-keys read and the unreferenced-blob sweep reach the upload through
/// `blob_uploads_blob_id_idx` with `enable_seqscan` left on, and a blob row
/// leaves with its foreign-key check answered by the same index.
#[tokio::test]
async fn a_blob_erase_reads_its_uploads_through_the_blob_index() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner.into_inner())
            .execute(pool)
            .await?;
        sqlx::query(
            "WITH b AS (
                 INSERT INTO proxima_core.blob (blob_id, owner_id, schema_id, content_hash)
                 SELECT uuidv7(), $1, 'blob/x', sha256(convert_to(n::text, 'UTF8'))
                   FROM generate_series(1, 30000) AS n
                 RETURNING blob_id, owner_id
             )
             INSERT INTO proxima_core.blob_uploads
                 (upload_id, owner_id, bucket, object_key, filename, mime, expected_byte_len,
                  status, blob_id, expires_at, object_store)
             SELECT u, owner_id, 'b', 'objects/' || u::text, 'f', 'text/plain', 10,
                    'completed', blob_id, now() + interval '1 day', 'obj:main'
               FROM (SELECT uuidv7() AS u, blob_id, owner_id FROM b) AS rows",
        )
        .bind(owner.into_inner())
        .execute(pool)
        .await?;
        // SQL-POLICY: fixed-fragment
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            "ANALYZE proxima_core.blob; ANALYZE proxima_core.blob_uploads",
        ))
        .execute(pool)
        .await?;
        let blob: Uuid = sqlx::query_scalar("SELECT blob_id FROM proxima_core.blob LIMIT 1")
            .fetch_one(pool)
            .await?;

        let mut tx = pg.platform_scope_for_host().begin().await?;
        let fence = format!(
            "EXPLAIN (FORMAT JSON, COSTS OFF) {}",
            proxima_storage_pg::access::owner_columns::upload_fence_keys_sql_for_tests()
        );
        // SQL-POLICY: fixed-fragment — the production fence-keys read.
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(fence))
            .bind(vec![blob])
            .fetch_one(&mut *tx)
            .await?;
        assert!(
            !seq_scans(&plan, "blob_uploads")
                && plan.to_string().contains("blob_uploads_blob_id_idx"),
            "the fence keys must read the blob index; plan:\n{plan}"
        );
        let sweep = format!(
            "EXPLAIN (FORMAT JSON, COSTS OFF) {}",
            proxima_storage_pg::verbs::forget::gc_unreferenced_blobs_sql_for_tests()
        );
        // SQL-POLICY: fixed-fragment — the production unreferenced-blob sweep.
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(sweep))
            .bind(vec![blob])
            .bind(owner.into_inner())
            .fetch_one(&mut *tx)
            .await?;
        assert!(
            !seq_scans(&plan, "blob_uploads"),
            "the sweep must not scan the store's uploads; plan:\n{plan}"
        );
        // The foreign-key check of a blob delete asks `blob_uploads` for the blob.
        let plan: serde_json::Value = sqlx::query_scalar(
            "EXPLAIN (FORMAT JSON, COSTS OFF)
             SELECT 1 FROM ONLY proxima_core.blob_uploads x WHERE x.blob_id = $1 FOR KEY SHARE OF x",
        )
        .bind(blob)
        .fetch_one(&mut *tx)
        .await?;
        assert!(
            plan.to_string().contains("blob_uploads_blob_id_idx"),
            "the foreign-key check must read the blob index; plan:\n{plan}"
        );
        tx.rollback().await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("blob erase EXPLAIN failed");
}

/// Sequential scans started, and rows deleted, per erased table.
async fn erase_counts(
    pool: &sqlx::PgPool,
) -> Result<std::collections::BTreeMap<String, (i64, i64)>, sqlx::Error> {
    sqlx::query("SELECT pg_stat_clear_snapshot()")
        .execute(pool)
        .await?;
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT relname::text, seq_scan, n_tup_del FROM pg_stat_user_tables
          WHERE schemaname = 'proxima_core'
            AND relname IN ('sketch', 'embedding_heads', 'embeddings',
                            'publication_outbox', 'publication_origin')",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(n, s, d)| (n, (s, d))).collect())
}

/// An owner erase reaches the rows it deletes through their keys, not by
/// reading every owner's table (#3596).
///
/// A small group beside a bystander with 5,000 memories, each with a sketch,
/// an embedding, a publication record and its origin. Erasing the small group
/// must not start a sequential scan of those tables: each table's statistics
/// say how many scans the erase started.
#[tokio::test]
async fn an_owner_erase_does_not_scan_other_owners_tables() {
    use proxima_core::GroupId;
    use proxima_core::owner_inverse::{
        EraseAuthorization, OwnerEraseOutcome, OwnerEraseTarget, OwnerSurfaces,
    };
    use proxima_core::storage_ports::OwnerInversePort;
    use proxima_storage_pg::test_fixtures::register_group_for_tests;

    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let target = GroupId::new(Uuid::now_v7());
        let bystander = GroupId::new(Uuid::now_v7());
        register_group_for_tests(pool, target).await?;
        register_group_for_tests(pool, bystander).await?;
        for (owner, count) in [(target, 40_i32), (bystander, 5_000_i32)] {
            sqlx::query(
                "WITH s AS (SELECT uuidv7() AS handle, uuidv7() AS t FROM generate_series(1, $2::int)),
                 memory AS (
                     INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, occurred_at, \"by\")
                     SELECT handle, t, 'fact', $1, 'erase/x', now(), '00000000-0000-0000-0000-000000000509'::uuid FROM s),
                 head AS (
                     INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
                     SELECT handle, 'fact', 'erase/x', $1, t FROM s),
                 sketch AS (
                     INSERT INTO proxima_core.sketch (t, owner_id, kind, text)
                     SELECT t, $1, 'fact', 'sketch' FROM s),
                 origin AS (
                     INSERT INTO proxima_core.publication_origin (t, original_owner_id, source_id)
                     SELECT t, $1, NULL FROM s),
                 outbox AS (
                     INSERT INTO proxima_core.publication_outbox
                         (t, owner_id, schema_id, schema_version, event_type, event_id,
                          envelope, envelope_digest, state, published_at)
                     SELECT t, $1, 'core/x', 1, 'type', 'e-' || t, '\\x00'::bytea,
                            decode(repeat('00', 32), 'hex'), 'published', now() FROM s),
                 heads AS (
                     INSERT INTO proxima_core.embedding_heads (entity_id, model_id, embedding_version, owner_id, dim)
                     SELECT t, 'm', 1, $1, 384 FROM s)
                 INSERT INTO proxima_core.embeddings (entity_id, model_id, dim, embedding_version, vec, owner_id)
                 SELECT t, 'm', 384, 1, array_fill(0.5::real, ARRAY[384])::vector, $1 FROM s",
            )
            .bind(owner.into_inner())
            .bind(count)
            .execute(pool)
            .await?;
        }
        // SQL-POLICY: fixed-fragment
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            "ANALYZE proxima_core.memory; ANALYZE proxima_core.sketch;
             ANALYZE proxima_core.publication_origin; ANALYZE proxima_core.publication_outbox;
             ANALYZE proxima_core.embedding_heads; ANALYZE proxima_core.embeddings",
        ))
        .execute(pool)
        .await?;

        // Sequential scans started, and rows deleted, per table, as the
        // statistics collector has them (`erase_counts`). Another backend's
        // counts arrive when it goes idle, so the deletes are the event to wait for.
        let before = erase_counts(pool).await?;
        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner {
            group_id: target,
        });
        let outcome = pg
            .erase_group_owner(
                &auth,
                target,
                &OwnerSurfaces::for_registry(
                    &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
                ),
            )
            .await?;
        assert!(matches!(outcome, OwnerEraseOutcome::Completed { .. }));
        let mut after = erase_counts(pool).await?;
        for _ in 0..100 {
            if after.iter().all(|(table, (_, deleted))| *deleted > before[table].1) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            after = erase_counts(pool).await?;
        }
        for (table, (scans, deleted)) in &after {
            assert!(
                *deleted > before[table].1,
                "the statistics never showed the erase's deletes from {table}"
            );
            assert_eq!(
                *scans, before[table].0,
                "the erase scanned {table} sequentially"
            );
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owner erase scan test failed");
}

/// Every table a forget deletes from by key has an index led by that key
/// (#3596).
///
/// The legs come from the declarations, so a table declared later is held to
/// the same rule. A plan cannot show this: with an index that merely contains
/// the column, the planner reads the whole index.
#[tokio::test]
async fn every_forget_leg_has_an_index_led_by_its_key() {
    use proxima_core::owner_inverse::OwnerSurfaces;
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let surfaces = OwnerSurfaces::for_registry(
            &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
        );
        let legs = surfaces.generated_forget_legs();
        assert!(!legs.is_empty(), "the shipped flavor declares forget legs");
        for (table, key_column) in legs {
            let led: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                     SELECT 1 FROM pg_index i
                       JOIN pg_attribute a
                         ON a.attrelid = i.indrelid AND a.attnum = i.indkey[0]
                      WHERE i.indrelid = $1::regclass AND i.indpred IS NULL
                        AND a.attname = $2)",
            )
            .bind(table)
            .bind(key_column)
            .fetch_one(pg.pool_for_tests())
            .await?;
            assert!(
                led,
                "{table} needs an index led by {key_column} for its forget leg"
            );
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("forget leg index check failed");
}
