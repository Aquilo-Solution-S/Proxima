//! Forget / hydrate / erase. UML §5c.
#![allow(
    clippy::missing_errors_doc,
    clippy::doc_markdown,
    clippy::too_many_lines
)]

use proxima_core::{ColdObjectStore, Owner, StorageError};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::sidecars::PgSidecarRegistryFrozen;

pub const COLD_FORMAT_VERSION: u8 = 4;

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

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct HotRow {
    handle: Uuid,
    t: Uuid,
    kind: String,
    owner_id: Uuid,
    schema_id: String,
    source_id: Option<String>,
    ingest_key: Option<String>,
    blob_id: Option<Uuid>,
    origins: Vec<Uuid>,
    refs: Vec<Uuid>,
    sidecar_tables: Vec<String>,
    content_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdRecord {
    row: HotRow,
    schema_id: String,
    sidecar_dumps: Vec<(String, String)>,
    /// Model ids that had vectors. UML §5c: vectors stay out of the object;
    /// hydrate enqueues embed jobs for these ids.
    embed_models: Vec<String>,
    /// Exact persisted one-liner. v4+; older cold objects restore from sidecar/kind.
    sketch: Option<String>,
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
    write_u16(
        &mut out,
        u16::try_from(rec.sidecar_dumps.len()).unwrap_or(0),
    );
    for (table, json) in &rec.sidecar_dumps {
        write_str(&mut out, table);
        write_str(&mut out, json);
    }
    write_str_list(&mut out, &rec.embed_models);
    write_opt_str(&mut out, rec.sketch.as_deref());
    out
}

fn decode_record(bytes: &[u8]) -> Result<ColdRecord, StorageError> {
    let mut i = 0;
    let version = read_u8(bytes, &mut i)?;
    if !matches!(version, 1..=4) {
        return Err(StorageError::Internal(format!(
            "unknown cold format {version}"
        )));
    }
    let row = HotRow {
        handle: read_uuid(bytes, &mut i)?,
        t: read_uuid(bytes, &mut i)?,
        kind: read_str(bytes, &mut i)?,
        owner_id: read_uuid(bytes, &mut i)?,
        schema_id: String::new(),
        source_id: read_opt_str(bytes, &mut i)?,
        ingest_key: read_opt_str(bytes, &mut i)?,
        blob_id: read_opt_uuid(bytes, &mut i)?,
        origins: read_uuid_list(bytes, &mut i)?,
        refs: read_uuid_list(bytes, &mut i)?,
        sidecar_tables: Vec::new(),
        content_id: None,
    };
    let schema_id = read_str(bytes, &mut i)?;
    let sidecar_dumps = if version >= 3 {
        let n = usize::from(read_u16(bytes, &mut i)?);
        let mut dumps = Vec::with_capacity(n);
        for _ in 0..n {
            dumps.push((read_str(bytes, &mut i)?, read_str(bytes, &mut i)?));
        }
        dumps
    } else {
        let _kind = read_u8(bytes, &mut i)?;
        let _sidecar = read_bytes(bytes, &mut i)?;
        Vec::new()
    };
    let embed_models = if version >= 2 {
        read_str_list(bytes, &mut i)?
    } else {
        Vec::new()
    };
    let sketch = if version >= 4 {
        read_opt_str(bytes, &mut i)?
    } else {
        None
    };
    Ok(ColdRecord {
        row,
        schema_id,
        sidecar_dumps,
        embed_models,
        sketch,
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
    let b = *bytes
        .get(*i)
        .ok_or_else(|| StorageError::Internal("cold eof".into()))?;
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
    String::from_utf8(read_bytes(bytes, i)?).map_err(|err| StorageError::Internal(err.to_string()))
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

async fn dump_stamped_sidecars(
    conn: &mut PgConnection,
    sidecars: &PgSidecarRegistryFrozen,
    tables: &[String],
    t: Uuid,
) -> Result<Vec<(String, String)>, StorageError> {
    let mut dumps = Vec::new();
    for table in tables {
        if !sidecars.is_memory_sidecar_table(table) {
            return Err(StorageError::ConstraintViolation(format!(
                "stamped sidecar table {table} is not registered"
            )));
        }
        let ident = PgIdent::table(table)?;
        let sql = format!(
            "SELECT to_jsonb(s) - COALESCE((
                 SELECT array_agg(a.attname::text)
                   FROM pg_attribute a
                   JOIN pg_class c ON c.oid = a.attrelid
                   JOIN pg_namespace n ON n.oid = c.relnamespace
                  WHERE n.nspname = split_part('{tbl}', '.', 1)
                    AND c.relname = split_part('{tbl}', '.', 2)
                    AND a.attgenerated <> ''
              ), '{{}}'::text[])
               FROM {tbl} s
              WHERE s.t = $1",
            tbl = ident.as_str()
        );
        // SQL-POLICY: PgIdent
        let json: Option<serde_json::Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(t)
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_err)?;
        if let Some(json) = json {
            dumps.push((table.clone(), json.to_string()));
        }
    }
    Ok(dumps)
}

async fn load_sketch_text(
    conn: &mut PgConnection,
    t: Uuid,
) -> Result<Option<String>, StorageError> {
    sqlx::query_scalar("SELECT text FROM proxima_core.sketch WHERE t = $1")
        .bind(t)
        .fetch_optional(conn)
        .await
        .map_err(map_err)
}

async fn load_embed_models(conn: &mut PgConnection, t: Uuid) -> Result<Vec<String>, StorageError> {
    let mut models: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT model_id FROM proxima_core.embeddings WHERE entity_id = $1",
    )
    .bind(t)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_err)?;
    models.sort();
    Ok(models)
}

