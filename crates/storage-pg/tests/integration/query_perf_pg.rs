use crate::common::{drop_db, fresh_pg, test_registry};

use std::sync::Arc;

use proxima_core::engine::Engine;
use proxima_core::error::ErrorCode;
use proxima_core::verbs::query::{EntityKind, QueryCursor, QueryPage, QueryRequest};
use proxima_core::{AuthPath, AuthzContext, GoalId, MemoryId, OwnerRef, OwnerRefKind, UserId};
use proxima_storage_pg::PgStorage;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
struct SeededRow {
    id: Uuid,
    created_at: time::OffsetDateTime,
}

fn plan_contains_node(plan: &Value, node: &str) -> bool {
    if plan.get("Node Type").and_then(Value::as_str) == Some(node) {
        return true;
    }
    plan.get("Plans")
        .and_then(Value::as_array)
        .is_some_and(|plans| plans.iter().any(|child| plan_contains_node(child, node)))
}

fn plan_uses_index(plan: &Value, index_name: &str) -> bool {
    plan.get("Index Name").and_then(Value::as_str) == Some(index_name)
        || plan
            .get("Plans")
            .and_then(Value::as_array)
            .is_some_and(|plans| plans.iter().any(|child| plan_uses_index(child, index_name)))
}

fn assert_uses_index(plan: &Value, index_name: &str, sql: &str) {
    assert!(
        plan_uses_index(plan, index_name),
        "expected plan to use {index_name}; sql:\n{sql}\nplan:\n{plan:#}"
    );
    assert!(
        plan_contains_node(plan, "Index Scan")
            || plan_contains_node(plan, "Index Only Scan")
            || plan_contains_node(plan, "Bitmap Index Scan"),
        "expected index-backed plan node; sql:\n{sql}\nplan:\n{plan:#}"
    );
}

fn plan_root(explain: &Value) -> Value {
    explain
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("Plan"))
        .cloned()
        .expect("EXPLAIN JSON has a root Plan")
}

fn owner_arrays(owner: OwnerRef) -> (Vec<OwnerRefKind>, Vec<Option<Uuid>>) {
    let (kind, id) = owner.columns();
    (vec![kind], vec![id])
}

fn personal_owner() -> OwnerRef {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
}

fn engine_for(pg: &PgStorage) -> Engine {
    Engine::new(test_registry()).with_storage_ports(Arc::new(pg.clone()).storage_ports())
}

async fn seed_memory_rows(
    pg: &PgStorage,
    owner: OwnerRef,
    kind: Option<EntityKind>,
    count: usize,
    start_offset_ms: i64,
    label: &str,
) -> Result<Vec<SeededRow>, sqlx::Error> {
    let base = time::OffsetDateTime::now_utc();
    let mut rows = Vec::with_capacity(count);
    for idx in 0..count {
        let offset = start_offset_ms + i64::try_from(idx).expect("fixture index fits i64");
        rows.push(SeededRow {
            id: Uuid::now_v7(),
            created_at: base - time::Duration::milliseconds(offset),
        });
    }
    let ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
    let created_at = rows.iter().map(|row| row.created_at).collect::<Vec<_>>();
    let text = rows
        .iter()
        .enumerate()
        .map(|(idx, _)| format!("{label} memory {idx}"))
        .collect::<Vec<_>>();
    let (owner_kind, owner_id) = owner.columns();
    let schema_id = match kind {
        None | Some(EntityKind::Fact) => "test/perf-fact-v1",
        Some(EntityKind::Abstraction) => "test/perf-abstraction-v1",
        Some(EntityKind::Perspective) => "test/perf-perspective-v1",
        Some(EntityKind::Goal) => unreachable!("Goal rows are not stored in memories"),
    };
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id,
             prompt_version, created_at)
         SELECT r.memory_id, $1, $2, $3, 1, $4::proxima_core.entity_kind, r.text,
                CASE
                    WHEN $4::proxima_core.entity_kind = 'Abstraction'::proxima_core.entity_kind
                        THEN 'AtoA'::proxima_core.memory_operator_kind
                    WHEN $4::proxima_core.entity_kind = 'Perspective'::proxima_core.entity_kind
                        THEN 'AtoP'::proxima_core.memory_operator_kind
                    ELSE NULL
                END,
                CASE
                    WHEN $4::proxima_core.entity_kind = 'Fact'::proxima_core.entity_kind THEN NULL
                    ELSE '00000000-0000-0000-0000-00000000a001'::uuid
                END,
                CASE
                    WHEN $4::proxima_core.entity_kind = 'Fact'::proxima_core.entity_kind THEN NULL
                    ELSE '00000000-0000-0000-0000-00000000a002'::uuid
                END,
                NULL,
                CASE WHEN $4::proxima_core.entity_kind = 'Fact'::proxima_core.entity_kind THEN NULL ELSE 'perf-model' END,
                CASE WHEN $4::proxima_core.entity_kind = 'Fact'::proxima_core.entity_kind THEN NULL ELSE 'perf-v1' END,
                r.created_at
           FROM unnest($5::uuid[], $6::timestamptz[], $7::text[])
             AS r(memory_id, created_at, text)",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(schema_id)
    .bind(kind.unwrap_or(EntityKind::Fact))
    .bind(ids)
    .bind(created_at)
    .bind(text)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(rows)
}

