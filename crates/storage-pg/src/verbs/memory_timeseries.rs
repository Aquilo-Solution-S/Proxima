//! Memory timeseries: the admission row every write lands, and the two
//! reads that go straight at it.
//!
//! Crate-internal write and reads: `ingest_fact_timeseries` is the row every
//! governed write path materializes through, so a public door onto it is a
//! second write path — one that skips the sidecar, the projection row, the
//! sketch and the embedding enqueue its callers add. `read_memory_by_t` /
//! `read_memory_head` take the same transaction and exist to assert on that
//! row from inside the crate; the authorized read surface is the query ports.

#![allow(
    clippy::missing_errors_doc,
    clippy::doc_markdown,
    clippy::too_many_lines
)]

use proxima_core::edge::EdgeEndpoint;
use proxima_core::verbs::fact_ingest::{FactIngestOutcome, FactWriteCommand};
use proxima_core::{MemoryId, Owner, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::{internal, map_err};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MemoryRow {
    pub handle: Uuid,
    pub t: Uuid,
    pub kind: String,
    pub owner_id: Uuid,
    pub source_id: Option<String>,
    pub ingest_key: Option<String>,
    pub origins: Vec<Uuid>,
    pub refs: Vec<Uuid>,
    pub goal_refs: Vec<Uuid>,
}

/// One txn: owners upsert + ingest_keys + memory_head + memory + announce(append).
///
/// `sidecar_tables` is the declared set forget will dump/delete — tables
/// actually inserted for this `t`, never the global registry.
/// `content_id` is the owner-scoped payload (required for A/P).
pub(crate) async fn ingest_fact_timeseries(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &FactWriteCommand,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
    sidecar_tables: &[String],
    content_id: Option<Uuid>,
) -> Result<FactIngestOutcome, StorageError> {
    let owner_id = crate::access::owner_columns::ensure_owner_row(tx.as_mut(), owner).await?;

    let source_id = draft.source_id.clone();
    let ingest_key = draft.ingest_key.clone();
    if source_id.is_some() != ingest_key.is_some() {
        return Err(StorageError::ConstraintViolation(
            "source_id and ingest_key must both be set or both be absent".into(),
        ));
    }

    let kind = if draft.kind.is_empty() {
        "fact"
    } else {
        draft.kind.as_str()
    };
    if !matches!(kind, "fact" | "abstraction" | "perspective") {
        return Err(StorageError::ConstraintViolation(
            "kind must be fact, abstraction, or perspective".into(),
        ));
    }
    if kind != "fact" && (source_id.is_some() || ingest_key.is_some()) {
        return Err(StorageError::ConstraintViolation(
            "A/P cannot carry source_id/ingest_key".into(),
        ));
    }
    let content_id = if let Some(id) = content_id {
        Some(id)
    } else if kind == "fact" {
        None
    } else {
        let text = draft
            .rendered_text
            .as_deref()
            .map_or(draft.payload.as_slice(), str::as_bytes);
        Some(
            super::content::ensure_text_content(tx, owner_id, draft.schema_id.as_str(), text)
                .await?,
        )
    };

    let mut persisted_origins = pin_memory_ids(origins)?;
    let (mut refs, goal_refs) = pin_reference_ids(references);
    // CHECK memory_fact_origins_chk: Facts cannot carry origins. Pins
    // a Fact declares (activation, evidence, prior request) live in refs.
    if kind == "fact" {
        for id in persisted_origins.drain(..) {
            if !refs.contains(&id) {
                refs.push(id);
            }
        }
    }

    let handle = draft.handle.unwrap_or_else(Uuid::now_v7);
    let t: Uuid = sqlx::query_scalar("SELECT uuidv7()")
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;

    if let (Some(source_id), Some(key)) = (source_id.as_deref(), ingest_key.as_deref()) {
        let claimed = sqlx::query(
            "INSERT INTO proxima_core.ingest_keys (owner_id, source_id, ingest_key, t)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (owner_id, source_id, ingest_key) DO NOTHING",
        )
        .bind(owner_id)
        .bind(source_id)
        .bind(key)
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
        if claimed.rows_affected() == 0 {
            let replay_t: Uuid = sqlx::query_scalar(
                "SELECT t FROM proxima_core.ingest_keys
                  WHERE owner_id = $1 AND source_id = $2 AND ingest_key = $3",
            )
            .bind(owner_id)
            .bind(source_id)
            .bind(key)
            .fetch_one(tx.as_mut())
            .await
            .map_err(map_err)?;
            // Forget keeps the `ingest_keys` row and moves the handle to the
            // `cooled` stub, so a source re-delivering a cooled admission still
            // replays. Reading `memory` alone misses the stub. A legacy cooled
            // row has NULL refs because its declaration was not persisted;
            // accepting it as an empty vector would silently bless a changed
            // declaration.
            let replay_row: (Uuid, Option<Vec<Uuid>>, Option<Vec<Uuid>>) = sqlx::query_as(
                "SELECT handle, refs, goal_refs FROM proxima_core.memory
                  WHERE t = $1 AND owner_id = $2
                 UNION ALL
                 SELECT handle, refs, goal_refs FROM proxima_core.cooled
                  WHERE t = $1 AND owner_id = $2
                 LIMIT 1",
            )
            .bind(replay_t)
            .bind(owner_id)
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?
            .ok_or_else(|| {
                internal(format!(
                    "ingest key claims t {replay_t} with no hot or cooled row"
                ))
            })?;
            let (Some(stored_refs), Some(stored_goal_refs)) = (replay_row.1, replay_row.2) else {
                return Err(StorageError::Conflict(
                    "fact replay references are unavailable for cooled admission".into(),
                ));
            };
            if stored_refs != refs || stored_goal_refs != goal_refs {
                return Err(StorageError::Conflict(
                    "fact replay changed declared refs".into(),
                ));
            }
            return Ok(FactIngestOutcome {
                receipt_id: None,
                memory_id: MemoryId::new(replay_t),
                change_event_seq: replay_t,
                idempotent_replay: true,
                cited_object_id: None,
                handle: replay_row.0,
            });
        }
    }

    let head = sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, $2::proxima_core.memory_kind, $3, $4, $5)
         ON CONFLICT (handle) DO UPDATE SET t = EXCLUDED.t
         WHERE proxima_core.memory_head.kind = EXCLUDED.kind
           AND proxima_core.memory_head.schema_id = EXCLUDED.schema_id
           AND proxima_core.memory_head.owner_id = EXCLUDED.owner_id
         RETURNING handle",
    )
    .bind(handle)
    .bind(kind)
    .bind(draft.schema_id.as_str())
    .bind(owner_id)
    .bind(t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    if head.is_none() {
        return Err(StorageError::ConstraintViolation(
            "memory_head kind/schema/owner mismatch".into(),
        ));
    }

    sqlx::query(
        "INSERT INTO proxima_core.memory
            (handle, t, kind, owner_id, schema_id, source_id, ingest_key, blob_id,
             content_id, origins, refs, goal_refs, sidecar_tables)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                 $13)",
    )
    .bind(handle)
    .bind(t)
    .bind(kind)
    .bind(owner_id)
    .bind(draft.schema_id.as_str())
    .bind(source_id.as_deref())
    .bind(ingest_key.as_deref())
    .bind(draft.blob_id)
    .bind(content_id)
    .bind(&persisted_origins)
    .bind(&refs)
    .bind(&goal_refs)
    .bind(sidecar_tables)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    let seq: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.announce
            (owner_id, op, entity, handle, t)
         VALUES ($1, 'append', 'memory', $2, $3)
         RETURNING seq",
    )
    .bind(owner_id)
    .bind(handle)
    .bind(t)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;

    super::sketch::upsert_sketch(
        tx,
        owner_id,
        t,
        kind,
        &super::sketch::sketch_line(kind, draft.rendered_text.as_deref(), &[]),
    )
    .await?;

    Ok(FactIngestOutcome {
        receipt_id: None,
        memory_id: MemoryId::new(t),
        change_event_seq: seq,
        idempotent_replay: false,
        cited_object_id: None,
        handle,
    })
}

