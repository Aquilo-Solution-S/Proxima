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
#[allow(clippy::too_many_lines)]
async fn migrations_apply_to_fresh_db() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        for table in [
            "owners",
            "memory",
            "memory_head",
            "ingest_keys",
            "announce",
            "owner_legal_holds",
            "compliance_audit_log",
            "delegated_authority_grants",
            "source_cursors",
        ] {
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
            "compliance_suppression_keys",
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
            !column_exists(&pg, "embeddings", "owner_kind").await,
            "embeddings carry owner_id only"
        );
        assert!(
            !column_exists(&pg, "embeddings", "entity_kind").await,
            "embeddings carry entity_id only"
        );
        assert!(
            column_exists(&pg, "memory", "schema_id").await,
            "schema_id is on each memory row (same value as the handle)"
        );
        assert!(
            column_exists(&pg, "memory", "sidecar_tables").await,
            "W4: forget dumps the per-t stamp, not the registry"
        );
        assert!(
            column_exists(&pg, "agent_note_v1", "embed_text").await,
            "W6: drain reads stored sidecar embed_text"
        );
        assert!(
            !column_exists(&pg, "memory", "schema_version").await,
            "no schema_version"
        );
        assert!(
            column_exists(&pg, "memory", "owner_id").await,
            "memory.owner_id is required"
        );
        assert!(
            !column_exists(&pg, "group_memberships", "created_at").await,
            "group_memberships has no created_at; list_group_members must not ORDER BY it"
        );

        for index in [
            "memory_owner_handle_t_idx",
            "memory_owner_t_handle_idx",
            "memory_owner_schema_t_idx",
            "memory_blob_id_idx",
            "memory_origins_gin",
            "memory_refs_gin",
            "memory_head_owner_schema_idx",
            "memory_head_owner_kind_idx",
            "group_memberships_member_user_id_idx",
            "embedding_jobs_pending_claim_idx",
            "owners_kind_idx",
            "announce_owner_seq_idx",
            "idx_embeddings_vec_hnsw",
            "agent_note_v1_search_tsv_gin",
            "utterance_v1_search_tsv_gin",
            "agent_derivation_v1_search_tsv_gin",
            "interpretation_v1_search_tsv_gin",
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
#[allow(clippy::too_many_lines)]
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
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, origins, refs)
             VALUES ($1, $2, 'fact', $3, 'core/test-v1', '{}', '{}')",
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
            "UPDATE proxima_core.memory_head SET schema_id = 'other' WHERE handle = $1",
        )
        .bind(handle)
        .execute(pool)
        .await
        .expect_err("memory_head schema_id is frozen");
        assert!(err.to_string().contains("frozen"), "got: {err}");

        let world = Uuid::from_u128(1);
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE proxima_core.memory_head SET owner_id = $2 WHERE handle = $1")
            .bind(handle)
            .bind(world)
            .execute(&mut *tx)
            .await
            .expect("memory_head.owner_id may move for publish");
        sqlx::query("UPDATE proxima_core.memory SET owner_id = $2 WHERE t = $1")
            .bind(t)
            .bind(world)
            .execute(&mut *tx)
            .await
            .expect("memory.owner_id may move for publish");
        tx.commit().await?;
        let moved: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(moved, world);

        let err = sqlx::query(
            "INSERT INTO proxima_core.memory (handle, kind, owner_id, schema_id)
             VALUES ($1, 'abstraction', $2, 'core/test-v1')",
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
