//! `ChangeHistory` verb — bounded newest-first read of `change_event`
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
use crate::verbs::consolidate::edge_event_visibility_predicate;
use crate::verbs::query::read_owner_columns;

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
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(read_owners);
    let (world_kind, world_id) =
        crate::access::owner_columns::owner_binds(&proxima_core::access::world());
    let limit = i64::from(req.limit.min(MAX_CHANGE_HISTORY_LIMIT));

    // Uses the shared edge guard over ce.edge_source_memory_id /
    // ce.edge_target_memory_id; client `req.owner` is not an access vector.
    // Plain `=` on ce.owner_id: the change_event CHECKs prove the column is
    // never NULL, so `=` selects exactly what IS NOT DISTINCT FROM did while
    // staying an index condition.
    let edge_visibility = edge_event_visibility_predicate(1, 2, 5, 6);
    let sql = format!(
        r"SELECT ce.seq FROM proxima_core.change_event ce
             WHERE EXISTS (
                SELECT 1
                  FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
                 WHERE ce.owner_kind = s.kind
                   AND ce.owner_id = s.id
             )
               AND ($3::uuid IS NULL OR ce.seq < $3)
               AND {edge_visibility}
             ORDER BY ce.seq DESC
             LIMIT $4"
    );
    // SQL-POLICY: fixed-fragment (edge_visibility is a fixed predicate over
    // numbered binds; every value is bound)
    let seqs: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(&read_owner_kinds)
        .bind(&read_owner_ids)
        .bind(req.before)
        .bind(limit)
        .bind(world_kind)
        .bind(world_id)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;

    let events: Vec<ChangeEvent> = hydrate_change_events_batch(pool, read_owners, &seqs).await?;

    // Same visibility-gated high-water query the memories page computes;
    // one implementation keeps the two verbs' semantics in lockstep.
    let high_water =
        crate::verbs::query::read_seq_high_water(pool, &read_owner_kinds, &read_owner_ids).await?;

    Ok(ChangeHistoryResponse {
        events,
        seq_high_water: high_water,
    })
}
