//! Forget / hydrate / erase. UML §5c.
#![allow(clippy::missing_errors_doc, clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::{ColdObjectStore, Owner, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;

pub const COLD_FORMAT_VERSION: u8 = 2;

#[must_use]
pub fn cold_object_key(owner_hash: &str, handle: Uuid, t: Uuid) -> String {
    format!("cold/{owner_hash}/{handle}/{t}")
}

/// Same owner hash as `proxima-blob-s3` (`proxima-owner-s3-key-v1`).
#[must_use]
pub fn owner_hash_hex(owner: &Owner) -> String {
    let kind = proxima_core::OwnerRefKind::of(owner);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proxima-owner-s3-key-v1\0");
    hasher.update(kind.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(owner.stable_key_uuid().as_bytes());
    hex_lower(hasher.finalize().as_bytes())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
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

#[async_trait::async_trait]
impl ColdObjectStore for MemoryColdStore {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        self.inner
            .lock()
            .expect("cold store")
            .insert(key.to_owned(), bytes.to_vec());
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.inner
            .lock()
            .expect("cold store")
            .get(key)
            .cloned()
            .ok_or(StorageError::NotFound)
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
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

#[derive(Debug, Clone)]
struct ColdRecord {
    row: HotRow,
    schema_id: String,
    sidecar_kind: u8,
    sidecar: Vec<u8>,
    /// Model ids that had vectors. UML §5c: vectors stay out of the object;
    /// hydrate enqueues embed jobs for these ids.
    embed_models: Vec<String>,
}

fn encode_record(rec: &ColdRecord) -> Vec<u8> {
    let mut out = vec![COLD_FORMAT_VERSION];
    write_uuid(&mut out, rec.row.handle);
    write_uuid(&mut out, rec.row.t);
    write_str(&mut out, &rec.row.kind);
    write_uuid(&mut out, rec.row.owner_id);
    write_opt_str(&mut out, rec.row.source_id.as_deref());
    write_opt_str(&mut out, rec.row.ingest_key.as_deref());
    write_opt_uuid(&mut out, rec.row.blob_id);
    write_uuid_list(&mut out, &rec.row.origins);
    write_uuid_list(&mut out, &rec.row.refs);
    write_str(&mut out, &rec.schema_id);
    out.push(rec.sidecar_kind);
    write_bytes(&mut out, &rec.sidecar);
    write_str_list(&mut out, &rec.embed_models);
    out
}

fn decode_record(bytes: &[u8]) -> Result<ColdRecord, StorageError> {
    let mut i = 0;
    let version = read_u8(bytes, &mut i)?;
    if version != 1 && version != COLD_FORMAT_VERSION {
        return Err(StorageError::Internal(format!(
            "unknown cold format {version}"
        )));
    }
    let row = HotRow {
        handle: read_uuid(bytes, &mut i)?,
        t: read_uuid(bytes, &mut i)?,
        kind: read_str(bytes, &mut i)?,
        owner_id: read_uuid(bytes, &mut i)?,
        source_id: read_opt_str(bytes, &mut i)?,
        ingest_key: read_opt_str(bytes, &mut i)?,
        blob_id: read_opt_uuid(bytes, &mut i)?,
        origins: read_uuid_list(bytes, &mut i)?,
        refs: read_uuid_list(bytes, &mut i)?,
    };
    let schema_id = read_str(bytes, &mut i)?;
    let sidecar_kind = read_u8(bytes, &mut i)?;
    let sidecar = read_bytes(bytes, &mut i)?;
    let embed_models = if version >= 2 {
        read_str_list(bytes, &mut i)?
    } else {
        Vec::new()
    };
    Ok(ColdRecord {
        row,
        schema_id,
        sidecar_kind,
        sidecar,
        embed_models,
    })
}

fn write_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn write_uuid(out: &mut Vec<u8>, value: Uuid) {
    out.extend_from_slice(value.as_bytes());
}

fn write_str(out: &mut Vec<u8>, value: &str) {
    write_bytes(out, value.as_bytes());
}

fn write_str_list(out: &mut Vec<u8>, values: &[String]) {
    write_u16(out, u16::try_from(values.len()).unwrap_or(u16::MAX));
    for value in values {
        write_str(out, value);
    }
}

fn read_str_list(bytes: &[u8], i: &mut usize) -> Result<Vec<String>, StorageError> {
    let n = usize::from(read_u16(bytes, i)?);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(read_str(bytes, i)?);
    }
    Ok(out)
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) {
    let len = u16::try_from(value.len()).unwrap_or(u16::MAX);
    write_u16(out, len);
    out.extend_from_slice(&value[..usize::from(len)]);
}

fn write_opt_str(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        None => out.push(0),
        Some(text) => {
            out.push(1);
            write_str(out, text);
        }
    }
}

fn write_opt_uuid(out: &mut Vec<u8>, value: Option<Uuid>) {
    match value {
        None => out.push(0),
        Some(id) => {
            out.push(1);
            write_uuid(out, id);
        }
    }
}

fn write_uuid_list(out: &mut Vec<u8>, values: &[Uuid]) {
    write_u16(out, u16::try_from(values.len()).unwrap_or(u16::MAX));
    for id in values {
        write_uuid(out, *id);
    }
}

fn read_u8(bytes: &[u8], i: &mut usize) -> Result<u8, StorageError> {
    let b = *bytes.get(*i).ok_or_else(|| StorageError::Internal("cold eof".into()))?;
    *i += 1;
    Ok(b)
}

fn read_u16(bytes: &[u8], i: &mut usize) -> Result<u16, StorageError> {
    let hi = read_u8(bytes, i)?;
    let lo = read_u8(bytes, i)?;
    Ok(u16::from_be_bytes([hi, lo]))
}

fn read_uuid(bytes: &[u8], i: &mut usize) -> Result<Uuid, StorageError> {
    let end = i.saturating_add(16);
    let slice = bytes
        .get(*i..end)
        .ok_or_else(|| StorageError::Internal("cold eof uuid".into()))?;
    *i = end;
    Uuid::from_slice(slice).map_err(|err| StorageError::Internal(err.to_string()))
}

fn read_bytes(bytes: &[u8], i: &mut usize) -> Result<Vec<u8>, StorageError> {
    let len = usize::from(read_u16(bytes, i)?);
    let end = i.saturating_add(len);
    let slice = bytes
        .get(*i..end)
        .ok_or_else(|| StorageError::Internal("cold eof bytes".into()))?;
    *i = end;
    Ok(slice.to_vec())
}

fn read_str(bytes: &[u8], i: &mut usize) -> Result<String, StorageError> {
    String::from_utf8(read_bytes(bytes, i)?)
        .map_err(|err| StorageError::Internal(err.to_string()))
}

fn read_opt_str(bytes: &[u8], i: &mut usize) -> Result<Option<String>, StorageError> {
    match read_u8(bytes, i)? {
        0 => Ok(None),
        _ => Ok(Some(read_str(bytes, i)?)),
    }
}

fn read_opt_uuid(bytes: &[u8], i: &mut usize) -> Result<Option<Uuid>, StorageError> {
    match read_u8(bytes, i)? {
        0 => Ok(None),
        _ => Ok(Some(read_uuid(bytes, i)?)),
    }
}

fn read_uuid_list(bytes: &[u8], i: &mut usize) -> Result<Vec<Uuid>, StorageError> {
    let n = usize::from(read_u16(bytes, i)?);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(read_uuid(bytes, i)?);
    }
    Ok(out)
}

