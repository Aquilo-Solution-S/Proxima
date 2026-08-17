//! Owned current-head handle lookup by sidecar column values.
//!
//! Tesla-valve admit: sidecar row → `memory.t` → `memory_head` (`h.t = m.t`).
//! Owner-only. A World-transferred series is a miss for the prior owner.

use proxima_core::verbs::query::SidecarAtom;
use proxima_core::{Owner, SchemaId, StorageError};
use sqlx::{PgExecutor, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::error::map_err;
use crate::pg_ident::PgIdent;

/// Current owned series handle for `schema_id` whose sidecar matches `columns`.
///
/// # Errors
///
/// `ConstraintViolation` when a column identifier is invalid.
/// `Internal` on query failure.
pub async fn owned_head_handle<'e, E>(
    executor: E,
    owner: Owner,
    schema_id: &SchemaId,
    sidecar_table: &str,
    columns: &[(&str, SidecarAtom)],
) -> Result<Option<Uuid>, StorageError>
where
    E: PgExecutor<'e>,
{
    if columns.is_empty() {
        return Err(StorageError::ConstraintViolation(
            "owned series-handle lookup requires at least one sidecar column".into(),
        ));
    }
    let table = PgIdent::table(sidecar_table)?;
    let col_idents = columns
        .iter()
        .map(|(column, _)| PgIdent::column(column))
        .collect::<Result<Vec<_>, _>>()?;

    // SQL-POLICY: PgIdent
    // SQL-POLICY: QueryBuilder-bound-values
    let mut builder = QueryBuilder::<Postgres>::new("SELECT h.handle FROM ");
    builder.push(table.as_str());
    // SQL-POLICY: fixed-fragment
    builder.push(
        " s JOIN proxima_core.memory m ON m.t = s.t \
         JOIN proxima_core.memory_head h ON h.handle = m.handle AND h.t = m.t \
         WHERE m.owner_id = ",
    );
    builder.push_bind(owner.stored_owner_id());
    // SQL-POLICY: fixed-fragment
    builder.push(" AND m.schema_id = ");
    builder.push_bind(schema_id.as_str());
    for (ident, (_, value)) in col_idents.iter().zip(columns) {
        // SQL-POLICY: PgIdent
        builder.push(" AND s.");
        builder.push(ident.as_str());
        // SQL-POLICY: fixed-fragment
        builder.push(" = ");
        push_atom(&mut builder, value);
    }
    // SQL-POLICY: fixed-fragment
    builder.push(" LIMIT 1");

    builder
        .build_query_scalar()
        .fetch_optional(executor)
        .await
        .map_err(map_err)
}

// SQL-POLICY: QueryBuilder-bound-values
fn push_atom(builder: &mut QueryBuilder<Postgres>, value: &SidecarAtom) {
    match value {
        SidecarAtom::Uuid(id) => {
            builder.push_bind(*id);
        }
        SidecarAtom::Text(text) => {
            builder.push_bind(text.clone());
        }
        SidecarAtom::I32(n) => {
            builder.push_bind(*n);
        }
        SidecarAtom::I64(n) => {
            builder.push_bind(*n);
        }
        SidecarAtom::Bool(flag) => {
            builder.push_bind(*flag);
        }
    }
}

/// Map a typed sidecar payload onto declared NK / series-key columns.
///
/// # Errors
///
/// `ConstraintViolation` when the payload is not a JSON object, a declared
/// column is missing, or the JSON value is not a sidecar atom.
pub fn sidecar_atoms_from_payload<P: serde::Serialize>(
    payload: &P,
    columns: &[&str],
) -> Result<Vec<(String, SidecarAtom)>, StorageError> {
    SidecarAtom::bind_columns(payload, columns).map_err(StorageError::ConstraintViolation)
}

#[cfg(test)]
mod tests {
    use super::sidecar_atoms_from_payload;
    use proxima_core::verbs::query::SidecarAtom;
    use serde::Serialize;
    use uuid::Uuid;

    #[derive(Serialize)]
    struct Sample {
        repo_id: Uuid,
        file_path: String,
        chunk_index: u32,
    }

    #[test]
    fn payload_columns_become_atoms() {
        let repo = Uuid::nil();
        let payload = Sample {
            repo_id: repo,
            file_path: "src/lib.rs".into(),
            chunk_index: 3,
        };
        let atoms = sidecar_atoms_from_payload(&payload, &["repo_id", "file_path", "chunk_index"])
            .expect("atoms");
        assert_eq!(atoms[0].1, SidecarAtom::Uuid(repo));
        assert_eq!(atoms[1].1, SidecarAtom::Text("src/lib.rs".into()));
        assert_eq!(atoms[2].1, SidecarAtom::I32(3));
    }

    #[test]
    fn missing_column_is_constraint() {
        let payload = Sample {
            repo_id: Uuid::nil(),
            file_path: "a.rs".into(),
            chunk_index: 0,
        };
        let err = sidecar_atoms_from_payload(&payload, &["missing"]).unwrap_err();
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn uuid_string_is_uuid_atom() {
        let id = Uuid::nil();
        let value = serde_json::Value::String(id.to_string());
        assert_eq!(
            SidecarAtom::from_json("repo_id", &value).unwrap(),
            SidecarAtom::Uuid(id)
        );
    }
}