async fn seed_goal_rows(
    pg: &PgStorage,
    owner: OwnerRef,
    count: usize,
    start_offset_ms: i64,
    label: &str,
) -> Result<Vec<SeededRow>, sqlx::Error> {
    let base = time::OffsetDateTime::now_utc();
    let mut rows = Vec::with_capacity(count);
    for idx in 0..count {
        let offset = start_offset_ms + i64::try_from(idx).expect("fixture index fits i64");
        rows.push(SeededRow {
            id: Uuid::now_v7(),
            created_at: base - time::Duration::milliseconds(offset),
        });
    }
    let ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
    let created_at = rows.iter().map(|row| row.created_at).collect::<Vec<_>>();
    let text = rows
        .iter()
        .enumerate()
        .map(|(idx, _)| format!("{label} goal {idx}"))
        .collect::<Vec<_>>();
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, schema_id, owner_kind, owner_id, text, state, authorship_kind,
             request_id, idempotency_key, schema_version, payload, title, created_at)
         SELECT r.goal_id, 'test/perf-goal-v1', $1, $2, r.text,
                'Active'::proxima_core.goal_state,
                'User'::proxima_core.goal_authorship_kind,
                'perf-' || r.goal_id::text,
                md5($1::text || ':' || COALESCE($2::text, '') || ':' || r.goal_id::text),
                1,
                '\\x01'::bytea,
                r.text,
                r.created_at
           FROM unnest($3::uuid[], $4::timestamptz[], $5::text[])
             AS r(goal_id, created_at, text)",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(ids)
    .bind(created_at)
    .bind(text)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(rows)
}

const FACT_KEYSET_EXPLAIN_SQL: &str = "EXPLAIN (FORMAT JSON, COSTS OFF)
SELECT page.memory_id
  FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
  JOIN LATERAL (
      SELECT m.memory_id, m.created_at
        FROM proxima_core.memories m
       WHERE m.owner_kind = s.kind
         AND m.owner_id = s.id
         AND m.tombstoned_at IS NULL
         AND m.kind = 'Fact'
         AND (m.created_at, m.memory_id) < ($3, $4)
       ORDER BY m.created_at DESC, m.memory_id DESC
       LIMIT 101
  ) page ON TRUE
 ORDER BY page.created_at DESC, page.memory_id DESC
 LIMIT 101";

const AP_KEYSET_EXPLAIN_SQL: &str = "EXPLAIN (FORMAT JSON, COSTS OFF)
SELECT page.memory_id
  FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
  JOIN LATERAL (
      SELECT m.memory_id, m.created_at
        FROM proxima_core.memories m
       WHERE m.owner_kind = s.kind
         AND m.owner_id = s.id
         AND m.tombstoned_at IS NULL
         AND m.kind = $3
         AND (m.created_at, m.memory_id) < ($4, $5)
       ORDER BY m.created_at DESC, m.memory_id DESC
       LIMIT 101
  ) page ON TRUE
 ORDER BY page.created_at DESC, page.memory_id DESC
 LIMIT 101";

const GOAL_KEYSET_EXPLAIN_SQL: &str = "EXPLAIN (FORMAT JSON, COSTS OFF)
SELECT page.goal_id
  FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
  JOIN LATERAL (
      SELECT g.goal_id, g.created_at
        FROM proxima_core.goals g
       WHERE g.owner_kind = s.kind
         AND g.owner_id = s.id
         AND (g.created_at, g.goal_id) < ($3, $4)
       ORDER BY g.created_at DESC, g.goal_id DESC
       LIMIT 101
  ) page ON TRUE
 ORDER BY page.created_at DESC, page.goal_id DESC
 LIMIT 101";

