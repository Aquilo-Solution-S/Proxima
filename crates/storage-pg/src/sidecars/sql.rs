use std::fmt::Write as _;

use super::{PgIdent, StorageError};

/// Convert a byte slice into a fixed 32-byte array.
///
/// # Errors
///
/// Returns `StorageError::Internal` when the input is not exactly 32 bytes.
pub fn bytes32(bytes: &[u8], column: &str) -> Result<[u8; 32], StorageError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        StorageError::Internal(format!(
            "{column} must be exactly 32 bytes, got {}",
            bytes.len()
        ))
    })
}

/// Convert a signed SQL `integer` value into `u32`.
///
/// # Errors
///
/// Returns `StorageError::Internal` when the database value is negative.
pub fn int_to_u32(value: i32, column: &str) -> Result<u32, StorageError> {
    u32::try_from(value).map_err(|err| StorageError::Internal(format!("invalid {column}: {err}")))
}

/// Convert a signed SQL `bigint` value into `u64`.
///
/// # Errors
///
/// Returns `StorageError::Internal` when the database value is negative.
pub fn int_to_u64(value: i64, column: &str) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|err| StorageError::Internal(format!("invalid {column}: {err}")))
}

/// Build a memory sidecar insert statement.
///
/// # Errors
///
/// Returns `StorageError::Internal` when the table, key column, or payload
/// column identifiers are not valid Postgres identifiers.
pub fn memory_insert_sql(
    table: &str,
    key_column: &str,
    columns: &[(&str, Option<&str>)],
) -> Result<String, StorageError> {
    let table = PgIdent::table(table)?.as_str();
    let key_column = PgIdent::column(key_column)?.as_str();
    let columns = columns
        .iter()
        .map(|(column, cast)| Ok((PgIdent::column(column)?.as_str(), *cast)))
        .collect::<Result<Vec<_>, StorageError>>()?;
    let mut sql = String::new();
    write!(&mut sql, "INSERT INTO {table} ({key_column}")
        .expect("writing SQL into String cannot fail");
    for (column, _) in &columns {
        write!(&mut sql, ", {column}").expect("writing SQL into String cannot fail");
    }
    sql.push_str(") VALUES ($1");
    for (index, (_, cast)) in columns.iter().enumerate() {
        write!(&mut sql, ", ${}", index + 2).expect("writing SQL into String cannot fail");
        if let Some(pg_type) = cast {
            // The cast target is a (possibly schema-qualified) Postgres type
            // name supplied via `pg_sidecar_cast!`; validate it as an identifier
            // so a flavor author cannot splice arbitrary SQL through `$pg_type`.
            let pg_type = PgIdent::table(pg_type)?.as_str();
            write!(&mut sql, "::{pg_type}").expect("writing SQL into String cannot fail");
        }
    }
    sql.push(')');
    Ok(sql)
}

/// Build a batched memory sidecar select statement.
///
/// # Errors
///
/// Returns `StorageError::Internal` when the table or key column are not valid
/// Postgres identifiers. The `columns` entries are compile-time SELECT
/// projection expressions emitted by `pg_sidecar_select_col!` from
/// `$column:ident` tokens — bare `col` for most kinds, `col::text AS col` for
/// enum columns. Rust's identifier grammar makes them injection-safe, and
/// because the enum form is a projection expression (not a bare identifier) it
/// must NOT be routed through `PgIdent::column`.
pub fn memory_select_batch_sql(
    table: &str,
    key_column: &str,
    columns: &[&str],
) -> Result<String, StorageError> {
    let table = PgIdent::table(table)?.as_str();
    let key_column = PgIdent::column(key_column)?.as_str();
    let mut sql = String::new();
    write!(&mut sql, "SELECT {key_column}").expect("writing SQL into String cannot fail");
    for column in columns {
        write!(&mut sql, ", {column}").expect("writing SQL into String cannot fail");
    }
    write!(&mut sql, " FROM {table} WHERE {key_column} = ANY($1)")
        .expect("writing SQL into String cannot fail");
    Ok(sql)
}
