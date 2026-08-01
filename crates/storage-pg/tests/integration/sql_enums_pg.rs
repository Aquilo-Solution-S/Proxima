#[tokio::test]
async fn core_closed_vocab_columns_use_sql_enums() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = crate::common::fresh_pg().await;

    let result = async {
        pg.run_migrations().await?;

        for (table, column) in [
            ("memories", "kind"),
            ("memories", "operator_kind"),
            ("goals", "state"),
            ("goals", "authorship_kind"),
            ("goals", "authorship_origin"),
            ("change_event", "kind"),
            ("change_event", "entity_kind"),
            ("edges", "source_kind"),
            ("edges", "target_kind"),
            ("edges", "kind"),
            ("goal_wake_config", "trigger_kind"),
        ] {
            assert_enum_column(pg.pool_for_tests(), "proxima_core", table, column).await?;
        }

        let leftovers: i64 = sqlx::query_scalar(
            "SELECT count(*)
               FROM pg_constraint c
               JOIN pg_namespace n ON n.oid = c.connamespace
              WHERE n.nspname = 'proxima_core'
                AND c.contype = 'c'
                AND pg_get_constraintdef(c.oid) LIKE '% IN (%'
                AND c.conname NOT IN (
                    'memories_kind_values_chk',
                    'goals_authorship_shape_chk',
                    'change_event_shape_chk'
                )",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(leftovers, 0, "membership-only CHECK constraints remain");

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result
}

async fn assert_enum_column(
    pool: &sqlx::PgPool,
    schema: &str,
    table: &str,
    column: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let typtype: Option<String> = sqlx::query_scalar(
        "SELECT t.typtype::text
           FROM pg_attribute a
           JOIN pg_class c ON c.oid = a.attrelid
           JOIN pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_type t ON t.oid = a.atttypid
          WHERE n.nspname = $1
            AND c.relname = $2
            AND a.attname = $3
            AND NOT a.attisdropped",
    )
    .bind(schema)
    .bind(table)
    .bind(column)
    .fetch_optional(pool)
    .await?;

    assert_eq!(
        typtype.as_deref(),
        Some("e"),
        "expected {schema}.{table}.{column} to be a SQL enum"
    );
    Ok(())
}
