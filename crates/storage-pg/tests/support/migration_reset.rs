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

/// Apply the reset baseline and its definitions before the store-number cutover.
/// The pool must connect as the migration role to an empty database.
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
    let mut staged = core_migrator();
    staged.migrations = Cow::Owned(
        staged
            .iter()
            .filter(|migration| migration.version <= 33)
            .cloned()
            .collect(),
    );
    assert_eq!(
        staged
            .iter()
            .map(|migration| migration.version)
            .collect::<Vec<_>>(),
        [33],
        "the fixture applies the reset baseline before store identities"
    );
    let mut connection = platform.acquire().await?;
    let mut transaction = begin_migration_transaction(&mut connection).await?;
    staged.run(transaction.as_mut()).await?;
    sqlx::query("SET LOCAL check_function_bodies = off")
        .execute(transaction.as_mut())
        .await?;
    for sql in [
        definitions::FUNCTIONS,
        pre_identity_definitions(definitions::TRIGGERS),
        pre_identity_definitions(definitions::POLICIES),
    ] {
        // SQL-POLICY: fixed-fragment — embedded definitions, with an asserted final section boundary.
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(transaction.as_mut())
            .await?;
    }
    transaction.commit().await?;
    Ok(())
}

fn pre_identity_definitions(sql: &'static str) -> &'static str {
    sql.split_once("\n-- store_identity\n")
        .expect("the store-identity section must follow all baseline definitions")
        .0
}
