use std::future::Future;
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, Connection, PgConnection, PgPool};
use uuid::Uuid;

const DEFAULT_ADMIN_URL: &str = "postgres://proxima:proxima@localhost/proxima";
const DROP_RETRIES: usize = 25;
const DROP_RETRY_DELAY: Duration = Duration::from_millis(200);
const SQLSTATE_DATABASE_ACCESSED: &str = "55006";
const SQLSTATE_UNDEFINED_DATABASE: &str = "3D000";
pub const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
pub const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// The admin connection URL from `PROXIMA_TEST_PG_URL`, or the local default
/// when unconfigured.
///
/// Trims, and treats an empty or whitespace-only value as unset — the rule
/// `proxima_core::env_value` states for every configuration variable in the
/// workspace. Applied by hand rather than by calling it, because this is a
/// deliberately minimal test-support crate (sqlx/tokio/tracing/uuid) and one
/// env var does not justify a dependency on `proxima-core`.
///
/// Without the empty check, `PROXIMA_TEST_PG_URL=` handed an empty string
/// straight to the connector, and every test in the run failed against a
/// connection error that named nothing an operator had typed.
#[must_use]
pub fn admin_url() -> String {
    std::env::var("PROXIMA_TEST_PG_URL")
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| DEFAULT_ADMIN_URL.into())
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

#[must_use]
pub fn fnv1a64_extend(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[must_use]
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    fnv1a64_extend(FNV_OFFSET_BASIS, bytes)
}

/// # Errors
///
/// Returns any database connection or `CREATE DATABASE` error.
pub async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(&admin_url()).await?;
    sqlx::raw_sql(AssertSqlSafe(format!(
        "CREATE DATABASE {}",
        quoted_ident(name)
    )))
    .execute(&mut conn)
    .await?;
    conn.close().await?;
    Ok(())
}

/// Ensure a pre-migrated template database exists.
///
/// Serialized by a session-scoped advisory lock on the admin DB. `build`
/// runs only when `template` did not already exist, against a
/// max-one-connection pool to the newly created template database.
///
/// # Errors
///
/// Returns admin connection/query errors, template creation errors,
/// template connection errors, or errors returned by `build`.
pub async fn ensure_template<F, Fut>(template: &str, build: F) -> Result<(), sqlx::Error>
where
    F: FnOnce(PgPool) -> Fut,
    Fut: Future<Output = Result<(), sqlx::Error>>,
{
    let mut conn = PgConnection::connect(&admin_url()).await?;
    let lock_key = advisory_lock_key(template);

    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(lock_key)
        .execute(&mut conn)
        .await?;

    let result = async {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
        )
        .bind(template)
        .fetch_one(&mut conn)
        .await?;

        if exists {
            return Ok(());
        }

        sqlx::raw_sql(AssertSqlSafe(format!(
            "CREATE DATABASE {}",
            quoted_ident(template)
        )))
        .execute(&mut conn)
        .await?;

        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&db_url(template))
            .await?;
        let build_result = build(pool.clone()).await;
        pool.close().await;
        build_result
    }
    .await;

    let unlock_result = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(lock_key)
        .execute(&mut conn)
        .await;
    let close_result = conn.close().await;

    result?;
    unlock_result?;
    close_result?;

    Ok(())
}

/// Create a unique database cloned from `template`.
///
/// Retries the transient `55006` source-template-accessed error raised
/// when concurrent test processes clone the same template.
///
/// # Errors
///
/// Returns admin connection errors, non-retryable `CREATE DATABASE`
/// errors, or the last retryable error after retries are exhausted.
pub async fn create_db_from_template(prefix: &str, template: &str) -> Result<String, sqlx::Error> {
    let name = unique_db_name(prefix);
    let mut conn = PgConnection::connect(&admin_url()).await?;
    let statement = format!(
        "CREATE DATABASE {} TEMPLATE {}",
        quoted_ident(&name),
        quoted_ident(template)
    );
    let mut last_error = None;

    for _ in 0..DROP_RETRIES {
        match sqlx::raw_sql(AssertSqlSafe(statement.clone()))
            .execute(&mut conn)
            .await
        {
            Ok(_) => {
                conn.close().await?;
                return Ok(name);
            }
            Err(err) if is_sqlstate(&err, SQLSTATE_DATABASE_ACCESSED) => {
                last_error = Some(err);
                tokio::time::sleep(DROP_RETRY_DELAY).await;
            }
            Err(err) => return Err(err),
        }
    }

    let _ = conn.close().await;
    Err(last_error.unwrap_or_else(|| {
        sqlx::Error::Protocol(
            "create_db_from_template exhausted retries with no recorded error".into(),
        )
    }))
}

/// # Errors
///
/// Returns database connection errors, `ALLOW_CONNECTIONS` errors,
/// non-retryable `DROP DATABASE` errors, or connection close errors.
pub async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(&admin_url()).await?;
    let quoted_name = quoted_ident(name);

    match sqlx::raw_sql(AssertSqlSafe(format!(
        "ALTER DATABASE {quoted_name} WITH ALLOW_CONNECTIONS false"
    )))
    .execute(&mut conn)
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
        // Terminate any lingering backends on the target DB (e.g. a pooled
        // connection a test hasn't dropped yet) before
        // dropping. ALLOW_CONNECTIONS is already false, so a terminated
        // backend cannot reconnect. Without this, a no-FORCE DROP DATABASE
        // BLOCKS ~11s per attempt before erroring "is being accessed by other
        // users", so the retry loop turns teardown into minutes. Termination
        // is async, so the retry loop still covers the brief exit window.
        let _ = sqlx::query(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE datname = $1::name AND pid <> pg_backend_pid()",
        )
        .bind(name)
        .execute(&mut conn)
        .await;

        match sqlx::raw_sql(AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {quoted_name}"
        )))
        .execute(&mut conn)
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

fn advisory_lock_key(input: &str) -> i64 {
    let hash = fnv1a64(input.as_bytes());
    i64::from_be_bytes(hash.to_be_bytes())
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
    use super::{advisory_lock_key, is_drop_retryable_sqlstate, quoted_ident, unique_db_name};

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

    #[test]
    fn advisory_lock_key_is_deterministic() {
        assert_eq!(
            advisory_lock_key("proxima_tmpl_core_abc"),
            advisory_lock_key("proxima_tmpl_core_abc")
        );
    }
}
