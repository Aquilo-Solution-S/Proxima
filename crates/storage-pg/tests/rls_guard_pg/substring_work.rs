//! Work bound for the substring arm under the shipped RLS policies (#490).

use proxima_core::verbs::query::like_pattern;
use proxima_core::{Credentials, GroupId, OwnerId, OwnerRoles, Role, UserId};
use proxima_storage_pg::begin_owner_transaction;
use proxima_storage_pg::verbs::query::substring_candidate_sql_for_tests;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Notes the big owner holds; every one is a memory of schema
/// `core/agent-note-v1`.
const BIG_OWNER_NOTES: i32 = 30_000;

async fn seed_utterances(admin: &PgPool, owner: Uuid, count: i32) {
    seed(
        admin,
        owner,
        count,
        "core/utterance-v1",
        "proxima_core.utterance_v1",
    )
    .await;
}

async fn seed_notes(admin: &PgPool, owner: Uuid, count: i32) {
    seed(
        admin,
        owner,
        count,
        "core/agent-note-v1",
        "proxima_core.agent_note_v1",
    )
    .await;
}

async fn seed(admin: &PgPool, owner: Uuid, count: i32, schema: &str, table: &str) {
    // One coarse-locked transaction: a write otherwise takes one advisory
    // lock per memory, so the corpus used to go in in 1_000-row batches.
    let mut stamped = admin.begin().await.unwrap();
    sqlx::query("LOCK TABLE proxima_core.memory, proxima_core.goal IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *stamped)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'group') ON CONFLICT DO NOTHING",
    )
    .bind(owner)
    .execute(&mut *stamped)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         SELECT uuidv7(), 'fact', $3, $1, uuidv7()
           FROM generate_series(1, $2)",
    )
    .bind(owner)
    .bind(count)
    .bind(schema)
    .execute(&mut *stamped)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO proxima_core.memory
             (handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via)
         SELECT h.handle, h.t, 'fact', $1, $2, ARRAY[$3], uuid_extract_timestamp(h.t),
                '00000000-0000-0000-0000-000000000509'::uuid, NULL
           FROM proxima_core.memory_head h
          WHERE h.owner_id = $1 AND h.schema_id = $2
            AND NOT EXISTS (SELECT 1 FROM proxima_core.memory m WHERE m.t = h.t)",
    )
    .bind(owner)
    .bind(schema)
    .bind(table)
    .execute(&mut *stamped)
    .await
    .unwrap();
    if schema == "core/utterance-v1" {
        sqlx::query(
            "INSERT INTO proxima_core.utterance_v1 (t, speaker, conversation_id, text)
             SELECT m.t, 'user', 'c', 'utterance ' || row_number() OVER (ORDER BY m.t)
               FROM proxima_core.memory m
              WHERE m.owner_id = $1 AND m.schema_id = 'core/utterance-v1'
                AND NOT EXISTS (SELECT 1 FROM proxima_core.utterance_v1 s WHERE s.t = m.t)",
        )
        .bind(owner)
        .execute(&mut *stamped)
        .await
        .unwrap();
    } else {
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             SELECT m.t, uuidv7(), 'note ' || row_number() OVER (ORDER BY m.t),
                    'body ' || row_number() OVER (ORDER BY m.t), '{}'
               FROM proxima_core.memory m
              WHERE m.owner_id = $1 AND m.schema_id = 'core/agent-note-v1'
                AND NOT EXISTS (SELECT 1 FROM proxima_core.agent_note_v1 s WHERE s.t = m.t)",
        )
        .bind(owner)
        .execute(&mut *stamped)
        .await
        .unwrap();
    }
    stamped.commit().await.unwrap();
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

fn text<'a>(node: &'a serde_json::Map<String, serde_json::Value>, key: &str) -> &'a str {
    node.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

fn metric(node: &serde_json::Map<String, serde_json::Value>, key: &str) -> f64 {
    node.get(key)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or_default()
}

/// What one leg of the substring arm read: the sidecar it searches, the
/// `memory` and `memory_head` rows it read (kept or filtered away, over every
/// loop) and the conditions of its scans of the owner-and-schema index.
struct LegWork {
    sidecar: String,
    memory_rows: f64,
    schema_index_conds: Vec<String>,
}

