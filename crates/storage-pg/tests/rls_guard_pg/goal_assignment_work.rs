//! Work bound for the goal queries under the shipped RLS policies: the Active
//! goals of one Perspective and of none (#539), and a page of an owner's goals
//! (#545), read about the goals they return however many goals the store holds.

use proxiima_core::verbs::goal_write::GoalState;
use proxiima_core::verbs::query::{EntityKind, GoalMotivationFilter, QueryRequest};
use proxiima_core::{Credentials, GroupId, MemoryId, OwnerId, OwnerRoles, Role, UserId};
use proxiima_storage_pg::begin_owner_transaction;
use proxiima_storage_pg::verbs::query::goal_page_sql_for_tests;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Goal series the owner holds, two versions each.
const SERIES: i32 = 30_000;
/// Perspectives the assigned series are spread over; every tenth series is
/// Active, and the first Perspective's series are all of them.
const PERSPECTIVES: i32 = 3_000;
/// One series in this many (the seventh) has no Perspective, and is Active.
const UNASSIGNED_EVERY: i32 = 1_500;

async fn seed(admin: &PgPool, owner: Uuid) {
    sqlx::query(
        "INSERT INTO proxiima_core.owners (owner_id, kind) VALUES ($1, 'group')
         ON CONFLICT DO NOTHING",
    )
    .bind(owner)
    .execute(admin)
    .await
    .unwrap();
    // A trigger-free bulk load: the planner sees the rows, no write admits them.
    let mut seed = admin.acquire().await.unwrap();
    sqlx::query("SET session_replication_role = replica")
        .execute(&mut *seed)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TEMP TABLE series AS
         SELECT n, uuidv7() AS handle, uuidv7() AS t1, uuidv7() AS t2,
                CASE WHEN n % $2 = 7 THEN NULL
                     ELSE uuid_in(md5('perspective' || (n % $3))::cstring) END AS assignment,
                (n % 10 = 0 OR n % $2 = 7) AS active
           FROM generate_series(1, $1) n",
    )
    .bind(SERIES)
    .bind(UNASSIGNED_EVERY)
    .bind(PERSPECTIVES)
    .execute(&mut *seed)
    .await
    .unwrap();
    seed_perspectives(&mut seed, owner).await;
    seed_goals(&mut seed, owner).await;
}

async fn seed_perspectives(seed: &mut sqlx::pool::PoolConnection<sqlx::Postgres>, owner: Uuid) {
    let perspective_content: Uuid = sqlx::query_scalar(
        "INSERT INTO proxiima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/test-perspective-v1', decode(repeat('00', 32), 'hex'))
         RETURNING content_id",
    )
    .bind(owner)
    .fetch_one(&mut **seed)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO proxiima_core.memory_head (handle, kind, schema_id, owner_id, t)
         SELECT uuidv7(), 'perspective', 'core/test-perspective-v1', $1,
                uuid_in(md5('perspective' || n)::cstring)
           FROM generate_series(0, $2 - 1) n",
    )
    .bind(owner)
    .bind(PERSPECTIVES)
    .execute(&mut **seed)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO proxiima_core.memory
             (handle, t, kind, owner_id, schema_id, content_id, occurred_at, \"by\", via)
         SELECT h.handle, h.t, h.kind, h.owner_id, h.schema_id, $2, NULL,
                '00000000-0000-0000-0000-000000000539'::uuid, NULL::uuid
           FROM proxiima_core.memory_head h
          WHERE h.owner_id = $1 AND h.schema_id = 'core/test-perspective-v1'",
    )
    .bind(owner)
    .bind(perspective_content)
    .execute(&mut **seed)
    .await
    .unwrap();
}

async fn seed_goals(seed: &mut sqlx::pool::PoolConnection<sqlx::Postgres>, owner: Uuid) {
    sqlx::query(
        "INSERT INTO proxiima_core.goal_head (handle, schema_id, owner_id, t)
         SELECT handle, 'core/simple-text-v1', $1, t2 FROM series",
    )
    .bind(owner)
    .execute(&mut **seed)
    .await
    .unwrap();
    // The first version of each series is Active and superseded; the head is
    // Active for the series marked so, Achieved otherwise.
    sqlx::query(
        "INSERT INTO proxiima_core.goal
             (handle, t, owner_id, schema_id, title, state, request_id, motivated_by, \"by\", text)
         SELECT handle, t1, $1, 'core/simple-text-v1', 'g' || n, 'Active', 'first-' || n, array_remove(ARRAY[assignment], NULL),
                '00000000-0000-0000-0000-000000000539'::uuid, 'text'
           FROM series",
    )
    .bind(owner)
    .execute(&mut **seed)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO proxiima_core.goal
             (handle, t, owner_id, schema_id, title, state, request_id, motivated_by, \"by\", text, close_fact_t)
         SELECT handle, t2, $1, 'core/simple-text-v1', 'g' || n,
                CASE WHEN active THEN 'Active' ELSE 'Achieved' END::proxiima_core.goal_state,
                'head-' || n, array_remove(ARRAY[assignment], NULL), '00000000-0000-0000-0000-000000000539'::uuid, 'text',
                CASE WHEN active THEN NULL ELSE uuidv7() END
           FROM series",
    )
    .bind(owner)
    .execute(&mut **seed)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO proxiima_core.goal_motivation_index (goal_t, perspective_t, owner_id)
         SELECT g.t, p.id, g.owner_id
           FROM proxiima_core.goal g
           CROSS JOIN LATERAL unnest(g.motivated_by) p(id)
          WHERE g.owner_id = $1",
    )
    .bind(owner)
    .execute(&mut **seed)
    .await
    .unwrap();
}

