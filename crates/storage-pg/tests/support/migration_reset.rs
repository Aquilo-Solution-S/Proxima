//! Shared test fixtures for the migration-reset adoption boundary.

use std::borrow::Cow;

use proxima_storage_pg::{begin_migration_transaction, core_migrator, definitions};
use sqlx::PgPool;

/// Decode the embedded historical ledger checksums.
///
/// # Panics
///
/// Panics if an entry contains a non-hexadecimal byte pair.
#[must_use]
pub fn pre_reset_pairs(entries: &[(i64, &str)]) -> Vec<(i64, Vec<u8>)> {
    entries
        .iter()
        .map(|(version, checksum)| (*version, decode_hex(checksum)))
        .collect()
}

/// Decode an embedded hexadecimal checksum.
///
/// # Panics
///
/// Panics if the checksum contains a non-hexadecimal byte pair.
#[must_use]
pub fn decode_hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).expect("hex"), 16).expect("hex"))
        .collect()
}

/// Build the pre-identity physical schema and owner policies for migration tests.
/// The pool must connect as the migration role to an empty database. Current
/// triggers require later attribution columns, so this fixture installs no
/// triggers; current guard behavior is tested against the complete schema.
///
/// # Errors
///
/// Returns connection, migration, or definitions failures.
///
/// # Panics
///
/// Panics if the embedded baseline or final store-identity section boundary changes.
pub async fn apply_pre_identity_schema(
    platform: &PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    apply_historical_schema(
        platform,
        &[33],
        pre_identity_definitions(definitions::POLICIES),
    )
    .await
}

/// Apply exactly the named historical migrations and their embedded owner policies.
/// Current functions are inert during setup, and future-column triggers are absent.
///
/// # Errors
///
/// Returns connection, migration, or policy installation failures.
///
/// # Panics
///
/// Panics when the requested migration history is absent from the embedded sources.
pub async fn apply_historical_schema(
    platform: &PgPool,
    versions: &[i64],
    policies: &'static str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut staged = core_migrator();
    staged.migrations = Cow::Owned(
        staged
            .iter()
            .filter(|migration| versions.contains(&migration.version))
            .cloned()
            .collect(),
    );
    assert_eq!(
        staged
            .iter()
            .map(|migration| migration.version)
            .collect::<Vec<_>>(),
        versions,
        "the fixture applies exactly its historical physical schema"
    );
    let mut connection = platform.acquire().await?;
    let mut transaction = begin_migration_transaction(&mut connection).await?;
    staged.run(transaction.as_mut()).await?;
    sqlx::query("SET LOCAL check_function_bodies = off")
        .execute(transaction.as_mut())
        .await?;
    let policies = if versions.contains(&FORGOTTEN_AT_MIGRATION) {
        policies
    } else {
        pre_forgotten_policies(policies)
    };
    for sql in [definitions::FUNCTIONS, policies] {
        // SQL-POLICY: fixed-fragment — embedded definitions, with an asserted final section boundary.
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(transaction.as_mut())
            .await?;
    }
    transaction.commit().await?;
    Ok(())
}

/// Migration 0039 adds `memory.forgotten_at`, which the memory owner policies
/// read (#488).
const FORGOTTEN_AT_MIGRATION: i64 = 39;
const FORGOTTEN_PREDICATE: &str = " AND ((forgotten_at IS NULL) OR (current_setting('app.proxima_lifecycle'::text, true) = 'on'::text))";

/// The owner policies as a store without `memory.forgotten_at` had them: the
/// current text minus the forgotten-memory predicate.
///
/// # Panics
///
/// Panics if the memory policies no longer carry the predicate.
fn pre_forgotten_policies(sql: &'static str) -> &'static str {
    if !sql.contains("ON proxima_core.memory USING") {
        return sql;
    }
    assert_eq!(
        sql.matches(FORGOTTEN_PREDICATE).count(),
        3,
        "the memory read, write and check expressions each carry the forgotten predicate"
    );
    Box::leak(sql.replace(FORGOTTEN_PREDICATE, "").into_boxed_str())
}

fn pre_identity_definitions(sql: &'static str) -> &'static str {
    sql.split_once("\n-- store_identity\n")
        .expect("the store-identity section must follow all baseline definitions")
        .0
}