/// Project authorized endpoints to the UUID array stored by Postgres while
/// preserving declaration order and removing duplicate pins. Origins are a
/// Memory-only column; a Goal origin is a malformed authorized carrier and
/// must not be silently discarded.
fn pin_memory_ids(pins: &[EdgeEndpoint]) -> Result<Vec<Uuid>, StorageError> {
    let mut ids = Vec::with_capacity(pins.len());
    for pin in pins {
        let Some(memory_id) = pin.memory_id() else {
            return Err(StorageError::ConstraintViolation(
                "memory origins must target a Memory".into(),
            ));
        };
        let id = memory_id.into_inner();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Split reference endpoints into the two columns that now carry them:
/// `memory.refs` for Memory targets, `memory.goal_refs` for Goal targets.
/// The column is the type — the trigger checks each against its own spine —
/// so the endpoint kind survives persistence instead of being re-derived by
/// every reader. Declaration order is preserved within each column and
/// duplicate pins are dropped.
pub(crate) fn pin_reference_ids(pins: &[EdgeEndpoint]) -> (Vec<Uuid>, Vec<Uuid>) {
    let mut memory_ids = Vec::with_capacity(pins.len());
    let mut goal_ids = Vec::new();
    for pin in pins {
        let id = pin.entity_id();
        let column = if matches!(pin.entity, proxima_core::EntityRef::Goal(_)) {
            &mut goal_ids
        } else {
            &mut memory_ids
        };
        if !column.contains(&id) {
            column.push(id);
        }
    }
    (memory_ids, goal_ids)
}

/// Read one admission row by `t`, inside the caller's transaction.
///
/// Test-only, and the gate says so rather than a comment: the only consumer
/// is the suite below, which asserts on the row a write just landed before
/// that write commits. The authorized read surface is the query ports; this
/// reaches the table directly and must not become a second one.
#[cfg(test)]
pub(crate) async fn read_memory_by_t(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
) -> Result<Option<MemoryRow>, StorageError> {
    sqlx::query_as::<_, MemoryRow>(
        "SELECT handle, t, kind::text, owner_id, source_id, ingest_key, origins, refs, goal_refs
           FROM proxima_core.memory
          WHERE t = $1",
    )
    .bind(t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(internal)
}

/// Read a series' current head row, inside the caller's transaction.
/// Test-only for the same reason as [`read_memory_by_t`].
#[cfg(test)]
pub(crate) async fn read_memory_head(
    tx: &mut Transaction<'_, Postgres>,
    handle: Uuid,
) -> Result<Option<MemoryRow>, StorageError> {
    sqlx::query_as::<_, MemoryRow>(
        "SELECT m.handle, m.t, m.kind::text, m.owner_id, m.source_id, m.ingest_key,
                m.origins, m.refs, m.goal_refs
           FROM proxima_core.memory_head h
           JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
          WHERE h.handle = $1",
    )
    .bind(handle)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(internal)
}

// The admission-row suite. Crate-internal for the same reason the verb is:
// it reads the row back through `read_memory_by_t` / `read_memory_head`
// inside the write's own transaction, which is not something the read ports
// offer and should not be.
#[cfg(test)]
#[path = "memory_timeseries_pg_tests.rs"]
mod memory_timeseries_pg_tests;
