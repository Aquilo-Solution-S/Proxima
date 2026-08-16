//! Fresh v0.0.8 CREATE set. Requires local PG.

use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

async fn table_exists(pg: &PgStorage, table_name: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM information_schema.tables
              WHERE table_schema = 'proxima_core'
                AND table_name = $1
         )",
    )
    .bind(table_name)
    .fetch_one(pg.pool_for_tests())
    .await
    .expect("table inventory query should succeed")
}

async fn column_exists(pg: &PgStorage, table: &str, column: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM information_schema.columns
              WHERE table_schema = 'proxima_core'
                AND table_name = $1
                AND column_name = $2
         )",
    )
    .bind(table)
    .bind(column)
    .fetch_one(pg.pool_for_tests())
    .await
    .expect("column inventory query should succeed")
}

async fn index_exists(pg: &PgStorage, index_name: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_indexes
              WHERE schemaname = 'proxima_core'
                AND indexname = $1
         )",
    )
    .bind(index_name)
    .fetch_one(pg.pool_for_tests())
    .await
    .expect("index inventory query should succeed")
}

#[tokio::test]
async fn migrations_apply_to_fresh_db() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        for table in ["owners", "memory", "memory_head", "ingest_keys", "announce"] {
            assert!(
                table_exists(&pg, table).await,
                "empty apply must create proxima_core.{table}"
            );
        }
        for dead in [
            "edges",
            "fact_entities",
            "fact_receipts",
            "memories",
            "goals",
            "change_event",
        ] {
            assert!(
                !table_exists(&pg, dead).await,
                "v0.0.8 must not create dead table {dead}"
            );
        }

        assert!(
            !column_exists(&pg, "memory", "owner_kind").await,
            "owner_kind must not live on memory"
        );
        assert!(
            !column_exists(&pg, "memory", "schema_id").await,
            "schema_id lives on memory_head only"
        );
        assert!(
            !column_exists(&pg, "memory", "schema_version").await,
            "no schema_version"
        );
        assert!(
            column_exists(&pg, "memory", "owner_id").await,
            "memory.owner_id is required"
        );

        for index in [
            "memory_owner_handle_t_idx",
            "memory_owner_t_handle_idx",
            "memory_origins_gin",
            "memory_refs_gin",
            "memory_head_owner_schema_idx",
            "memory_head_owner_kind_idx",
            "owners_kind_idx",
            "announce_owner_seq_idx",
        ] {
            assert!(
                index_exists(&pg, index).await,
                "missing UML §7 index {index}"
            );
        }

        let core_versions: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM public._sqlx_migrations
              WHERE success AND version <= 9999",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(core_versions, 1, "v0.0.8 is one core migration");

        let world: (Uuid, String) = sqlx::query_as(
            "SELECT owner_id, kind::text FROM proxima_core.owners
              WHERE owner_id = '00000000-0000-0000-0000-000000000001'::uuid",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(world.0, Uuid::from_u128(1));
        assert_eq!(world.1, "world");

        let owner_null: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM information_schema.columns
              WHERE table_schema = 'proxima_core'
                AND table_name IN ('memory', 'memory_head', 'ingest_keys', 'announce', 'owners')
                AND column_name = 'owner_id'
                AND is_nullable = 'YES'",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(owner_null.0, 0, "owner_id must be NOT NULL");

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("migrations integration test failed");
}

#[tokio::test]
async fn memory_is_append_only_and_head_t_only() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = Uuid::now_v7();
        let handle = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner)
            .execute(pool)
            .await?;
        let t = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/test-v1', $2, $3)",
        )
        .bind(handle)
        .bind(owner)
        .bind(t)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, origins, refs)
             VALUES ($1, $2, 'fact', $3, '{}', '{}')",
        )
        .bind(handle)
        .bind(t)
        .bind(owner)
        .execute(pool)
        .await?;

        let err = sqlx::query("UPDATE proxima_core.memory SET kind = 'abstraction' WHERE t = $1")
            .bind(t)
            .execute(pool)
            .await
            .expect_err("memory is append-only");
        assert!(err.to_string().contains("append-only"), "got: {err}");

        let later = Uuid::now_v7();
        sqlx::query("UPDATE proxima_core.memory_head SET t = $2 WHERE handle = $1")
            .bind(handle)
            .bind(later)
            .execute(pool)
            .await
            .expect("memory_head.t may move");

        let err = sqlx::query(
            "UPDATE proxima_core.memory_head SET kind = 'abstraction' WHERE handle = $1",
        )
        .bind(handle)
        .execute(pool)
        .await
        .expect_err("memory_head kind is frozen");
        assert!(err.to_string().contains("frozen"), "got: {err}");

        let err = sqlx::query(
            "INSERT INTO proxima_core.memory (handle, kind, owner_id)
             VALUES ($1, 'abstraction', $2)",
        )
        .bind(handle)
        .bind(owner)
        .execute(pool)
        .await
        .expect_err("kind must match head");
        assert!(
            err.to_string().contains("kind/owner") || err.to_string().contains("23514"),
            "got: {err}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("append-only / head freeze test failed");
}

#[tokio::test]
async fn pre_v008_database_fails_closed() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;

        sqlx::query("CREATE SCHEMA proxima_core")
            .execute(pg.pool_for_tests())
            .await?;
        sqlx::query("CREATE TABLE proxima_core.edges (edge_id uuid NOT NULL)")
            .execute(pg.pool_for_tests())
            .await?;
        sqlx::query(
            "CREATE TABLE public._sqlx_migrations (
                 version bigint PRIMARY KEY,
                 description text NOT NULL,
                 installed_on timestamptz NOT NULL DEFAULT now(),
                 success boolean NOT NULL,
                 checksum bytea NOT NULL,
                 execution_time bigint NOT NULL
             )",
        )
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO public._sqlx_migrations
                 (version, description, success, checksum, execution_time)
             VALUES
                 (1, 'init', true, decode('00', 'hex'), 0),
                 (5, 'group ownership access', true, decode('00', 'hex'), 0)",
        )
        .execute(pg.pool_for_tests())
        .await?;

        let err = pg
            .run_migrations()
            .await
            .expect_err("pre-v0.0.8 DB must fail closed");
        let msg = err.to_string();
        assert!(
            msg.contains("reset") || msg.contains("stamp") || msg.contains("v0.0.4"),
            "error must explain reset, got: {msg}",
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("pre-v0.0.8 fail-closed test failed");
}