async fn explain_fact_keyset(
    pool: &PgPool,
    owner: OwnerRef,
    cursor: SeededRow,
) -> Result<Value, sqlx::Error> {
    let (owner_kinds, owner_ids) = owner_arrays(owner);
    let explain = sqlx::query_scalar::<_, Value>(FACT_KEYSET_EXPLAIN_SQL)
        .bind(owner_kinds)
        .bind(owner_ids)
        .bind(cursor.created_at)
        .bind(cursor.id)
        .fetch_one(pool)
        .await?;
    Ok(plan_root(&explain))
}

async fn explain_memory_kind_keyset(
    pool: &PgPool,
    owner: OwnerRef,
    kind: EntityKind,
    cursor: SeededRow,
) -> Result<Value, sqlx::Error> {
    let (owner_kinds, owner_ids) = owner_arrays(owner);
    let explain = sqlx::query_scalar::<_, Value>(AP_KEYSET_EXPLAIN_SQL)
        .bind(owner_kinds)
        .bind(owner_ids)
        .bind(kind)
        .bind(cursor.created_at)
        .bind(cursor.id)
        .fetch_one(pool)
        .await?;
    Ok(plan_root(&explain))
}

async fn explain_goal_keyset(
    pool: &PgPool,
    owner: OwnerRef,
    cursor: SeededRow,
) -> Result<Value, sqlx::Error> {
    let (owner_kinds, owner_ids) = owner_arrays(owner);
    let explain = sqlx::query_scalar::<_, Value>(GOAL_KEYSET_EXPLAIN_SQL)
        .bind(owner_kinds)
        .bind(owner_ids)
        .bind(cursor.created_at)
        .bind(cursor.id)
        .fetch_one(pool)
        .await?;
    Ok(plan_root(&explain))
}

