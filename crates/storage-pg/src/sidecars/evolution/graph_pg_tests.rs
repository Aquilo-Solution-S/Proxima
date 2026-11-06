use crate::sidecars::evolution::graph;
use crate::test_fixtures::create_core_db;
use proxiima_pg_testkit::{drop_db, split_role_urls};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

const SOURCE_SCHEMA: &str = "test/fact-v1";
const TARGET_SCHEMA: &str = "test/fact-v2";
const GOAL_SCHEMA: &str = "test/goal-v1";
const ABSTRACTION_SOURCE_SCHEMA: &str = "test/abstraction-v1";
const ABSTRACTION_TARGET_SCHEMA: &str = "test/abstraction-v2";

async fn owner_id(pool: &PgPool) -> Result<Uuid, sqlx::Error> {
    let owner = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('app.proxiima_scope', 'platform', true)")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO proxiima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(owner)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(owner)
}

async fn seed_goal_and_fact(pool: &PgPool, owner: Uuid) -> Result<(Uuid, Uuid, Uuid), sqlx::Error> {
    let goal_handle = Uuid::now_v7();
    let goal_t = Uuid::now_v7();
    let fact_handle = Uuid::now_v7();
    let fact_t = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('app.proxiima_scope', 'platform', true)")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.goal_head (handle, schema_id, owner_id, t)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(goal_handle)
    .bind(GOAL_SCHEMA)
    .bind(owner)
    .bind(goal_t)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.goal
         (handle, t, owner_id, title, state, request_id, \"by\", via)
         VALUES ($1, $2, $3, 'graph test goal', 'Active', 'graph-test', $3, NULL)",
    )
    .bind(goal_handle)
    .bind(goal_t)
    .bind(owner)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'fact', $2, $3, $4)",
    )
    .bind(fact_handle)
    .bind(SOURCE_SCHEMA)
    .bind(owner)
    .bind(fact_t)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.memory
         (handle, t, kind, owner_id, schema_id, occurred_at, goal_refs, \"by\")
         VALUES ($1, $2, 'fact', $3, $4, now(), $5, $3)",
    )
    .bind(fact_handle)
    .bind(fact_t)
    .bind(owner)
    .bind(SOURCE_SCHEMA)
    .bind(vec![goal_t])
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((goal_t, fact_handle, fact_t))
}

async fn migrate_fact(
    pool: &PgPool,
    owner: Uuid,
    fact_handle: Uuid,
    fact_t: Uuid,
    goal_refs: Vec<Uuid>,
) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('app.proxiima_scope', 'platform', true)")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.schema_evolution_authority
         (record_kind, source_id, target_id, source_schema, target_schema,
          source_handle, target_handle, record_owner, primary_target)
         VALUES ('memory', $1, $1, $2, $3, $4, $4, $5, true)",
    )
    .bind(fact_t)
    .bind(SOURCE_SCHEMA)
    .bind(TARGET_SCHEMA)
    .bind(fact_handle)
    .bind(owner)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE proxiima_core.memory_head SET schema_id = $1 WHERE handle = $2")
        .bind(TARGET_SCHEMA)
        .bind(fact_handle)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE proxiima_core.memory SET schema_id = $1, goal_refs = $2 WHERE t = $3")
        .bind(TARGET_SCHEMA)
        .bind(goal_refs)
        .bind(fact_t)
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

