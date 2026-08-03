//! Boot a fresh transient DB, apply migrations, assert
//! tables exist, drop the DB. Requires admin access to a
//! local PG cluster (<postgres://postgres@localhost>).

use crate::common::{create_db, db_url, drop_db};
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

async fn column_exists(pg: &PgStorage, column_name: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM information_schema.columns
              WHERE table_schema IN ('proxima_core', 'proxima_code')
                AND column_name = $1
         )",
    )
    .bind(column_name)
    .fetch_one(pg.pool_for_tests())
    .await
    .expect("column inventory query should succeed")
}

async fn enum_type_exists(pg: &PgStorage, type_name: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_type t
               JOIN pg_namespace n ON n.oid = t.typnamespace
              WHERE n.nspname = 'proxima_core'
                AND t.typname = $1
         )",
    )
    .bind(type_name)
    .fetch_one(pg.pool_for_tests())
    .await
    .expect("enum inventory query should succeed")
}

async fn check_constraint_exists(pg: &PgStorage, table: &str, constraint: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM information_schema.table_constraints
              WHERE table_schema = 'proxima_core'
                AND table_name = $1
                AND constraint_name = $2
                AND constraint_type = 'CHECK'
         )",
    )
    .bind(table)
    .bind(constraint)
    .fetch_one(pg.pool_for_tests())
    .await
    .expect("constraint inventory query should succeed")
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

        let row: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM information_schema.tables \
             WHERE table_schema = 'proxima_core'",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(
            row.0 >= 7,
            "expected >=7 tables in proxima_core, got {}",
            row.0
        );

        // After the Owner = OwnerRef collapse, the legacy owner org column must be GONE
        // from every proxima_core table. This is the keystone gate for the
        // DDL-drop migration — a single missed column would silently keep org
        // in storage and pass the table-count check above.
        let org_cols: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM information_schema.columns \
             WHERE table_schema = 'proxima_core' AND column_name = ('owner_' || 'org_id')",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            org_cols.0, 0,
            "legacy owner org column must be absent from proxima_core after Owner=OwnerRef collapse; found {} column(s)",
            org_cols.0
        );

        for index_name in ["idx_agent_derivation_v1_search", "idx_agent_note_v1_search"] {
            assert!(
                !index_exists(&pg, index_name).await,
                "{index_name} must be dropped by v0.0.6; lexical search ranks candidates from a CTE"
            );
        }
        for index_name in [
            "idx_edges_owner",
            "idx_edges_source_memory",
            "idx_edges_source_goal",
            "idx_edges_source_fact_entity",
            "idx_edges_target_memory",
            "idx_edges_target_goal",
            "idx_edges_target_fact_entity",
            "idx_goals_owner_state",
        ] {
            assert!(
                !index_exists(&pg, index_name).await,
                "{index_name} must be dropped by v0.0.6 as prefix-redundant"
            );
        }
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("migrations integration test failed");
}

#[tokio::test]
async fn fresh_v004_baseline_has_no_legacy_access_or_goal_tables() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        for table_name in [
            format!("entity_{}", "owner"),
            format!("read_{}_matrix", "scope"),
            format!("goal_{}", "parents"),
            format!("{}_wake_entries", "personality"),
            ["master", "token", "personality"].join("_"),
            format!("subject_{}", "personality"),
            "events".to_string(),
        ] {
            assert!(
                !table_exists(&pg, &table_name).await,
                "proxima_core.{table_name} must not exist in the v0.0.4 baseline",
            );
        }

        for column_name in [
            format!("owner_{}", "principal_kind"),
            format!("owner_{}", "principal_id"),
            format!("owner_{}", "org_id"),
            format!("entity_{}_instance_id", "personality"),
            format!("{}_instance_id", "personality"),
            format!("wake_{}_depth", "chain"),
        ] {
            assert!(
                !column_exists(&pg, &column_name).await,
                "legacy owner/personality column {column_name} must not exist",
            );
        }

        for type_name in [
            format!("wake_{}_trigger_kind", "entry"),
            format!("wake_{}_goal_scope", "entry"),
        ] {
            assert!(
                !enum_type_exists(&pg, &type_name).await,
                "legacy enum type {type_name} must not exist",
            );
        }

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("v0.0.4 baseline inventory test failed");
}

