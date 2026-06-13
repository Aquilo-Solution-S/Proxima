//! `Subscribe` verb — backfill from `change_event` then attach to the
//! live broadcast stream, owner-filtered.
//!
//! The live receiver is taken first so no events fall in the gap window
//! between backfill and live attachment.

use proxima_core::verbs::subscribe::ChangeEventStream;
use proxima_core::{ChangeEvent, OwnerPrincipalKind, Principal, StorageError};
use sqlx::PgPool;
use tokio::sync::broadcast;

use crate::outbox::hydrate_change_event;

pub(crate) async fn subscribe_changes(
    pool: &PgPool,
    tx: &broadcast::Sender<ChangeEvent>,
    principal: &Principal,
    since: Option<uuid::Uuid>,
) -> Result<ChangeEventStream, StorageError> {
    use futures_util::StreamExt;
    use tokio_stream::wrappers::BroadcastStream;

    // 1. Attach to live stream first to avoid a gap window.
    let rx = tx.subscribe();

    // 2. Backfill: SELECT change_event seqs matching this owner
    //    with seq > since (or all if None), ORDER BY seq ASC.
    let (owner_kind, owner_principal_id) = match principal {
        Principal::User(u) => (OwnerPrincipalKind::User, u.into_inner()),
        Principal::Group(g) => (OwnerPrincipalKind::Group, g.into_inner()),
    };
    let seqs: Vec<uuid::Uuid> = match since {
        Some(since_seq) => sqlx::query_scalar!(
            r#"SELECT seq FROM proxima_core.change_event
                 WHERE owner_principal_kind = $1 AND owner_principal_id = $2
                   AND seq > $3
                 ORDER BY seq ASC"#,
            owner_kind as OwnerPrincipalKind,
            owner_principal_id,
            since_seq,
        )
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?,
        None => sqlx::query_scalar!(
            r#"SELECT seq FROM proxima_core.change_event
                 WHERE owner_principal_kind = $1 AND owner_principal_id = $2
                 ORDER BY seq ASC"#,
            owner_kind as OwnerPrincipalKind,
            owner_principal_id,
        )
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?,
    };

    // 3. Hydrate each backfill seq.
    let mut backfill = Vec::with_capacity(seqs.len());
    for seq in seqs {
        if let Some(ce) = hydrate_change_event(pool, seq).await? {
            backfill.push(ce);
        }
    }

    // 4. Build the live half. Use BroadcastStream then filter
    //    Lagged errors out (treat as gap; clients will resume
    //    via since).
    let live = BroadcastStream::new(rx).filter_map(|res| async { res.ok() });

    // 5. Concatenate backfill + live, filter by Owner principal.
    let owner_principal = principal.clone();
    let combined = futures_util::stream::iter(backfill)
        .chain(live)
        .filter(move |ce| {
            let m = ce.owner.principal == owner_principal;
            async move { m }
        });

    Ok(Box::pin(combined))
}
