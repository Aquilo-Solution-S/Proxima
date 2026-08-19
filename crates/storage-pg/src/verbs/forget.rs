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

/// Deterministic in `(owner_hash, handle, t)`: one object per Memory `t`,
/// not one per forget attempt. Two racing forgets of the same `t` therefore
/// PUT the same logical record to the same key, and a hydrate → forget cycle
/// overwrites in place instead of orphaning the object hydrate left behind.
/// `compensate_forget_put` depends on this: the key is not owned by one
/// attempt, so a losing attempt must not delete it.
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

fn encode_record(rec: &ColdRecord) -> Result<Vec<u8>, StorageError> {
    let mut out = vec![COLD_FORMAT_VERSION];
    write_uuid(&mut out, rec.row.handle);
    write_uuid(&mut out, rec.row.t);
    write_str(&mut out, &rec.row.kind)?;
    write_uuid(&mut out, rec.row.owner_id);
    write_opt_str(&mut out, rec.row.source_id.as_deref())?;
    write_opt_str(&mut out, rec.row.ingest_key.as_deref())?;
    write_opt_uuid(&mut out, rec.row.blob_id);
    write_uuid_list(&mut out, &rec.row.origins)?;
    write_uuid_list(&mut out, &rec.row.refs)?;
    write_str(&mut out, &rec.schema_id)?;
    write_count(&mut out, rec.sidecar_dumps.len())?;
    for (table, json) in &rec.sidecar_dumps {
        write_str(&mut out, table)?;
        write_str(&mut out, json)?;
    }
    write_str_list(&mut out, &rec.embed_models)?;
    write_opt_str(&mut out, rec.sketch.as_deref())?;
    Ok(out)
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

fn write_count(out: &mut Vec<u8>, n: usize) -> Result<(), StorageError> {
    let len = u16::try_from(n)
        .map_err(|_| StorageError::ConstraintViolation("cold field count exceeds 65535".into()))?;
    write_u16(out, len);
    Ok(())
}

fn write_str(out: &mut Vec<u8>, value: &str) -> Result<(), StorageError> {
    write_bytes(out, value.as_bytes())
}

fn write_str_list(out: &mut Vec<u8>, values: &[String]) -> Result<(), StorageError> {
    write_count(out, values.len())?;
    for value in values {
        write_str(out, value)?;
    }
    Ok(())
}

fn read_str_list(bytes: &[u8], i: &mut usize) -> Result<Vec<String>, StorageError> {
    let n = usize::from(read_u16(bytes, i)?);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(read_str(bytes, i)?);
    }
    Ok(out)
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) -> Result<(), StorageError> {
    let len = u16::try_from(value.len())
        .map_err(|_| StorageError::ConstraintViolation("cold field exceeds 65535 bytes".into()))?;
    write_u16(out, len);
    out.extend_from_slice(value);
    Ok(())
}

fn write_opt_str(out: &mut Vec<u8>, value: Option<&str>) -> Result<(), StorageError> {
    match value {
        None => out.push(0),
        Some(text) => {
            out.push(1);
            write_str(out, text)?;
        }
    }
    Ok(())
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

fn write_uuid_list(out: &mut Vec<u8>, values: &[Uuid]) -> Result<(), StorageError> {
    write_count(out, values.len())?;
    for id in values {
        write_uuid(out, *id);
    }
    Ok(())
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

const HOT_ROW_FOR_UPDATE_OWNED_SQL: &str = "SELECT handle, t, kind::text, owner_id, schema_id, source_id, ingest_key, blob_id, origins, refs, sidecar_tables, content_id
           FROM proxima_core.memory
          WHERE t = $1 AND owner_id = $2
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
/// dump differs from `snapshot` (late sidecar). Owner is the permit owner;
/// a concurrent publish that rewrote `owner_id` is `NotFound`.
/// Production callers hold the per-memory forget advisory lock across their
/// PUT, but do not hold this row lock across cold I/O. This function
/// compensates the object if the locator write fails.
pub async fn commit_forget(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    cold: &dyn ColdObjectStore,
    object_key: &str,
    snapshot: &ColdRecord,
    expected_owner_id: Uuid,
) -> Result<(), StorageError> {
    let t = snapshot.row.t;
    let locked: HotRow = sqlx::query_as(HOT_ROW_FOR_UPDATE_OWNED_SQL)
        .bind(t)
        .bind(expected_owner_id)
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
        cold.put(object_key, &encode_record(&current)?).await?;
    }
    let persist =
        persist_cooled_after_put(tx, sidecars, object_key, &current, expected_owner_id).await;
    if persist.is_err() {
        delete_cold_object(cold, object_key).await;
    }
    persist
}

async fn persist_cooled_after_put(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    object_key: &str,
    current: &ColdRecord,
    expected_owner_id: Uuid,
) -> Result<(), StorageError> {
    let t = current.row.t;
    sqlx::query(
        "INSERT INTO proxima_core.cooled
            (t, handle, owner_id, kind, object_key, blob_id, content_id, source_id, ingest_key)
         VALUES ($1, $2, $3, $4::proxima_core.memory_kind, $5, $6, $7, $8, $9)",
    )
    .bind(current.row.t)
    .bind(current.row.handle)
    .bind(current.row.owner_id)
    .bind(&current.row.kind)
    .bind(object_key)
    .bind(current.row.blob_id)
    .bind(current.row.content_id)
    .bind(current.row.source_id.as_deref())
    .bind(current.row.ingest_key.as_deref())
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    delete_memory_dependents(tx, sidecars, &current.row.sidecar_tables, t).await?;
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1 AND owner_id = $2")
        .bind(t)
        .bind(expected_owner_id)
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

/// Undo the pre-commit PUT of a forget attempt that failed before it
/// committed, unless a committed cooled locator names that exact key.
///
/// Production forgets serialize before PUT. The locator check also makes a
/// raw losing [`commit_forget`] safe without retaining payload after a
/// concurrent publish or hard erase, both of which leave no matching row.
async fn compensate_forget_put(
    tx: &mut Transaction<'_, Postgres>,
    cold: &dyn ColdObjectStore,
    object_key: &str,
    t: Uuid,
    err: &StorageError,
) {
    if matches!(err, StorageError::NotFound) {
        let referenced = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
                 SELECT 1 FROM proxima_core.cooled
                  WHERE t = $1 AND object_key = $2
             )",
        )
        .bind(t)
        .bind(object_key)
        .fetch_one(tx.as_mut())
        .await;
        match referenced {
            Ok(true) => return,
            Ok(false) => {}
            Err(lookup_err) => {
                tracing::warn!(
                    error = %lookup_err,
                    key = object_key,
                    t = %t,
                    "failed to verify cold locator while compensating forget PUT"
                );
            }
        }
    }
    delete_cold_object(cold, object_key).await;
}

