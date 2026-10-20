//! Setup errors that name the test server's connection budget.
//!
//! A test binary that does not compose through the example-engine fixture
//! wraps its connection, migration, store-number and boot steps with these,
//! so a setup step that ran out of connections reports the server's
//! `max_connections` and its current backends instead of only a pool
//! timeout.

use std::fmt::Display;

use proxima_pg_testkit::describe_connection_error;

/// `result`'s value, or its error as `context: error`. When the error reads
/// as the server or a pool out of connections, the text also carries the
/// test server's connection use ([`describe_connection_error`]).
///
/// # Errors
///
/// The described error.
pub async fn described<T, E: Display>(context: &str, result: Result<T, E>) -> Result<T, String> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => Err(describe_connection_error(context, &error).await),
    }
}

/// [`described`] for setup that cannot go on without its value.
///
/// # Panics
///
/// With the described error.
pub async fn expect_described<T, E: Display>(context: &str, result: Result<T, E>) -> T {
    described(context, result)
        .await
        .unwrap_or_else(|why| panic!("{why}"))
}
