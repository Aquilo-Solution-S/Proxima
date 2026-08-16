//! Forget / hydrate / erase. UML §5c.
#![allow(clippy::missing_errors_doc, clippy::doc_markdown)]

use proxima_core::{Owner, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;

pub const COLD_FORMAT_VERSION: u8 = 1;

#[must_use]
pub fn cold_object_key(owner_hash: &str, handle: Uuid, t: Uuid) -> String {
    format!("cold/{owner_hash}/{handle}/{t}")
}

pub trait ColdStore: Send + Sync {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError>;
    fn get(&self, key: &str) -> Result<Vec<u8>, StorageError>;
    fn delete(&self, key: &str) -> Result<(), StorageError>;
}

#[derive(Default)]
pub struct MemoryColdStore {
    inner: std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>>,
}

impl std::fmt::Debug for MemoryColdStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryColdStore").finish_non_exhaustive()
    }
}

impl ColdStore for MemoryColdStore {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        self.inner
            .lock()
            .expect("cold store")
            .insert(key.to_owned(), bytes.to_vec());
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.inner
            .lock()
            .expect("cold store")
            .get(key)
            .cloned()
            .ok_or(StorageError::NotFound)
    }

    fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.inner.lock().expect("cold store").remove(key);
        Ok(())
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct HotRow {
    handle: Uuid,
    t: Uuid,
    kind: String,
    owner_id: Uuid,
    source_id: Option<String>,
    ingest_key: Option<String>,
    blob_id: Option<Uuid>,
    origins: Vec<Uuid>,
    refs: Vec<Uuid>,
}

pub async fn forget_memory(
    tx: &mut Transaction<'_, Postgres>,
    cold: &dyn ColdStore,
    object_key: &str,
    t: Uuid,
) -> Result<(), StorageError> {
    let row: HotRow = sqlx::query_as(
        "SELECT handle, t, kind::text, owner_id, source_id, ingest_key, blob_id, origins, refs
           FROM proxima_core.memory
          WHERE t = $1
          FOR UPDATE",
    )
    .bind(t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    .ok_or(StorageError::NotFound)?;

    let mut payload = vec![COLD_FORMAT_VERSION];
    payload.extend(row.handle.as_bytes());
    payload.extend(row.t.as_bytes());
    payload.extend(row.kind.as_bytes());
    payload.push(0);
    cold.put(object_key, &payload)?;

    sqlx::query(
        "INSERT INTO proxima_core.cooled (t, handle, owner_id, kind, object_key)
         VALUES ($1, $2, $3, $4::proxima_core.memory_kind, $5)",
    )
    .bind(row.t)
    .bind(row.handle)
    .bind(row.owner_id)
    .bind(&row.kind)
    .bind(object_key)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        "DELETE FROM proxima_core.citation_uploaded_blob_page_span_v1
          WHERE citation_mapping_id IN (
                SELECT citation_mapping_id
                  FROM proxima_core.citation_mappings
                 WHERE memory_id = $1
          )",
    )
    .bind(t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.citation_mappings WHERE memory_id = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;

    let remaining: Option<Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_core.memory WHERE handle = $1 ORDER BY t DESC LIMIT 1",
    )
    .bind(row.handle)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    sqlx::query("UPDATE proxima_core.memory_head SET t = $2 WHERE handle = $1")
        .bind(row.handle)
        .bind(remaining.unwrap_or(row.t))
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.announce (owner_id, op, entity, handle, t)
         VALUES ($1, 'forget', 'memory', $2, $3)",
    )
    .bind(row.owner_id)
    .bind(row.handle)
    .bind(row.t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    let _ = (row.source_id, row.ingest_key, row.blob_id, row.origins, row.refs);
    Ok(())
}

pub async fn hydrate_memory(
    tx: &mut Transaction<'_, Postgres>,
    cold: &dyn ColdStore,
    t: Uuid,
) -> Result<(), StorageError> {
    let (handle, owner_id, kind, object_key): (Uuid, Uuid, String, String) = sqlx::query_as(
        "SELECT handle, owner_id, kind::text, object_key
           FROM proxima_core.cooled WHERE t = $1",
    )
    .bind(t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    .ok_or(StorageError::NotFound)?;

    let _bytes = cold.get(&object_key)?;

    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, origins, refs)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, '{}', '{}')",
    )
    .bind(handle)
    .bind(t)
    .bind(&kind)
    .bind(owner_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query("DELETE FROM proxima_core.cooled WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;

    sqlx::query("UPDATE proxima_core.memory_head SET t = $2 WHERE handle = $1")
        .bind(handle)
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.announce (owner_id, op, entity, handle, t)
         VALUES ($1, 'append', 'memory', $2, $3)",
    )
    .bind(owner_id)
    .bind(handle)
    .bind(t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(())
}

pub async fn erase_memory(
    tx: &mut Transaction<'_, Postgres>,
    cold: &dyn ColdStore,
    owner: &Owner,
    t: Uuid,
) -> Result<(), StorageError> {
    if matches!(owner, Owner::World) {
        return Err(StorageError::ConstraintViolation(
            "World is never abandoned".into(),
        ));
    }
    if let Ok(key) = sqlx::query_scalar::<_, String>(
        "SELECT object_key FROM proxima_core.cooled WHERE t = $1 AND owner_id = $2",
    )
    .bind(t)
    .bind(owner.stored_owner_id())
    .fetch_one(tx.as_mut())
    .await
    {
        let _ = cold.delete(&key);
        sqlx::query("DELETE FROM proxima_core.cooled WHERE t = $1")
            .bind(t)
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
    }
    sqlx::query(
        "DELETE FROM proxima_core.citation_uploaded_blob_page_span_v1
          WHERE citation_mapping_id IN (
                SELECT citation_mapping_id
                  FROM proxima_core.citation_mappings
                 WHERE memory_id = $1
          )",
    )
    .bind(t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.citation_mappings WHERE memory_id = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1 AND owner_id = $2")
        .bind(t)
        .bind(owner.stored_owner_id())
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.ingest_keys WHERE t = $1 AND owner_id = $2")
        .bind(t)
        .bind(owner.stored_owner_id())
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query(
        "INSERT INTO proxima_core.announce (owner_id, op, entity, handle, t)
         SELECT $1, 'erase', 'memory', $2, $3",
    )
    .bind(owner.stored_owner_id())
    .bind(t)
    .bind(t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(())
}
