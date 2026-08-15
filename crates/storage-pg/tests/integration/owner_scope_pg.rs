//! Golden, behaviour, and plan pins for the Query verb's read-owner scope
//! (sql-sweep S4/S6).
//!
//! The scope is an equality join plus an explicit World arm carried only
//! when World is in the read set — the split `search.rs` already ships —
//! and the seq high-water is per-owner index probes merged by a top-1.
//! What this replaced joined with `IS NOT DISTINCT FROM`, which is correct
//! but reaches no `(owner_kind, owner_id, ...)` index prefix.
//!
//! `IS NOT DISTINCT FROM` was load-bearing, so these tests pin what the
//! respelling could break: (a) both statements byte for byte, (b) that the
//! World arm appears exactly when World is in the read set and never spells
//! INDF against `s`, (c) that published rows still come back and a mixed
//! page is still the GLOBAL top-N rather than both arms\' pages
//! concatenated, and (d) that the page rides an owner index under DEFAULT
//! planner costing with a crowd in the table (the S36 trap).

use proxima_core::storage_ports::*;
use proxima_core::verbs::change_history::ChangeHistoryRequest;
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::{QueryPage, QueryRequest, SupersessionStatus, TombstoneFilter};
use proxima_core::{EntityKind, GoalId, MemoryId, OwnerRef, UserId};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use crate::common::{drop_db, fresh_pg, owner_parts, seed_memory};

/// The persisted result of an owner transfer: the row becomes World-owned.
async fn publish_memory_to_world(pg: &PgStorage, memory_id: MemoryId) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE proxima_core.memories
            SET owner_kind = 'world', owner_id = NULL
          WHERE memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .execute(pg.pool_for_tests())
    .await
    .map(|_| ())
}

async fn seed_goal(
    pg: &PgStorage,
    owner: &OwnerRef,
    title: &str,
) -> Result<GoalId, Box<dyn std::error::Error>> {
    let (owner_kind, owner_id) = owner_parts(owner);
    let goal_id = GoalId::new(Uuid::now_v7());
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, owner_kind, owner_id, schema_id, schema_version,
             title, text, payload, state, supersedes,
             authorship_kind, request_id, idempotency_key)
         VALUES ($1, $2, $3, 'core/simple-text-v1', 1,
                 $4, $4, convert_to('{}', 'UTF8'), $5, NULL,
                 'User', $4,
                 md5($2::text || ':' || COALESCE($3::text, 'world') || ':' || $4))",
    )
    .bind(goal_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(title)
    .bind(GoalState::Active)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(goal_id)
}

struct Corpus {
    owner: OwnerRef,
}

/// One personal owner's facts, a superseded abstraction pair, a
/// World-published memory, and one goal per owner kind — enough for the
/// heads filter, both World arms, and the goal page to all have work to do.
async fn seed_corpus(pg: &PgStorage) -> Result<Corpus, Box<dyn std::error::Error>> {
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    for text in ["scope fact a", "scope fact b", "scope fact c"] {
        seed_memory(pg, &owner, EntityKind::Fact, text).await?;
    }
    let prior = seed_memory(pg, &owner, EntityKind::Abstraction, "the first summary").await?;
    let head = seed_memory(pg, &owner, EntityKind::Abstraction, "the second summary").await?;
    sqlx::query("UPDATE proxima_core.memories SET supersedes = $1 WHERE memory_id = $2")
        .bind(prior.into_inner())
        .bind(head.into_inner())
        .execute(pg.pool_for_tests())
        .await?;
    let published = seed_memory(pg, &owner, EntityKind::Perspective, "the published view").await?;
    publish_memory_to_world(pg, published).await?;
    seed_goal(pg, &owner, "the private goal").await?;
    seed_goal(pg, &OwnerRef::World, "the published goal").await?;
    Ok(Corpus { owner })
}

fn request(
    read_owners: Vec<OwnerRef>,
    entity_kind: Option<EntityKind>,
    limit: u32,
) -> QueryRequest {
    QueryRequest {
        owner: read_owners[0],
        read_owners,
        entity_kind,
        schema_id: None,
        supersession: SupersessionStatus::HeadsOnly,
        tombstones: TombstoneFilter::PresentOnly,
        goal_state: None,
        limit,
        page: QueryPage::default(),
        include_payloads: false,
        memory_ids: Vec::new(),
        goal_ids: Vec::new(),
        stateful_heads: Vec::new(),
    }
}

