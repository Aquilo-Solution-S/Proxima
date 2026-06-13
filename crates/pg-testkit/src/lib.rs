use std::time::Duration;

use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

const DEFAULT_ADMIN_URL: &str = "postgres://proxima:proxima@localhost/proxima";
const DROP_RETRIES: usize = 25;
const DROP_RETRY_DELAY: Duration = Duration::from_millis(200);
const SQLSTATE_DATABASE_ACCESSED: &str = "55006";
const SQLSTATE_UNDEFINED_DATABASE: &str = "3D000";

#[must_use]
pub fn admin_url() -> String {
    std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| DEFAULT_ADMIN_URL.into())
}

#[must_use]
pub fn db_url(name: &str) -> String {
    let admin = admin_url();
    match admin.rfind('/') {
        Some(idx) => format!("{}/{}", &admin[..idx], name),
        None => format!("{admin}/{name}"),
    }
}

#[must_use]
pub fn unique_db_name(prefix: &str) -> String {
    format!("{}_{}", prefix, Uuid::now_v7().simple())
}

/// # Errors
///
/// Returns any database connection or `CREATE DATABASE` error.
pub async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(&admin_url()).await?;
    conn.execute(format!("CREATE DATABASE {}", quoted_ident(name)).as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

/// # Errors
///
/// Returns database connection errors, `ALLOW_CONNECTIONS` errors,
/// non-retryable `DROP DATABASE` errors, or connection close errors.
pub async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(&admin_url()).await?;
    let quoted_name = quoted_ident(name);

    match conn
        .execute(format!("ALTER DATABASE {quoted_name} WITH ALLOW_CONNECTIONS false").as_str())
        .await
    {
        Ok(_) => {}
        Err(err) if is_sqlstate(&err, SQLSTATE_UNDEFINED_DATABASE) => {
            conn.close().await?;
            return Ok(());
        }
        Err(err) => return Err(err),
    }

    for _ in 0..DROP_RETRIES {
        match conn
            .execute(format!("DROP DATABASE IF EXISTS {quoted_name}").as_str())
            .await
        {
            Ok(_) => {
                conn.close().await?;
                return Ok(());
            }
            Err(err) if is_drop_retryable(&err) => {
                tokio::time::sleep(DROP_RETRY_DELAY).await;
            }
            Err(err) => return Err(err),
        }
    }

    tracing::warn!(
        database = name,
        "test database still has active backends after teardown retries; leaving it for external cleanup"
    );
    conn.close().await?;
    Ok(())
}

fn quoted_ident(input: &str) -> String {
    format!("\"{}\"", input.replace('"', "\"\""))
}

fn is_drop_retryable(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db_err) => db_err
            .code()
            .is_some_and(|code| is_drop_retryable_sqlstate(&code)),
        _ => false,
    }
}

fn is_drop_retryable_sqlstate(sqlstate: &str) -> bool {
    sqlstate == SQLSTATE_DATABASE_ACCESSED
}

fn is_sqlstate(err: &sqlx::Error, expected: &str) -> bool {
    match err {
        sqlx::Error::Database(db_err) => db_err.code().is_some_and(|code| code == expected),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_drop_retryable_sqlstate, quoted_ident, unique_db_name};

    #[test]
    fn unique_db_name_uses_prefix_and_simple_uuidv7() {
        let name = unique_db_name("proxima_test");

        assert!(name.starts_with("proxima_test_"));
        assert_eq!(name.len(), "proxima_test_".len() + 32);
    }

    #[test]
    fn quoted_ident_escapes_embedded_quotes() {
        assert_eq!(quoted_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn retry_classifier_retries_database_accessed_only() {
        assert!(is_drop_retryable_sqlstate("55006"));
        assert!(!is_drop_retryable_sqlstate("42501"));
        assert!(!is_drop_retryable_sqlstate("42P04"));
    }
}
