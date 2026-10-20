//! Read a Fact receipt by ingest identity on the caller's transaction.

use proxima_core::{IngestReceipt, MemoryId, OwnerId, SchemaId, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;

/// Read the admission `(owner, source_id, ingest_key)` names.
///
/// Runs on `tx`, which the caller already holds — a shared lock taken in
/// that transaction covers this read, and this read share-locks the
/// admission until the caller commits or rolls back. A transfer or erase
/// of that admission waits. Nothing is written.
///
/// `tx` is a transaction [`crate::begin_owner_transaction`] opened: owner
/// row security applies, and a forgotten admission stays hidden, the same
/// way other owner reads hide it. The `owner` predicate is the caller's
/// owner either way. A key with no visible admission is `Ok(None)`.
///
/// This is not an ingest replay. Replaying admits or returns
/// [`proxima_core::FactIngestOutcome`].
///
/// # Errors
///
/// [`StorageError::ConstraintViolation`] when `source_id` or `ingest_key`
/// is empty or contains a NUL. Storage errors from the read.
pub async fn ingest_receipt(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    source_id: &str,
    ingest_key: &str,
) -> Result<Option<IngestReceipt>, StorageError> {
    require_ingest_identity(source_id, ingest_key)?;
    let row: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT m.t, m.handle, m.schema_id
           FROM proxima_core.ingest_keys k
           JOIN proxima_core.memory m
             ON m.t = k.t AND m.owner_id = k.owner_id
          WHERE k.owner_id = $1
            AND k.source_id = $2
            AND k.ingest_key = $3
            AND m.source_id = $2
            AND m.ingest_key = $3
            FOR SHARE OF m",
    )
    .bind(owner.into_inner())
    .bind(source_id)
    .bind(ingest_key)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(row.map(|(memory_id, handle, schema_id)| IngestReceipt {
        memory_id: MemoryId::new(memory_id),
        handle,
        schema_id: SchemaId::new(schema_id),
    }))
}

pub(crate) fn require_ingest_identity(
    source_id: &str,
    ingest_key: &str,
) -> Result<(), StorageError> {
    identity_text("source_id", source_id)?;
    identity_text("ingest_key", ingest_key)?;
    Ok(())
}

fn identity_text(field: &str, value: &str) -> Result<(), StorageError> {
    if value.is_empty() {
        return Err(StorageError::ConstraintViolation(format!(
            "{field} is empty"
        )));
    }
    if value.contains('\0') {
        return Err(StorageError::ConstraintViolation(format!(
            "{field} contains a NUL"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::require_ingest_identity;
    use proxima_core::StorageError;

    #[test]
    fn an_empty_or_nul_identity_is_refused() {
        for (source, key, field) in [
            ("", "delivery", "source_id"),
            ("src", "", "ingest_key"),
            ("src\0", "delivery", "source_id"),
            ("src", "del\0ivery", "ingest_key"),
        ] {
            let err = require_ingest_identity(source, key).expect_err(field);
            assert!(
                matches!(err, StorageError::ConstraintViolation(ref message) if message.contains(field)),
                "{field}: {err}"
            );
        }
        require_ingest_identity("src", "delivery").expect("a real identity");
    }
}
