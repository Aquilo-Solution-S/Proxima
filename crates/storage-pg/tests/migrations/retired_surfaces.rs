use std::borrow::Cow;

use proxima_pg_testkit::{create_db, drop_db, split_role_urls};
use proxima_storage_pg::core_migrator;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

async fn staged_database(name: &str) -> Result<(PgPool, String), Box<dyn std::error::Error>> {
    create_db(name).await?;
    let (_, platform_url) = split_role_urls(name).await?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&platform_url)
        .await?;
    let mut migrator = core_migrator();
    migrator.migrations = Cow::Owned(
        migrator
            .iter()
            .filter(|migration| migration.version <= 30)
            .cloned()
            .collect(),
    );
    migrator.run(&pool).await?;
    sqlx::query("SELECT set_config('app.proxima_scope', 'platform', false)")
        .execute(&pool)
        .await?;
    Ok((pool, platform_url))
}

async fn upgrade(pool: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
    core_migrator().run(pool).await?;
    Ok(())
}

#[allow(clippy::too_many_lines)] // one released call fixture and its retained controls
async fn seed_rows(pool: &PgPool) -> Result<(Uuid, Uuid, Uuid), sqlx::Error> {
    let owner = Uuid::now_v7();
    let ordinary_fact = Uuid::now_v7();
    let ordinary_handle = Uuid::now_v7();
    let call = Uuid::now_v7();
    let call_handle = Uuid::now_v7();
    let goal_handle = Uuid::now_v7();
    let goal_t = Uuid::now_v7();
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(owner)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'fact', 'core/mcp-call-logged-v1', $2, $3),
                ($4, 'fact', 'core/ordinary-fact-v1', $2, $5)",
    )
    .bind(call_handle)
    .bind(owner)
    .bind(call)
    .bind(ordinary_handle)
    .bind(ordinary_fact)
    .execute(pool)
    .await?;
    let blob = Uuid::now_v7();
    let content = Uuid::now_v7();
    sqlx::query("INSERT INTO proxima_core.blob (blob_id, owner_id, schema_id, content_hash) VALUES ($1, $2, 'core/mcp-call-io-v1', $3)")
        .bind(blob).bind(owner).bind(vec![7_u8; 32]).execute(pool).await?;
    sqlx::query("INSERT INTO proxima_core.content (content_id, owner_id, schema_id, content_hash) VALUES ($1, $2, 'core/mcp-call-logged-v1', $3)")
        .bind(content).bind(owner).bind(vec![8_u8; 32]).execute(pool).await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory
         (handle, t, kind, owner_id, schema_id, source_id, ingest_key, sidecar_tables, occurred_at, blob_id, content_id)
         VALUES ($1, $2, 'fact', $3, 'core/mcp-call-logged-v1', 'core/mcp-call', 'call-receipt', ARRAY['proxima_core.mcp_call_logged_v1'], now(), $6, $7),
                ($4, $5, 'fact', $3, 'core/ordinary-fact-v1', 'ordinary', 'ordinary', '{}', now(), NULL, NULL)",
    )
    .bind(call_handle).bind(call).bind(owner).bind(ordinary_handle).bind(ordinary_fact)
    .bind(blob).bind(content).execute(pool).await?;
    sqlx::query("INSERT INTO proxima_core.ingest_keys (owner_id, source_id, ingest_key, t) VALUES ($1, 'core/mcp-call', 'call-receipt', $2)")
        .bind(owner).bind(call).execute(pool).await?;
    sqlx::query(
        "INSERT INTO proxima_core.mcp_call_logged_v1
         (t, owner_id, tool_name, actor_oid, actor_upn, ok, latency_ms, io_byte_len, io_truncated, io_content_hash)
         VALUES ($1, $2, 'retired-tool', 'oid', 'upn', true, 1, 1, false, $3)",
    )
    .bind(call).bind(owner).bind(vec![0_u8; 32]).execute(pool).await?;
    sqlx::query(
        "INSERT INTO proxima_core.cooled
         (t, handle, owner_id, kind, object_key, source_id, ingest_key, origins, refs, goal_refs, blob_id, content_id)
         SELECT t, handle, owner_id, kind, 'cold/retired-call', source_id, ingest_key, origins, refs, goal_refs, blob_id, content_id
         FROM proxima_core.memory WHERE t = $1",
    )
    .bind(call).execute(pool).await?;
    sqlx::query(
        "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
         VALUES ($1, 'test/retained-goal', $2, $3)",
    )
    .bind(goal_handle)
    .bind(owner)
    .bind(goal_t)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.goal
         (handle, t, owner_id, title, state, request_id, text)
         VALUES ($1, $2, $3, 'retained goal', 'Active', 'retained-goal', 'retained text')",
    )
    .bind(goal_handle)
    .bind(goal_t)
    .bind(owner)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.publication_outbox
         (t, owner_id, schema_id, schema_version, event_type, event_id, envelope, envelope_digest)
         VALUES ($1, $2, 'test/ordinary', 1, 'fact', $3, $4, $5)",
    )
    .bind(Uuid::now_v7())
    .bind(owner)
    .bind(format!("event-{owner}"))
    .bind(vec![0_u8])
    .bind(vec![0_u8; 32])
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.publication_outbox
         (t, owner_id, schema_id, schema_version, event_type, event_id, envelope, envelope_digest)
         VALUES ($1, $2, 'core/mcp-call-logged-v1', 1, 'fact', $3, $4, $5)",
    )
    .bind(call)
    .bind(owner)
    .bind(format!("call-{call}"))
    .bind(vec![1_u8])
    .bind(vec![1_u8; 32])
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.publication_delivery (t, consumer) VALUES ($1, 'default')",
    )
    .bind(call)
    .execute(pool)
    .await?;
    sqlx::query("INSERT INTO proxima_core.publication_origin (t, original_owner_id, source_id) VALUES ($1, $2, 'core/mcp-call')")
        .bind(call).bind(owner).execute(pool).await?;
    Ok((owner, ordinary_fact, call))
}

