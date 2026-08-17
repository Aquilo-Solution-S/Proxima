//! Shared SQL fragments for `MemorySearchProjection` text columns.

use std::fmt::Write as _;

use proxima_core::verbs::schema::MemorySearchProjectionField;
use proxima_core::{SearchProjectionColumnKind, StorageError};

use crate::pg_ident::PgIdent;

pub(crate) fn projection_search_text(
    fields: &[MemorySearchProjectionField],
) -> Result<String, StorageError> {
    let mut expressions = Vec::with_capacity(fields.len());
    for field in fields {
        if matches!(field.kind, SearchProjectionColumnKind::MemoryText) {
            return Err(StorageError::Internal(
                "core sidecar search has no memory.text; declare sidecar columns".into(),
            ));
        }
        let column = PgIdent::column(&field.column)?;
        let expression = match field.kind {
            SearchProjectionColumnKind::Text => {
                format!("NULLIF(c.{}::text, '')", column.as_str())
            }
            SearchProjectionColumnKind::TextArray => {
                format!("NULLIF(array_to_string(c.{}, ' '), '')", column.as_str())
            }
            SearchProjectionColumnKind::MemoryText => unreachable!("handled above"),
        };
        expressions.push(expression);
    }
    if expressions.is_empty() {
        return Err(StorageError::Internal(
            "search projection has no text fields".into(),
        ));
    }
    let mut sql = String::from("NULLIF(concat_ws(' '");
    for expression in expressions {
        let _ = write!(sql, ", {expression}");
    }
    sql.push_str("), '')");
    Ok(sql)
}