fn walk(
    plan: &serde_json::Value,
    visit: &mut impl FnMut(&serde_json::Map<String, serde_json::Value>),
) {
    match plan {
        serde_json::Value::Array(xs) => xs.iter().for_each(|node| walk(node, visit)),
        serde_json::Value::Object(node) => {
            visit(node);
            node.values().for_each(|node| walk(node, visit));
        }
        _ => {}
    }
}

fn metric(node: &serde_json::Map<String, serde_json::Value>, key: &str) -> f64 {
    node.get(key)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or_default()
}

/// Run the deployed goal statement for `req` as a reader of `owner`, plain or
/// under `EXPLAIN (ANALYZE, FORMAT JSON)`; `states` binds `$2` and `assignment`
/// `$3` when the request filters on it.
async fn run(
    runtime: &PgPool,
    owner: Uuid,
    req: &QueryRequest,
    assignment: Option<Uuid>,
    explain: bool,
) -> Vec<sqlx::postgres::PgRow> {
    let sql = goal_page_sql_for_tests(req);
    let sql = if explain {
        format!("EXPLAIN (ANALYZE, FORMAT JSON) {sql}")
    } else {
        sql
    };
    let group = OwnerId::from(GroupId::new(owner));
    let verifier = super::SyntheticVerifier {
        roles: OwnerRoles::for_subject(UserId::new(Uuid::now_v7()), vec![(group, Role::viewer())])
            .unwrap(),
    };
    let authz = proxiima_core::authenticate(&verifier, &Credentials::Bearer("goals".into()))
        .await
        .unwrap();
    let mut tx = begin_owner_transaction(runtime, authz.owner_scope().unwrap())
        .await
        .unwrap();
    // SQL-POLICY: fixed-fragment — the closed production goal-page builder.
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(vec![owner])
        .bind(vec!["Active"]);
    if let Some(assignment) = assignment {
        query = query.bind(vec![assignment]);
    }
    let rows = query.fetch_all(&mut *tx).await.unwrap();
    tx.rollback().await.unwrap();
    rows
}

async fn run_listing(
    runtime: &PgPool,
    owner: Uuid,
    req: &QueryRequest,
    explain: bool,
) -> Vec<sqlx::postgres::PgRow> {
    run(runtime, owner, req, None, explain).await
}

fn goals_under(filter: GoalMotivationFilter) -> QueryRequest {
    let mut req = QueryRequest::readable();
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_states.insert(GoalState::Active);
    req.motivated_by = filter;
    req.limit = 50;
    req
}

/// What one run read of `goal` and `goal_head`: rows kept or filtered away,
/// over every loop, and whether any scan of `goal` was sequential.
fn goal_work(plan: &serde_json::Value) -> (f64, bool) {
    let (mut rows, mut sequential) = (0.0, false);
    walk(plan, &mut |node| {
        let relation = node
            .get("Relation Name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if relation == "goal" || relation == "goal_head" {
            rows += (metric(node, "Actual Rows")
                + metric(node, "Rows Removed by Filter")
                + metric(node, "Rows Removed by Index Recheck"))
                * metric(node, "Actual Loops").max(1.0);
        }
        let node_type = node
            .get("Node Type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if relation == "goal" && (node_type == "Seq Scan" || node_type == "Parallel Seq Scan") {
            sequential = true;
        }
    });
    (rows, sequential)
}

