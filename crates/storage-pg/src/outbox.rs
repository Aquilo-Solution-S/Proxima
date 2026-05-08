//! Outbox publisher — tails `change_event` via LISTEN and fans
//! out typed `ChangeEvents` on a tokio broadcast channel.

use std::time::Duration;

use futures_util::StreamExt;
use proxima_core::{
    ChangeEvent, ChangeEventKind, EntityKind, EntityRef, GoalId, GroupId, MemoryId, OrgId, Owner,
    Principal, SchemaId, SchemaVersion, StorageError, UserId,
};
use sqlx::postgres::PgListener;
use tokio::sync::{broadcast, oneshot};
use tracing::error;
use uuid::Uuid;

pub const NOTIFY_CHANNEL: &str = "proxima_change_event";
pub const BROADCAST_CAPACITY: usize = 1024;
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const BACKFILL_BATCH: i64 = 1000;

pub(crate) type ReadySignal = oneshot::Sender<Result<(), StorageError>>;

#[derive(Debug, sqlx::FromRow)]
struct ChangeEventRow {
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
    edge_id: Option<Uuid>,
    edge_relation: Option<String>,
    edge_source_memory_id: Option<Uuid>,
    edge_source_goal_id: Option<Uuid>,
    edge_target_memory_id: Option<Uuid>,
    edge_target_goal_id: Option<Uuid>,
    entity_personality_type_id: Option<String>,
    entity_personality_instance_id: Option<Uuid>,
    wake_chain_depth: i16,
}

const CHANGE_EVENT_COLUMNS: &str = "seq, owner_principal_kind, owner_principal_id, owner_org_id, kind, \
    entity_kind, entity_memory_id, entity_goal_id, \
    entity_schema_id, entity_schema_version, \
    supersedes_memory_id, supersedes_goal_id, \
    edge_id, edge_relation, \
    edge_source_memory_id, edge_source_goal_id, \
    edge_target_memory_id, edge_target_goal_id, \
    entity_personality_type_id, entity_personality_instance_id, \
    wake_chain_depth";

/// Hydrate a single `change_event` row into a typed `ChangeEvent`.
///
/// The migration guarantees exactly one of `(entity_memory_id,
/// entity_goal_id)` is non-NULL for `EntityAppend`, and same for
/// supersedes columns.
pub(crate) async fn hydrate_change_event(
    pool: &sqlx::PgPool,
    seq: Uuid,
) -> Result<Option<ChangeEvent>, StorageError> {
    let sql = format!("SELECT {CHANGE_EVENT_COLUMNS} FROM proxima_core.change_event WHERE seq = $1");
    let row: Option<ChangeEventRow> = sqlx::query_as(&sql)
        .bind(seq)
        .fetch_optional(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

    row.map(decode_change_event_row).transpose()
}

/// Batched hydrate. Returns events ordered by `seq DESC`; the caller
/// is responsible for any further reordering.
pub(crate) async fn hydrate_change_events_batch(
    pool: &sqlx::PgPool,
    seqs: &[Uuid],
) -> Result<Vec<ChangeEvent>, StorageError> {
    if seqs.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT {CHANGE_EVENT_COLUMNS} FROM proxima_core.change_event \
         WHERE seq = ANY($1::uuid[]) ORDER BY seq DESC"
    );
    let rows: Vec<ChangeEventRow> = sqlx::query_as(&sql)
        .bind(seqs)
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

    rows.into_iter().map(decode_change_event_row).collect()
}

#[allow(clippy::too_many_lines)]
fn decode_change_event_row(row: ChangeEventRow) -> Result<ChangeEvent, StorageError> {
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

    let authoring_instance = decode_personality(
        row.entity_personality_type_id.as_deref(),
        row.entity_personality_instance_id,
    );
    let wake_chain_depth = u16::try_from(row.wake_chain_depth).unwrap_or(0);

    if row.kind == "EdgeAppend" {
        let edge_id = row
            .edge_id
            .ok_or_else(|| StorageError::Internal("missing edge_id".into()))?;
        let relation = row
            .edge_relation
            .ok_or_else(|| StorageError::Internal("missing edge_relation".into()))?;
        let source = decode_entity_ref(row.edge_source_memory_id, row.edge_source_goal_id)?;
        let target = decode_entity_ref(row.edge_target_memory_id, row.edge_target_goal_id)?;
        return Ok(ChangeEvent {
            seq: row.seq,
            owner,
            kind: ChangeEventKind::EdgeAppend {
                edge_id,
                relation,
                source,
                target,
            },
            authoring_personality_instance_id: authoring_instance,
            wake_chain_depth,
        });
    }

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
        None => return Err(StorageError::Internal("missing entity_kind".into())),
    };

    let entity = match (row.entity_memory_id, row.entity_goal_id) {
        (Some(m), None) => EntityRef::Memory(MemoryId::new(m)),
        (None, Some(g)) => EntityRef::Goal(GoalId::new(g)),
        (Some(_), Some(_)) | (None, None) => {
            return Err(StorageError::Internal(
                "change_event entity columns violate CHECK constraint".into(),
            ));
        }
    };

    let schema_id = SchemaId::new(
        row.entity_schema_id
            .ok_or_else(|| StorageError::Internal("missing entity_schema_id".into()))?,
    );
    let schema_version = SchemaVersion::new(
        row.entity_schema_version
            .ok_or_else(|| StorageError::Internal("missing entity_schema_version".into()))?
            .cast_unsigned(),
    );

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

    let kind = match row.kind.as_str() {
        "EntityAppend" => ChangeEventKind::EntityAppend {
            entity_kind,
            entity,
            schema_id,
            schema_version,
            supersedes,
        },
        other => {
            return Err(StorageError::Internal(format!(
                "unknown change_event kind: {other}"
            )));
        }
    };

    Ok(ChangeEvent {
        seq: row.seq,
        owner,
        kind,
        authoring_personality_instance_id: authoring_instance,
        wake_chain_depth,
    })
}

