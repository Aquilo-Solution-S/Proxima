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
    .fetch_one(pg.pool())
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
    .fetch_one(pg.pool())
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
    .fetch_one(pg.pool())
    .await?;
    Ok(row.0)
}

#[tokio::test]
async fn fact_entity_schema_matches_task_1_contract() {
    let (pg, db_name) = migrate()
        .await
        .expect("fresh pg migration should be available");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        for (column_name, data_type) in [
            ("fact_entity_id", "uuid"),
            ("owner_principal_kind", "USER-DEFINED"),
            ("owner_principal_id", "uuid"),
            ("owner_org_id", "uuid"),
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
            "owner_principal_kind",
            "owner_principal_id",
            "owner_org_id",
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
        assert!(memories_chk.contains("event_id IS NOT NULL"));
        assert!(memories_chk.contains("kind IS NULL"));
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
        for (constraint_name, index_name, index_column) in [
            (
                "edges_source_fact_entity_id_fkey",
                "idx_edges_source_fact_entity",
                "source_fact_entity_id",
            ),
            (
                "edges_target_fact_entity_id_fkey",
                "idx_edges_target_fact_entity",
                "target_fact_entity_id",
            ),
        ] {
            let fk = constraint_def(&pg, "edges", constraint_name).await?;
            assert!(fk.contains("REFERENCES proxima_core.fact_entities(fact_entity_id)"));
            assert!(fk.contains("ON DELETE RESTRICT"));
            let index = index_def(&pg, index_name).await?;
            assert!(index.contains(index_column));
            assert!(index.contains(&format!("WHERE ({index_column} IS NOT NULL)")));
        }

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
