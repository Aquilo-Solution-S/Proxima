//! Owner-scoped Content upsert (v0.0.8).

use proxima_core::{SidecarPayload, StorageError, canonical_json_bytes};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;

/// Blake3-32 of canonical JSON for each payload, in `schema_id` order.
pub fn hash_sidecar_payloads(payloads: &[SidecarPayload]) -> Result<[u8; 32], StorageError> {
    if payloads.is_empty() {
        return Err(StorageError::ConstraintViolation(
            "content hash requires at least one sidecar payload".into(),
        ));
    }
    let mut parts = Vec::with_capacity(payloads.len());
    for payload in payloads {
        let json = payload
            .to_protocol_json()
            .map_err(StorageError::ConstraintViolation)?;
        parts.push((
            payload.schema_id.as_str().to_owned(),
            canonical_json_bytes(&json),
        ));
    }
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proxima-content-v1\0");
    for (schema, bytes) in &parts {
        hasher.update(schema.as_bytes());
        hasher.update(b"\0");
        hasher.update(&(u64::try_from(bytes.len()).unwrap_or(0)).to_le_bytes());
        hasher.update(bytes);
        hasher.update(b"\0");
    }
    Ok(*hasher.finalize().as_bytes())
}

/// Insert or reuse `(owner, schema, hash)`.
pub async fn ensure_content(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    schema_id: &str,
    content_hash: &[u8; 32],
) -> Result<Uuid, StorageError> {
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, $2, $3)
         ON CONFLICT (owner_id, schema_id, content_hash) DO NOTHING
         RETURNING content_id",
    )
    .bind(owner_id)
    .bind(schema_id)
    .bind(content_hash.as_slice())
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    if let Some(id) = inserted {
        return Ok(id);
    }
    sqlx::query_scalar(
        "SELECT content_id FROM proxima_core.content
          WHERE owner_id = $1 AND schema_id = $2 AND content_hash = $3",
    )
    .bind(owner_id)
    .bind(schema_id)
    .bind(content_hash.as_slice())
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)
}

/// A/P without a hashed sidecar still get an owner-scoped Content row.
pub async fn ensure_text_content(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    schema_id: &str,
    text: &[u8],
) -> Result<Uuid, StorageError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proxima-content-text-v1\0");
    hasher.update(schema_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(text);
    ensure_content(tx, owner_id, schema_id, hasher.finalize().as_bytes()).await
}

pub async fn ensure_content_from_payloads(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    schema_id: &str,
    payloads: &[SidecarPayload],
) -> Result<Option<Uuid>, StorageError> {
    if payloads.is_empty() {
        return Ok(None);
    }
    let hash = hash_sidecar_payloads(payloads)?;
    Ok(Some(ensure_content(tx, owner_id, schema_id, &hash).await?))
}

/// Drop Content rows no hot or cooled admission names.
pub async fn gc_unreferenced_content(
    tx: &mut Transaction<'_, Postgres>,
    content_id: Uuid,
) -> Result<(), StorageError> {
    sqlx::query(
        "DELETE FROM proxima_core.content c
          WHERE c.content_id = $1
            AND NOT EXISTS (
                    SELECT 1 FROM proxima_core.memory m WHERE m.content_id = c.content_id
                )
            AND NOT EXISTS (
                    SELECT 1 FROM proxima_core.cooled k WHERE k.content_id = c.content_id
                )",
    )
    .bind(content_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(())
}
