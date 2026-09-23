//! Falsifier for the owner-RLS trigger scope bridge: each platform-scoped
//! integrity routine hands the runtime caller its own `app.proxima_scope` back,
//! both on normal return and after a refusal caught by a savepoint.

use proxima_core::{Credentials, GroupId, OwnerRef, OwnerRoles, Role, UserId};
use proxima_storage_pg::begin_owner_transaction;
use sqlx::PgConnection;
use sqlx::postgres::PgQueryResult;
use uuid::Uuid;

const BRIDGED_ROUTINES: [&str; 10] = [
    "assert_erased_pin_target_insert",
    "memory_erase_witness",
    "cooled_erase_witness",
    "goal_erase_witness",
    "cooled_identity_seal",
    "goal_pin_target_checks",
    "wake_pin_target_checks",
    "memory_pin_checks",
    "cooled_forget_grounding",
    "pins_have_grounding_support",
];

async fn scope(connection: &mut PgConnection) -> Option<String> {
    sqlx::query_scalar("SELECT current_setting('app.proxima_scope', true)")
        .fetch_one(connection)
        .await
        .unwrap()
}

async fn assert_owner_scope(connection: &mut PgConnection, path: &str) {
    assert_eq!(
        scope(connection).await.as_deref(),
        Some("owner"),
        "{path} must restore the caller's owner scope"
    );
}

async fn savepoint(connection: &mut PgConnection) {
    sqlx::query("SAVEPOINT trigger_scope")
        .execute(connection)
        .await
        .unwrap();
}

/// Expect `outcome` to be the routine's own refusal, unwind the savepoint, and
/// prove the caller's scope is intact for the rest of the transaction.
async fn assert_refusal_restores_scope(
    connection: &mut PgConnection,
    routine: &str,
    code: &str,
    message: &str,
    outcome: Result<PgQueryResult, sqlx::Error>,
) {
    let error = outcome.expect_err(routine);
    let database = error.as_database_error().expect("database refusal");
    assert_eq!(database.code().as_deref(), Some(code), "{routine}: {error}");
    assert!(database.message().contains(message), "{routine}: {error}");
    sqlx::query("ROLLBACK TO SAVEPOINT trigger_scope")
        .execute(&mut *connection)
        .await
        .unwrap();
    assert_owner_scope(connection, &format!("{routine} refusal")).await;
}

/// Insert a head and Memory row; non-Facts get the content row they require.
async fn insert_memory(
    connection: &mut PgConnection,
    owner: Uuid,
    kind: &str,
    origins: &[Uuid],
) -> Result<Uuid, sqlx::Error> {
    let handle = Uuid::now_v7();
    let t = Uuid::now_v7();
    let content_id = if kind == "fact" {
        None
    } else {
        let content_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.content(content_id, owner_id, schema_id, content_hash)
             VALUES ($1, $2, 'core/test', $3)",
        )
        .bind(content_id)
        .bind(owner)
        .bind([content_id.as_bytes().as_slice(), t.as_bytes()].concat())
        .execute(&mut *connection)
        .await?;
        Some(content_id)
    };
    sqlx::query(
        "INSERT INTO proxima_core.memory_head(handle, schema_id, kind, owner_id, t)
         VALUES ($1, 'core/test', $2::text::proxima_core.memory_kind, $3, $4)",
    )
    .bind(handle)
    .bind(kind)
    .bind(owner)
    .bind(t)
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory(handle, t, kind, owner_id, schema_id, content_id, origins)
         VALUES ($1, $2, $3::text::proxima_core.memory_kind, $4, 'core/test', $5, $6)",
    )
    .bind(handle)
    .bind(t)
    .bind(kind)
    .bind(owner)
    .bind(content_id)
    .bind(origins)
    .execute(&mut *connection)
    .await?;
    Ok(t)
}

async fn cool(connection: &mut PgConnection, t: Uuid) -> Result<PgQueryResult, sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxima_core.cooled
             (t, handle, owner_id, kind, object_key, source_id, ingest_key, blob_id,
              content_id, origins, refs, goal_refs, cooled_at)
         SELECT m.t, m.handle, m.owner_id, m.kind, 'cold/' || m.t::text, m.source_id,
                m.ingest_key, m.blob_id, m.content_id, m.origins, m.refs, m.goal_refs, now()
           FROM proxima_core.memory m WHERE m.t = $1",
    )
    .bind(t)
    .execute(connection)
    .await
}

