//! `EventHistory` verb — bounded newest-first read of `change_event`
//! rows for one Owner. See `crates/core/src/verbs/event_history.rs`.

use proxima_core::verbs::event_history::{
    EventHistoryRequest, EventHistoryResponse, MAX_EVENT_HISTORY_LIMIT,
};
use proxima_core::{ChangeEvent, OwnerPrincipalKind, Principal, StorageError};
use sqlx::PgPool;
use uuid::Uuid;

use crate::outbox::hydrate_change_events_batch;

pub(crate) async fn event_history(
    pool: &PgPool,
    req: &EventHistoryRequest,
) -> Result<EventHistoryResponse, StorageError> {
    let (owner_kind, owner_principal_id) = match &req.owner.principal {
        Principal::User(u) => (OwnerPrincipalKind::User, u.into_inner()),
        Principal::Group(g) => (OwnerPrincipalKind::Group, g.into_inner()),
    };
    let limit = i64::from(req.limit.min(MAX_EVENT_HISTORY_LIMIT));

    let seqs: Vec<Uuid> = match req.before {
        Some(before) => sqlx::query_scalar!(
            r#"SELECT seq FROM proxima_core.change_event
                 WHERE owner_principal_kind = $1 AND owner_principal_id = $2
                   AND seq < $3
                 ORDER BY seq DESC LIMIT $4"#,
            owner_kind as OwnerPrincipalKind,
            owner_principal_id,
            before,
            limit,
        )
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?,
        None => sqlx::query_scalar!(
            r#"SELECT seq FROM proxima_core.change_event
                 WHERE owner_principal_kind = $1 AND owner_principal_id = $2
                 ORDER BY seq DESC LIMIT $3"#,
            owner_kind as OwnerPrincipalKind,
            owner_principal_id,
            limit,
        )
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?,
    };

    let events: Vec<ChangeEvent> = hydrate_change_events_batch(pool, &seqs).await?;

    let high_water = sqlx::query_scalar!(
        r#"SELECT seq FROM proxima_core.change_event
             WHERE owner_principal_kind = $1 AND owner_principal_id = $2
             ORDER BY seq DESC LIMIT 1"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;

    Ok(EventHistoryResponse {
        events,
        seq_high_water: high_water,
    })
}
