//! Current heads of a contract-declared stateful Fact sidecar.

use proxima_core::verbs::query::{
    ResolvedSidecarHeads, SidecarAtom, SidecarHead, SidecarHeadsPage,
};
use proxima_core::{MemoryId, OwnerId, StorageError};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Postgres, QueryBuilder, Row, TypeInfo, ValueRef};

use crate::error::map_err;
use crate::pg_ident::PgIdent;

use super::push_atom;

pub(crate) async fn sidecar_heads_by_key_on_connection(
    connection: &mut PgConnection,
    read_owners: &[OwnerId],
    query: &ResolvedSidecarHeads<'_>,
) -> Result<SidecarHeadsPage, StorageError> {
    let table = PgIdent::table(query.sidecar_table())?;
    let memory_key = PgIdent::column(query.memory_key_column())?;
    let columns = query
        .key_columns()
        .iter()
        .map(|column| PgIdent::column(column))
        .collect::<Result<Vec<_>, _>>()?;

    // SQL-POLICY: QueryBuilder-bound-values
    let mut sql = QueryBuilder::<Postgres>::new("SELECT m.t");
    for column in &columns {
        // SQL-POLICY: PgIdent — every column comes from the frozen contract.
        sql.push(", s.").push(column.as_str());
    }
    // SQL-POLICY: PgIdent — table and memory key come from the frozen contract.
    sql.push(" FROM ").push(table.as_str());
    sql.push(" s JOIN proxima_core.memory m /* FORGET-POLICY: live-only (head listing excludes forgotten admissions) */ ON m.t = s.");
    // SQL-POLICY: PgIdent
    sql.push(memory_key.as_str());
    sql.push(" JOIN proxima_core.memory_head h ON h.handle = m.handle AND h.t = m.t WHERE m.forgotten_at IS NULL AND m.owner_id = ANY(");
    sql.push_bind(
        read_owners
            .iter()
            .map(|owner| owner.into_inner())
            .collect::<Vec<_>>(),
    );
    sql.push("::uuid[]) AND m.schema_id = ");
    sql.push_bind(query.schema_id().as_str());
    sql.push(" AND m.kind = 'fact'");
    for (column, value) in columns.iter().zip(query.key_prefix()) {
        // SQL-POLICY: PgIdent — positional prefix values cannot name columns.
        sql.push(" AND s.").push(column.as_str());
        sql.push(" = ");
        push_atom(&mut sql, value);
    }
    if let Some(after) = query.after() {
        // SQL-POLICY: QueryBuilder-bound-values
        sql.push(" AND m.t < ").push_bind(after.into_inner());
    }
    // SQL-POLICY: QueryBuilder-bound-values
    sql.push(" ORDER BY m.t DESC LIMIT ");
    sql.push_bind(i64::from(query.limit()) + 1);
    let mut rows = sql
        .build()
        .fetch_all(connection)
        .await
        .map_err(prefix_query_error)?;
    let limit = usize::try_from(query.limit())
        .map_err(|error| StorageError::Internal(error.to_string()))?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let heads = rows
        .iter()
        .map(|row| head_from_row(row, query.key_columns()))
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = if has_more {
        heads.last().map(|head| head.memory_id)
    } else {
        None
    };
    Ok(SidecarHeadsPage { heads, next_cursor })
}

fn head_from_row(row: &PgRow, columns: &[&str]) -> Result<SidecarHead, StorageError> {
    let memory_id = MemoryId::new(row.try_get(0).map_err(map_err)?);
    let natural_key = columns
        .iter()
        .enumerate()
        .map(|(index, column)| Ok(((*column).to_owned(), atom_from_row(row, index + 1)?)))
        .collect::<Result<Vec<_>, StorageError>>()?;
    Ok(SidecarHead {
        memory_id,
        admitted_at: memory_id.created_at(),
        natural_key,
    })
}

/// Decode actual PG types, so UUID-looking text stays text. Null or a type
/// outside the scalar key vocabulary is a schema/data refusal, never a guess.
fn atom_from_row(row: &PgRow, index: usize) -> Result<SidecarAtom, StorageError> {
    let raw = row.try_get_raw(index).map_err(map_err)?;
    if raw.is_null() {
        return Err(StorageError::ConstraintViolation(
            "natural-key column is null".into(),
        ));
    }
    match raw.type_info().name() {
        "UUID" => row.try_get(index).map(SidecarAtom::Uuid),
        "TEXT" | "VARCHAR" | "BPCHAR" => row.try_get(index).map(SidecarAtom::Text),
        "INT2" => row
            .try_get::<i16, _>(index)
            .map(|value| SidecarAtom::I32(i32::from(value))),
        "INT4" => row.try_get(index).map(SidecarAtom::I32),
        "INT8" => row.try_get(index).map(SidecarAtom::I64),
        "BOOL" => row.try_get(index).map(SidecarAtom::Bool),
        other => {
            return Err(StorageError::ConstraintViolation(format!(
                "unsupported natural-key column type {other}"
            )));
        }
    }
    .map_err(map_err)
}

fn prefix_query_error(error: sqlx::Error) -> StorageError {
    match &error {
        sqlx::Error::Database(db)
            if matches!(
                db.code().as_deref(),
                Some("42883" | "42804" | "22P02" | "22P03")
            ) =>
        {
            StorageError::ConstraintViolation(
                "natural-key prefix does not match declared column types".into(),
            )
        }
        _ => map_err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn key_rows_preserve_scalar_types_and_refuse_null_or_unsupported_data() {
        let Some(url) = proxima_pg_testkit::admin_url_or_skip() else {
            return;
        };
        let pool = sqlx::PgPool::connect(&url)
            .await
            .expect("owned PG connection");
        let row = sqlx::query(
            "SELECT '00000000-0000-0000-0000-000000000001'::uuid,
                    '00000000-0000-0000-0000-000000000001'::text,
                    2::smallint, 3::integer, 4::bigint, true,
                    NULL::text, ARRAY[1]::integer[]",
        )
        .fetch_one(&pool)
        .await
        .expect("scalar key row");
        let expected = [
            SidecarAtom::Uuid(uuid::Uuid::from_u128(1)),
            SidecarAtom::Text("00000000-0000-0000-0000-000000000001".into()),
            SidecarAtom::I32(2),
            SidecarAtom::I32(3),
            SidecarAtom::I64(4),
            SidecarAtom::Bool(true),
        ];
        for (index, expected) in expected.iter().enumerate() {
            assert_eq!(&atom_from_row(&row, index).expect("scalar atom"), expected);
        }
        for index in [6, 7] {
            assert!(matches!(
                atom_from_row(&row, index),
                Err(StorageError::ConstraintViolation(_))
            ));
        }
    }
}
