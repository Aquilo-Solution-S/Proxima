//! The `retention` rows: one per owner and schema (docs/data-and-erasure.md).

use proxiima_core::storage_ports::OwnerWritePermit;
use proxiima_core::{KeepDays, OwnerId, Retention, SchemaId, StorageError};
use uuid::Uuid;

use crate::error::map_err;

/// Upsert the permit owner's row for `schema`, or delete it for `None`.
pub(crate) async fn set_retention<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    permit: &OwnerWritePermit,
    schema: &SchemaId,
    keep_days: Option<KeepDays>,
) -> Result<(), StorageError> {
    let owner_id = permit.owner().into_inner();
    let query = match keep_days {
        Some(days) => sqlx::query(
            "INSERT INTO proxiima_core.retention (owner_id, schema_id, keep_days)
             VALUES ($1, $2, $3)
             ON CONFLICT (owner_id, schema_id)
             DO UPDATE SET keep_days = EXCLUDED.keep_days",
        )
        .bind(owner_id)
        .bind(schema.as_str())
        .bind(i32::from(days.get())),
        None => sqlx::query(
            "DELETE FROM proxiima_core.retention
              WHERE owner_id = $1 AND schema_id = $2",
        )
        .bind(owner_id)
        .bind(schema.as_str()),
    };
    query.execute(executor).await.map_err(map_err)?;
    Ok(())
}

/// The rows of `owners`, or of every owner for `None`, by owner and schema.
pub(crate) async fn read_retention<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    owners: Option<&[OwnerId]>,
) -> Result<Vec<Retention>, StorageError> {
    let owners: Option<Vec<Uuid>> =
        owners.map(|owners| owners.iter().map(|owner| owner.into_inner()).collect());
    let rows: Vec<(Uuid, String, i32)> = sqlx::query_as(
        "SELECT owner_id, schema_id, keep_days
           FROM proxiima_core.retention
          WHERE $1::uuid[] IS NULL OR owner_id = ANY($1::uuid[])
          ORDER BY owner_id, schema_id",
    )
    .bind(owners)
    .fetch_all(executor)
    .await
    .map_err(map_err)?;
    rows.into_iter().map(retention_row).collect()
}

/// A stored row, parsed where it enters. `keep_days` is checked by the
/// table, so a value out of range is a store that is not this schema's.
fn retention_row(
    (owner, schema, keep_days): (Uuid, String, i32),
) -> Result<Retention, StorageError> {
    let keep_days = KeepDays::new(i64::from(keep_days))
        .map_err(|err| StorageError::Internal(format!("retention row of {owner}: {err}")))?;
    Ok(Retention {
        owner: OwnerId::new(owner),
        schema: SchemaId::new(schema),
        keep_days,
    })
}

#[cfg(test)]
mod tests {
    use super::retention_row;

    #[test]
    fn a_stored_row_out_of_range_is_refused_where_it_enters() {
        let owner = uuid::Uuid::now_v7();
        for bad in [0, 36_501] {
            assert!(retention_row((owner, "s.x".into(), bad)).is_err(), "{bad}");
        }
        let row = retention_row((owner, "s.x".into(), 7)).expect("in range");
        assert_eq!(row.keep_days.get(), 7);
    }
}