#[tokio::test]
async fn fresh_v004_baseline_enforces_owner_ref_shape_constraints() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        // Owner-shadow tables: rows attribute a write/author action, so World
        // must never appear — both checks required.
        for table in [
            "change_event",
            "citation_mappings",
            "cited_object_uploads",
            "cited_objects",
            "edges",
            "embeddings",
            "embedding_jobs",
            "fact_entities",
            "fact_receipts",
            "owner_fact_retention",
            "source_batches",
        ] {
            assert!(
                check_constraint_exists(&pg, table, &format!("{table}_owner_ref_shape_chk")).await,
                "proxima_core.{table} must enforce nullable OwnerRef shape",
            );
            assert!(
                check_constraint_exists(&pg, table, &format!("{table}_world_not_write_owner_chk"))
                    .await,
                "proxima_core.{table} must reject world as a write owner",
            );
        }

        // Entity home-row tables: since v0.0.5 (0008_v005.sql), World OWNERSHIP
        // is representable — it is the persisted result of the publish-to-World
        // owner transfer. The shape check stays; the blanket world check must
        // stay GONE (reintroducing it would break Engine::publish_to_world).
        for table in ["goals", "memories"] {
            assert!(
                check_constraint_exists(&pg, table, &format!("{table}_owner_ref_shape_chk")).await,
                "proxima_core.{table} must enforce nullable OwnerRef shape",
            );
            assert!(
                !check_constraint_exists(&pg, table, &format!("{table}_world_not_write_owner_chk"))
                    .await,
                "proxima_core.{table} must permit World-owned rows (publish-to-World transfer); \
                 world_not_write_owner_chk must not be reintroduced",
            );
        }

        let err = sqlx::query(
            "INSERT INTO proxima_core.memories
                (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
                 operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
             VALUES ($1, 'personal', NULL, 'test/owner-shape', 1, 'Abstraction',
                     'bad owner', 'AtoA',
                     '00000000-0000-0000-0000-000000000401'::uuid,
                     '00000000-0000-0000-0000-000000000402'::uuid, NULL,
                     'test-model', 'v1')"
        )
        .bind(Uuid::now_v7())
        .execute(pg.pool_for_tests())
        .await
        .expect_err("personal owner with NULL owner_id must be rejected");
        assert!(err.to_string().contains("owner_ref_shape"));

        // World owner shape is still constrained on the home-row tables:
        // world with a non-NULL owner_id violates the shape check.
        let err = sqlx::query(
            "INSERT INTO proxima_core.goals
                (goal_id, owner_kind, owner_id, request_id, idempotency_key, state, supersedes,
                 authorship_kind, schema_id, schema_version, payload, title, text)
             VALUES ($1, 'world', $2, $3, $3, 'Active', NULL,
                     'User', 'core/simple-text-v1', 1, $4, 'bad', 'bad')",
        )
        .bind(Uuid::now_v7())
        .bind(Uuid::now_v7())
        .bind(format!("bad-world-shape:{}", Uuid::now_v7()))
        .bind(b"{}".as_slice())
        .execute(pg.pool_for_tests())
        .await
        .expect_err("world owner with non-NULL owner_id must be rejected");
        assert!(err.to_string().contains("owner_ref_shape"));

        // World write ATTRIBUTION stays impossible where it always was: the
        // owner-shadow tables. change_event is the canonical write record.
        let err = sqlx::query(
            "INSERT INTO proxima_core.change_event
                (seq, owner_kind, owner_id, kind, entity_kind, entity_memory_id,
                 entity_schema_id, entity_schema_version)
             VALUES ($1, 'world', NULL, 'EntityAppend', 'Fact', $2, 'test/owner-shape', 1)",
        )
        .bind(Uuid::now_v7())
        .bind(Uuid::now_v7())
        .execute(pg.pool_for_tests())
        .await
        .expect_err("world write attribution must be rejected on owner-shadow tables");
        let msg = err.to_string();
        assert!(
            msg.contains("world_not_write_owner"),
            "expected world_not_write_owner rejection, got: {msg}"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("owner-ref shape constraint test failed");
}

#[tokio::test]
async fn append_only_triggers_reject_content_mutation_but_allow_noop() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner_id = Uuid::now_v7();

        // A valid Abstraction memory row.
        let memory_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memories
                (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
                 operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
             VALUES ($1, 'personal', $2, 'test/append-only', 1, 'Abstraction', 'original',
                     'AtoA', $3, $4, NULL, 'test-model', 'v1')",
        )
        .bind(memory_id)
        .bind(owner_id)
        .bind(Uuid::now_v7())
        .bind(Uuid::now_v7())
        .execute(pool)
        .await?;

        // Mutating a frozen content column is rejected by the trigger…
        let err = sqlx::query("UPDATE proxima_core.memories SET text = 'tampered' WHERE memory_id = $1")
            .bind(memory_id)
            .execute(pool)
            .await
            .expect_err("memories.text must be append-only");
        assert!(err.to_string().contains("append-only"), "got: {err}");

        // …but a whitelisted column (publish-to-World owner transfer) still updates.
        sqlx::query(
            "UPDATE proxima_core.memories SET owner_kind = 'world', owner_id = NULL WHERE memory_id = $1",
        )
        .bind(memory_id)
        .execute(pool)
        .await
        .expect("owner transfer is an allowed memories update");

        // Generic provenance/sidecar trigger: a content-addressed no-op upsert
        // (re-set a column to its identical value, the RETURNING path used by
        // cited_objects) is allowed; a real change is rejected.
        let cited_object_id = Uuid::now_v7();
        let content_hash = vec![7u8; 32];
        sqlx::query(
            "INSERT INTO proxima_core.cited_objects
                (cited_object_id, schema_id, owner_kind, owner_id, content_hash)
             VALUES ($1, 'test/cited', 'personal', $2, $3)",
        )
        .bind(cited_object_id)
        .bind(owner_id)
        .bind(&content_hash)
        .execute(pool)
        .await?;
        sqlx::query(
            "UPDATE proxima_core.cited_objects SET schema_id = 'test/cited' WHERE cited_object_id = $1",
        )
        .bind(cited_object_id)
        .execute(pool)
        .await
        .expect("no-op update is permitted (content-addressed upsert RETURNING path)");
        let err = sqlx::query(
            "UPDATE proxima_core.cited_objects SET schema_id = 'test/changed' WHERE cited_object_id = $1",
        )
        .bind(cited_object_id)
        .execute(pool)
        .await
        .expect_err("cited_objects content change must be append-only");
        assert!(err.to_string().contains("append-only"), "got: {err}");

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("append-only trigger test failed");
}