/// The memories page, byte for byte. World is in the read set here, so the
/// statement carries both arms; `the_world_arm_appears_only_for_a_world_\
/// read_set` covers the other shape.
#[tokio::test]
async fn the_memory_page_sql_is_pinned() -> Result<(), Box<dyn std::error::Error>> {
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let req = request(vec![owner, OwnerRef::World], None, 10);
    let sql = proxima_storage_pg::verbs::query::memory_page_sql_for_tests(&req)?;
    assert_eq!(
        sql,
        "SELECT page.memory_id, page.created_at, page.owner_kind, page.owner_id, page.schema_id, \
         page.schema_version, page.kind FROM ( (SELECT lat.* FROM \
         unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id) JOIN LATERAL ( SELECT \
         m.memory_id, m.created_at, m.owner_kind, m.owner_id, m.schema_id, m.schema_version, m.kind \
         FROM proxima_core.memories m WHERE m.owner_kind = s.kind AND m.owner_id = s.id AND \
         m.tombstoned_at IS NULL AND NOT EXISTS (SELECT 1 FROM proxima_core.memories m2 WHERE \
         m2.supersedes = m.memory_id AND m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind \
         AND m2.owner_id IS NOT DISTINCT FROM m.owner_id) ORDER BY m.created_at DESC, m.memory_id \
         DESC LIMIT 10) lat ON TRUE) UNION ALL (SELECT m.memory_id, m.created_at, m.owner_kind, \
         m.owner_id, m.schema_id, m.schema_version, m.kind FROM proxima_core.memories m WHERE \
         m.owner_kind = 'world' AND m.owner_id IS NULL AND m.tombstoned_at IS NULL AND NOT EXISTS \
         (SELECT 1 FROM proxima_core.memories m2 WHERE m2.supersedes = m.memory_id AND \
         m2.tombstoned_at IS NULL AND m2.owner_kind = m.owner_kind AND m2.owner_id IS NOT DISTINCT \
         FROM m.owner_id) ORDER BY m.created_at DESC, m.memory_id DESC LIMIT 10)) page ORDER BY \
         page.created_at DESC, page.memory_id DESC LIMIT 10"
    );
    Ok(())
}

/// The goals page, byte for byte, same read set.
#[tokio::test]
async fn the_goal_page_sql_is_pinned() -> Result<(), Box<dyn std::error::Error>> {
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let req = request(vec![owner, OwnerRef::World], None, 10);
    let sql = proxima_storage_pg::verbs::query::goal_page_sql_for_tests(&req);
    assert_eq!(
        sql,
        "SELECT page.goal_id, page.created_at, page.schema_id, page.schema_version, page.owner_kind, \
         page.owner_id, page.title, page.text, page.state, page.supersedes, page.payload, \
         page.dependency_goal_ids FROM ( (SELECT lat.* FROM unnest($1::proxima_core.owner_ref_kind[], \
         $2::uuid[]) AS s(kind, id) JOIN LATERAL ( SELECT g.goal_id, g.created_at, g.schema_id, \
         g.schema_version, g.owner_kind, g.owner_id, g.title, g.text, g.state, g.supersedes, \
         ''::bytea AS payload, g.dependency_goal_ids FROM proxima_core.goals g WHERE g.owner_kind = \
         s.kind AND g.owner_id = s.id AND NOT EXISTS (SELECT 1 FROM proxima_core.goals g2 WHERE \
         g2.supersedes = g.goal_id AND g2.owner_kind = g.owner_kind AND g2.owner_id IS NOT DISTINCT \
         FROM g.owner_id) ORDER BY g.created_at DESC, g.goal_id DESC LIMIT 10) lat ON TRUE) UNION ALL \
         (SELECT g.goal_id, g.created_at, g.schema_id, g.schema_version, g.owner_kind, g.owner_id, \
         g.title, g.text, g.state, g.supersedes, ''::bytea AS payload, g.dependency_goal_ids FROM \
         proxima_core.goals g WHERE g.owner_kind = 'world' AND g.owner_id IS NULL AND NOT EXISTS \
         (SELECT 1 FROM proxima_core.goals g2 WHERE g2.supersedes = g.goal_id AND g2.owner_kind = \
         g.owner_kind AND g2.owner_id IS NOT DISTINCT FROM g.owner_id) ORDER BY g.created_at DESC, \
         g.goal_id DESC LIMIT 10)) page ORDER BY page.created_at DESC, page.goal_id DESC LIMIT 10"
    );
    Ok(())
}

