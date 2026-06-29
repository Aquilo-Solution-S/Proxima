//! `EventHistory` verb — bounded newest-first read of `change_event`
//! rows visible to the authenticated read-owner set. See
//! `crates/core/src/verbs/event_history.rs`.

use proxima_core::verbs::event_history::{
    EventHistoryRequest, EventHistoryResponse, MAX_EVENT_HISTORY_LIMIT,
};
use proxima_core::{ChangeEvent, OwnerRef, OwnerRefKind, StorageError};
use sqlx::PgPool;
use uuid::Uuid;

use crate::change_event::hydrate_change_events_batch;
use crate::error::internal;
use crate::verbs::consolidate::edge_event_visibility_predicate;

pub(crate) async fn event_history(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    req: &EventHistoryRequest,
) -> Result<EventHistoryResponse, StorageError> {
    if read_owners.is_empty() {
        return Ok(EventHistoryResponse {
            events: Vec::new(),
            seq_high_water: None,
        });
    }
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(read_owners);
    let (world_kind, world_id) =
        crate::access::owner_columns::owner_binds(&proxima_core::access::world());
    let limit = i64::from(req.limit.min(MAX_EVENT_HISTORY_LIMIT));

    // Uses the shared edge guard over ce.edge_source_memory_id /
    // ce.edge_target_memory_id; client `req.principal` is not an access vector.
    let edge_visibility = edge_event_visibility_predicate(1, 2, 5, 6);
    let sql = format!(
        r"SELECT ce.seq FROM proxima_core.change_event ce
             WHERE EXISTS (
                SELECT 1
                  FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
                 WHERE ce.owner_kind = s.kind
                   AND ce.owner_id IS NOT DISTINCT FROM s.id
             )
               AND ($3::uuid IS NULL OR ce.seq < $3)
               AND {edge_visibility}
             ORDER BY ce.seq DESC
             LIMIT $4"
    );
    let seqs: Vec<Uuid> = sqlx::query_scalar(&sql)
        .bind(&read_owner_kinds)
        .bind(&read_owner_ids)
        .bind(req.before)
        .bind(limit)
        .bind(world_kind)
        .bind(world_id)
        .fetch_all(pool)
        .await
        .map_err(internal)?;

    let events: Vec<ChangeEvent> = hydrate_change_events_batch(pool, &seqs).await?;

    let high_water_visibility = edge_event_visibility_predicate(1, 2, 3, 4);
    let high_water_sql = format!(
        r"SELECT ce.seq FROM proxima_core.change_event ce
             WHERE EXISTS (
                SELECT 1
                  FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
                 WHERE ce.owner_kind = s.kind
                   AND ce.owner_id IS NOT DISTINCT FROM s.id
             )
               AND {high_water_visibility}
             ORDER BY ce.seq DESC
             LIMIT 1"
    );
    let high_water = sqlx::query_scalar(&high_water_sql)
        .bind(&read_owner_kinds)
        .bind(&read_owner_ids)
        .bind(world_kind)
        .bind(world_id)
        .fetch_optional(pool)
        .await
        .map_err(internal)?;

    Ok(EventHistoryResponse {
        events,
        seq_high_water: high_water,
    })
}

fn read_owner_columns(read_owners: &[OwnerRef]) -> (Vec<OwnerRefKind>, Vec<Option<uuid::Uuid>>) {
    crate::access::owner_columns::owner_arrays(read_owners)
}