/// Cold objects a committed erase still owes the object store.
///
/// An erase marks each locator in `proxima_core.cold_purge_pending` inside the
/// transaction that deletes its `cooled` row, and destroys the object only
/// after that transaction commits ([`purge_cold_objects_after_commit`]).
/// Deleting from inside the transaction loses the object outright when the
/// transaction later rolls back — the locator comes back, the bytes do not —
/// and a crash between commit and destruction leaves a reclaimable mark rather
/// than a `cooled` row naming nothing.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ColdPurgePlan {
    object_keys: Vec<String>,
}

impl ColdPurgePlan {
    pub(crate) fn from_keys(object_keys: Vec<String>) -> Self {
        Self { object_keys }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.object_keys.is_empty()
    }

    #[must_use]
    pub fn object_keys(&self) -> &[String] {
        &self.object_keys
    }
}

/// Result of one post-commit exact-key purge attempt.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ColdPurgeOutcome {
    pub attempted: u64,
    pub purged: u64,
    pub failed: u64,
    /// At least one durable row could not be reconciled. This deliberately
    /// over-reports when object deletion succeeded but the database write did
    /// not: repeating an exact-key delete is safe.
    pub pending: bool,
}

/// Destroy the cold objects a committed erase marked pending, clearing each
/// mark as its object goes.
///
/// Best effort by construction: the erase is already committed, so a refusing
/// object store cannot undo it. A key whose destruction (or whose mark clear)
/// fails keeps its `cold_purge_pending` row, which is the durable record an
/// operator retry reads — the same over-report-pending-rather-than-lose-it rule
/// the cited-object purge follows. Returns the number of objects destroyed and
/// cleared.
pub async fn purge_cold_objects_after_commit(
    pool: &PgPool,
    cold: &dyn ColdObjectStore,
    plan: &ColdPurgePlan,
) -> ColdPurgeOutcome {
    let mut outcome = ColdPurgeOutcome {
        attempted: u64::try_from(plan.object_keys().len()).unwrap_or(u64::MAX),
        ..ColdPurgeOutcome::default()
    };
    for key in plan.object_keys() {
        match cold.delete(key).await {
            Ok(()) | Err(StorageError::NotFound) => match clear_cold_purge_pending(pool, key).await
            {
                Ok(()) => outcome.purged = outcome.purged.saturating_add(1),
                Err(error) => {
                    outcome.failed = outcome.failed.saturating_add(1);
                    tracing::warn!(
                        %error,
                        key,
                        "destroyed a cold object but failed to clear its purge-pending mark"
                    );
                }
            },
            Err(error) => {
                outcome.failed = outcome.failed.saturating_add(1);
                tracing::warn!(
                    %error,
                    key,
                    "cold object outlives its committed erase; the purge-pending mark stays for retry"
                );
            }
        }
    }
    outcome.pending = outcome.purged != outcome.attempted;
    outcome
}