/// The high-water statement, byte for byte. `change_event`'s CHECKs prove
/// `owner_id` is never NULL there, so `=` and `IS NOT DISTINCT FROM` admit
/// identical rows — which is what lets this be a per-owner index probe
/// rather than a whole-table ordered walk (adjudicated in
/// docs/wave2-adjudications.md).
#[tokio::test]
async fn the_high_water_sql_is_pinned() -> Result<(), Box<dyn std::error::Error>> {
    let sql = proxima_storage_pg::verbs::query::read_seq_high_water_sql_for_tests();
    assert_eq!(
        sql,
        r"SELECT hw.seq
         FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
         JOIN LATERAL (
             SELECT ce.seq FROM proxima_core.change_event ce
              WHERE ce.owner_kind = s.kind AND ce.owner_id = s.id
                AND (
                    ce.edge_kind IS NULL
                    OR (
                        EXISTS (
                            SELECT 1
                              FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) seo
                              JOIN unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS rs(kind, id)
                                ON seo.owner_kind = rs.kind
                               AND seo.owner_id IS NOT DISTINCT FROM rs.id
                             WHERE seo.entity_id = COALESCE(
        (SELECT fe.current_memory_id FROM proxima_core.fact_entities fe
          WHERE fe.fact_entity_id = ce.edge_source_id),
        ce.edge_source_id)
                        )
                        AND NOT (
                            EXISTS (
                                SELECT 1
                                  FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) weo
                                 WHERE weo.entity_id = COALESCE(
        (SELECT fe.current_memory_id FROM proxima_core.fact_entities fe
          WHERE fe.fact_entity_id = ce.edge_source_id),
        ce.edge_source_id)
                                   AND weo.owner_kind = $3
                                   AND weo.owner_id IS NOT DISTINCT FROM $4
                            )
                            AND NOT EXISTS (
                                SELECT 1
                                  FROM (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) teo
                                  JOIN unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS rt(kind, id)
                                    ON teo.owner_kind = rt.kind
                                   AND teo.owner_id IS NOT DISTINCT FROM rt.id
                                 WHERE teo.entity_id = COALESCE(
        (SELECT fe.current_memory_id FROM proxima_core.fact_entities fe
          WHERE fe.fact_entity_id = ce.edge_target_id),
        ce.edge_target_id)
                            )
                        )
                    )
               )
              ORDER BY ce.seq DESC LIMIT 1
         ) hw ON TRUE
         ORDER BY hw.seq DESC LIMIT 1"
    );
    Ok(())
}

/// The scope splits by read-set shape: with World present it carries the
/// constant World arm behind UNION ALL; without World it is a plain
/// equality lateral and nothing else. Neither shape spells INDF against
/// `s` — that is the whole point — so a World row is reachable only
/// through the explicit arm, and the arm must not appear when it would
/// widen the read.
#[tokio::test]
async fn the_world_arm_appears_only_for_a_world_read_set() -> Result<(), Box<dyn std::error::Error>>
{
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let with_world = proxima_storage_pg::verbs::query::memory_page_sql_for_tests(&request(
        vec![owner, OwnerRef::World],
        None,
        10,
    ))?;
    assert!(with_world.contains("m.owner_kind = s.kind AND m.owner_id = s.id"));
    assert!(with_world.contains("UNION ALL"));
    assert!(with_world.contains("m.owner_kind = 'world' AND m.owner_id IS NULL"));
    assert!(!with_world.contains("IS NOT DISTINCT FROM s.id"));

    let without_world = proxima_storage_pg::verbs::query::memory_page_sql_for_tests(&request(
        vec![owner],
        None,
        10,
    ))?;
    assert!(without_world.contains("m.owner_kind = s.kind AND m.owner_id = s.id"));
    assert!(!without_world.contains("UNION ALL"));
    assert!(!without_world.contains("'world'"));
    Ok(())
}