fn leg_work(leg: &serde_json::Value) -> LegWork {
    let mut work = LegWork {
        sidecar: String::new(),
        memory_rows: 0.0,
        schema_index_conds: Vec::new(),
    };
    walk(leg, &mut |node| {
        let relation = text(node, "Relation Name");
        if relation.ends_with("_v1") && work.sidecar.is_empty() {
            relation.clone_into(&mut work.sidecar);
        }
        if relation == "memory" || relation == "memory_head" {
            work.memory_rows += (metric(node, "Actual Rows")
                + metric(node, "Rows Removed by Filter")
                + metric(node, "Rows Removed by Index Recheck"))
                * metric(node, "Actual Loops").max(1.0);
        }
        if text(node, "Index Name") == "memory_owner_schema_t_idx" {
            work.schema_index_conds
                .push(text(node, "Index Cond").to_owned());
        }
    });
    work
}

async fn owner_tx(runtime: &PgPool, reader: Uuid) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let owner = OwnerId::from(GroupId::new(reader));
    let verifier = super::SyntheticVerifier {
        roles: OwnerRoles::for_subject(UserId::new(Uuid::now_v7()), vec![(owner, Role::viewer())])
            .unwrap(),
    };
    let authz = proxima_core::authenticate(&verifier, &Credentials::Bearer("substring".into()))
        .await
        .unwrap();
    begin_owner_transaction(runtime, authz.owner_scope().unwrap())
        .await
        .unwrap()
}

/// Run the deployed substring statement on `tx`, plain or under
/// `EXPLAIN (ANALYZE, FORMAT JSON)`, with every schema named missing and a
/// window of 20.
async fn run_candidates_on(
    tx: &mut sqlx::PgConnection,
    reader: Uuid,
    query: &str,
    explain: bool,
) -> Vec<sqlx::postgres::PgRow> {
    let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
    let schema_ids: Vec<&str> = registry
        .search_projections()
        .iter()
        .map(|projection| projection.schema_id.as_str())
        .collect();
    let sql = substring_candidate_sql_for_tests(registry.search_projections(), false).unwrap();
    let sql = if explain {
        format!("EXPLAIN (ANALYZE, FORMAT JSON) {sql}")
    } else {
        sql
    };
    // SQL-POLICY: fixed-fragment — the closed production substring builder.
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(like_pattern(query))
        .bind(20_i64)
        .bind(None::<Vec<String>>)
        .bind(None::<time::OffsetDateTime>)
        .bind(None::<time::OffsetDateTime>)
        .bind(None::<Uuid>)
        .bind(vec![reader])
        .bind(schema_ids)
        .bind(false)
        .bind(true)
        .bind(None::<&str>)
        .bind(false)
        .fetch_all(tx)
        .await
        .unwrap()
}

/// As `reader`, the way a request reaches the arm.
async fn run_candidates(
    runtime: &PgPool,
    reader: Uuid,
    query: &str,
    explain: bool,
) -> Vec<sqlx::postgres::PgRow> {
    let mut tx = owner_tx(runtime, reader).await;
    let rows = run_candidates_on(&mut tx, reader, query, explain).await;
    tx.rollback().await.unwrap();
    rows
}

