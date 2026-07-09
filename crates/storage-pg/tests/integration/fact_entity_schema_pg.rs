use proxima_storage_pg::PgStorage;

async fn migrate() -> Result<(PgStorage, String), Box<dyn std::error::Error>> {
    let (pg, db_name) = crate::common::fresh_pg().await;
    pg.run_migrations().await?;
    Ok((pg, db_name))
}

async fn column_exists(
    pg: &PgStorage,
    table_name: &str,
    column_name: &str,
    data_type: &str,
) -> Result<(), sqlx::Error> {
    let found: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
             SELECT 1
               FROM information_schema.columns
              WHERE table_schema = 'proxima_core'
                AND table_name = $1
                AND column_name = $2
                AND data_type = $3
         )",
    )
    .bind(table_name)
    .bind(column_name)
    .bind(data_type)
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert!(
        found.0,
        "missing proxima_core.{table_name}.{column_name}::{data_type}"
    );
    Ok(())
}

async fn constraint_def(
    pg: &PgStorage,
    table_name: &str,
    constraint_name: &str,
) -> Result<String, sqlx::Error> {
    let row: (String,) = sqlx::query_as(
        "SELECT pg_get_constraintdef(c.oid)
           FROM pg_constraint c
           JOIN pg_class t ON t.oid = c.conrelid
           JOIN pg_namespace n ON n.oid = t.relnamespace
          WHERE n.nspname = 'proxima_core'
            AND t.relname = $1
            AND c.conname = $2",
    )
    .bind(table_name)
    .bind(constraint_name)
    .fetch_one(pg.pool_for_tests())
    .await?;
    Ok(row.0)
}

