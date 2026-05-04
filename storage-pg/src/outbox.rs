//! Outbox publisher — tails `change_event` via LISTEN and fans
//! out typed `ChangeEvents` on a tokio broadcast channel.

use std::time::Duration;

use futures_util::StreamExt;
use proxima_core::{
    ChangeEvent, ChangeEventKind, EntityKind, EntityRef, GoalId, GroupId, MemoryId, OrgId, Owner,
    Principal, SchemaId, SchemaVersion, StorageError, UserId,
};
use sqlx::postgres::PgListener;
use tokio::sync::broadcast;
use tracing::{error, warn};
use uuid::Uuid;

pub const NOTIFY_CHANNEL: &str = "proxima_change_event";
pub const BROADCAST_CAPACITY: usize = 1024;

/// Hydrate a single `change_event` row into a typed `ChangeEvent`.
///
/// The migration guarantees exactly one of `(entity_memory_id,
/// entity_goal_id)` is non-NULL for `EntityAppend`, and same for
/// supersedes columns.
#[allow(clippy::too_many_lines)]
async fn hydrate_change_event(
    pool: &sqlx::PgPool,
    seq: Uuid,
) -> Result<Option<ChangeEvent>, StorageError> {
    #[derive(Debug, sqlx::FromRow)]
    struct Row {
        seq: Uuid,
        owner_principal_kind: String,
        owner_principal_id: Uuid,
        owner_org_id: Uuid,
        kind: String,
        entity_kind: Option<String>,
        entity_memory_id: Option<Uuid>,
        entity_goal_id: Option<Uuid>,
        entity_schema_id: Option<String>,
        entity_schema_version: Option<i32>,
        supersedes_memory_id: Option<Uuid>,
        supersedes_goal_id: Option<Uuid>,
    }

    let row: Option<Row> = sqlx::query_as(
        "SELECT seq, owner_principal_kind, owner_principal_id, owner_org_id, kind,
                entity_kind, entity_memory_id, entity_goal_id,
                entity_schema_id, entity_schema_version,
                supersedes_memory_id, supersedes_goal_id
         FROM proxima_core.change_event WHERE seq = $1",
    )
    .bind(seq)
    .fetch_optional(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;

    let Some(row) = row else {
        return Ok(None);
    };

    // Decode owner.
    let principal = match row.owner_principal_kind.as_str() {
        "User" => Principal::User(UserId::new(row.owner_principal_id)),
        "Group" => Principal::Group(GroupId::new(row.owner_principal_id)),
        other => {
            return Err(StorageError::Internal(format!(
                "unknown principal kind: {other}"
            )));
        }
    };
    let owner = Owner {
        principal,
        org_id: OrgId::new(row.owner_org_id),
    };

    // Decode entity_kind.
    let entity_kind = match row.entity_kind.as_deref() {
        Some("Fact") => EntityKind::Fact,
        Some("Abstraction") => EntityKind::Abstraction,
        Some("Perspective") => EntityKind::Perspective,
        Some("Goal") => EntityKind::Goal,
        Some(other) => {
            return Err(StorageError::Internal(format!(
                "unknown entity_kind: {other}"
            )));
        }
        None => {
            // EdgeAppend path — skip for M2.
            return Ok(None);
        }
    };

    // Decode entity ref — exactly one of memory/goal is non-NULL.
    let entity = match (row.entity_memory_id, row.entity_goal_id) {
        (Some(m), None) => EntityRef::Memory(MemoryId::new(m)),
        (None, Some(g)) => EntityRef::Goal(GoalId::new(g)),
        (Some(_), Some(_)) | (None, None) => {
            return Err(StorageError::Internal(
                "change_event entity columns violate CHECK constraint".into(),
            ));
        }
    };

    // Decode schema.
    let schema_id = SchemaId::new(
        row.entity_schema_id
            .ok_or_else(|| StorageError::Internal("missing entity_schema_id".into()))?,
    );
    let schema_version = SchemaVersion::new(
        row.entity_schema_version
            .ok_or_else(|| StorageError::Internal("missing entity_schema_version".into()))?
            .cast_unsigned(),
    );

    // Decode supersedes.
    let supersedes = match (row.supersedes_memory_id, row.supersedes_goal_id) {
        (Some(m), None) => Some(EntityRef::Memory(MemoryId::new(m))),
        (None, Some(g)) => Some(EntityRef::Goal(GoalId::new(g))),
        (None, None) => None,
        (Some(_), Some(_)) => {
            return Err(StorageError::Internal(
                "change_event supersedes columns violate CHECK constraint".into(),
            ));
        }
    };

    // Build ChangeEventKind.
    let kind = match row.kind.as_str() {
        "EntityAppend" => ChangeEventKind::EntityAppend {
            entity_kind,
            entity,
            schema_id,
            schema_version,
            supersedes,
        },
        "EdgeAppend" => {
            // M3+ — skip for now.
            warn!("EdgeAppend change_event received but not yet handled");
            return Ok(None);
        }
        other => {
            return Err(StorageError::Internal(format!(
                "unknown change_event kind: {other}"
            )));
        }
    };

    Ok(Some(ChangeEvent {
        seq: row.seq,
        owner,
        kind,
    }))
}

/// Background task that LISTENs on `NOTIFY_CHANNEL` and
/// publishes typed `ChangeEvents` to the broadcast channel.
pub async fn outbox_publisher(pool: sqlx::PgPool, tx: broadcast::Sender<ChangeEvent>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        match run_listener(&pool, tx.clone()).await {
            Ok(()) => {
                // Normal exit (shouldn't happen — listener runs forever).
                break;
            }
            Err(e) => {
                error!("outbox listener error: {e}, reconnecting in {:?}", backoff);
                tokio::time::sleep(backoff).await;
                backoff = backoff.saturating_mul(2);
            }
        }
    }
}

async fn run_listener(
    pool: &sqlx::PgPool,
    tx: broadcast::Sender<ChangeEvent>,
) -> Result<(), StorageError> {
    let mut listener = PgListener::connect_with(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

    listener
        .listen(NOTIFY_CHANNEL)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

    let mut stream = listener.into_stream();

    while let Some(notification) = stream.next().await {
        let notification = notification.map_err(|e| StorageError::Internal(e.to_string()))?;

        let payload = notification.payload();
        let seq: Uuid = payload
            .parse()
            .map_err(|_| StorageError::Internal(format!("invalid seq UUID: {payload}")))?;

        if let Some(ce) = hydrate_change_event(pool, seq).await? {
            // Ignore send errors — no live receivers is OK.
            let _ = tx.send(ce);
        }
        // Row was deleted or EdgeAppend — skip.
    }

    Ok(())
}
