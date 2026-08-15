//! Behaviour and plan pins for the endpoint-prefiltered edge scans
//! (sql-sweep S5).
//!
//! Every prefiltered statement keeps the shipped resolved-endpoint predicate
//! verbatim as its residual, so what can go wrong is the prefilter dropping
//! a row the residual would have kept. The sharp case is a Fact-entity head:
//! the raw endpoint column holds the fact-entity id, not the memory id, so
//! only `head_probe` can reach it. These tests pin that case across all
//! three readers, pin the World-source redaction the prefilter must not
//! relax, and pin that the edges scan rides the endpoint indexes under
//! DEFAULT planner costing with a crowd in the table (the S36 trap: a
//! one-row fixture with seqscan disabled proves capability, not the plan
//! the corpus gets).

use proxima_core::storage_ports::*;
use proxima_core::verbs::query::{
    MemoryLineageDirection, MemoryLineageRequest, QueryPage, QueryRequest, SupersessionStatus,
    TombstoneFilter,
};
use proxima_core::{EdgeKind, EntityKind, MemoryId, OwnerRef, UserId};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use crate::common::{drop_db, fresh_pg, seed_memory, seed_memory_edge};

/// A Fact-entity row heading `memory`, straight into the table: the read
/// paths under test resolve `FactEntityHead` endpoints through
/// `current_memory_id`, and that resolution is exactly what the prefilter
/// must not lose.
async fn seed_fact_entity_head(
    pg: &PgStorage,
    owner: &OwnerRef,
    memory: MemoryId,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let fact_entity_id = Uuid::now_v7();
    let (owner_kind, owner_id) = crate::common::owner_parts(owner);
    sqlx::query(
        "INSERT INTO proxima_core.fact_entities
            (fact_entity_id, owner_kind, owner_id, schema_id, schema_version,
             natural_key, current_memory_id, current_created_at)
         VALUES ($1, $2, $3, 'test/edge-access-fact-v1', 1,
                 ARRAY['prefilter-probe'], $4, now())",
    )
    .bind(fact_entity_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(memory.into_inner())
    .execute(pg.pool_for_tests())
    .await?;
    Ok(fact_entity_id)
}

/// An edge whose target is a Fact-entity head, straight into the table
/// (same rationale as `seed_memory_edge`: read-path fixtures need graphs
/// production writes only as part of larger verbs).
async fn seed_head_target_edge(
    pg: &PgStorage,
    owner: &OwnerRef,
    source: (EntityKind, MemoryId),
    fact_entity_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let (owner_kind, owner_id) = crate::common::owner_parts(owner);
    let (source_kind, source_memory_id) = source;
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (source_kind, source_id, target_kind, target_id, kind, owner_kind, owner_id)
         VALUES ($1::text::proxima_core.edge_endpoint_kind, $2,
                 'FactEntityHead', $3, 'origin', $4, $5)",
    )
    .bind(source_kind.as_str())
    .bind(source_memory_id.into_inner())
    .bind(fact_entity_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

/// Move a memory to the World owner, the way `publish_to_world` does.
/// Edges cannot be written out of a World memory —
/// `validate_edge_invariants` requires the edge's owner to equal the source
/// endpoint's owner and `edges_world_not_write_owner_chk` forbids a World
/// edge owner — so the only way a World memory acquires outgoing edges is
/// this one: they are written under its original owner, and publishing
/// moves the row afterwards. That is precisely the state the walk's
/// redaction rule exists for.
async fn publish_to_world(pg: &PgStorage, memory: MemoryId) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE proxima_core.memories
            SET owner_kind = 'world', owner_id = NULL
          WHERE memory_id = $1",
    )
    .bind(memory.into_inner())
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

struct Graph {
    owner: OwnerRef,
    fact: MemoryId,
    head_fact: MemoryId,
    abstraction: MemoryId,
    perspective: MemoryId,
}

/// P ← A ← {F1, head(F2)}: a plain chain plus one edge that reaches its
/// memory only through a Fact-entity head.
async fn seed_graph(pg: &PgStorage) -> Result<Graph, Box<dyn std::error::Error>> {
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let fact = seed_memory(pg, &owner, EntityKind::Fact, "the plain observation").await?;
    let head_fact = seed_memory(pg, &owner, EntityKind::Fact, "the headed observation").await?;
    let abstraction = seed_memory(pg, &owner, EntityKind::Abstraction, "the summary").await?;
    let perspective = seed_memory(pg, &owner, EntityKind::Perspective, "the judgment").await?;
    seed_memory_edge(
        pg,
        &owner,
        (EntityKind::Abstraction, abstraction),
        (EntityKind::Fact, fact),
        EdgeKind::Origin,
    )
    .await?;
    seed_memory_edge(
        pg,
        &owner,
        (EntityKind::Perspective, perspective),
        (EntityKind::Abstraction, abstraction),
        EdgeKind::Origin,
    )
    .await?;
    let head = seed_fact_entity_head(pg, &owner, head_fact).await?;
    seed_head_target_edge(pg, &owner, (EntityKind::Abstraction, abstraction), head).await?;
    Ok(Graph {
        owner,
        fact,
        head_fact,
        abstraction,
        perspective,
    })
}

/// The neighbor window still reaches every requested memory, including the
/// one whose only edge names it through a Fact-entity head — the row the
/// raw-column filter alone would miss and `head_probe` exists to keep.
#[tokio::test]
async fn the_neighbor_window_reaches_head_resolved_endpoints()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let graph = seed_graph(&pg).await?;

        for (requested, expected) in [
            (vec![graph.abstraction], 3_usize),
            (vec![graph.head_fact], 1),
            (vec![graph.fact, graph.perspective], 2),
        ] {
            let rows = pg
                .load_neighbor_memory_edges(&[graph.owner], &requested, 50)
                .await?;
            assert_eq!(
                rows.len(),
                expected,
                "neighbor window for {requested:?} returned {rows:?}"
            );
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

fn snapshot_request(owner: OwnerRef) -> QueryRequest {
    QueryRequest {
        owner,
        read_owners: vec![owner],
        entity_kind: None,
        schema_id: None,
        supersession: SupersessionStatus::HeadsOnly,
        tombstones: TombstoneFilter::PresentOnly,
        goal_state: None,
        limit: 50,
        page: QueryPage::default(),
        include_payloads: false,
        memory_ids: Vec::new(),
        goal_ids: Vec::new(),
        stateful_heads: Vec::new(),
    }
}

/// The snapshot closure covers the whole fixture graph, head-resolved
/// endpoints included.
#[tokio::test]
async fn the_snapshot_closure_covers_head_resolved_endpoints()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let graph = seed_graph(&pg).await?;
        let response = pg
            .query_memories(&snapshot_request(graph.owner), &[])
            .await?;
        assert_eq!(
            response.edges.len(),
            3,
            "the closure must cover the whole fixture graph, got {:?}",
            response.edges
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

fn walk_request(
    owner: OwnerRef,
    start: MemoryId,
    direction: MemoryLineageDirection,
) -> MemoryLineageRequest {
    MemoryLineageRequest {
        owner,
        start_memory_id: start,
        direction,
        depth: 4,
        limit: 50,
        after: None,
    }
}

/// The walk traverses both directions. Descendants from the headed fact is
/// the sharp case: the seed edge's raw target column is the fact-entity id,
/// so only the head probe anchors the step.
#[tokio::test]
async fn the_walk_traverses_head_resolved_endpoints_in_both_directions()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let graph = seed_graph(&pg).await?;

        for (start, direction, expected) in [
            (
                graph.perspective,
                MemoryLineageDirection::Ancestors,
                3_usize,
            ),
            (graph.fact, MemoryLineageDirection::Descendants, 2),
            (graph.head_fact, MemoryLineageDirection::Descendants, 2),
        ] {
            let walk = pg
                .walk_memory_lineage(&[graph.owner], &walk_request(graph.owner, start, direction))
                .await?;
            assert_eq!(
                walk.edges.len(),
                expected,
                "{direction:?} walk from {start:?} returned {:?}",
                walk.edges
            );
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// A World-owned memory is readable by everyone, so an edge out of one
/// would otherwise disclose the existence of a private memory it was made
/// from. The walk drops such an edge outright — `NOT (source_world_visible
/// AND NOT target_readable)` — and the prefilter must not relax that, since
/// the prefilter runs *before* the visibility residual and the redaction is
/// the one predicate whose loss returns rows rather than dropping them.
///
/// The readable World-sourced edge in the same fixture is the control: it
/// proves the walk reaches edges from this start at all, so the redacted
/// case coming back empty is the rule and not a dead traversal.
#[tokio::test]
async fn a_world_sourced_edge_is_redacted_when_its_target_is_unreadable()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let author = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let outsider = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let secret = seed_memory(&pg, &outsider, EntityKind::Fact, "the private source").await?;
        let published = seed_memory(
            &pg,
            &author,
            EntityKind::Abstraction,
            "the published summary",
        )
        .await?;
        let also_published = seed_memory(
            &pg,
            &author,
            EntityKind::Abstraction,
            "the published sibling",
        )
        .await?;
        seed_memory_edge(
            &pg,
            &author,
            (EntityKind::Abstraction, published),
            (EntityKind::Fact, secret),
            EdgeKind::Origin,
        )
        .await?;
        seed_memory_edge(
            &pg,
            &author,
            (EntityKind::Abstraction, published),
            (EntityKind::Abstraction, also_published),
            EdgeKind::Origin,
        )
        .await?;
        publish_to_world(&pg, published).await?;
        publish_to_world(&pg, also_published).await?;

        // World is a read-set member, never an actor — a request made *as*
        // World is refused before it reaches SQL ("denied context authorizes
        // nothing"). The stranger below owns nothing in this fixture, so
        // every row it reaches, it reaches through World.
        let stranger = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let walk = pg
            .walk_memory_lineage(
                &[stranger, OwnerRef::World],
                &walk_request(stranger, published, MemoryLineageDirection::Ancestors),
            )
            .await?;

        assert_eq!(
            walk.edges.len(),
            1,
            "the World-sourced walk must return the readable edge and only that; got {:?}",
            walk.edges
        );
        // Dropping the rule does not leak the target's identity — the
        // projection layer would still report it as `Redacted`. What it leaks
        // is the EXISTENCE of an unreadable ancestor of a published memory,
        // so the assertion has to be that the only edge to come back is the
        // control, with no redacted or unavailable projection beside it.
        let leaks: Vec<_> = walk
            .edges
            .iter()
            .filter(|edge| {
                !matches!(
                    &edge.edge.target,
                    proxima_core::EdgeTargetProjection::Visible { target }
                        if target.memory_id() == Some(also_published)
                )
            })
            .collect();
        assert!(
            leaks.is_empty(),
            "the walk disclosed an unreadable ancestor of a published memory: {leaks:?}"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

fn plan_seq_scans_relation(plan: &serde_json::Value, relation: &str) -> bool {
    if plan.get("Node Type").and_then(serde_json::Value::as_str) == Some("Seq Scan")
        && plan
            .get("Relation Name")
            .and_then(serde_json::Value::as_str)
            == Some(relation)
    {
        return true;
    }
    plan.get("Plans")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|plans| {
            plans
                .iter()
                .any(|child| plan_seq_scans_relation(child, relation))
        })
}

/// Under default costing with a crowd in `edges`, the prefiltered neighbor
/// window and snapshot closure scan edges through the endpoint indexes
/// rather than sequentially. The crowd rows touch none of the requested
/// ids, so an index path is both available and worth choosing — if the
/// prefilter's quals stop being index conditions (e.g. someone moves them
/// back onto the resolved columns), this fails.
#[tokio::test]
async fn the_prefilter_scans_edges_through_the_endpoint_indexes()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let graph = seed_graph(&pg).await?;
        let (owner_kind, owner_id) = crate::common::owner_parts(&graph.owner);
        // A crowd ring of real memories and edges (the edge trigger checks
        // endpoint existence, kind, and ownership), touching none of the
        // fixture ids.
        // SQL-POLICY: fixed-fragment — fixture crowd; every value is bound.
        sqlx::query(
            "WITH crowd AS (
                 INSERT INTO proxima_core.memories
                     (memory_id, owner_kind, owner_id, schema_id, schema_version,
                      kind, text, operator_kind, operator_id, input_contract_id,
                      source_batch_id, model_id, prompt_version)
                 SELECT gen_random_uuid(), $1, $2, 'test/edge-access-v1', 1,
                        'Abstraction', 'crowd', 'AtoA',
                        '00000000-0000-0000-0000-000000000101'::uuid,
                        '00000000-0000-0000-0000-000000000102'::uuid,
                        NULL, 'test-model', 'edge-access-v1'
                   FROM generate_series(1, 8000)
                 RETURNING memory_id
             ),
             numbered AS (
                 SELECT memory_id, row_number() OVER () AS rn FROM crowd
             )
             INSERT INTO proxima_core.edges
                 (source_kind, source_id, target_kind, target_id, kind,
                  owner_kind, owner_id)
             SELECT 'Abstraction', a.memory_id, 'Abstraction', b.memory_id,
                    'origin', $1, $2
               FROM numbered a
               JOIN numbered b ON b.rn = (a.rn % 8000) + 1",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query("ANALYZE proxima_core.edges")
            .execute(pg.pool_for_tests())
            .await?;

        let (world_kind, world_id) = crate::common::owner_parts(&OwnerRef::World);

        let neighbor_sql = proxima_storage_pg::neighbor_memory_edges_sql_for_tests();
        // SQL-POLICY: fixed-fragment — EXPLAIN prefix over the audited
        // production constant; only bound values vary.
        let neighbor_plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "EXPLAIN (FORMAT JSON, COSTS OFF) {neighbor_sql}"
        )))
        .bind(vec![owner_kind])
        .bind(vec![owner_id])
        .bind(world_kind)
        .bind(world_id)
        .bind(vec![graph.abstraction.into_inner()])
        .bind(50_i64)
        .fetch_one(pg.pool_for_tests())
        .await?;

        let closure_sql =
            proxima_storage_pg::verbs::query::edges_between_visible_nodes_sql_for_tests();
        // SQL-POLICY: fixed-fragment — EXPLAIN prefix over the audited
        // production constant; only bound values vary.
        let closure_plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "EXPLAIN (FORMAT JSON, COSTS OFF) {closure_sql}"
        )))
        .bind(vec![
            graph.abstraction.into_inner(),
            graph.fact.into_inner(),
            graph.head_fact.into_inner(),
            graph.perspective.into_inner(),
        ])
        .bind(Vec::<Uuid>::new())
        .bind(50_i64)
        .fetch_one(pg.pool_for_tests())
        .await?;

        // The neighbor window ORs both endpoint columns, so its bitmap must
        // reach both indexes; the closure ANDs two OR-groups, so the planner
        // may (rightly) index whichever side it prefers and filter the other.
        for (name, plan, indexes_expected) in [
            (
                "neighbor",
                &neighbor_plan,
                &["idx_edges_source", "idx_edges_target"][..],
            ),
            ("closure", &closure_plan, &["idx_edges_"][..]),
        ] {
            let root = &plan[0]["Plan"];
            assert!(
                !plan_seq_scans_relation(root, "edges"),
                "{name} plan seq-scans edges: {plan}"
            );
            let rendered = plan.to_string();
            for index in indexes_expected {
                assert!(
                    rendered.contains(index),
                    "{name} plan skips {index}: {plan}"
                );
            }
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}