#[tokio::test]
async fn graph_accepts_only_authorized_erased_origin_witness() {
    let database = format!("proxiima_test_{}", Uuid::now_v7().simple());
    create_core_db(&database)
        .await
        .expect("PG required for test");
    let result = async {
        let (_, platform_url) = split_role_urls(&database).await?;
        let platform = PgPool::connect(&platform_url).await?;
        let owner = owner_id(&platform).await?;
        let fact_handle = Uuid::now_v7();
        let fact_t = Uuid::now_v7();
        let support_fact_handle = Uuid::now_v7();
        let support_fact_t = Uuid::now_v7();
        let old_abstraction_handle = Uuid::now_v7();
        let old_abstraction_t = Uuid::now_v7();
        let new_abstraction_handle = Uuid::now_v7();
        let new_abstraction_t = Uuid::now_v7();
        let ids = GraphSeedIds {
            fact_handle,
            fact_t,
            support_fact_handle,
            support_fact_t,
            old_abstraction_handle,
            old_abstraction_t,
        };
        let content_id = seed_graph_memory(&platform, owner, &ids).await?;

        let mut tx = platform.begin().await?;
        sqlx::query("SELECT set_config('app.proxiima_scope', 'platform', true)")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO proxiima_core.schema_evolution_authority
             (record_kind, source_id, target_id, source_schema, target_schema,
              source_handle, target_handle, record_owner, primary_target)
             VALUES ('memory', $1, $2, $3, $4, $5, $6, $7, true)",
        )
        .bind(old_abstraction_t)
        .bind(new_abstraction_t)
        .bind(ABSTRACTION_SOURCE_SCHEMA)
        .bind(ABSTRACTION_TARGET_SCHEMA)
        .bind(old_abstraction_handle)
        .bind(new_abstraction_handle)
        .bind(owner)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM proxiima_core.memory WHERE t = $1")
            .bind(fact_t)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO proxiima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'abstraction', $2, $3, $4)",
        )
        .bind(new_abstraction_handle)
        .bind(ABSTRACTION_TARGET_SCHEMA)
        .bind(owner)
        .bind(new_abstraction_t)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO proxiima_core.memory
             (handle, t, kind, owner_id, schema_id, origins, content_id, \"by\")
             VALUES ($1, $2, 'abstraction', $3, $4, $5, $6, $3)",
        )
        .bind(new_abstraction_handle)
        .bind(new_abstraction_t)
        .bind(owner)
        .bind(ABSTRACTION_TARGET_SCHEMA)
        .bind(vec![fact_t])
        .bind(content_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM proxiima_core.memory WHERE t = $1")
            .bind(old_abstraction_t)
            .execute(&mut *tx)
            .await?;
        let witness_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM proxiima_core.erased_pin_target WHERE t = $1")
                .bind(old_abstraction_t)
                .fetch_one(&mut *tx)
                .await?;
        assert_eq!(
            witness_count, 0,
            "mapped source deletion must not mint a witness"
        );
        graph::validate(&mut tx)
            .await
            .expect("the captured Fact origin witness is valid for its migrated target");

        assert_ordinary_origin_refused(&mut tx, owner, fact_t, support_fact_t, content_id).await?;
        tx.rollback().await?;
        Ok::<(), sqlx::Error>(())
    }
    .await;
    drop_db(&database).await.expect("drop test database");
    result.expect("erased origin witness graph validation");
}

async fn assert_ordinary_origin_refused(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: Uuid,
    fact_t: Uuid,
    support_fact_t: Uuid,
    content_id: Uuid,
) -> Result<(), sqlx::Error> {
    let ordinary_handle = Uuid::now_v7();
    let ordinary_t = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxiima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'abstraction', $2, $3, $4)",
    )
    .bind(ordinary_handle)
    .bind("test/ordinary-abstraction")
    .bind(owner)
    .bind(ordinary_t)
    .execute(&mut **tx)
    .await?;
    let error = sqlx::query(
        "INSERT INTO proxiima_core.memory
         (handle, t, kind, owner_id, schema_id, origins, refs, content_id, \"by\")
         VALUES ($1, $2, 'abstraction', $3, $4, $5, $6, $7, $3)",
    )
    .bind(ordinary_handle)
    .bind(ordinary_t)
    .bind(owner)
    .bind("test/ordinary-abstraction")
    .bind(vec![fact_t])
    .bind(vec![support_fact_t])
    .bind(content_id)
    .execute(&mut **tx)
    .await
    .expect_err("an ordinary abstraction cannot invent an erased origin");
    assert!(error.to_string().contains("origin pin"), "{error}");
    Ok(())
}

struct GraphSeedIds {
    fact_handle: Uuid,
    fact_t: Uuid,
    support_fact_handle: Uuid,
    support_fact_t: Uuid,
    old_abstraction_handle: Uuid,
    old_abstraction_t: Uuid,
}