async fn clear_cold_purge_pending(pool: &PgPool, object_key: &str) -> Result<(), StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    let operation_id: Option<Option<Uuid>> = sqlx::query_scalar(
        "DELETE FROM proxima_core.cold_purge_pending
          WHERE object_key = $1
          RETURNING compliance_operation_id",
    )
    .bind(object_key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;
    if let Some(Some(operation_id)) = operation_id {
        sqlx::query(
            "UPDATE proxima_core.compliance_audit_log a
                SET cold_object_purge_pending = false
              WHERE a.operation_id = $1
                AND NOT EXISTS (
                    SELECT 1
                      FROM proxima_core.cold_purge_pending p
                     WHERE p.compliance_operation_id = a.operation_id
                )",
        )
        .bind(operation_id)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    }
    tx.commit().await.map_err(map_err)?;
    Ok(())
}

pub(crate) async fn delete_cold_object(cold: &dyn ColdObjectStore, object_key: &str) {
    match cold.delete(object_key).await {
        Ok(()) | Err(StorageError::NotFound) => {}
        Err(err) => {
            tracing::warn!(
                error = %err,
                key = object_key,
                "failed to compensate an untracked cold object"
            );
        }
    }
}

async fn lock_forget_memory_tx(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
) -> Result<(), StorageError> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(
             hashtextextended('proxima-forget:' || $1::text, 0)
         )",
    )
    .bind(t)
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
    expected_owner_id: Uuid,
) -> Result<(), StorageError> {
    lock_forget_memory_tx(tx, t).await?;
    let rec = snapshot_hot(tx.as_mut(), sidecars, t).await?;
    if rec.row.owner_id != expected_owner_id {
        return Err(StorageError::NotFound);
    }
    cold.put(object_key, &encode_record(&rec)?).await?;
    match commit_forget(tx, sidecars, cold, object_key, &rec, expected_owner_id).await {
        Ok(()) => Ok(()),
        Err(err) => {
            compensate_forget_put(tx, cold, object_key, t, &err).await;
            Err(err)
        }
    }
}

/// One-shot Engine path: `put` with no open transaction.
pub async fn forget_memory_oneshot(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    cold: &dyn ColdObjectStore,
    object_key: &str,
    t: Uuid,
    expected_owner_id: Uuid,
) -> Result<(), StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    lock_forget_memory_tx(&mut tx, t).await?;
    let rec = snapshot_hot(tx.as_mut(), sidecars, t).await?;
    if rec.row.owner_id != expected_owner_id {
        return Err(StorageError::NotFound);
    }
    cold.put(object_key, &encode_record(&rec)?).await?;
    if let Err(err) =
        commit_forget(&mut tx, sidecars, cold, object_key, &rec, expected_owner_id).await
    {
        compensate_forget_put(&mut tx, cold, object_key, t, &err).await;
        let _ = tx.rollback().await;
        return Err(err);
    }
    if let Err(err) = tx.commit().await.map_err(map_err) {
        tracing::warn!(
            error = %err,
            key = object_key,
            "retaining cold object after ambiguous forget commit"
        );
        return Err(err);
    }
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

/// Hard-delete one admission of an abandoned owner.
///
/// Returns the cold objects the erase still owes the object store: the caller
/// commits this transaction and then calls
/// [`purge_cold_objects_after_commit`]. The cold delete cannot run here —
/// a rollback after it would restore the `cooled` locator over destroyed
/// bytes.
pub async fn erase_memory(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    owner: &Owner,
    t: Uuid,
) -> Result<ColdPurgePlan, StorageError> {
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
    let pending: Vec<String> = sqlx::query_scalar(
        "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id)
         SELECT c.object_key, c.owner_id
           FROM proxima_core.cooled c
          WHERE c.t = $1 AND c.owner_id = $2
         ON CONFLICT (object_key) DO UPDATE SET enqueued_at = now()
         RETURNING object_key",
    )
    .bind(t)
    .bind(owner.stored_owner_id())
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.cooled WHERE t = $1 AND owner_id = $2")
        .bind(t)
        .bind(owner.stored_owner_id())
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
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
    // The series handle, not t: a ChangeHistory reader pages by handle, and a
    // t-shaped handle matches no series. Keyless rows (no hot or cooled row
    // left to read it from) fall back to t.
    .bind(handle.unwrap_or(t))
    .bind(t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(ColdPurgePlan::from_keys(pending))
}

#[cfg(test)]
mod tests {
    use super::{StorageError, write_bytes};

    #[test]
    fn cold_field_over_u16_fails_closed() {
        let mut out = Vec::new();
        let err = write_bytes(&mut out, &vec![0; 65_536]).expect_err("overflow");
        assert!(
            matches!(err, StorageError::ConstraintViolation(ref msg) if msg.contains("65535")),
            "got {err:?}"
        );
        assert!(out.is_empty());
    }
}