#[tokio::test]
async fn motivated_and_unmotivated_goal_reads_do_not_scan_the_goal_table() {
    let (database, admin, runtime, platform, platform_role, runtime_role, _) =
        super::setup_full_schema().await;
    let owner = Uuid::now_v7();
    seed(&admin, owner).await;
    super::vacuum_all_visible(&admin, &[super::Vacuumed::Goal]).await;
    let first_perspective: Uuid =
        sqlx::query_scalar("SELECT uuid_in(md5('perspective0')::cstring)")
            .fetch_one(&admin)
            .await
            .unwrap();

    // What each read returns: the first Perspective's Active series are the
    // multiples of PERSPECTIVES that are also multiples of ten.
    let assigned = goals_under(GoalMotivationFilter::MotivatedBy(vec![MemoryId::new(
        first_perspective,
    )]));
    let unassigned = goals_under(GoalMotivationFilter::Unmotivated);
    let expected_assigned = usize::try_from(SERIES / PERSPECTIVES).unwrap();
    let expected_unassigned = usize::try_from(SERIES / UNASSIGNED_EVERY).unwrap();
    assert_eq!(
        run(&runtime, owner, &assigned, Some(first_perspective), false)
            .await
            .len(),
        expected_assigned
    );
    assert_eq!(
        run(&runtime, owner, &unassigned, None, false).await.len(),
        expected_unassigned
    );

    // And how much of the owner's goals each read walks to return them.
    for (what, req, assignment, returned) in [
        (
            "assigned",
            &assigned,
            Some(first_perspective),
            expected_assigned,
        ),
        ("unassigned", &unassigned, None, expected_unassigned),
    ] {
        let rows = run(&runtime, owner, req, assignment, true).await;
        let plan: serde_json::Value = rows[0].get(0);
        let (read, sequential) = goal_work(&plan);
        assert!(
            !sequential && read < 25.0 * f64::from(u32::try_from(returned).unwrap()),
            "the {what} read walked {read} goal rows for {returned} goals \
             (sequential: {sequential}); plan: {plan}"
        );
    }
    runtime.close().await;
    platform.close().await;
    super::cleanup(&database, admin, &platform_role, &runtime_role).await;
}

/// An owner of few goals, written before everyone else's so that its series
/// are the oldest handles in the store, lists its goals without reading the
/// store's: the page walks the owner's heads, not the heads of other owners.
#[tokio::test]
async fn an_owner_of_few_goals_lists_them_without_reading_the_store() {
    let (database, admin, runtime, platform, platform_role, runtime_role, _) =
        super::setup_full_schema().await;
    let (small, large) = (Uuid::now_v7(), Uuid::now_v7());
    for (owner, series) in [(small, 300), (large, i64::from(SERIES))] {
        sqlx::query(
            "INSERT INTO proxiima_core.owners (owner_id, kind) VALUES ($1, 'group')
             ON CONFLICT DO NOTHING",
        )
        .bind(owner)
        .execute(&admin)
        .await
        .unwrap();
        let mut seed = admin.acquire().await.unwrap();
        sqlx::query("SET session_replication_role = replica")
            .execute(&mut *seed)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TEMP TABLE listed AS
             SELECT n, uuidv7() AS handle, uuidv7() AS t1, uuidv7() AS t2
               FROM generate_series(1, $1) n",
        )
        .bind(series)
        .execute(&mut *seed)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO proxiima_core.goal_head (handle, schema_id, owner_id, t)
             SELECT handle, 'core/simple-text-v1', $1, t2 FROM listed",
        )
        .bind(owner)
        .execute(&mut *seed)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO proxiima_core.goal (handle, t, owner_id, schema_id, title, state, request_id, \"by\", text)
             SELECT handle, t1, $1, 'core/simple-text-v1', 'g' || n, 'Active', 'first-' || n,
                    '00000000-0000-0000-0000-000000000545'::uuid, 'text'
               FROM listed",
        )
        .bind(owner)
        .execute(&mut *seed)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO proxiima_core.goal
                 (handle, t, owner_id, schema_id, title, state, request_id, \"by\", text, close_fact_t)
             SELECT handle, t2, $1, 'core/simple-text-v1', 'g' || n,
                    CASE WHEN n % 10 = 0 THEN 'Active' ELSE 'Achieved' END::proxiima_core.goal_state,
                    'head-' || n, '00000000-0000-0000-0000-000000000545'::uuid, 'text',
                    CASE WHEN n % 10 = 0 THEN NULL ELSE uuidv7() END
               FROM listed",
        )
        .bind(owner)
        .execute(&mut *seed)
        .await
        .unwrap();
        sqlx::query("DROP TABLE listed")
            .execute(&mut *seed)
            .await
            .unwrap();
    }
    super::vacuum_all_visible(&admin, &[super::Vacuumed::Goal]).await;
    super::vacuum_all_visible(&admin, &[super::Vacuumed::GoalHead]).await;

    // The small owner's Active goals are every tenth of its 300 series; the
    // page is capped at 50.
    let mut req = QueryRequest::readable();
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_states.insert(GoalState::Active);
    req.limit = 50;
    for (who, owner, returned) in [("small", small, 30_usize), ("large", large, 51)] {
        let rows = run_listing(&runtime, owner, &req, false).await;
        assert_eq!(rows.len(), returned, "{who} owner's page");
        let rows = run_listing(&runtime, owner, &req, true).await;
        let plan: serde_json::Value = rows[0].get(0);
        let (read, sequential) = goal_work(&plan);
        assert!(
            !sequential && read < 40.0 * f64::from(u32::try_from(returned).unwrap()),
            "the {who} owner's page walked {read} rows for {returned} goals \
             (sequential: {sequential}); plan: {plan}"
        );
    }
    runtime.close().await;
    platform.close().await;
    super::cleanup(&database, admin, &platform_role, &runtime_role).await;
}