const HOT_ROW_SQL: &str = "SELECT handle, t, kind::text, owner_id, schema_id, source_id, ingest_key, blob_id, origins, refs, sidecar_tables, content_id
           FROM proxima_core.memory
          WHERE t = $1";

const HOT_ROW_FOR_UPDATE_SQL: &str = "SELECT handle, t, kind::text, owner_id, schema_id, source_id, ingest_key, blob_id, origins, refs, sidecar_tables, content_id
           FROM proxima_core.memory
          WHERE t = $1
          FOR UPDATE";

/// Hot row + registered sidecars + embed model ids. No row lock.
pub async fn snapshot_hot(
    conn: &mut PgConnection,
    sidecars: &PgSidecarRegistryFrozen,
    t: Uuid,
) -> Result<ColdRecord, StorageError> {
    let row: HotRow = sqlx::query_as(HOT_ROW_SQL)
        .bind(t)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_err)?
        .ok_or(StorageError::NotFound)?;
    let schema_id = row.schema_id.clone();
    let sidecar_dumps = dump_stamped_sidecars(conn, sidecars, &row.sidecar_tables, t).await?;
    let embed_models = load_embed_models(conn, t).await?;
    let sketch = load_sketch_text(conn, t).await?;
    Ok(ColdRecord {
        row,
        schema_id,
        sidecar_dumps,
        embed_models,
        sketch,
    })
}