#[tokio::test]
async fn pre_v004_database_fails_closed_before_checksum_migration() {
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
        sqlx::query(concat!(
            "CREATE TABLE proxima_core.entity_",
            "owner (entity_id uuid NOT NULL)"
        ))
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
            .expect_err("pre-v0.0.4 DB must fail closed");
        let msg = err.to_string();
        assert!(
            msg.contains("v0.0.4") && msg.contains("reset"),
            "error must explain v0.0.4 reset requirement, got: {msg}",
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("pre-v0.0.4 fail-closed test failed");
}

/// A dev database that applied the pre-squash v0.0.7 lane (versions 12..15)
/// fails closed with the remedy in the message.
///
/// Without the preflight it still fails — version 11's checksum changed when
/// the five files were folded into it — but with `SQLx`'s bare "previously
/// applied but has been modified", which names neither the cause nor the
/// reset. The fixture is a real migrated database plus the four orphaned
/// ledger rows, because that is exactly the state such a database is in.
#[tokio::test]
async fn pre_squash_v007_lane_database_fails_closed_with_the_reset_remedy() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        sqlx::query(
            "INSERT INTO public._sqlx_migrations
                 (version, description, success, checksum, execution_time)
             SELECT v, 'v007 lane', true, decode('00', 'hex'), 0
               FROM unnest(ARRAY[12, 13, 14, 15]::bigint[]) AS v",
        )
        .execute(pg.pool_for_tests())
        .await?;

        let err = pg
            .run_migrations()
            .await
            .expect_err("a pre-squash v0.0.7 lane database must fail closed");
        let msg = err.to_string();
        assert!(
            msg.contains("0011_v007.sql") && msg.contains("--reset"),
            "error must name the squashed file and the reset remedy, got: {msg}",
        );
        assert!(
            msg.contains("[12, 13, 14, 15]"),
            "error must name the orphaned versions it found, got: {msg}",
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("pre-squash v0.0.7 lane fail-closed test failed");
}

#[tokio::test]
async fn pre_v004_database_with_only_old_version_one_checksum_fails_closed() {
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
             VALUES (1, 'init', true, decode('00', 'hex'), 0)",
        )
        .execute(pg.pool_for_tests())
        .await?;

        let err = pg
            .run_migrations()
            .await
            .expect_err("old version-1 checksum must fail closed");
        let msg = err.to_string();
        assert!(
            msg.contains("v0.0.4") && msg.contains("reset"),
            "error must explain v0.0.4 reset requirement, got: {msg}",
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("pre-v0.0.4 checksum-only fail-closed test failed");
}