/// A World-published row is reachable only through the explicit World arm,
/// so the read set decides whether it comes back — and dropping the arm
/// would be silent, since every OTHER row still returns. Each read set is
/// therefore asserted against its own expected count, per entity-kind
/// stream, and once more across a keyset page boundary.
#[tokio::test]
async fn a_published_row_is_reachable_exactly_when_world_is_in_the_read_set()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let corpus = seed_corpus(&pg).await?;

        // 3 facts + the abstraction head + the World perspective; one goal
        // per owner kind.
        let widest = request(vec![corpus.owner, OwnerRef::World], None, 50);
        let widest_response = pg.query_memories(&widest, &[]).await?;
        assert_eq!(widest_response.memories.len(), 5);
        assert_eq!(widest_response.goals.len(), 2);

        // Drop World and exactly the published rows go: the perspective and
        // the World goal.
        let narrow = request(vec![corpus.owner], None, 50);
        let narrow_response = pg.query_memories(&narrow, &[]).await?;
        assert_eq!(
            narrow_response.memories.len(),
            4,
            "only the World perspective may drop, got {:?}",
            narrow_response.memories
        );
        assert_eq!(narrow_response.goals.len(), 1);

        for (entity_kind, with_world, without_world) in [
            (Some(EntityKind::Fact), 3_usize, 3_usize),
            (Some(EntityKind::Abstraction), 1, 1),
            (Some(EntityKind::Perspective), 1, 0),
            (Some(EntityKind::Goal), 2, 1),
        ] {
            let wide = pg
                .query_memories(
                    &request(vec![corpus.owner, OwnerRef::World], entity_kind, 50),
                    &[],
                )
                .await?;
            let tight = pg
                .query_memories(&request(vec![corpus.owner], entity_kind, 50), &[])
                .await?;
            let count = |r: &proxima_core::verbs::query::QueryResponse| {
                if matches!(entity_kind, Some(EntityKind::Goal)) {
                    r.goals.len()
                } else {
                    r.memories.len()
                }
            };
            assert_eq!(count(&wide), with_world, "{entity_kind:?} with World");
            assert_eq!(
                count(&tight),
                without_world,
                "{entity_kind:?} without World"
            );
        }

        // Keyset page boundary: the outer ORDER BY/LIMIT over the union has
        // to hand out a cursor that resumes correctly across both arms.
        let mut req = request(
            vec![corpus.owner, OwnerRef::World],
            Some(EntityKind::Fact),
            2,
        );
        let page1 = pg.query_memories(&req, &[]).await?;
        assert_eq!(page1.memories.len(), 2);
        let cursor = page1
            .next_cursor
            .expect("three facts at limit 2 leave a second page");
        req.page.after = Some(cursor);
        let page2 = pg.query_memories(&req, &[]).await?;
        assert_eq!(page2.memories.len(), 1);
        let walked_ids: Vec<_> = page1
            .memories
            .iter()
            .chain(page2.memories.iter())
            .map(|m| m.id)
            .collect();
        let whole_stream_ids: Vec<_> = pg
            .query_memories(
                &request(
                    vec![corpus.owner, OwnerRef::World],
                    Some(EntityKind::Fact),
                    50,
                ),
                &[],
            )
            .await?
            .memories
            .iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(
            walked_ids, whole_stream_ids,
            "paging must not reorder or drop rows"
        );

        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// The change-history verb reads the seq high-water through the same
