mod common;

#[tokio::test]
async fn goal_closed_vocab_columns_use_sql_enums() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = common::migrated().await else {
        return Ok(());
    };

    let result = async {
        let typtype: Option<String> = sqlx::query_scalar(
            "SELECT t.typtype::text
               FROM pg_attribute a
               JOIN pg_class c ON c.oid = a.attrelid
               JOIN pg_namespace n ON n.oid = c.relnamespace
               JOIN pg_type t ON t.oid = a.atttypid
              WHERE n.nspname = 'proxima_goal'
                AND c.relname = 'task_goal_v1'
                AND a.attname = 'priority'
                AND NOT a.attisdropped",
        )
        .fetch_optional(pg.pool())
        .await?;
        assert_eq!(
            typtype.as_deref(),
            Some("e"),
            "expected proxima_goal.task_goal_v1.priority to be a SQL enum"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = common::drop_db(&db_name).await;
    result
}