#[tokio::test]
async fn migration_0031_erases_call_records_and_retains_ordinary_rows() {
    let name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let (pool, _) = staged_database(&name).await?;
        let (_, ordinary, call) = seed_rows(&pool).await?;
        upgrade(&pool).await?;
        let ordinary_count: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1").bind(ordinary).fetch_one(&pool).await?;
        assert_eq!(ordinary_count, 1);
        let call_count: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1").bind(call).fetch_one(&pool).await?;
        assert_eq!(call_count, 0);
        let cold_count: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cold_purge_pending WHERE object_key = 'cold/retired-call'").fetch_one(&pool).await?;
        assert_eq!(cold_count, 1);
        assert!(!sqlx::query_scalar::<_, bool>("SELECT to_regclass('proxima_core.mcp_call_logged_v1') IS NOT NULL").fetch_one(&pool).await?);
        let leftovers: (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM proxima_core.cooled WHERE t = $1),
                    (SELECT count(*) FROM proxima_core.ingest_keys WHERE t = $1),
                    (SELECT count(*) FROM proxima_core.content WHERE schema_id = 'core/mcp-call-logged-v1'),
                    (SELECT count(*) FROM proxima_core.blob WHERE schema_id = 'core/mcp-call-io-v1'),
                    (SELECT count(*) FROM proxima_core.publication_origin WHERE t = $1),
                    (SELECT count(*) FROM proxima_core.publication_outbox WHERE t = $1),
                    (SELECT count(*) FROM proxima_core.publication_delivery WHERE t = $1)",
        ).bind(call).fetch_one(&pool).await?;
        assert_eq!(leftovers, (0, 0, 0, 0, 0, 0, 0));
        let goal: (String, String) = sqlx::query_as("SELECT title, text FROM proxima_core.goal")
            .fetch_one(&pool).await?;
        assert_eq!(goal, ("retained goal".into(), "retained text".into()));
        let envelopes: Vec<Vec<u8>> = sqlx::query_scalar("SELECT envelope FROM proxima_core.publication_outbox")
            .fetch_all(&pool).await?;
        assert_eq!(envelopes, vec![vec![0_u8]]);
        pool.close().await;
        Ok(())
    }.await;
    let _ = drop_db(&name).await;
    result.expect("retired call migration failed");
}

#[tokio::test]
async fn migration_0031_refuses_retained_retired_io_atomically() {
    let name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let (pool, _) = staged_database(&name).await?;
        let (owner, ordinary, _) = seed_rows(&pool).await?;
        let blob = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.blob (blob_id, owner_id, schema_id, content_hash) VALUES ($1, $2, 'core/mcp-call-io-v1', $3)")
            .bind(blob).bind(owner).bind(vec![1_u8; 32]).execute(&pool).await?;
        sqlx::query("ALTER TABLE proxima_core.memory DISABLE TRIGGER memory_append_only").execute(&pool).await?;
        sqlx::query("UPDATE proxima_core.memory SET blob_id = $1 WHERE t = $2").bind(blob).bind(ordinary).execute(&pool).await?;
        sqlx::query("ALTER TABLE proxima_core.memory ENABLE TRIGGER memory_append_only").execute(&pool).await?;
        let error = core_migrator().run(&pool).await.expect_err("retained retired I/O must refuse atomically");
        assert!(error.to_string().contains("retired call I/O"));
        let blob_count: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.blob WHERE blob_id = $1").bind(blob).fetch_one(&pool).await?;
        assert_eq!(blob_count, 1);
        assert!(sqlx::query_scalar::<_, bool>("SELECT to_regclass('proxima_core.wake_config') IS NOT NULL").fetch_one(&pool).await?);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proxima_core.cold_purge_pending").fetch_one(&pool).await?, 0);
        Ok(())
    }.await;
    let _ = drop_db(&name).await;
    result.expect("retired I/O refusal test failed");
}
