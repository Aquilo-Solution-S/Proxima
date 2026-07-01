use sqlx::{Postgres, QueryBuilder};

async fn unsafe_query(pool: &sqlx::PgPool, user_input: &str) -> sqlx::Result<()> {
    let sql = format!(r#"SELECT * FROM proxima_core.memories WHERE schema_key = '{user_input}'"#);
    sqlx::query(sql.as_str()).execute(pool).await?;

    let mut builder = QueryBuilder::<Postgres>::new("SELECT * FROM proxima_core.memories WHERE ");
    builder.push(user_input);
    builder.build().execute(pool).await?;
    Ok(())
}