async fn seed_graph_memory(
    platform: &PgPool,
    owner: Uuid,
    ids: &GraphSeedIds,
) -> Result<Uuid, sqlx::Error> {
    let content_id = Uuid::now_v7();
    let mut seed = platform.begin().await?;
    sqlx::query("SELECT set_config('app.proxiima_scope', 'platform', true)")
        .execute(&mut *seed)
        .await?;
    for (handle, t) in [
        (ids.fact_handle, ids.fact_t),
        (ids.support_fact_handle, ids.support_fact_t),
    ] {
        sqlx::query(
            "INSERT INTO proxiima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', $2, $3, $4)",
        )
        .bind(handle)
        .bind(SOURCE_SCHEMA)
        .bind(owner)
        .bind(t)
        .execute(&mut *seed)
        .await?;
        sqlx::query(
            "INSERT INTO proxiima_core.memory
             (handle, t, kind, owner_id, schema_id, occurred_at, \"by\")
             VALUES ($1, $2, 'fact', $3, $4, now(), $3)",
        )
        .bind(handle)
        .bind(t)
        .bind(owner)
        .bind(SOURCE_SCHEMA)
        .execute(&mut *seed)
        .await?;
    }
    sqlx::query(
        "INSERT INTO proxiima_core.content (content_id, owner_id, schema_id, content_hash)
         VALUES ($1, $2, $3, decode(repeat('03', 32), 'hex'))",
    )
    .bind(content_id)
    .bind(owner)
    .bind(ABSTRACTION_SOURCE_SCHEMA)
    .execute(&mut *seed)
    .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'abstraction', $2, $3, $4)",
    )
    .bind(ids.old_abstraction_handle)
    .bind(ABSTRACTION_SOURCE_SCHEMA)
    .bind(owner)
    .bind(ids.old_abstraction_t)
    .execute(&mut *seed)
    .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.memory
         (handle, t, kind, owner_id, schema_id, origins, content_id, \"by\")
         VALUES ($1, $2, 'abstraction', $3, $4, $5, $6, $3)",
    )
    .bind(ids.old_abstraction_handle)
    .bind(ids.old_abstraction_t)
    .bind(owner)
    .bind(ABSTRACTION_SOURCE_SCHEMA)
    .bind(vec![ids.fact_t])
    .bind(content_id)
    .execute(&mut *seed)
    .await?;
    seed.commit().await?;
    Ok(content_id)
}

#[tokio::test]
async fn graph_accepts_migrated_fact_goal_reference() {
    let database = format!("proxiima_test_{}", Uuid::now_v7().simple());
    create_core_db(&database)
        .await
        .expect("PG required for test");
    let result = async {
        let (_, platform_url) = split_role_urls(&database).await?;
        let platform = PgPool::connect(&platform_url).await?;
        let owner = owner_id(&platform).await?;
        let (goal_t, fact_handle, fact_t) = seed_goal_and_fact(&platform, owner).await?;
        let mut tx = migrate_fact(&platform, owner, fact_handle, fact_t, vec![goal_t]).await?;
        graph::validate(&mut tx)
            .await
            .expect("a live Goal is valid for memory.goal_refs");
        tx.rollback().await?;
        Ok::<(), sqlx::Error>(())
    }
    .await;
    drop_db(&database).await.expect("drop test database");
    result.expect("live Goal graph validation");
}

#[tokio::test]
async fn graph_rejects_migrated_fact_goal_reference_to_fact() {
    let database = format!("proxiima_test_{}", Uuid::now_v7().simple());
    create_core_db(&database)
        .await
        .expect("PG required for test");
    let result = async {
        let (_, platform_url) = split_role_urls(&database).await?;
        let platform = PgPool::connect(&platform_url).await?;
        let owner = owner_id(&platform).await?;
        let (_goal_t, fact_handle, fact_t) = seed_goal_and_fact(&platform, owner).await?;
        let mut tx = migrate_fact(&platform, owner, fact_handle, fact_t, vec![fact_t]).await?;
        let error = graph::validate(&mut tx)
            .await
            .expect_err("goal_refs must not accept a Fact target");
        assert!(error.to_string().contains("provenance"), "{error}");
        tx.rollback().await?;
        Ok::<(), sqlx::Error>(())
    }
    .await;
    drop_db(&database).await.expect("drop test database");
    result.expect("wrong-role graph validation");
}
