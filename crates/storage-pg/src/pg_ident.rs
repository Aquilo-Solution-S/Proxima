use proxima_core::StorageError;

/// Validated Postgres identifier used when dynamic SQL must splice a
/// build-time sidecar table or column name. Bind parameters cannot
/// stand in for identifiers, so callers must validate first.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PgIdent<'a>(&'a str);

impl<'a> PgIdent<'a> {
    pub(crate) fn table(value: &'a str) -> Result<Self, StorageError> {
        if is_qualified_table_ident(value) {
            Ok(Self(value))
        } else {
            Err(StorageError::Internal(format!(
                "invalid table identifier: {value:?}"
            )))
        }
    }

    pub(crate) fn column(value: &'a str) -> Result<Self, StorageError> {
        if is_ident_part(value) {
            Ok(Self(value))
        } else {
            Err(StorageError::Internal(format!(
                "invalid column identifier: {value:?}"
            )))
        }
    }

    pub(crate) fn as_str(self) -> &'a str {
        self.0
    }
}

fn is_qualified_table_ident(value: &str) -> bool {
    let mut parts = value.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    if !is_ident_part(first) {
        return false;
    }
    match (parts.next(), parts.next()) {
        (None, None) => true,
        (Some(second), None) => is_ident_part(second),
        _ => false,
    }
}

fn is_ident_part(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::PgIdent;

    #[test]
    fn table_accepts_schema_qualified_identifiers() {
        assert_eq!(
            PgIdent::table("proxima_code.commit_summary_v1")
                .unwrap()
                .as_str(),
            "proxima_code.commit_summary_v1"
        );
        assert_eq!(PgIdent::table("_local").unwrap().as_str(), "_local");
    }

    #[test]
    fn table_rejects_loose_or_malformed_identifiers() {
        for value in ["", "a..b", "1.2", "schema.", ".table", "a.b.c", "a-b"] {
            assert!(PgIdent::table(value).is_err(), "{value:?} should fail");
        }
    }

    #[test]
    fn column_rejects_qualified_identifiers() {
        assert!(PgIdent::column("repo_id").is_ok());
        assert!(PgIdent::column("sidecar.repo_id").is_err());
    }
}
