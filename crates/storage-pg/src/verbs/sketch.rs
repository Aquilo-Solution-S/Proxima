//! Persisted recall/think one-liners. Rebuildable plumbing (not Lean).

use proxima_core::{EntityKind, MemoryId, OwnerRef, SidecarPayload, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;

pub const SKETCH_CHARS: usize = 160;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchRow {
    pub id: MemoryId,
    pub owner: OwnerRef,
    pub kind: EntityKind,
    pub text: String,
}

pub fn sketch_line(kind: &str, rendered: Option<&str>, payloads: &[SidecarPayload]) -> String {
    for payload in payloads {
        if let Some(line) = payload_sketch(payload) {
            return truncate_sketch(&line);
        }
    }
    if let Some(text) = rendered.map(str::trim).filter(|text| !text.is_empty()) {
        let first = text.lines().next().unwrap_or(text);
        return truncate_sketch(first);
    }
    truncate_sketch(kind)
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
    kind: &str,
    text: &str,
) -> Result<(), StorageError> {
    let kind = sketch_kind_sql(kind)?;
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

pub async fn delete_sketch(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
) -> Result<(), StorageError> {
    sqlx::query("DELETE FROM proxima_core.sketch WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(())
}

pub async fn load_sketches(
    pool: &sqlx::PgPool,
    read_owners: &[OwnerRef],
    ids: &[MemoryId],
) -> Result<Vec<SketchRow>, StorageError> {
    if ids.is_empty() || read_owners.is_empty() {
        return Ok(Vec::new());
    }
    let ts: Vec<Uuid> = ids.iter().copied().map(MemoryId::into_inner).collect();
    let owner_ids: Vec<Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let rows: Vec<(Uuid, Uuid, String, String, String)> = sqlx::query_as(
        "SELECT s.t, s.owner_id, o.kind::text, s.kind::text, s.text
           FROM proxima_core.sketch s
           JOIN proxima_core.owners o ON o.owner_id = s.owner_id
          WHERE s.t = ANY($1::uuid[])
            AND s.owner_id = ANY($2::uuid[])",
    )
    .bind(&ts)
    .bind(&owner_ids)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .filter_map(|(t, owner_id, owner_kind, kind, text)| {
            Some(SketchRow {
                id: MemoryId::new(t),
                owner: owner_from(owner_kind.as_str(), owner_id)?,
                kind: parse_kind(&kind)?,
                text,
            })
        })
        .collect())
}

fn sketch_kind_sql(kind: &str) -> Result<&'static str, StorageError> {
    match kind {
        "fact" | "Fact" => Ok("fact"),
        "abstraction" | "Abstraction" => Ok("abstraction"),
        "perspective" | "Perspective" => Ok("perspective"),
        "goal" | "Goal" => Ok("goal"),
        other => Err(StorageError::ConstraintViolation(format!(
            "unknown sketch kind {other}"
        ))),
    }
}

fn parse_kind(kind: &str) -> Option<EntityKind> {
    match kind {
        "fact" => Some(EntityKind::Fact),
        "abstraction" => Some(EntityKind::Abstraction),
        "perspective" => Some(EntityKind::Perspective),
        "goal" => Some(EntityKind::Goal),
        _ => None,
    }
}

fn owner_from(kind: &str, owner_id: Uuid) -> Option<OwnerRef> {
    match kind {
        "personal" => Some(OwnerRef::Personal(proxima_core::UserId::new(owner_id))),
        "group" => Some(OwnerRef::Group(proxima_core::GroupId::new(owner_id))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_title_beats_rendered_body() {
        let payload = SidecarPayload::abstraction(proxima_core::AgentDerivationV1 {
            title: "Pattern".into(),
            body: "long body".into(),
            tags: vec![],
            idempotency_key: None,
            source_memory_ids: vec![],
            model_id: "m".into(),
            client_name: "c".into(),
            client_version: "0".into(),
        });
        assert_eq!(
            sketch_line("abstraction", Some("long body"), &[payload]),
            "Pattern"
        );
    }

    #[test]
    fn rendered_first_line_when_no_sidecar() {
        assert_eq!(sketch_line("fact", Some("Title\n\nbody"), &[]), "Title");
    }
}
