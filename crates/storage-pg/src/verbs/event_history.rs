//! `EventHistory` verb — bounded newest-first read of `change_event`
//! rows for one Owner. See `crates/core/src/verbs/event_history.rs`.

use proxima_core::verbs::event_history::{
    EventHistoryRequest, EventHistoryResponse, MAX_EVENT_HISTORY_LIMIT,
};
use proxima_core::{ChangeEvent, Principal, StorageError};
use sqlx::PgPool;
use uuid::Uuid;

use crate::outbox::hydrate_change_event;

pub(crate) async fn event_history(
    pool: &PgPool,
    req: &EventHistoryRequest,
) -> Result<EventHistoryResponse, StorageError> {
    let owner_kind: &str = match &req.owner.principal {
        Principal::User(_) => "User",
        Principal::Group(_) => "Group",
    };
    let owner_principal_id = match &req.owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    let limit = i64::from(req.limit.min(MAX_EVENT_HISTORY_LIMIT));

    let rows: Vec<(Uuid,)> = match req.before {
        Some(before) => sqlx::query_as(
            "SELECT seq FROM proxima_core.change_event \
             WHERE owner_principal_kind = $1 AND owner_principal_id = $2 \
               AND seq < $3 \
             ORDER BY seq DESC LIMIT $4",
        )
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(before)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?,
        None => sqlx::query_as(
            "SELECT seq FROM proxima_core.change_event \
             WHERE owner_principal_kind = $1 AND owner_principal_id = $2 \
             ORDER BY seq DESC LIMIT $3",
        )
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?,
    };

    let mut events: Vec<ChangeEvent> = Vec::with_capacity(rows.len());
    for (seq,) in rows {
        if let Some(event) = hydrate_change_event(pool, seq).await? {
            events.push(event);
        }
    }

    let high_water: Option<(Uuid,)> = sqlx::query_as(
        "SELECT seq FROM proxima_core.change_event \
         WHERE owner_principal_kind = $1 AND owner_principal_id = $2 \
         ORDER BY seq DESC LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;

    Ok(EventHistoryResponse {
        events,
        seq_high_water: high_water.map(|(seq,)| seq),
    })
}