const SIDECAR_NONE: u8 = 0;
const SIDECAR_NOTE: u8 = 1;
const SIDECAR_UTTERANCE: u8 = 2;
const SIDECAR_DERIVATION: u8 = 3;
const SIDECAR_INTERPRET: u8 = 4;

async fn load_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
) -> Result<(u8, Vec<u8>), StorageError> {
    if let Some((title, body, tags)) = sqlx::query_as::<_, (String, String, Vec<String>)>(
        "SELECT title, body, tags FROM proxima_core.agent_note_v1 WHERE t = $1",
    )
    .bind(t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    {
        let mut buf = Vec::new();
        write_str(&mut buf, &title);
        write_str(&mut buf, &body);
        write_u16(&mut buf, u16::try_from(tags.len()).unwrap_or(0));
        for tag in tags {
            write_str(&mut buf, &tag);
        }
        return Ok((SIDECAR_NOTE, buf));
    }
    if let Some((speaker, conversation_id, text)) = sqlx::query_as::<_, (String, String, String)>(
        "SELECT speaker, conversation_id, text FROM proxima_core.utterance_v1 WHERE t = $1",
    )
    .bind(t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    {
        let mut buf = Vec::new();
        write_str(&mut buf, &speaker);
        write_str(&mut buf, &conversation_id);
        write_str(&mut buf, &text);
        return Ok((SIDECAR_UTTERANCE, buf));
    }
    if let Some((title, body)) = sqlx::query_as::<_, (String, String)>(
        "SELECT title, body FROM proxima_core.agent_derivation_v1 WHERE t = $1",
    )
    .bind(t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    {
        let mut buf = Vec::new();
        write_str(&mut buf, &title);
        write_str(&mut buf, &body);
        return Ok((SIDECAR_DERIVATION, buf));
    }
    if let Some(claim) = sqlx::query_scalar::<_, String>(
        "SELECT claim FROM proxima_core.interpretation_v1 WHERE t = $1",
    )
    .bind(t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    {
        let mut buf = Vec::new();
        write_str(&mut buf, &claim);
        return Ok((SIDECAR_INTERPRET, buf));
    }
    Ok((SIDECAR_NONE, Vec::new()))
}

async fn restore_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
    kind: u8,
    bytes: &[u8],
) -> Result<(), StorageError> {
    let mut i = 0;
    match kind {
        SIDECAR_NONE => Ok(()),
        SIDECAR_NOTE => {
            let title = read_str(bytes, &mut i)?;
            let body = read_str(bytes, &mut i)?;
            let n = usize::from(read_u16(bytes, &mut i)?);
            let mut tags = Vec::with_capacity(n);
            for _ in 0..n {
                tags.push(read_str(bytes, &mut i)?);
            }
            sqlx::query(
                "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
                 VALUES ($1, $1, $2, $3, $4)",
            )
            .bind(t)
            .bind(title)
            .bind(body)
            .bind(&tags)
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
            Ok(())
        }
        SIDECAR_UTTERANCE => {
            let speaker = read_str(bytes, &mut i)?;
            let conversation_id = read_str(bytes, &mut i)?;
            let text = read_str(bytes, &mut i)?;
            sqlx::query(
                "INSERT INTO proxima_core.utterance_v1 (t, speaker, conversation_id, text)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(t)
            .bind(speaker)
            .bind(conversation_id)
            .bind(text)
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
            Ok(())
        }
        SIDECAR_DERIVATION => {
            let title = read_str(bytes, &mut i)?;
            let body = read_str(bytes, &mut i)?;
            sqlx::query(
                "INSERT INTO proxima_core.agent_derivation_v1
                    (t, title, body, tags, source_memory_ids,
                     model_id, client_name, client_version)
                 VALUES ($1, $2, $3, ARRAY[]::text[], ARRAY[]::uuid[],
                         'hydrate', 'hydrate', 'v1')",
            )
            .bind(t)
            .bind(title)
            .bind(body)
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
            Ok(())
        }
        SIDECAR_INTERPRET => {
            let claim = read_str(bytes, &mut i)?;
            sqlx::query(
                "INSERT INTO proxima_core.interpretation_v1
                    (t, claim, confidence, subject_memory_ids, subject_kinds,
                     model_id, client_name, client_version)
                 VALUES ($1, $2, 0, ARRAY[]::uuid[], ARRAY[]::proxima_core.interpretation_subject_kind[],
                         'hydrate', 'hydrate', 'v1')",
            )
            .bind(t)
            .bind(claim)
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
            Ok(())
        }
        other => Err(StorageError::Internal(format!(
            "unknown sidecar kind {other}"
        ))),
    }
}

async fn enqueue_embed_jobs(
    tx: &mut Transaction<'_, Postgres>,
    rec: &ColdRecord,
) -> Result<(), StorageError> {
    if rec.embed_models.is_empty() {
        return Ok(());
    }
    let kind = match rec.row.kind.as_str() {
        "abstraction" => proxima_core::EntityKind::Abstraction,
        "perspective" => proxima_core::EntityKind::Perspective,
        _ => proxima_core::EntityKind::Fact,
    };
    let owner_kind: String = sqlx::query_scalar(
        "SELECT kind::text FROM proxima_core.owners WHERE owner_id = $1",
    )
    .bind(rec.row.owner_id)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;
    let owner_kind = match owner_kind.as_str() {
        "world" => proxima_core::OwnerRefKind::World,
        "group" => proxima_core::OwnerRefKind::Group,
        _ => proxima_core::OwnerRefKind::Personal,
    };
    for model_id in &rec.embed_models {
        crate::verbs::fact_embeddings::enqueue_embedding_job_in_tx(
            tx,
            owner_kind,
            Some(rec.row.owner_id),
            kind,
            rec.row.t,
            model_id,
        )
        .await?;
    }
    Ok(())
}

async fn delete_memory_dependents(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
) -> Result<(), StorageError> {
    sqlx::query("DELETE FROM proxima_core.agent_note_v1 WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.utterance_v1 WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.agent_derivation_v1 WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.interpretation_v1 WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.mcp_call_logged_v1 WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.embedding_jobs WHERE entity_id = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.embedding_heads WHERE entity_id = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.embeddings WHERE entity_id = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(())
}

pub async fn forget_memory(
    tx: &mut Transaction<'_, Postgres>,
    cold: &dyn ColdObjectStore,
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
    let schema_id: String = sqlx::query_scalar(
        "SELECT schema_id FROM proxima_core.memory_head WHERE handle = $1",
    )
    .bind(row.handle)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;
    let (sidecar_kind, sidecar) = load_sidecar(tx, t).await?;
    let embed_models: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT model_id FROM proxima_core.embeddings WHERE entity_id = $1",
    )
    .bind(t)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    let payload = encode_record(&ColdRecord {
        row: row.clone(),
        schema_id,
        sidecar_kind,
        sidecar,
        embed_models,
    });
    cold.put(object_key, &payload).await?;

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

    delete_memory_dependents(tx, t).await?;
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
    Ok(())
}

pub async fn hydrate_memory(
    tx: &mut Transaction<'_, Postgres>,
    cold: &dyn ColdObjectStore,
    t: Uuid,
) -> Result<(), StorageError> {
    let object_key: String = sqlx::query_scalar(
        "SELECT object_key FROM proxima_core.cooled WHERE t = $1",
    )
    .bind(t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    .ok_or(StorageError::NotFound)?;

    let rec = decode_record(&cold.get(&object_key).await?)?;
    sqlx::query(
        "INSERT INTO proxima_core.memory
            (handle, t, kind, owner_id, source_id, ingest_key, blob_id, origins, refs)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, $5, $6, $7, $8, $9)",
    )
    .bind(rec.row.handle)
    .bind(rec.row.t)
    .bind(&rec.row.kind)
    .bind(rec.row.owner_id)
    .bind(rec.row.source_id.as_deref())
    .bind(rec.row.ingest_key.as_deref())
    .bind(rec.row.blob_id)
    .bind(&rec.row.origins)
    .bind(&rec.row.refs)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    restore_sidecar(tx, t, rec.sidecar_kind, &rec.sidecar).await?;
    enqueue_embed_jobs(tx, &rec).await?;

    sqlx::query("DELETE FROM proxima_core.cooled WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;

    sqlx::query("UPDATE proxima_core.memory_head SET t = $2 WHERE handle = $1")
        .bind(rec.row.handle)
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;

    sqlx::query(
        "INSERT INTO proxima_core.announce (owner_id, op, entity, handle, t)
         VALUES ($1, 'append', 'memory', $2, $3)",
    )
    .bind(rec.row.owner_id)
    .bind(rec.row.handle)
    .bind(t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(())
}

pub async fn erase_memory(
    tx: &mut Transaction<'_, Postgres>,
    cold: &dyn ColdObjectStore,
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
        let _ = cold.delete(&key).await;
        sqlx::query("DELETE FROM proxima_core.cooled WHERE t = $1")
            .bind(t)
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
    }
    delete_memory_dependents(tx, t).await?;
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