#[tokio::test]
async fn query_memories_fact_keyset_uses_fact_partial_index() {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = personal_owner();
        let other = personal_owner();
        let facts = seed_memory_rows(&pg, owner, None, 5_000, 10_000, "target-fact").await?;
        seed_memory_rows(
            &pg,
            owner,
            Some(EntityKind::Abstraction),
            5_000,
            0,
            "newer-abstraction",
        )
        .await?;
        seed_memory_rows(&pg, other, None, 5_000, 0, "other-fact").await?;
        sqlx::query("ANALYZE proxima_core.memories")
            .execute(pg.pool_for_tests())
            .await?;

        let plan = explain_fact_keyset(pg.pool_for_tests(), owner, facts[100]).await?;
        assert_uses_index(
            &plan,
            "idx_memories_owner_fact_created_id_live",
            FACT_KEYSET_EXPLAIN_SQL,
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("query_memories_fact_keyset_uses_fact_partial_index failed");
}

#[tokio::test]
async fn query_memories_ap_keyset_uses_kind_created_id_index() {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = personal_owner();
        let other = personal_owner();
        let abstractions = seed_memory_rows(
            &pg,
            owner,
            Some(EntityKind::Abstraction),
            5_000,
            0,
            "target-abstraction",
        )
        .await?;
        let perspectives = seed_memory_rows(
            &pg,
            owner,
            Some(EntityKind::Perspective),
            5_000,
            10_000,
            "target-perspective",
        )
        .await?;
        seed_memory_rows(&pg, owner, None, 5_000, 0, "mixed-fact").await?;
        seed_memory_rows(
            &pg,
            other,
            Some(EntityKind::Abstraction),
            5_000,
            0,
            "other-abstraction",
        )
        .await?;
        sqlx::query("ANALYZE proxima_core.memories")
            .execute(pg.pool_for_tests())
            .await?;

        let abstraction_plan = explain_memory_kind_keyset(
            pg.pool_for_tests(),
            owner,
            EntityKind::Abstraction,
            abstractions[100],
        )
        .await?;
        assert_uses_index(
            &abstraction_plan,
            "idx_memories_owner_kind_created_id_live",
            AP_KEYSET_EXPLAIN_SQL,
        );

        let perspective_plan = explain_memory_kind_keyset(
            pg.pool_for_tests(),
            owner,
            EntityKind::Perspective,
            perspectives[100],
        )
        .await?;
        assert_uses_index(
            &perspective_plan,
            "idx_memories_owner_kind_created_id_live",
            AP_KEYSET_EXPLAIN_SQL,
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("query_memories_ap_keyset_uses_kind_created_id_index failed");
}

#[tokio::test]
async fn query_goals_keyset_uses_owner_created_id_index() {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = personal_owner();
        let other = personal_owner();
        let goals = seed_goal_rows(&pg, owner, 5_000, 0, "target").await?;
        seed_goal_rows(&pg, other, 5_000, 0, "other").await?;
        sqlx::query("ANALYZE proxima_core.goals")
            .execute(pg.pool_for_tests())
            .await?;

        let plan = explain_goal_keyset(pg.pool_for_tests(), owner, goals[100]).await?;
        assert_uses_index(&plan, "idx_goals_owner_created_id", GOAL_KEYSET_EXPLAIN_SQL);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("query_goals_keyset_uses_owner_created_id_index failed");
}

#[test]
fn query_memories_has_no_offset_clause() {
    let query_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/verbs/query");
    for entry in std::fs::read_dir(query_dir).expect("query source directory exists") {
        let entry = entry.expect("query source entry is readable");
        let path = entry.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("query source is readable");
        let has_offset_token = source
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .any(|token| token.eq_ignore_ascii_case("offset"));
        assert!(
            !has_offset_token,
            "query source must use keyset pagination, not OFFSET: {}",
            path.display()
        );
        if path.file_name().and_then(std::ffi::OsStr::to_str) == Some("memories.rs")
            || path.file_name().and_then(std::ffi::OsStr::to_str) == Some("goals.rs")
        {
            assert!(
                source.contains("unnest($1::proxima_core.owner_ref_kind[], $2::uuid[])"),
                "query source must keep set-based owner authorization: {}",
                path.display()
            );
            assert!(
                !source.contains("resolve_membership"),
                "query source must not resolve membership per row: {}",
                path.display()
            );
        }
    }
}

#[tokio::test]
async fn query_cursor_second_page_has_no_overlap() {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = personal_owner();
        let seeded = seed_memory_rows(&pg, owner, None, 7, 0, "page").await?;
        let engine = engine_for(&pg);
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let mut req = QueryRequest::for_owner(owner);
        req.entity_kind = Some(EntityKind::Fact);
        req.limit = 3;

        let first = engine.query(&authz, &req).await?;
        assert_eq!(
            first
                .memories
                .iter()
                .map(|row| row.id.into_inner())
                .collect::<Vec<_>>(),
            seeded.iter().take(3).map(|row| row.id).collect::<Vec<_>>()
        );
        let Some(cursor) = first.next_cursor.clone() else {
            panic!("first page should expose a next cursor");
        };

        req.page = QueryPage {
            after: Some(cursor),
        };
        let second = engine.query(&authz, &req).await?;
        let second_ids = second
            .memories
            .iter()
            .map(|row| row.id.into_inner())
            .collect::<Vec<_>>();
        assert_eq!(
            second_ids,
            seeded
                .iter()
                .skip(3)
                .take(3)
                .map(|row| row.id)
                .collect::<Vec<_>>()
        );
        let first_ids = first
            .memories
            .iter()
            .map(|row| row.id.into_inner())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            second_ids.iter().all(|id| !first_ids.contains(id)),
            "second keyset page overlapped the first page"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("query_cursor_second_page_has_no_overlap failed");
}

#[tokio::test]
async fn mixed_query_rejects_cursor() {
    let owner = personal_owner();
    let engine = Engine::new(test_registry());
    let mut req = QueryRequest::for_owner(owner);
    req.page.after = Some(QueryCursor::Memory {
        created_at: time::OffsetDateTime::now_utc(),
        memory_id: MemoryId::new(Uuid::now_v7()),
    });

    let err = engine
        .query(
            &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            &req,
        )
        .await
        .expect_err("mixed Query must reject a cursor before storage dispatch");
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(err.message.contains("single entity_kind"));
}

#[tokio::test]
async fn cursor_kind_mismatch_rejects() {
    let owner = personal_owner();
    let engine = Engine::new(test_registry());
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

    let mut goal_req = QueryRequest::for_owner(owner);
    goal_req.entity_kind = Some(EntityKind::Goal);
    goal_req.page.after = Some(QueryCursor::Memory {
        created_at: time::OffsetDateTime::now_utc(),
        memory_id: MemoryId::new(Uuid::now_v7()),
    });
    let goal_err = engine
        .query(&authz, &goal_req)
        .await
        .expect_err("Goal Query must reject Memory cursor");
    assert_eq!(goal_err.code, ErrorCode::InvalidArgument);

    let mut memory_req = QueryRequest::for_owner(owner);
    memory_req.entity_kind = Some(EntityKind::Fact);
    memory_req.page.after = Some(QueryCursor::Goal {
        created_at: time::OffsetDateTime::now_utc(),
        goal_id: GoalId::new(Uuid::now_v7()),
    });
    let memory_err = engine
        .query(&authz, &memory_req)
        .await
        .expect_err("Memory Query must reject Goal cursor");
    assert_eq!(memory_err.code, ErrorCode::InvalidArgument);
}
