//! EXPLAIN plan-shape guards for the hot reads.
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
    memory_page_sql_for_tests, ranked_projection_sql_for_tests, search_admit_sql_for_tests,
    semantic_search_sql_for_tests, set_hnsw_search_sql_for_tests, substring_sql_for_tests,
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
    // The stamp and the row it promises land in one transaction: a memory row
    // that names a sidecar table it has no row in is refused at COMMIT.
    let mut stamped = pool.begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory
             (handle, t, kind, owner_id, schema_id, sidecar_tables)
         VALUES ($1, $2, 'fact', $3, 'core/agent-note-v1',
                 ARRAY['proxima_core.agent_note_v1'])",
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

/// How many decoy notes the plan pin needs: enough that walking them all
/// is visibly the worse plan.
const CORPUS_ROWS: i32 = 4_000;

/// Seed `CORPUS_ROWS` notes for `owner`, every one of them matching the
/// query the pin runs, each with its admission and projection row.
///
/// Three statements, not one: `memory_align_head` is a BEFORE INSERT trigger
/// that reads `memory_head` back, and every CTE of one statement reads the
/// same snapshot, so heads written in a sibling CTE are invisible to it.
/// `agent_note_v1_declared_by_memory` reads `memory` back the same way, so
/// the admissions need a statement of their own too. The projection rows
/// ride along with the sidecars, because referential integrity is an AFTER
/// trigger.
async fn seed_projection_corpus(pool: &sqlx::PgPool, owner: OwnerRef) -> Result<(), sqlx::Error> {
    let owner_id = owner.stored_owner_id();
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
             (handle, t, kind, owner_id, schema_id, sidecar_tables)
         SELECT h.handle, h.t, 'fact', $1, 'core/agent-note-v1',
                ARRAY['proxima_core.agent_note_v1']
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
                AND NOT EXISTS (
                        SELECT 1 FROM proxima_core.agent_note_v1 s WHERE s.t = m.t)
         ), notes AS (
             INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             SELECT t, uuidv7(), 'Needle ' || n, 'needle body ' || n, '{}' FROM ids
             RETURNING t
         )
         INSERT INTO proxima_core.projection (memory_id, schema_id, owner_id, search_tsv)
         SELECT t, 'core/agent-note-v1', $1,
                to_tsvector('english', 'needle title needle body ' || n)
           FROM ids",
    )
    .bind(owner_id)
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
        // The situation the composite index exists for: a NEIGHBOUR owner
        // whose whole corpus matches the same query word. Without it the
        // table is two rows, every access path costs the same, and the
        // plan says nothing about the composite index at all.
        let neighbour = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        seed_projection_corpus(pool, neighbour).await?;
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

        let owner_ids = vec![owner.stored_owner_id()];
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

        let inbound = inbound_pin_sql_for_tests(true, Some(EdgeKind::Origin), false);
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
