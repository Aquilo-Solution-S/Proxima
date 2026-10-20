//! Persisted recall/think one-liners. Rebuildable plumbing (not Lean).

use proxima_core::{EntityKind, MemoryId, OwnerId, SidecarPayload, StorageError};
use sqlx::PgConnection;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;
use crate::pg_enums::PgSketchKind;

pub const SKETCH_CHARS: usize = 160;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchRow {
    pub id: MemoryId,
    pub owner: OwnerId,
    pub kind: EntityKind,
    pub text: String,
}

pub fn sketch_line(
    kind: PgSketchKind,
    rendered: Option<&str>,
    payloads: &[SidecarPayload],
) -> String {
    for payload in payloads {
        if let Some(line) = payload_sketch(payload) {
            return truncate_sketch(&line);
        }
    }
    if let Some(text) = rendered.map(str::trim).filter(|text| !text.is_empty()) {
        let first = text.lines().next().unwrap_or(text);
        return truncate_sketch(first);
    }
    truncate_sketch(kind.label())
}

pub fn payload_sketch(payload: &SidecarPayload) -> Option<String> {
    let value = payload.to_protocol_json().ok()?;
    ["title", "claim", "body", "text"]
        .iter()
        .find_map(|key| {
            value
                .get(*key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
        })
        .map(ToOwned::to_owned)
}

pub fn truncate_sketch(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "sketch".into();
    }
    if trimmed.chars().count() <= SKETCH_CHARS {
        return trimmed.to_string();
    }
    trimmed.chars().take(SKETCH_CHARS).collect()
}

pub async fn upsert_sketch(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    t: Uuid,
    kind: PgSketchKind,
    text: &str,
) -> Result<(), StorageError> {
    let text = truncate_sketch(text);
    sqlx::query(
        "INSERT INTO proxima_core.sketch (t, owner_id, kind, text)
         VALUES ($1, $2, $3::proxima_core.sketch_kind, $4)
         ON CONFLICT (t) DO UPDATE
           SET owner_id = EXCLUDED.owner_id,
               kind = EXCLUDED.kind,
               text = EXCLUDED.text",
    )
    .bind(t)
    .bind(owner_id)
    .bind(kind)
    .bind(&text)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(())
}

/// A sketch row is readable only through its parent: a live memory, or a
/// goal, with the same `t`. The sketch outlives a forget (a restore needs the
/// text back), so a read that started from a caller-supplied id must not
/// return a forgotten memory's sketch. The explicit `forgotten_at IS NULL`
/// keeps that true in the lifecycle scope too; owner RLS on `memory` and
/// `goal` decides what the reader may see at all.
macro_rules! sketch_parent_readable {
    () => {
        "(EXISTS (SELECT 1 FROM proxima_core.memory m /* FORGET-POLICY: live-only (a forgotten memory's sketch is not readable) */ \
           WHERE m.t = s.t AND m.forgotten_at IS NULL) \
          OR EXISTS (SELECT 1 FROM proxima_core.goal g WHERE g.t = s.t))"
    };
}

/// The persisted sketch text of each memory in `ids` that has one: the
/// memory's own text, the display body a read falls back to when the schema
/// names no body field and the payload has no `body`/`text` key. Callers
/// pass ids that are already admitted; this lookup authorizes nothing.
pub async fn load_sketch_texts_on_connection(
    connection: &mut PgConnection,
    ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, String>, StorageError> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let rows: Vec<(Uuid, String)> = sqlx::query_as(concat!(
        "SELECT s.t, s.text FROM proxima_core.sketch s WHERE s.t = ANY($1::uuid[]) AND ",
        sketch_parent_readable!(),
    ))
    .bind(ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(map_err)?;
    Ok(rows.into_iter().collect())
}

pub async fn load_sketches_on_connection(
    connection: &mut PgConnection,
    read_owners: &[OwnerId],
    ids: &[MemoryId],
) -> Result<Vec<SketchRow>, StorageError> {
    if ids.is_empty() || read_owners.is_empty() {
        return Ok(Vec::new());
    }
    let ts: Vec<Uuid> = ids.iter().copied().map(MemoryId::into_inner).collect();
    let owners: Vec<Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerId::into_inner)
        .collect();
    let rows: Vec<(Uuid, Uuid, PgSketchKind, String)> = sqlx::query_as(concat!(
        "SELECT s.t, s.owner_id, s.kind, s.text FROM proxima_core.sketch s JOIN proxima_core.owners o ON o.owner_id = s.owner_id WHERE s.t = ANY($1::uuid[]) AND s.owner_id = ANY($2::uuid[]) AND ",
        sketch_parent_readable!(),
    )).bind(&ts).bind(&owners).fetch_all(&mut *connection).await.map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(|(t, oid, kind, text)| SketchRow {
            id: MemoryId::new(t),
            owner: OwnerId::new(oid),
            kind: kind.into(),
            text,
        })
        .collect())
}