/// `FOR UPDATE` + cooled stub + hot delete. Re-PUTs only when the locked
/// dump differs from `snapshot` (owner transfer or late sidecar).
pub async fn commit_forget(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    cold: &dyn ColdObjectStore,
    object_key: &str,
    snapshot: &ColdRecord,
) -> Result<(), StorageError> {
    let t = snapshot.row.t;
    let locked: HotRow = sqlx::query_as(HOT_ROW_FOR_UPDATE_SQL)
        .bind(t)
        .fetch_optional(tx.as_mut())
        .await
        .map_err(map_err)?
        .ok_or(StorageError::NotFound)?;
    let schema_id = locked.schema_id.clone();
    let sidecar_dumps =
        dump_stamped_sidecars(tx.as_mut(), sidecars, &locked.sidecar_tables, t).await?;
    let embed_models = load_embed_models(tx.as_mut(), t).await?;
    let sketch = load_sketch_text(tx.as_mut(), t).await?;
    let current = ColdRecord {
        row: locked,
        schema_id,
        sidecar_dumps,
        embed_models,
        sketch,
    };
    if current != *snapshot {
        cold.put(object_key, &encode_record(&current)).await?;
    }

    sqlx::query(
        "INSERT INTO proxima_core.cooled (t, handle, owner_id, kind, object_key, content_id)
         VALUES ($1, $2, $3, $4::proxima_core.memory_kind, $5, $6)",
    )
    .bind(current.row.t)
    .bind(current.row.handle)
    .bind(current.row.owner_id)
    .bind(&current.row.kind)
    .bind(object_key)
    .bind(current.row.content_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    delete_memory_dependents(tx, sidecars, &current.row.sidecar_tables, t).await?;
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sync_memory_head(tx, current.row.handle).await?;
    // Content stays while the cooled stub names it.

    sqlx::query(
        "INSERT INTO proxima_core.announce (owner_id, op, entity, handle, t)
         VALUES ($1, 'forget', 'memory', $2, $3)",
    )
    .bind(current.row.owner_id)
    .bind(current.row.handle)
    .bind(current.row.t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn insertable_columns(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
) -> Result<Vec<String>, StorageError> {
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT a.attname::text
           FROM pg_attribute a
           JOIN pg_class c ON c.oid = a.attrelid
           JOIN pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = split_part($1, '.', 1)
            AND c.relname = split_part($1, '.', 2)
            AND a.attnum > 0
            AND NOT a.attisdropped
            AND a.attgenerated = ''
          ORDER BY a.attnum",
    )
    .bind(table)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    let mut columns = Vec::with_capacity(names.len());
    for name in names {
        columns.push(PgIdent::column(&name)?.as_str().to_owned());
    }
    Ok(columns)
}

async fn restore_registered_sidecars(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    dumps: &[(String, String)],
) -> Result<(), StorageError> {
    let allowed = sidecars.memory_sidecar_tables();
    for (table, json) in dumps {
        if !allowed.contains(&table.as_str()) {
            return Err(StorageError::ConstraintViolation(format!(
                "cold sidecar table {table} is not registered"
            )));
        }
        let ident = PgIdent::table(table)?;
        let columns = insertable_columns(tx, ident.as_str()).await?;
        if columns.is_empty() {
            return Err(StorageError::Internal(format!(
                "no insertable columns for {table}"
            )));
        }
        let col_list = columns.join(", ");
        let sql = format!(
            "INSERT INTO {tbl} ({cols})
             SELECT {cols} FROM jsonb_populate_record(NULL::{tbl}, $1::jsonb) AS p",
            tbl = ident.as_str(),
            cols = col_list
        );
        // SQL-POLICY: PgIdent
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(json)
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

async fn delete_stamped_sidecars(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    tables: &[String],
    t: Uuid,
) -> Result<(), StorageError> {
    for table in tables {
        if !sidecars.is_memory_sidecar_table(table) {
            return Err(StorageError::ConstraintViolation(format!(
                "stamped sidecar table {table} is not registered"
            )));
        }
        let ident = PgIdent::table(table)?;
        // SQL-POLICY: PgIdent
        let sql = format!("DELETE FROM {tbl} WHERE t = $1", tbl = ident.as_str());
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(t)
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
    }
    Ok(())
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
    let owner_kind: String =
        sqlx::query_scalar("SELECT kind::text FROM proxima_core.owners WHERE owner_id = $1")
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
    sidecars: &PgSidecarRegistryFrozen,
    sidecar_tables: &[String],
    t: Uuid,
) -> Result<(), StorageError> {
    delete_stamped_sidecars(tx, sidecars, sidecar_tables, t).await?;
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
    super::sketch::delete_sketch(tx, t).await?;
    Ok(())
}

/// Rewind `memory_head.t` to the latest remaining hot row, or delete the
/// head when the series is empty. `memory.handle` FK requires the memory
/// row to be gone first.
pub(crate) async fn sync_memory_head(
    tx: &mut Transaction<'_, Postgres>,
    handle: Uuid,
) -> Result<(), StorageError> {
    let remaining: Option<Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_core.memory WHERE handle = $1 ORDER BY t DESC LIMIT 1",
    )
    .bind(handle)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    match remaining {
        Some(t) => {
            sqlx::query("UPDATE proxima_core.memory_head SET t = $2 WHERE handle = $1")
                .bind(handle)
                .bind(t)
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
        }
        None => {
            sqlx::query("DELETE FROM proxima_core.memory_head WHERE handle = $1")
                .bind(handle)
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
        }
    }
    Ok(())
}

/// Same as [`sync_memory_head`] for `goal` / `goal_head`.
/// Owner-erase uses set SQL; this is the single-handle form.
#[allow(dead_code)]
pub(crate) async fn sync_goal_head(
    tx: &mut Transaction<'_, Postgres>,
    handle: Uuid,
) -> Result<(), StorageError> {
    let remaining: Option<Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_core.goal WHERE handle = $1 ORDER BY t DESC LIMIT 1",
    )
    .bind(handle)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    match remaining {
        Some(t) => {
            sqlx::query("UPDATE proxima_core.goal_head SET t = $2 WHERE handle = $1")
                .bind(handle)
                .bind(t)
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
        }
        None => {
            sqlx::query("DELETE FROM proxima_core.goal_head WHERE handle = $1")
                .bind(handle)
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
        }
    }
    Ok(())
}

async fn ensure_memory_head(
    tx: &mut Transaction<'_, Postgres>,
    rec: &ColdRecord,
) -> Result<(), StorageError> {
    let head = sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, $2::proxima_core.memory_kind, $3, $4, $5)
         ON CONFLICT (handle) DO UPDATE SET t = EXCLUDED.t
         WHERE proxima_core.memory_head.kind = EXCLUDED.kind
           AND proxima_core.memory_head.schema_id = EXCLUDED.schema_id
           AND proxima_core.memory_head.owner_id = EXCLUDED.owner_id
         RETURNING handle",
    )
    .bind(rec.row.handle)
    .bind(&rec.row.kind)
    .bind(&rec.schema_id)
    .bind(rec.row.owner_id)
    .bind(rec.row.t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    if head.is_none() {
        return Err(StorageError::ConstraintViolation(
            "memory_head kind/schema/owner mismatch".into(),
        ));
    }
    Ok(())
}

pub async fn forget_memory(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    cold: &dyn ColdObjectStore,
    object_key: &str,
    t: Uuid,
) -> Result<(), StorageError> {
    let rec = snapshot_hot(tx.as_mut(), sidecars, t).await?;
    cold.put(object_key, &encode_record(&rec)).await?;
    commit_forget(tx, sidecars, cold, object_key, &rec).await
}

/// One-shot Engine path: `put` with no open transaction.
pub async fn forget_memory_oneshot(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    cold: &dyn ColdObjectStore,
    object_key: &str,
    t: Uuid,
) -> Result<(), StorageError> {
    let rec = {
        let mut conn = pool.acquire().await.map_err(map_err)?;
        snapshot_hot(&mut conn, sidecars, t).await?
    };
    cold.put(object_key, &encode_record(&rec)).await?;
    let mut tx = pool.begin().await.map_err(map_err)?;
    commit_forget(&mut tx, sidecars, cold, object_key, &rec).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(())
}

pub async fn hydrate_memory(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    cold: &dyn ColdObjectStore,
    t: Uuid,
) -> Result<(), StorageError> {
    let object_key: String =
        sqlx::query_scalar("SELECT object_key FROM proxima_core.cooled WHERE t = $1")
            .bind(t)
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?
            .ok_or(StorageError::NotFound)?;

    let rec = decode_record(&cold.get(&object_key).await?)?;
    ensure_memory_head(tx, &rec).await?;
    let cooled_content: Option<Uuid> = sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT content_id FROM proxima_core.cooled WHERE t = $1",
    )
    .bind(t)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;
    sqlx::query(
        "INSERT INTO proxima_core.memory
            (handle, t, kind, owner_id, schema_id, source_id, ingest_key, blob_id,
             content_id, origins, refs, sidecar_tables)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(rec.row.handle)
    .bind(rec.row.t)
    .bind(&rec.row.kind)
    .bind(rec.row.owner_id)
    .bind(&rec.schema_id)
    .bind(rec.row.source_id.as_deref())
    .bind(rec.row.ingest_key.as_deref())
    .bind(rec.row.blob_id)
    .bind(cooled_content)
    .bind(&rec.row.origins)
    .bind(&rec.row.refs)
    .bind(
        rec.sidecar_dumps
            .iter()
            .map(|(table, _)| table.clone())
            .collect::<Vec<_>>(),
    )
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    restore_registered_sidecars(tx, sidecars, &rec.sidecar_dumps).await?;
    let hydrate_line = rec.sketch.clone().unwrap_or_else(|| {
        rec.sidecar_dumps
            .iter()
            .find_map(|(_, json)| {
                let value: serde_json::Value = serde_json::from_str(json).ok()?;
                ["title", "claim", "body", "text"].iter().find_map(|key| {
                    value
                        .get(*key)
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .map(ToOwned::to_owned)
                })
            })
            .unwrap_or_else(|| rec.row.kind.clone())
    });
    super::sketch::upsert_sketch(
        tx,
        rec.row.owner_id,
        rec.row.t,
        &rec.row.kind,
        &hydrate_line,
    )
    .await?;
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
    sidecars: &PgSidecarRegistryFrozen,
    cold: &dyn ColdObjectStore,
    owner: &Owner,
    t: Uuid,
) -> Result<(), StorageError> {
    if matches!(owner, Owner::World) {
        return Err(StorageError::ConstraintViolation(
            "World is never abandoned".into(),
        ));
    }
    let handle: Option<Uuid> =
        sqlx::query_scalar("SELECT handle FROM proxima_core.memory WHERE t = $1 AND owner_id = $2")
            .bind(t)
            .bind(owner.stored_owner_id())
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?;
    let handle = match handle {
        Some(handle) => Some(handle),
        None => sqlx::query_scalar(
            "SELECT handle FROM proxima_core.cooled WHERE t = $1 AND owner_id = $2",
        )
        .bind(t)
        .bind(owner.stored_owner_id())
        .fetch_optional(tx.as_mut())
        .await
        .map_err(map_err)?,
    };
    let content_id = sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT content_id FROM proxima_core.memory WHERE t = $1 AND owner_id = $2
         UNION ALL
         SELECT content_id FROM proxima_core.cooled WHERE t = $1 AND owner_id = $2
         LIMIT 1",
    )
    .bind(t)
    .bind(owner.stored_owner_id())
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    .flatten();
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
    let stamped: Vec<String> = sqlx::query_scalar(
        "SELECT unnest(sidecar_tables) FROM proxima_core.memory WHERE t = $1 AND owner_id = $2",
    )
    .bind(t)
    .bind(owner.stored_owner_id())
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    delete_memory_dependents(tx, sidecars, &stamped, t).await?;
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1 AND owner_id = $2")
        .bind(t)
        .bind(owner.stored_owner_id())
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    if let Some(id) = content_id {
        super::content::gc_unreferenced_content(tx, id).await?;
    }
    if let Some(handle) = handle {
        sync_memory_head(tx, handle).await?;
    }
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