/// The substring arm with the owner's notes the only memories it could match
/// and a query that matches none of them (#490).
///
/// Every leg of the arm drives from `memory` and probes its own sidecar. The
/// request names the schemas the ranked arm found nothing for, which after a
/// typo is all of them, so a leg that named only that set read every memory
/// of the owner: 30,000 notes, each probed for a row in a sidecar that holds
/// no note. A leg reaches only the memories of the schemas stored in its own
/// sidecar, through `memory_owner_schema_t_idx`, as the runtime role under
/// the shipped policies.
#[tokio::test]
async fn substring_legs_read_only_memories_of_their_own_schemas() {
    let (database, admin, runtime, platform, platform_role, runtime_role, _) =
        super::setup_full_schema().await;
    let big = Uuid::now_v7();
    seed_notes(&admin, big, BIG_OWNER_NOTES).await;
    let small = Uuid::now_v7();
    seed_utterances(&admin, small, 3).await;
    // The other sidecars are not empty either, so a leg is not cheap just
    // because its table is. Twenty owners keep the planner from treating
    // `utterance_v1` as one fat relation of a single other owner.
    for _ in 0..20 {
        seed_utterances(&admin, Uuid::now_v7(), 500).await;
    }
    sqlx::query("VACUUM (ANALYZE) proxima_core.memory")
        .execute(&admin)
        .await
        .unwrap();
    let rows = run_candidates(&runtime, big, "nomatchtypo", true).await;
    let plan: serde_json::Value = rows[0].get(0);
    let mut legs = Vec::new();
    walk(&plan, &mut |node| {
        if text(node, "Node Type") == "Append" {
            let children = node["Plans"].as_array().expect("an Append has children");
            legs = children.iter().map(leg_work).collect();
        }
    });
    let sidecars: Vec<&str> = legs.iter().map(|leg| leg.sidecar.as_str()).collect();
    assert_eq!(
        sidecars,
        [
            "agent_derivation_v1",
            "agent_note_v1",
            "interpretation_v1",
            "utterance_v1"
        ],
        "one leg per sidecar; plan: {plan}"
    );
    for leg in legs.iter().filter(|leg| leg.sidecar != "agent_note_v1") {
        assert!(
            leg.memory_rows < 1.0,
            "the {} leg read {} memory rows of the owner, none of a schema it searches; plan: {plan}",
            leg.sidecar,
            leg.memory_rows
        );
    }
    let utterance = legs
        .iter()
        .find(|leg| leg.sidecar == "utterance_v1")
        .unwrap();
    assert!(
        utterance
            .schema_index_conds
            .iter()
            .any(|cond| cond.contains("'{core/utterance-v1}'")),
        "the utterance leg reaches memory through the owner-and-schema index, \
         bounded by its own schema; plan: {plan}"
    );

    // The narrowing loses nothing: each leg still finds what its sidecar holds.
    for (reader, query, expected) in [(big, "note 12", 20), (small, "utterance 2", 1)] {
        let found = run_candidates(&runtime, reader, query, false).await;
        assert_eq!(found.len(), expected, "{query}");
    }
    runtime.close().await;
    platform.close().await;
    super::cleanup(&database, admin, &platform_role, &runtime_role).await;
}

/// The substring statement as the candidate role reads the sidecar through its
/// trigram index, with sequential scans left enabled (#554).
///
/// The candidate function runs as `proxima_search`. Owner row security does
/// not apply to that role on the core sidecars (the `proxima_search_candidates`
/// policy), so the planner may use an index for `LIKE`, which is not leakproof
/// and so could never be an index qual under a security qual. Membership and
/// ownership stay with `memory`, whose policy the same statement still joins.
#[tokio::test]
async fn substring_arm_reads_the_sidecar_through_its_trigram_index() {
    let (database, admin, runtime, platform, platform_role, runtime_role, _) =
        super::setup_full_schema().await;
    let owner = Uuid::now_v7();
    seed_notes(&admin, owner, BIG_OWNER_NOTES).await;
    sqlx::query("VACUUM (ANALYZE) proxima_core.agent_note_v1, proxima_core.memory")
        .execute(&admin)
        .await
        .unwrap();
    let mut tx = admin.begin().await.unwrap();
    sqlx::query("SET LOCAL ROLE proxima_search")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "SELECT set_config('app.proxima_scope', 'owner', true),
                set_config('app.owner', $1, true),
                set_config('app.read_abstraction', $1, true),
                set_config('app.read_perspective', $1, true),
                set_config('app.read_goal', $1, true)",
    )
    .bind(format!("{{{owner}}}"))
    .execute(&mut *tx)
    .await
    .unwrap();
    let rows = run_candidates_on(&mut tx, owner, "nomatchtypo", true).await;
    tx.rollback().await.unwrap();
    let plan: serde_json::Value = rows[0].get(0);
    let mut indexes = Vec::new();
    let mut note_scans = Vec::new();
    walk(&plan, &mut |node| {
        if !text(node, "Index Name").is_empty() {
            indexes.push(text(node, "Index Name").to_owned());
        }
        if text(node, "Relation Name") == "agent_note_v1" {
            note_scans.push(text(node, "Node Type").to_owned());
        }
    });
    assert!(
        indexes.contains(&"agent_note_v1_substring_trgm".to_owned()),
        "the note leg reads its trigram index; plan: {plan}"
    );
    assert!(
        note_scans.iter().all(|scan| scan != "Seq Scan"),
        "no sequential scan of the {BIG_OWNER_NOTES} notes; plan: {plan}"
    );
    runtime.close().await;
    platform.close().await;
    super::cleanup(&database, admin, &platform_role, &runtime_role).await;
}
