//! Outbox publisher — tails `change_event` via LISTEN and fans
//! out typed `ChangeEvents` on a tokio broadcast channel.

use std::time::Duration;

use futures_util::StreamExt;
use proxima_core::{
    ChangeEvent, ChangeEventKind, ChangeEventKindTag, EntityKind, EntityRef, GoalId, MemoryId,
    OrgId, Owner, OwnerPrincipalKind, SchemaId, SchemaVersion, StorageError,
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

struct ChangeEventRow {
    seq: Uuid,
    owner_principal_kind: OwnerPrincipalKind,
    owner_principal_id: Uuid,
    owner_org_id: Uuid,
    kind: ChangeEventKindTag,
    entity_kind: Option<EntityKind>,
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
    entity_personality_instance_id: Option<Uuid>,
    wake_chain_depth: i16,
}

/// Hydrate a single `change_event` row into a typed `ChangeEvent`.
///
/// The migration guarantees exactly one of `(entity_memory_id,
/// entity_goal_id)` is non-NULL for `EntityAppend`, and same for
/// supersedes columns.
pub(crate) async fn hydrate_change_event(
    pool: &sqlx::PgPool,
    seq: Uuid,
) -> Result<Option<ChangeEvent>, StorageError> {
    let row = sqlx::query!(
        r#"SELECT seq,
                  owner_principal_kind AS "owner_principal_kind: OwnerPrincipalKind",
                  owner_principal_id, owner_org_id,
                  kind AS "kind: ChangeEventKindTag",
                  entity_kind AS "entity_kind: EntityKind",
                  entity_memory_id, entity_goal_id,
                  entity_schema_id, entity_schema_version,
                  supersedes_memory_id, supersedes_goal_id,
                  edge_id, edge_relation,
                  edge_source_memory_id, edge_source_goal_id,
                  edge_target_memory_id, edge_target_goal_id,
                  entity_personality_instance_id,
                  wake_chain_depth
             FROM proxima_core.change_event WHERE seq = $1"#,
        seq,
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;

    row.map(|r| ChangeEventRow {
        seq: r.seq,
        owner_principal_kind: r.owner_principal_kind,
        owner_principal_id: r.owner_principal_id,
        owner_org_id: r.owner_org_id,
        kind: r.kind,
        entity_kind: r.entity_kind,
        entity_memory_id: r.entity_memory_id,
        entity_goal_id: r.entity_goal_id,
        entity_schema_id: r.entity_schema_id,
        entity_schema_version: r.entity_schema_version,
        supersedes_memory_id: r.supersedes_memory_id,
        supersedes_goal_id: r.supersedes_goal_id,
        edge_id: r.edge_id,
        edge_relation: r.edge_relation,
        edge_source_memory_id: r.edge_source_memory_id,
        edge_source_goal_id: r.edge_source_goal_id,
        edge_target_memory_id: r.edge_target_memory_id,
        edge_target_goal_id: r.edge_target_goal_id,
        entity_personality_instance_id: r.entity_personality_instance_id,
        wake_chain_depth: r.wake_chain_depth,
    })
    .map(decode_change_event_row)
    .transpose()
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
    let rows = sqlx::query!(
        r#"SELECT seq,
                  owner_principal_kind AS "owner_principal_kind: OwnerPrincipalKind",
                  owner_principal_id, owner_org_id,
                  kind AS "kind: ChangeEventKindTag",
                  entity_kind AS "entity_kind: EntityKind",
                  entity_memory_id, entity_goal_id,
                  entity_schema_id, entity_schema_version,
                  supersedes_memory_id, supersedes_goal_id,
                  edge_id, edge_relation,
                  edge_source_memory_id, edge_source_goal_id,
                  edge_target_memory_id, edge_target_goal_id,
                  entity_personality_instance_id,
                  wake_chain_depth
             FROM proxima_core.change_event
             WHERE seq = ANY($1::uuid[]) ORDER BY seq DESC"#,
        seqs,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;

    rows.into_iter()
        .map(|r| ChangeEventRow {
            seq: r.seq,
            owner_principal_kind: r.owner_principal_kind,
            owner_principal_id: r.owner_principal_id,
            owner_org_id: r.owner_org_id,
            kind: r.kind,
            entity_kind: r.entity_kind,
            entity_memory_id: r.entity_memory_id,
            entity_goal_id: r.entity_goal_id,
            entity_schema_id: r.entity_schema_id,
            entity_schema_version: r.entity_schema_version,
            supersedes_memory_id: r.supersedes_memory_id,
            supersedes_goal_id: r.supersedes_goal_id,
            edge_id: r.edge_id,
            edge_relation: r.edge_relation,
            edge_source_memory_id: r.edge_source_memory_id,
            edge_source_goal_id: r.edge_source_goal_id,
            edge_target_memory_id: r.edge_target_memory_id,
            edge_target_goal_id: r.edge_target_goal_id,
            entity_personality_instance_id: r.entity_personality_instance_id,
            wake_chain_depth: r.wake_chain_depth,
        })
        .map(decode_change_event_row)
        .collect()
}

fn decode_change_event_row(row: ChangeEventRow) -> Result<ChangeEvent, StorageError> {
    let owner = Owner {
        principal: row.owner_principal_kind.with_uuid(row.owner_principal_id),
        org_id: OrgId::new(row.owner_org_id),
    };

    let authoring_instance = decode_personality(row.entity_personality_instance_id);
    let wake_chain_depth = u16::try_from(row.wake_chain_depth).unwrap_or(0);

    let kind = match row.kind {
        ChangeEventKindTag::EdgeAppend => {
            let edge_id = row
                .edge_id
                .ok_or_else(|| StorageError::Internal("missing edge_id".into()))?;
            let relation = row
                .edge_relation
                .ok_or_else(|| StorageError::Internal("missing edge_relation".into()))?;
            let source = decode_entity_ref(row.edge_source_memory_id, row.edge_source_goal_id)?;
            let target = decode_entity_ref(row.edge_target_memory_id, row.edge_target_goal_id)?;
            ChangeEventKind::EdgeAppend {
                edge_id,
                relation,
                source,
                target,
            }
        }
        ChangeEventKindTag::EntityAppend => {
            let entity_kind = row
                .entity_kind
                .ok_or_else(|| StorageError::Internal("missing entity_kind".into()))?;
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
            ChangeEventKind::EntityAppend {
                entity_kind,
                entity,
                schema_id,
                schema_version,
                supersedes,
            }
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

/// Map a row's optional personality instance to the public
/// `ChangeEvent` shape. Nil uuid marks external authoring.
fn decode_personality(instance_id: Option<Uuid>) -> Option<Uuid> {
    instance_id.filter(|id| !id.is_nil())
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
        let rows = sqlx::query!(
            "SELECT seq FROM proxima_core.change_event
             WHERE seq > $1 ORDER BY seq LIMIT $2",
            prev,
            BACKFILL_BATCH,
        )
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

        if rows.is_empty() {
            break;
        }

        for row in rows {
            if let Some(ce) = hydrate_change_event(pool, row.seq).await? {
                let _ = tx.send(ce);
            }
            *last_seen_seq = Some(row.seq);
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