/// Map the row's `entity_personality_type_id` / `_instance_id` columns
/// (always populated post-migration; sentinel `'external/event-source'`
/// + nil-uuid for external ingestions) to the public `Option<...>`
///   shape on `ChangeEvent`.
fn decode_personality(type_id: Option<&str>, instance_id: Option<Uuid>) -> Option<Uuid> {
    const EXTERNAL_SENTINEL: &str = "external/event-source";
    match (type_id, instance_id) {
        (Some(t), Some(i)) if t != EXTERNAL_SENTINEL => Some(i),
        _ => None,
    }
}

fn decode_entity_ref(
    memory_id: Option<Uuid>,
    goal_id: Option<Uuid>,
) -> Result<EntityRef, StorageError> {
    match (memory_id, goal_id) {
        (Some(m), None) => Ok(EntityRef::Memory(MemoryId::new(m))),
        (None, Some(g)) => Ok(EntityRef::Goal(GoalId::new(g))),
        (Some(_), Some(_)) | (None, None) => Err(StorageError::Internal(
            "change_event endpoint columns violate CHECK constraint".into(),
        )),
    }
}

/// Background task that LISTENs on `NOTIFY_CHANNEL` and publishes
/// typed `ChangeEvent`s to the broadcast channel.
///
/// `ready_tx` is consumed on the first successful LISTEN+backfill
/// pass; subsequent reconnects do not re-signal. The publisher
/// tracks `last_seen_seq` across reconnects so backfill on a
/// reconnect catches only what was missed during the disconnected
/// window.
pub async fn outbox_publisher(
    pool: sqlx::PgPool,
    tx: broadcast::Sender<ChangeEvent>,
    mut ready_tx: Option<ReadySignal>,
) {
    let mut last_seen_seq: Option<Uuid> = None;
    let mut backoff = Duration::from_secs(1);
    loop {
        match run_listener(&pool, tx.clone(), &mut last_seen_seq, ready_tx.take()).await {
            Ok(()) => {
                // Stream ended cleanly (shouldn't happen — listener runs forever).
                break;
            }
            Err(e) => {
                error!("outbox listener error: {e}, reconnecting in {:?}", backoff);
                tokio::time::sleep(backoff).await;
                backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
            }
        }
    }
}

async fn run_listener(
    pool: &sqlx::PgPool,
    tx: broadcast::Sender<ChangeEvent>,
    last_seen_seq: &mut Option<Uuid>,
    ready_tx: Option<ReadySignal>,
) -> Result<(), StorageError> {
    // Bind LISTEN before reading so any commit that lands during
    // the backfill SELECT below also queues a notification on this
    // session — the dedup step at the bottom drops the overlap.
    let mut listener = PgListener::connect_with(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
    listener
        .listen(NOTIFY_CHANNEL)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

    // Backfill: first boot drains everything from nil; reconnect drains
    // anything missed while the listener session was down. Chunked so
    // a long downtime doesn't pull millions of rows into memory at once.
    loop {
        let prev = last_seen_seq.unwrap_or_else(Uuid::nil);
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT seq FROM proxima_core.change_event \
             WHERE seq > $1 ORDER BY seq LIMIT $2",
        )
        .bind(prev)
        .bind(BACKFILL_BATCH)
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

        if rows.is_empty() {
            break;
        }

        for (seq,) in rows {
            if let Some(ce) = hydrate_change_event(pool, seq).await? {
                let _ = tx.send(ce);
            }
            *last_seen_seq = Some(seq);
        }
    }

    // LISTEN bound + backfill drained — callers awaiting startup
    // can now safely commit writes and expect their notifications.
    if let Some(sig) = ready_tx {
        let _ = sig.send(Ok(()));
    }

    let mut stream = listener.into_stream();
    while let Some(notification) = stream.next().await {
        let notification = notification.map_err(|e| StorageError::Internal(e.to_string()))?;

        let payload = notification.payload();
        let seq: Uuid = payload
            .parse()
            .map_err(|_| StorageError::Internal(format!("invalid seq UUID: {payload}")))?;

        // Dedup against backfill: a row committed between LISTEN
        // bind and backfill SELECT is delivered through both paths.
        if last_seen_seq.is_some_and(|s| s >= seq) {
            continue;
        }

        if let Some(ce) = hydrate_change_event(pool, seq).await? {
            let _ = tx.send(ce);
        }
        *last_seen_seq = Some(seq);
    }

    Ok(())
}