async fn insert_goal(
    connection: &mut PgConnection,
    owner: Uuid,
    t: Uuid,
) -> Result<PgQueryResult, sqlx::Error> {
    let handle = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.goal_head(handle, schema_id, owner_id, t)
         VALUES ($1, 'core/task-goal-v1', $2, $3)",
    )
    .bind(handle)
    .bind(owner)
    .bind(t)
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.goal(handle, t, owner_id, title, state, request_id)
         VALUES ($1, $2, $3, 'scope bridge', 'Active', $4)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner)
    .bind(handle.to_string())
    .execute(&mut *connection)
    .await
}

/// Hold a routine's lifecycle advisory lock from another session so the
/// routine fails inside its body, after it has bound platform scope.
async fn contend(connection: &mut PgConnection, holder: &mut PgConnection, t: Uuid) {
    let held: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_lock(hashtextextended('proxima-forget:' || $1::text, 0))",
    )
    .bind(t)
    .fetch_one(holder)
    .await
    .unwrap();
    assert!(held, "target {t} must not be locked by the caller already");
    savepoint(connection).await;
    sqlx::query("SET LOCAL lock_timeout = '200ms'")
        .execute(connection)
        .await
        .unwrap();
}

async fn release(holder: &mut PgConnection) {
    sqlx::query("SELECT pg_advisory_unlock_all()")
        .execute(holder)
        .await
        .unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one disposable fixture drives every bridged routine"
)]
async fn trigger_bridge_restores_caller_scope_on_return_and_refusal() {
    let (database, admin, runtime, platform, platform_role, runtime_role, _password) =
        super::setup_full_schema().await;
    let owner = Uuid::now_v7();
    sqlx::query("INSERT INTO proxima_core.owners(owner_id, kind) VALUES ($1, 'group')")
        .bind(owner)
        .execute(&admin)
        .await
        .unwrap();

    // Structural half: no bridged routine carries a function-level scope
    // setting, every one is a fixed-path definer closed to PUBLIC that binds
    // platform once and restores before every RETURN.  The count also guards
    // returns no caller can observe: assert_erased_pin_target_insert returns
    // only inside record_erased_pin_target, and two early returns are
    // unreachable from live rows (malformed-array seal, empty historical restore).
    let unbridged: Vec<String> = sqlx::query_scalar(
        r"SELECT p.proname::text
           FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
          WHERE n.nspname = 'proxima_core' AND p.proname = ANY($1::text[])
            AND (NOT p.prosecdef
                 OR p.proconfig IS DISTINCT FROM ARRAY['search_path=pg_catalog, proxima_core']
                 OR has_function_privilege($2, p.oid, 'EXECUTE')
                 OR regexp_count(p.prosrc, 'set_config\(''app\.proxima_scope'', ''platform'', true\)') <> 1
                 OR regexp_count(p.prosrc, '\mRETURN\M')
                    <> regexp_count(p.prosrc, 'set_config\(''app\.proxima_scope'', COALESCE\(previous_scope, ''''\), true\);\s+RETURN\M'))",
    )
    .bind(BRIDGED_ROUTINES.to_vec())
    .bind(&runtime_role)
    .fetch_all(&admin)
    .await
    .unwrap();
    assert!(unbridged.is_empty(), "unbridged routines: {unbridged:?}");

    let mut seed = admin.acquire().await.unwrap();
    let fact = insert_memory(&mut seed, owner, "fact", &[]).await.unwrap();
    let hard_erase = insert_memory(&mut seed, owner, "fact", &[]).await.unwrap();
    let grounding = insert_memory(&mut seed, owner, "abstraction", &[fact])
        .await
        .unwrap();
    insert_memory(&mut seed, owner, "perspective", &[grounding])
        .await
        .unwrap();
    // Erase targets the runtime transaction has never locked: its own
    // lifecycle locks are transaction-scoped and would block the contender.
    let erase_cooled = insert_memory(&mut seed, owner, "fact", &[]).await.unwrap();
    cool(&mut seed, erase_cooled).await.unwrap();
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
        .bind(erase_cooled)
        .execute(&mut *seed)
        .await
        .unwrap();
    let erase_goal = Uuid::now_v7();
    insert_goal(&mut seed, owner, erase_goal).await.unwrap();
    drop(seed);

    let verifier = super::SyntheticVerifier {
        roles: OwnerRoles::for_subject(
            UserId::new(Uuid::now_v7()),
            [(OwnerRef::Group(GroupId::new(owner)), Role::admin())],
        )
        .unwrap(),
    };
    let authz = proxima_core::authenticate(&verifier, &Credentials::Bearer("scope".into()))
        .await
        .unwrap();
    let mut tx = begin_owner_transaction(&runtime, authz.owner_scope().unwrap())
        .await
        .unwrap();
    assert_owner_scope(&mut tx, "owner transaction").await;

    // memory_pin_checks: Fact early return, full pin path (nesting
    // pins_have_grounding_support), and an ungrounded refusal.
    let cooled_fact = insert_memory(&mut tx, owner, "fact", &[]).await.unwrap();
    assert_owner_scope(&mut tx, "memory_pin_checks fact return").await;
    let abstraction = insert_memory(&mut tx, owner, "abstraction", &[fact])
        .await
        .unwrap();
    assert_owner_scope(&mut tx, "memory_pin_checks pinned return").await;
    savepoint(&mut tx).await;
    let outcome = insert_memory(&mut tx, owner, "abstraction", &[Uuid::now_v7()])
        .await
        .map(|_| PgQueryResult::default());
    assert_refusal_restores_scope(
        &mut tx,
        "memory_pin_checks",
        "23514",
        "non-fact must pin",
        outcome,
    )
    .await;

    // goal_pin_target_checks
    let goal = Uuid::now_v7();
    insert_goal(&mut tx, owner, goal).await.unwrap();
    assert_owner_scope(&mut tx, "goal_pin_target_checks return").await;
    savepoint(&mut tx).await;
    let outcome = insert_goal(&mut tx, owner, fact).await;
    assert_refusal_restores_scope(
        &mut tx,
        "goal_pin_target_checks",
        "23505",
        "collides with an existing entity",
        outcome,
    )
    .await;

    // wake_pin_target_checks
    sqlx::query(
        "INSERT INTO proxima_core.wake_config
             (owner_id, trigger_kind, trigger_t, hard_memory_t, tool_ids, prompt)
         VALUES ($1, 'fact_memory', $2, ARRAY[$2]::uuid[], ARRAY['core.remember'], 'wake')",
    )
    .bind(owner)
    .bind(fact)
    .execute(&mut *tx)
    .await
    .unwrap();
    assert_owner_scope(&mut tx, "wake_pin_target_checks return").await;
    savepoint(&mut tx).await;
    let outcome = sqlx::query(
        "INSERT INTO proxima_core.wake_config
             (owner_id, trigger_kind, trigger_t, hard_memory_t, tool_ids, prompt)
         VALUES ($1, 'fact_memory', $2, ARRAY[$3]::uuid[], ARRAY['core.remember'], 'wake')",
    )
    .bind(owner)
    .bind(fact)
    .bind(Uuid::now_v7())
    .execute(&mut *tx)
    .await;
    assert_refusal_restores_scope(
        &mut tx,
        "wake_pin_target_checks",
        "23503",
        "hard context memory does not exist",
        outcome,
    )
    .await;

    // cooled_forget_grounding (Fact early return) + cooled_identity_seal, then
    // the forget's Memory delete (memory_erase_witness, no witness written).
    cool(&mut tx, cooled_fact).await.unwrap();
    assert_owner_scope(&mut tx, "cooled_identity_seal return").await;
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
        .bind(cooled_fact)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_owner_scope(&mut tx, "memory_erase_witness forget return").await;
    savepoint(&mut tx).await;
    let outcome = sqlx::query(
        "INSERT INTO proxima_core.cooled
             (t, handle, owner_id, kind, object_key, origins, refs, goal_refs, cooled_at)
         VALUES ($1, $1, $2, 'fact', 'cold/unsealed', '{}', '{}', '{}', now())",
    )
    .bind(Uuid::now_v7())
    .bind(owner)
    .execute(&mut *tx)
    .await;
    assert_refusal_restores_scope(
        &mut tx,
        "cooled_identity_seal",
        "23514",
        "does not seal its hot Memory",
        outcome,
    )
    .await;

    // cooled_forget_grounding: non-Fact path and the ungrounded-depender refusal.
    cool(&mut tx, abstraction).await.unwrap();
    assert_owner_scope(&mut tx, "cooled_forget_grounding return").await;
    savepoint(&mut tx).await;
    let outcome = cool(&mut tx, grounding).await;
    assert_refusal_restores_scope(
        &mut tx,
        "cooled_forget_grounding",
        "23514",
        "would leave an ungrounded memory",
        outcome,
    )
    .await;

    // The three erase witnesses, each refused inside its body by a held
    // lifecycle lock, then run to completion. Completion nests
    // record_erased_pin_target and assert_erased_pin_target_insert.
    let mut holder = admin.acquire().await.unwrap();
    for (routine, statement, target) in [
        (
            "memory_erase_witness",
            "DELETE FROM proxima_core.memory WHERE t = $1",
            hard_erase,
        ),
        (
            "cooled_erase_witness",
            "DELETE FROM proxima_core.cooled WHERE t = $1",
            erase_cooled,
        ),
        (
            "goal_erase_witness",
            "DELETE FROM proxima_core.goal WHERE t = $1",
            erase_goal,
        ),
    ] {
        contend(&mut tx, &mut holder, target).await;
        // SQL-POLICY: fixed-fragment — one of the three literal DELETEs above.
        let outcome = sqlx::query(statement).bind(target).execute(&mut *tx).await;
        release(&mut holder).await;
        assert_refusal_restores_scope(&mut tx, routine, "55P03", "lock timeout", outcome).await;
        // SQL-POLICY: fixed-fragment — one of the three literal DELETEs above.
        let deleted = sqlx::query(statement)
            .bind(target)
            .execute(&mut *tx)
            .await
            .unwrap();
        assert_eq!(deleted.rows_affected(), 1, "{routine}");
        assert_owner_scope(&mut tx, &format!("{routine} return")).await;
    }

    drop(holder);

    // assert_erased_pin_target_insert: a direct witness write is refused.
    savepoint(&mut tx).await;
    let outcome =
        sqlx::query("INSERT INTO proxima_core.erased_pin_target(t, kind) VALUES ($1, 'fact')")
            .bind(Uuid::now_v7())
            .execute(&mut *tx)
            .await;
    assert_refusal_restores_scope(
        &mut tx,
        "assert_erased_pin_target_insert",
        "42501",
        "written only by a target deletion trigger",
        outcome,
    )
    .await;
    tx.rollback().await.unwrap();

    // pins_have_grounding_support is closed to runtime; its owner calls it
    // directly under owner scope to observe its own restore.
    let mut runtime_tx = runtime.begin().await.unwrap();
    let refused = sqlx::query(
        "SELECT proxima_core.pins_have_grounding_support(ARRAY[$1]::uuid[], NULL, NULL)",
    )
    .bind(fact)
    .execute(&mut *runtime_tx)
    .await
    .expect_err("runtime must not execute a bridged routine");
    assert_eq!(
        refused
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("42501")
    );
    runtime_tx.rollback().await.unwrap();
    let mut platform_tx = platform.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.proxima_scope', 'owner', true)")
        .execute(&mut *platform_tx)
        .await
        .unwrap();
    let supported: bool = sqlx::query_scalar(
        "SELECT proxima_core.pins_have_grounding_support(ARRAY[$1]::uuid[], NULL, NULL)",
    )
    .bind(fact)
    .fetch_one(&mut *platform_tx)
    .await
    .unwrap();
    assert!(supported, "a hot Fact grounds under the platform bridge");
    assert_owner_scope(&mut platform_tx, "pins_have_grounding_support return").await;
    platform_tx.rollback().await.unwrap();

    runtime.close().await;
    platform.close().await;
    super::cleanup(&database, admin, &platform_role, &runtime_role).await;
}