/// per-owner probe. Merging per-owner top-1s is only the whole maximum
/// because every arm is probed, so this asserts the probe finds the corpus
/// writes rather than an empty union.
#[tokio::test]
async fn the_high_water_probe_reaches_the_read_set() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let corpus = seed_corpus(&pg).await?;
        let history = pg
            .change_history(
                &[corpus.owner, OwnerRef::World],
                &ChangeHistoryRequest {
                    owner: corpus.owner,
                    limit: 20,
                    before: None,
                },
            )
            .await?;
        assert!(
            history.seq_high_water.is_some(),
            "the per-owner high-water probe must find the corpus writes"
        );
        assert!(!history.events.is_empty());
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// A mixed Personal+World corpus where EACH arm alone can fill the page:
/// the ON shape's outer ORDER BY/LIMIT after the UNION ALL is what
/// restores the global top-N, and the snapshot stream (`entity_kind`
/// None) has no Rust-side truncation — dropping that outer LIMIT would
/// hand back both arms' pages (8 rows for a limit of 4) and fail here.
/// Seeding alternates owners so the true top-4 interleaves both arms.
#[tokio::test]
async fn the_mixed_owner_page_is_the_global_top_n() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        for i in 0..5 {
            seed_memory(&pg, &owner, EntityKind::Fact, &format!("private fact {i}")).await?;
            let published = seed_memory(
                &pg,
                &owner,
                EntityKind::Fact,
                &format!("published fact {i}"),
            )
            .await?;
            publish_memory_to_world(&pg, published).await?;
            seed_goal(&pg, &owner, &format!("private goal {i}")).await?;
            seed_goal(&pg, &OwnerRef::World, &format!("published goal {i}")).await?;
        }
        let read_owners = vec![owner, OwnerRef::World];

        // Snapshot stream: page limit 4 against 5 live rows per arm. Losing
        // the outer LIMIT hands back both arms' pages — 8 rows for a limit
        // of 4 — and this stream has no Rust-side truncation to hide it.
        let req = request(read_owners.clone(), None, 4);
        let response = pg.query_memories(&req, &[]).await?;
        assert_eq!(
            response.memories.len(),
            4,
            "the memories page must be the global top-4, got {:?}",
            response.memories
        );
        assert_eq!(
            response.goals.len(),
            4,
            "the goals page must be the global top-4, got {:?}",
            response.goals
        );

        // Single-kind stream across the keyset boundary under the same
        // pressure: walking every page must yield each row exactly once.
        let mut req = request(read_owners, Some(EntityKind::Fact), 4);
        let mut walked = Vec::new();
        loop {
            let page = pg.query_memories(&req, &[]).await?;
            walked.extend(page.memories.iter().map(|m| m.id));
            match page.next_cursor {
                Some(cursor) => req.page.after = Some(cursor),
                None => break,
            }
        }
        let mut deduped = walked.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(
            walked.len(),
            deduped.len(),
            "paging repeated a row across the union arms: {walked:?}"
        );
        assert_eq!(walked.len(), 10, "5 private + 5 published facts");
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

/// Under default costing with a 20k-row crowd owned by someone else, the
/// equality page probes an `(owner_kind, owner_id, ...)` memories index
/// instead of scanning the table — the exact plan the INDF spelling makes
/// unreachable (sql-sweep S4's measured 74-239x).
#[tokio::test]
async fn the_equality_page_rides_the_owner_index() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let corpus = seed_corpus(&pg).await?;
        let crowd_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let (crowd_kind, crowd_id) = owner_parts(&crowd_owner);
        // SQL-POLICY: fixed-fragment — fixture crowd; every value is bound.
        sqlx::query(
            "INSERT INTO proxima_core.memories
                (memory_id, owner_kind, owner_id, schema_id, schema_version,
                 kind, text, operator_kind, operator_id, input_contract_id,
                 source_batch_id, model_id, prompt_version)
             SELECT gen_random_uuid(), $1, $2, 'test/edge-access-v1', 1,
                    'Abstraction', 'crowd', 'AtoA',
                    '00000000-0000-0000-0000-000000000101'::uuid,
                    '00000000-0000-0000-0000-000000000102'::uuid,
                    NULL, 'test-model', 'edge-access-v1'
               FROM generate_series(1, 20000)",
        )
        .bind(crowd_kind)
        .bind(crowd_id)
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query("ANALYZE proxima_core.memories")
            .execute(pg.pool_for_tests())
            .await?;

        let req = request(vec![corpus.owner], Some(EntityKind::Abstraction), 10);
        let sql = proxima_storage_pg::verbs::query::memory_page_sql_for_tests(&req)?;
        let (owner_kind, owner_id) = owner_parts(&corpus.owner);
        // SQL-POLICY: fixed-fragment — EXPLAIN prefix over the audited
        // production builder's parameterized SQL; only bound values vary.
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "EXPLAIN (FORMAT JSON, COSTS OFF) {sql}"
        )))
        .bind(vec![owner_kind])
        .bind(vec![owner_id])
        .fetch_one(pg.pool_for_tests())
        .await?;

        let root = &plan[0]["Plan"];
        assert!(
            !plan_seq_scans_relation(root, "memories"),
            "equality page plan seq-scans memories: {plan}"
        );
        assert!(
            plan.to_string().contains("idx_memories_owner"),
            "equality page plan skips the owner indexes: {plan}"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}