async fn insert_projection_fact(admin: &PgPool, owner: Uuid) -> Uuid {
    let fact_t = Uuid::now_v7();
    let fact_handle = Uuid::now_v7();
    let fact_content: Uuid = sqlx::query_scalar(
        "INSERT INTO proxiima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/test-fact', decode(repeat('11', 32), 'hex'))
         RETURNING content_id",
    )
    .bind(owner)
    .fetch_one(admin)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO proxiima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'fact', 'core/test-fact', $2, $3)",
    )
    .bind(fact_handle)
    .bind(owner)
    .bind(fact_t)
    .execute(admin)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO proxiima_core.memory
             (handle, t, owner_id, kind, schema_id, content_id, occurred_at, \"by\", via)
         VALUES ($1, $2, $3, 'fact', 'core/test-fact', $4, now(), $3, NULL)",
    )
    .bind(fact_handle)
    .bind(fact_t)
    .bind(owner)
    .bind(fact_content)
    .execute(admin)
    .await
    .unwrap();
    fact_t
}

#[tokio::test]
async fn motivation_projection_matches_canonical_arrays_after_append() {
    let (database, admin, runtime, platform, platform_role, runtime_role, _) =
        super::setup_full_schema().await;
    let owner = Uuid::now_v7();
    let perspective = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxiima_core.owners (owner_id, kind) VALUES ($1, 'group'), ($2, 'group')",
    )
    .bind(owner)
    .bind(Uuid::now_v7())
    .execute(&admin)
    .await
    .unwrap();
    let fact_t = insert_projection_fact(&admin, owner).await;
    let perspective_handle = Uuid::now_v7();
    let perspective_content: Uuid = sqlx::query_scalar(
        "INSERT INTO proxiima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/test', decode(repeat('00', 32), 'hex'))
         RETURNING content_id",
    )
    .bind(owner)
    .fetch_one(&admin)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO proxiima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'perspective', 'core/test', $2, $3)",
    )
    .bind(perspective_handle)
    .bind(owner)
    .bind(perspective)
    .execute(&admin)
    .await
    .unwrap();
    // The trigger-maintained projection is populated by the normal Goal write
    // path. This direct platform fixture only supplies the exact target row.
    sqlx::query(
        "INSERT INTO proxiima_core.memory
             (handle, t, owner_id, kind, schema_id, content_id, occurred_at, \"by\", via, refs)
         VALUES ($1, $2, $3, 'perspective', 'core/test', $4, NULL, $3, NULL, ARRAY[$5])",
    )
    .bind(perspective_handle)
    .bind(perspective)
    .bind(owner)
    .bind(perspective_content)
    .bind(fact_t)
    .execute(&admin)
    .await
    .unwrap();
    let goal_t = Uuid::now_v7();
    let handle = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxiima_core.goal_head (handle, schema_id, owner_id, t)
         VALUES ($1, 'core/simple-text-v1', $2, $3)",
    )
    .bind(handle)
    .bind(owner)
    .bind(goal_t)
    .execute(&admin)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO proxiima_core.goal
             (handle, t, owner_id, schema_id, title, state, request_id, motivated_by, \"by\", text)
         VALUES ($1, $2, $3, 'core/simple-text-v1', 'g', 'Active', 'projection-test-g', $4, $3, 'text')",
    )
    .bind(handle)
    .bind(goal_t)
    .bind(owner)
    .bind(vec![perspective])
    .execute(&admin)
    .await
    .unwrap();
    let indexed: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "SELECT goal_t, perspective_t, owner_id
           FROM proxiima_core.goal_motivation_index
          WHERE goal_t = $1",
    )
    .bind(goal_t)
    .fetch_all(&admin)
    .await
    .unwrap();
    assert_eq!(indexed, vec![(goal_t, perspective, owner)]);

    runtime.close().await;
    platform.close().await;
    super::cleanup(&database, admin, &platform_role, &runtime_role).await;
}
