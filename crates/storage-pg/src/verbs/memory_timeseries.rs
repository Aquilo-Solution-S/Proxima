//! Timeseries Fact write/read (v0.0.8). UML §8.
#![allow(
    clippy::missing_errors_doc,
    clippy::doc_markdown,
    clippy::too_many_lines
)]

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
}

/// One txn: owners upsert + ingest_keys + memory_head + memory + announce(append).
///
/// `sidecar_tables` is the declared set forget will dump/delete — tables
/// actually inserted for this `t`, never the global registry.
/// `content_id` is the owner-scoped payload (required for A/P).
pub async fn ingest_fact_timeseries(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    draft: &FactWriteCommand,
    sidecar_tables: &[String],
    content_id: Option<Uuid>,
) -> Result<FactIngestOutcome, StorageError> {
    crate::access::owner_columns::reject_world_write_owner(owner)?;
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

    let mut origins: Vec<Uuid> = draft
        .derived_from
        .iter()
        .filter_map(|ep| ep.memory_id().map(proxima_core::MemoryId::into_inner))
        .collect();
    let mut refs = draft.refs.clone();
    // CHECK memory_fact_origins_chk: Facts cannot carry origins. Pins
    // a Fact declares (activation, evidence, prior request) live in refs.
    if kind == "fact" {
        for id in origins.drain(..) {
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
            let replay_handle: Uuid =
                sqlx::query_scalar("SELECT handle FROM proxima_core.memory WHERE t = $1")
                    .bind(replay_t)
                    .fetch_one(tx.as_mut())
                    .await
                    .map_err(map_err)?;
            return Ok(FactIngestOutcome {
                receipt_id: None,
                memory_id: MemoryId::new(replay_t),
                change_event_seq: replay_t,
                idempotent_replay: true,
                cited_object_id: None,
                handle: replay_handle,
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
             content_id, origins, refs, sidecar_tables)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
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
    .bind(&origins)
    .bind(&refs)
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

pub async fn read_memory_by_t(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
) -> Result<Option<MemoryRow>, StorageError> {
    sqlx::query_as::<_, MemoryRow>(
        "SELECT handle, t, kind::text, owner_id, source_id, ingest_key, origins, refs
           FROM proxima_core.memory
          WHERE t = $1",
    )
    .bind(t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(internal)
}

pub async fn read_memory_head(
    tx: &mut Transaction<'_, Postgres>,
    handle: Uuid,
) -> Result<Option<MemoryRow>, StorageError> {
    sqlx::query_as::<_, MemoryRow>(
        "SELECT m.handle, m.t, m.kind::text, m.owner_id, m.source_id, m.ingest_key,
                m.origins, m.refs
           FROM proxima_core.memory_head h
           JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
          WHERE h.handle = $1",
    )
    .bind(handle)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(internal)
}
