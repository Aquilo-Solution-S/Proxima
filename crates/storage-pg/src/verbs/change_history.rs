//! `ChangeHistory` verb — bounded newest-first read of `announce`
//! rows visible to the authenticated read-owner set. See
//! `crates/core/src/verbs/change_history.rs`.

use proxima_core::verbs::change_history::{
    ChangeHistoryRequest, ChangeHistoryResponse, MAX_CHANGE_HISTORY_LIMIT,
};
use proxima_core::{ChangeEvent, OwnerRef, StorageError};
use sqlx::PgPool;
use uuid::Uuid;

use crate::change_event::hydrate_change_events_batch;
use crate::error::map_err;

pub(crate) async fn change_history(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    req: &ChangeHistoryRequest,
) -> Result<ChangeHistoryResponse, StorageError> {
    if read_owners.is_empty() {
        return Ok(ChangeHistoryResponse {
            events: Vec::new(),
            seq_high_water: None,
        });
    }
    let owner_ids: Vec<Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let limit = i64::from(req.limit.min(MAX_CHANGE_HISTORY_LIMIT));
    let seqs: Vec<Uuid> = sqlx::query_scalar(
        "SELECT seq FROM proxima_core.announce
          WHERE owner_id = ANY($1::uuid[])
            AND ($2::uuid IS NULL OR seq < $2)
          ORDER BY seq DESC
          LIMIT $3",
    )
    .bind(&owner_ids)
    .bind(req.before)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let events: Vec<ChangeEvent> = hydrate_change_events_batch(pool, read_owners, &seqs).await?;

    let owner_ids: Vec<Uuid> = read_owners
        .iter()
        .copied()
        .map(proxima_core::OwnerRef::stored_owner_id)
        .collect();
    let high_water = crate::verbs::query::read_seq_high_water(pool, &owner_ids).await?;

    Ok(ChangeHistoryResponse {
        events,
        seq_high_water: high_water,
    })
}