async fn index_def(pg: &PgStorage, index_name: &str) -> Result<String, sqlx::Error> {
    let row: (String,) = sqlx::query_as(
        "SELECT indexdef
           FROM pg_indexes
          WHERE schemaname = 'proxima_core'
            AND indexname = $1",
    )
    .bind(index_name)
    .fetch_one(pg.pool_for_tests())
    .await?;
    Ok(row.0)
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn fact_entity_schema_matches_task_1_contract() {
    let (pg, db_name) = migrate()
        .await
        .expect("fresh pg migration should be available");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        for (column_name, data_type) in [
            ("fact_entity_id", "uuid"),
            ("owner_kind", "USER-DEFINED"),
            ("owner_id", "uuid"),
            ("schema_id", "text"),
            ("schema_version", "integer"),
            ("natural_key", "ARRAY"),
            ("current_memory_id", "uuid"),
            ("current_created_at", "timestamp with time zone"),
            ("created_at", "timestamp with time zone"),
        ] {
            column_exists(&pg, "fact_entities", column_name, data_type).await?;
        }

        let pkey = constraint_def(&pg, "fact_entities", "fact_entities_pkey").await?;
        assert!(pkey.contains("PRIMARY KEY (fact_entity_id)"));
        let version_chk = constraint_def(
            &pg,
            "fact_entities",
            "fact_entities_schema_version_positive_chk",
        )
        .await?;
        assert!(version_chk.contains("schema_version > 0"));

        let identity = constraint_def(&pg, "fact_entities", "fact_entities_identity_uq").await?;
        assert!(identity.contains("UNIQUE"));
        for column in [
            "owner_kind",
            "owner_id",
            "schema_id",
            "schema_version",
            "natural_key",
        ] {
            assert!(identity.contains(column), "identity guard missing {column}");
        }

        let current_fk =
            constraint_def(&pg, "fact_entities", "fact_entities_current_memory_id_fkey").await?;
        assert!(current_fk.contains("REFERENCES proxima_core.memories(memory_id)"));
        assert!(current_fk.contains("ON DELETE RESTRICT"));

        column_exists(&pg, "memories", "fact_entity_id", "uuid").await?;
        let memories_chk = constraint_def(&pg, "memories", "memories_fact_entity_chk").await?;
        assert!(memories_chk.contains("fact_entity_id IS NULL"));
        assert!(memories_chk.contains("kind IS NULL"));
        assert!(!memories_chk.contains(&format!("{} IS NOT NULL", "receipt_id")));
        let memories_fk = constraint_def(&pg, "memories", "memories_fact_entity_id_fkey").await?;
        assert!(memories_fk.contains("REFERENCES proxima_core.fact_entities(fact_entity_id)"));
        assert!(memories_fk.contains("ON DELETE SET NULL"));
        assert!(index_def(&pg, "idx_memories_fact_entity")
            .await?
            .contains("WHERE (fact_entity_id IS NOT NULL)"));

        for column_name in ["source_fact_entity_id", "target_fact_entity_id"] {
            column_exists(&pg, "edges", column_name, "uuid").await?;
        }
        let source_chk = constraint_def(&pg, "edges", "edges_source_endpoint_chk").await?;
        assert!(source_chk.contains("num_nonnulls(source_memory_id, source_goal_id, source_fact_entity_id) = 1"));
        assert!(source_chk.contains("source_fact_entity_id IS NULL"));
        assert!(source_chk.contains("source_kind = 'Fact'"));
        let target_chk = constraint_def(&pg, "edges", "edges_target_endpoint_chk").await?;
        assert!(target_chk.contains("num_nonnulls(target_memory_id, target_goal_id, target_fact_entity_id) = 1"));
        assert!(target_chk.contains("target_fact_entity_id IS NULL"));
        assert!(target_chk.contains("target_kind = 'Fact'"));
        let source_fk = constraint_def(&pg, "edges", "edges_source_fact_entity_id_fkey").await?;
        assert!(source_fk.contains("REFERENCES proxima_core.fact_entities(fact_entity_id)"));
        assert!(source_fk.contains("ON DELETE RESTRICT"));
        // The bare `idx_edges_{source,target}_fact_entity` partial indexes were
        // dropped in 0010_v006 as prefix-redundant; the `_created` supersets
        // (fact_entity_id, created_at) WHERE (… IS NOT NULL) serve the same
        // endpoint lookups, so the contract pins those instead.
        for (index_name, index_column) in [
            ("idx_edges_source_fact_entity_created", "source_fact_entity_id"),
            ("idx_edges_target_fact_entity_created", "target_fact_entity_id"),
        ] {
            let index = index_def(&pg, index_name).await?;
            assert!(index.contains(index_column));
            assert!(index.contains(&format!("WHERE ({index_column} IS NOT NULL)")));
        }
        let target_fk_count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM pg_constraint c
               JOIN pg_class t ON t.oid = c.conrelid
               JOIN pg_namespace n ON n.oid = t.relnamespace
              WHERE n.nspname = 'proxima_core'
                AND t.relname = 'edges'
                AND c.conname IN (
                    'edges_target_memory_id_fkey',
                    'edges_target_goal_id_fkey',
                    'edges_target_fact_entity_id_fkey'
                )",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(target_fk_count, 0, "PR9 target endpoints are projection-relaxed");

        for column_name in ["edge_source_fact_entity_id", "edge_target_fact_entity_id"] {
            column_exists(&pg, "change_event", column_name, "uuid").await?;
        }
        let change_chk =
            constraint_def(&pg, "change_event", "change_event_endpoint_chk").await?;
        assert!(change_chk.contains("edge_source_fact_entity_id"));
        assert!(change_chk.contains("edge_target_fact_entity_id"));
        assert!(change_chk.contains("num_nonnulls(edge_source_memory_id, edge_source_goal_id, edge_source_fact_entity_id) = 1"));
        assert!(change_chk.contains("num_nonnulls(edge_target_memory_id, edge_target_goal_id, edge_target_fact_entity_id) = 1"));

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("fact-entity schema contract");
}

/// K6 (analysis 2026-07-05): the `memories` append-only trigger makes content,
/// identity, and provenance columns DB-hard immutable — an admin script cannot
/// silently rewrite a Fact — while leaving the legitimately-mutable columns
/// (here: `tombstoned_at`) writable.
#[tokio::test]
async fn memories_immutability_trigger_blocks_content_rewrite() {
    let (pg, db_name) = migrate().await.expect("migrate");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let memory_id = uuid::Uuid::now_v7();
        let owner_id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memories
                 (memory_id, owner_kind, owner_id, schema_id, schema_version, text)
             VALUES ($1, 'personal', $2, 'test/fact', 1, 'original content')",
        )
        .bind(memory_id)
        .bind(owner_id)
        .execute(pg.pool_for_tests())
        .await?;

        // Immutable content column: the trigger rejects the rewrite.
        let err = sqlx::query("UPDATE proxima_core.memories SET text = 'rewritten' WHERE memory_id = $1")
            .bind(memory_id)
            .execute(pg.pool_for_tests())
            .await
            .expect_err("rewriting Fact text must be rejected by the append-only trigger");
        assert!(
            err.to_string().contains("append-only"),
            "expected append-only rejection, got: {err}"
        );

        // Mutable column: tombstoning succeeds.
        sqlx::query("UPDATE proxima_core.memories SET tombstoned_at = now() WHERE memory_id = $1")
            .bind(memory_id)
            .execute(pg.pool_for_tests())
            .await?;

        // The blocked rewrite left the content untouched.
        let text: String =
            sqlx::query_scalar("SELECT text FROM proxima_core.memories WHERE memory_id = $1")
                .bind(memory_id)
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(text, "original content");
        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("memories immutability trigger");
}
