//! The mutable core functions, triggers and policies pass.

use proxima_core::{RELEASE_VERSION, StorageError};
use sha2::{Digest, Sha384};
use sqlx::PgConnection;

use crate::VectorStorage;
use crate::error::internal;

pub const FUNCTIONS: &str = include_str!("../definitions/functions.sql");
pub const TRIGGERS: &str = include_str!("../definitions/triggers.sql");
pub const POLICIES: &str = include_str!("../definitions/policies.sql");
/// The vector candidate functions: applied only to a store that keeps its
/// vectors in pgvector.
pub const VECTOR: &str = include_str!("../definitions/vector.sql");

/// The definitions a store of this kind holds, in application order.
fn files(vectors: VectorStorage) -> &'static [&'static str] {
    match vectors {
        VectorStorage::Local => &[FUNCTIONS, TRIGGERS, POLICIES, VECTOR],
        VectorStorage::HostIndex => &[FUNCTIONS, TRIGGERS, POLICIES],
    }
}

fn checksum(vectors: VectorStorage) -> Vec<u8> {
    let mut digest = Sha384::new();
    for (index, sql) in files(vectors).iter().enumerate() {
        if index > 0 {
            digest.update([0]);
        }
        digest.update(sql.as_bytes());
    }
    digest.finalize().to_vec()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Release(u64, u64, u64);

impl Release {
    fn parse(value: &str) -> Result<Self, StorageError> {
        let invalid = || StorageError::Internal(format!("invalid definitions release {value:?}"));
        let mut parts = value.split('.');
        let mut component = || {
            let part = parts.next().ok_or_else(invalid)?;
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid());
            }
            part.parse::<u64>().map_err(|_| invalid())
        };
        let release = Self(component()?, component()?, component()?);
        if parts.next().is_some() {
            return Err(invalid());
        }
        Ok(release)
    }
}

enum AppliedDefinitions {
    Current { checksum: String },
    Newer,
}

impl AppliedDefinitions {
    async fn read(
        connection: &mut PgConnection,
        policy: crate::MissingMigrationPolicy,
    ) -> Result<Option<Self>, StorageError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT to_regprocedure('proxima_core.definitions_applied()') IS NOT NULL",
        )
        .fetch_one(&mut *connection)
        .await
        .map_err(internal)?;
        if !exists {
            return Ok(None);
        }
        let record: Option<(String, String)> =
            sqlx::query_as("SELECT checksum, release FROM proxima_core.definitions_applied()")
                .fetch_optional(&mut *connection)
                .await
                .map_err(internal)?;
        let Some((checksum, release)) = record else {
            return Ok(None);
        };
        let recorded = Release::parse(&release)?;
        let binary = Release::parse(RELEASE_VERSION)?;
        if recorded > binary {
            if policy == crate::MissingMigrationPolicy::Reject {
                return Err(StorageError::Internal(format!(
                    "database definitions were applied by newer release {release}; this binary is {RELEASE_VERSION}"
                )));
            }
            return Ok(Some(AppliedDefinitions::Newer));
        }
        Ok(Some(Self::Current { checksum }))
    }
}

/// The SHA-384 checksum used by the definitions record and test templates.
#[doc(hidden)]
#[must_use]
pub fn checksum_bytes(vectors: VectorStorage) -> Vec<u8> {
    checksum(vectors)
}

/// The definitions checksum as the record function returns it.
#[doc(hidden)]
#[must_use]
pub fn checksum_hex(vectors: VectorStorage) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    checksum(vectors)
        .iter()
        .flat_map(|byte| {
            [
                char::from(HEX[usize::from(byte >> 4)]),
                char::from(HEX[usize::from(byte & 15)]),
            ]
        })
        .collect()
}

/// Replace the definitions in the caller's migration transaction when needed.
///
/// # Errors
///
/// Refuses a newer or malformed recorded release and propagates SQL failures.
pub(crate) async fn apply(
    connection: &mut PgConnection,
    policy: crate::MissingMigrationPolicy,
    vectors: VectorStorage,
) -> Result<(), StorageError> {
    if current_matches_with_policy(connection, policy, vectors).await? {
        return Ok(());
    }
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT format('%I.%I', n.nspname, c.relname)
           FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = 'proxima_core' AND c.relkind IN ('r', 'p')
          ORDER BY c.relname",
    )
    .fetch_all(&mut *connection)
    .await
    .map_err(internal)?;
    for table in tables {
        // SQL-POLICY: fixed-fragment — PostgreSQL format('%I') quotes catalog identifiers.
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "LOCK TABLE {table} IN SHARE ROW EXCLUSIVE MODE"
        )))
        .execute(&mut *connection)
        .await
        .map_err(internal)?;
    }
    sqlx::query("SET LOCAL check_function_bodies = off")
        .execute(&mut *connection)
        .await
        .map_err(internal)?;
    for sql in files(vectors).iter().copied() {
        // SQL-POLICY: fixed-fragment — the embedded definitions files.
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(&mut *connection)
            .await
            .map_err(internal)?;
    }
    let checksum = checksum_hex(vectors);
    // SQL-POLICY: fixed-fragment — generated hex checksum and compiled release constant.
    let record = format!(
        "CREATE OR REPLACE FUNCTION proxima_core.definitions_applied()
         RETURNS TABLE(checksum text, release text) LANGUAGE sql IMMUTABLE
         AS $$ SELECT '{checksum}'::text, '{RELEASE_VERSION}'::text $$"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(record))
        .execute(&mut *connection)
        .await
        .map_err(internal)?;
    Ok(())
}

/// Whether the recorded definitions checksum matches this binary.
///
/// # Errors
///
/// Refuses a newer or malformed recorded release and propagates catalog errors.
pub async fn current_matches(
    connection: &mut PgConnection,
    vectors: VectorStorage,
) -> Result<bool, StorageError> {
    current_matches_with_policy(connection, crate::MissingMigrationPolicy::Reject, vectors).await
}

/// Whether the recorded definitions are current under the missing-version policy.
///
/// A newer recorded release is current under [`crate::MissingMigrationPolicy::Allow`]
/// and is refused under [`crate::MissingMigrationPolicy::Reject`].
///
/// # Errors
///
/// Returns malformed-release, newer-release-under-`Reject`, or catalog errors.
#[doc(hidden)]
pub async fn current_matches_with_policy(
    connection: &mut PgConnection,
    policy: crate::MissingMigrationPolicy,
    vectors: VectorStorage,
) -> Result<bool, StorageError> {
    Ok(match AppliedDefinitions::read(connection, policy).await? {
        Some(AppliedDefinitions::Newer) => true,
        Some(AppliedDefinitions::Current { checksum }) => checksum == checksum_hex(vectors),
        None => false,
    })
}

#[cfg(test)]
mod tests {
    use super::Release;

    #[test]
    fn release_parser_refuses_malformed_database_values() {
        for invalid in [
            "",
            "0.0",
            "0.0.36.1",
            "0.0.+36",
            "v0.0.36",
            "0..36",
            "0.0.18446744073709551616",
        ] {
            assert!(Release::parse(invalid).is_err(), "{invalid:?}");
        }
        assert!(Release::parse("0.0.36").unwrap() > Release::parse("0.0.9").unwrap());
    }
}
