use sqlx::{Postgres, QueryBuilder};

async fn unsafe_query(pool: &sqlx::PgPool, user_input: &str) -> sqlx::Result<()> {
    let sql = format!(r#"SELECT * FROM proxima_core.memories WHERE schema_key = '{user_input}'"#);
    sqlx::query(sql.as_str()).execute(pool).await?;

    let mut builder = QueryBuilder::<Postgres>::new("SELECT * FROM proxima_core.memories WHERE ");
    builder.push(user_input);
    builder.build().execute(pool).await?;
    Ok(())
}

// Regression: `proxima_core.` inside a raw string whose opening delimiter and
// offending text sit on different lines. A line-scoped scanner can miss this;
// the dynamic-argument check on `sqlx::query` still has to catch it.
async fn unsafe_multiline_raw_string(pool: &sqlx::PgPool, schema_key: &str) -> sqlx::Result<()> {
    let sql = format!(
        r#"
        SELECT *
          FROM proxima_core.memories
         WHERE schema_key = '{schema_key}'
        "#
    );
    sqlx::query(sql.as_str()).execute(pool).await?;
    Ok(())
}

// Regression: SQL text assembled from concatenated literal fragments rather
// than a single string literal.
async fn unsafe_concatenated_fragments(pool: &sqlx::PgPool, filter: &str) -> sqlx::Result<()> {
    let sql = "SELECT * FROM ".to_string()
        + "proxima_core."
        + "memories WHERE schema_key = '"
        + filter
        + "'";
    sqlx::query(sql.as_str()).execute(pool).await?;
    Ok(())
}

// Regression: `format!` result assigned to a variable, then passed to
// `sqlx::query` rather than inlined at the call site.
async fn unsafe_format_result(pool: &sqlx::PgPool, table_suffix: &str) -> sqlx::Result<()> {
    let sql = format!("SELECT * FROM proxima_core.{table_suffix}");
    sqlx::query(sql.as_str()).execute(pool).await?;
    Ok(())
}

// Regression: `proxima_core::` is Rust path syntax (a module/type path), not
// a schema-qualified SQL identifier. It must never be flagged.
fn safe_rust_path_reference() -> u32 {
    proxima_core::schema::MEMORIES_TABLE_VERSION
}
