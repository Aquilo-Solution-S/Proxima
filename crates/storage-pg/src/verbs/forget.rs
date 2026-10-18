//! Forget / erase.
#![allow(clippy::missing_errors_doc, clippy::doc_markdown)]

use std::collections::BTreeSet;

use proxima_core::flavor::{EraseLeg, ForgetLeg, KeyShape};
use proxima_core::owner_inverse::OwnerSurfaces;
use proxima_core::{ColdObjectStore, OwnerId, StorageError};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::sidecars::PgSidecarRegistryFrozen;

/// The only accepted cold-object format. A record in another version is
/// refused with that version.
pub const COLD_FORMAT_VERSION: u8 = 10;

// The persisted key derivation lives in core, the lowest crate shared by
// storage-pg and blob-s3; re-exported here so the storage path names it.
pub use proxima_core::cold_object_key;

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
    fn backend(&self) -> &'static str {
        // This cannot be an S3 bucket name. A host that boots without its
        // S3 configuration must not acknowledge that bucket's durable debts
        // by deleting from an unrelated, empty in-process map.
        "memory://"
    }

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
pub(crate) struct HotRow {
    pub(crate) handle: Uuid,
    pub(crate) t: Uuid,
    pub(crate) kind: String,
    pub(crate) owner_id: Uuid,
    pub(crate) by: Uuid,
    pub(crate) via: Option<Uuid>,
    pub(crate) schema_id: String,
    pub(crate) source_id: Option<String>,
    pub(crate) ingest_key: Option<String>,
    pub(crate) blob_id: Option<Uuid>,
    pub(crate) origins: Vec<Uuid>,
    pub(crate) refs: Vec<Uuid>,
    pub(crate) goal_refs: Vec<Uuid>,
    pub(crate) sidecar_tables: Vec<String>,
    pub(crate) content_id: Option<Uuid>,
    pub(crate) occurred_at: Option<time::OffsetDateTime>,
}

/// The rows a lifecycle call may reach: one owner's, of every stored Memory
/// kind up to the permit's write limit. Forget and hydration run in platform
/// scope, so this is where the rule narrows their rows.
#[derive(Debug, Clone)]
pub(crate) struct Reach {
    owner_id: Uuid,
    kinds: Vec<&'static str>,
}

impl Reach {
    pub(crate) fn of(permit: &proxima_core::storage_ports::OwnerWritePermit) -> Self {
        Self {
            owner_id: permit.owner().into_inner(),
            kinds: kinds_within(permit.access_kind()),
        }
    }
}

/// The stored Memory kinds at or below `limit`.
pub(crate) fn kinds_within(limit: proxima_core::AccessKind) -> Vec<&'static str> {
    use proxima_core::AccessKind;
    [
        (AccessKind::Fact, "fact"),
        (AccessKind::Abstraction, "abstraction"),
        (AccessKind::Perspective, "perspective"),
    ]
    .into_iter()
    .filter(|(kind, _)| kind.rank() <= limit.rank())
    .map(|(_, name)| name)
    .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct CooledRow {
    pub(crate) t: Uuid,
    pub(crate) handle: Uuid,
    pub(crate) owner_id: Uuid,
    pub(crate) by: Uuid,
    pub(crate) via: Option<Uuid>,
    pub(crate) kind: String,
    pub(crate) object_key: String,
    pub(crate) blob_id: Option<Uuid>,
    pub(crate) content_id: Option<Uuid>,
    pub(crate) source_id: Option<String>,
    pub(crate) ingest_key: Option<String>,
    pub(crate) origins: Option<Vec<Uuid>>,
    pub(crate) refs: Option<Vec<Uuid>>,
    pub(crate) goal_refs: Option<Vec<Uuid>>,
    pub(crate) cold_digest: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdRecord {
    pub(crate) row: HotRow,
    pub(crate) schema_id: String,
    pub(crate) sidecar_dumps: Vec<(String, String)>,
    /// Contract-declared `ON DELETE CASCADE` detail rows. Each declaration is
    /// present even when it has zero rows, so a cold record cannot add a
    /// forged table or omit a surface during restore.
    pub(crate) detail_dumps: Vec<(String, Vec<String>)>,
    /// Embedding spaces that had vectors. Vectors stay out of the object;
    /// hydrate enqueues embed jobs in these spaces.
    pub(crate) embed_spaces: Vec<proxima_core::EmbeddingSpace>,
    /// Exact persisted one-liner.
    pub(crate) sketch: Option<String>,
    /// Format version of the record.
    pub(crate) format_version: u8,
}

fn encode_record(rec: &ColdRecord) -> Result<Vec<u8>, StorageError> {
    let mut out = vec![COLD_FORMAT_VERSION];
    write_uuid(&mut out, rec.row.handle);
    write_uuid(&mut out, rec.row.t);
    write_str(&mut out, &rec.row.kind)?;
    write_uuid(&mut out, rec.row.owner_id);
    write_uuid(&mut out, rec.row.by);
    write_opt_uuid(&mut out, rec.row.via);
    write_opt_str(&mut out, rec.row.source_id.as_deref())?;
    write_opt_str(&mut out, rec.row.ingest_key.as_deref())?;
    write_opt_uuid(&mut out, rec.row.blob_id);
    write_uuid_list(&mut out, &rec.row.origins)?;
    write_uuid_list(&mut out, &rec.row.refs)?;
    write_uuid_list(&mut out, &rec.row.goal_refs)?;
    write_opt_time(&mut out, rec.row.occurred_at)?;
    write_str(&mut out, &rec.schema_id)?;
    // The dump names are data from the cold boundary. Persist the original
    // Memory stamp separately so a caller cannot mint a new valid sidecar by
    // appending its name to the dump list during hydration.
    write_str_list(&mut out, &rec.row.sidecar_tables)?;
    write_count(&mut out, rec.sidecar_dumps.len())?;
    for (table, json) in &rec.sidecar_dumps {
        write_str(&mut out, table)?;
        write_str(&mut out, json)?;
    }
    let embed_models: Vec<String> = rec
        .embed_spaces
        .iter()
        .map(|space| space.model_id().to_owned())
        .collect();
    write_str_list(&mut out, &embed_models)?;
    write_count(&mut out, rec.embed_spaces.len())?;
    for space in &rec.embed_spaces {
        let width = u16::try_from(space.dim().width())
            .map_err(|_| StorageError::Internal("embedding width exceeds u16".into()))?;
        write_u16(&mut out, width);
    }
    write_opt_str(&mut out, rec.sketch.as_deref())?;
    write_count(&mut out, rec.detail_dumps.len())?;
    for (table, rows) in &rec.detail_dumps {
        write_str(&mut out, table)?;
        write_count(&mut out, rows.len())?;
        for row in rows {
            write_str(&mut out, row)?;
        }
    }
    Ok(out)
}

pub(crate) fn cold_digest(bytes: &[u8]) -> Vec<u8> {
    blake3::hash(bytes).as_bytes().to_vec()
}

/// The spaces a cold object's `t` had vectors in: one width per model.
fn read_embed_spaces(
    bytes: &[u8],
    i: &mut usize,
) -> Result<Vec<proxima_core::EmbeddingSpace>, StorageError> {
    let models = read_str_list(bytes, i)?;
    if usize::from(read_u16(bytes, i)?) != models.len() {
        return Err(StorageError::Internal(
            "cold object embedding widths do not match its models".into(),
        ));
    }
    models
        .into_iter()
        .map(|model_id| {
            let width = usize::from(read_u16(bytes, i)?);
            let dim = proxima_core::EmbeddingDim::from_width(width).ok_or_else(|| {
                StorageError::Internal(format!("cold object embedding width {width} has no lane"))
            })?;
            Ok(proxima_core::EmbeddingSpace::new(model_id, dim))
        })
        .collect()
}

fn decode_record(bytes: &[u8]) -> Result<ColdRecord, StorageError> {
    let mut i = 0;
    let version = read_u8(bytes, &mut i)?;
    if version != COLD_FORMAT_VERSION {
        return Err(StorageError::Internal(format!(
            "unsupported cold format {version}; expected {COLD_FORMAT_VERSION}"
        )));
    }
    let mut row = HotRow {
        handle: read_uuid(bytes, &mut i)?,
        t: read_uuid(bytes, &mut i)?,
        kind: read_str(bytes, &mut i)?,
        owner_id: read_uuid(bytes, &mut i)?,
        by: read_uuid(bytes, &mut i)?,
        via: read_opt_uuid(bytes, &mut i)?,
        schema_id: String::new(),
        source_id: read_opt_str(bytes, &mut i)?,
        ingest_key: read_opt_str(bytes, &mut i)?,
        blob_id: read_opt_uuid(bytes, &mut i)?,
        origins: read_uuid_list(bytes, &mut i)?,
        refs: read_uuid_list(bytes, &mut i)?,
        goal_refs: read_uuid_list(bytes, &mut i)?,
        sidecar_tables: Vec::new(),
        content_id: None,
        occurred_at: None,
    };
    row.occurred_at = read_opt_time(bytes, &mut i)?;
    let schema_id = read_str(bytes, &mut i)?;
    row.sidecar_tables = read_str_list(bytes, &mut i)?;
    let n = usize::from(read_u16(bytes, &mut i)?);
    let mut sidecar_dumps = Vec::with_capacity(n);
    for _ in 0..n {
        sidecar_dumps.push((read_str(bytes, &mut i)?, read_str(bytes, &mut i)?));
    }
    let embed_spaces = read_embed_spaces(bytes, &mut i)?;
    let sketch = read_opt_str(bytes, &mut i)?;
    let n = usize::from(read_u16(bytes, &mut i)?);
    let mut detail_dumps = Vec::with_capacity(n);
    for _ in 0..n {
        let table = read_str(bytes, &mut i)?;
        let rows = usize::from(read_u16(bytes, &mut i)?);
        let mut values = Vec::with_capacity(rows);
        for _ in 0..rows {
            values.push(read_str(bytes, &mut i)?);
        }
        detail_dumps.push((table, values));
    }
    if i != bytes.len() {
        return Err(StorageError::Internal(
            "cold object has trailing bytes".into(),
        ));
    }
    Ok(ColdRecord {
        row,
        schema_id,
        sidecar_dumps,
        detail_dumps,
        embed_spaces,
        sketch,
        format_version: version,
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

/// An optional instant as microseconds since the Unix epoch, the precision a
/// `timestamptz` column holds.
fn write_opt_time(
    out: &mut Vec<u8>,
    value: Option<time::OffsetDateTime>,
) -> Result<(), StorageError> {
    match value {
        None => out.push(0),
        Some(at) => {
            let micros = i64::try_from(at.unix_timestamp_nanos().div_euclid(1_000))
                .map_err(|_| StorageError::Internal("cold time exceeds i64 microseconds".into()))?;
            out.push(1);
            out.extend_from_slice(&micros.to_be_bytes());
        }
    }
    Ok(())
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

fn read_opt_time(
    bytes: &[u8],
    i: &mut usize,
) -> Result<Option<time::OffsetDateTime>, StorageError> {
    if read_u8(bytes, i)? == 0 {
        return Ok(None);
    }
    let end = i.saturating_add(8);
    let slice: [u8; 8] = bytes
        .get(*i..end)
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(|| StorageError::Internal("cold eof time".into()))?;
    *i = end;
    let micros = i64::from_be_bytes(slice);
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(micros) * 1_000)
        .map(Some)
        .map_err(|err| StorageError::Internal(format!("cold time: {err}")))
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

/// Owner-pinned sidecars do not take part in forget/hydrate at all.
///
/// They are not the Memory's data: they belong to the owner that acted, and
/// after a transfer that is not the owner doing the forgetting. Cooling them
/// into the Memory's cold object would let the receiving owner delete — or,
/// via a cold-object erase, permanently destroy — another owner's audit
/// trail. They simply stay in the hot table, which is safe because they hold
/// no foreign key into `memory`.
///
/// The same predicate decides which stamps get a presence trigger: an
/// owner-pinned stamp is a record of a past write, not a claim about the
/// present, so `stamp ⊆ rows` cannot be asked of it. One function, so the
/// forget lane and the write-time constraint cannot drift apart.
fn is_owner_pinned(sidecars: &PgSidecarRegistryFrozen, table: &str) -> bool {
    sidecars.is_owner_pinned_memory_sidecar_table(table)
}

/// The tables a cold record must carry a dump for: the row's own stamp, minus
/// the owner-pinned tables the registry deliberately leaves in place.
///
/// One function so the writer that produces `sidecar_dumps` and the verifier
/// that admits them cannot drift. They used to disagree: the dump silently
/// skipped a stamped table whose row was absent, while the verifier demanded
/// element-for-element equality — so such a record cooled and could then
/// never be hydrated.
fn dumpable_stamped_tables<'a>(
    stamp: &'a [String],
    sidecars: &PgSidecarRegistryFrozen,
) -> Vec<&'a str> {
    stamp
        .iter()
        .map(String::as_str)
        .filter(|table| !is_owner_pinned(sidecars, table))
        .collect()
}

async fn dump_stamped_sidecars(
    conn: &mut PgConnection,
    sidecars: &PgSidecarRegistryFrozen,
    tables: &[String],
    t: Uuid,
) -> Result<Vec<(String, String)>, StorageError> {
    for table in tables {
        if !sidecars.is_memory_sidecar_table(table) {
            return Err(StorageError::ConstraintViolation(format!(
                "stamped sidecar table {table} is not registered"
            )));
        }
    }
    let dumpable = dumpable_stamped_tables(tables, sidecars);
    let mut dumps = Vec::with_capacity(dumpable.len());
    for table in dumpable {
        // The row is found on the column the table's registration declares,
        // never on a literal `t`. The delete half below reads the same
        // column off `ForgetLeg::Dumped { key_column }`, and
        // `freeze_against`'s `check_memory_key_against_contracts` holds the
        // two declarations equal — so a sidecar keyed on anything else is
        // dumped and deleted, rather than deleted with an empty cold record.
        // `is_memory_sidecar_table` above already refused an unregistered
        // table, so the lookup cannot miss; the arm names what to fix if it
        // ever does rather than falling back.
        let key = PgIdent::column(sidecars.memory_key_column(table).ok_or_else(|| {
            StorageError::ConstraintViolation(format!(
                "stamped sidecar table {table} declares no memory-key column; register the \
                 payload with `pg_sidecar!(key: …)` so the dump knows which column to find the \
                 forgotten row on"
            ))
        })?)?;
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
              WHERE s.{key} = $1",
            tbl = ident.as_str(),
            key = key.as_str()
        );
        // SQL-POLICY: PgIdent
        let json: Option<serde_json::Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(t)
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_err)?;
        // The stamp is the memory row's own declaration. A stamped table with
        // no row means the row's account of itself no longer matches the
        // physical state, and cooling it would mint a cold object whose dump
        // list can never equal its stamp. Refuse here, where the divergence
        // is visible and the memory is still whole.
        let Some(json) = json else {
            return Err(StorageError::ConstraintViolation(format!(
                "stamped sidecar table {table} has no row for the memory being forgotten"
            )));
        };
        dumps.push((table.to_owned(), json.to_string()));
    }
    Ok(dumps)
}

/// Dump every contract-declared cascaded MemoryT detail relation, including
/// an explicit empty entry. The parent sidecar's FK will delete these rows
/// during cooling, so a cold record that only carries the parent row is not a
/// complete representation of the Memory payload.
async fn dump_cascaded_details(
    conn: &mut PgConnection,
    surfaces: &OwnerSurfaces,
    schema_id: &str,
    t: Uuid,
) -> Result<Vec<(String, Vec<String>)>, StorageError> {
    let details = surfaces.cascaded_details_for_schema(schema_id);
    let mut dumps = Vec::with_capacity(details.len());
    for detail in details {
        let table = PgIdent::table(detail.table)?;
        let key = PgIdent::column(detail.key_column)?;
        let sql = format!(
            "SELECT COALESCE(jsonb_agg(row_json ORDER BY row_json::text), '[]'::jsonb)
               FROM (
                 SELECT to_jsonb(s) - COALESCE((
                     SELECT array_agg(a.attname::text)
                       FROM pg_attribute a
                       JOIN pg_class c ON c.oid = a.attrelid
                       JOIN pg_namespace n ON n.oid = c.relnamespace
                      WHERE n.nspname = split_part('{tbl}', '.', 1)
                        AND c.relname = split_part('{tbl}', '.', 2)
                        AND a.attgenerated <> ''
                   ), '{{}}'::text[]) AS row_json
                   FROM {tbl} s
                  WHERE s.{key} = $1
               ) dumped",
            tbl = table.as_str(),
            key = key.as_str(),
        );
        // SQL-POLICY: PgIdent
        let json: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(t)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_err)?;
        let rows = json
            .as_array()
            .ok_or_else(|| {
                StorageError::Internal(format!(
                    "cascaded detail dump for {} is not an array",
                    detail.table
                ))
            })?
            .iter()
            .map(serde_json::Value::to_string)
            .collect();
        dumps.push((detail.table.to_owned(), rows));
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

async fn load_embed_spaces(
    conn: &mut PgConnection,
    t: Uuid,
) -> Result<Vec<proxima_core::EmbeddingSpace>, StorageError> {
    let rows: Vec<(String, i16)> = sqlx::query_as(
        "SELECT DISTINCT model_id, dim
           FROM proxima_core.embeddings
          WHERE entity_id = $1
          ORDER BY model_id, dim",
    )
    .bind(t)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_err)?;
    rows.into_iter()
        .map(|(model_id, dim)| crate::pgvector::stored_space(model_id, dim))
        .collect()
}

const HOT_ROW_SQL: &str = "SELECT handle, t, kind::text, owner_id, \"by\", via, schema_id, source_id, ingest_key, blob_id, origins, refs, goal_refs, sidecar_tables, content_id, occurred_at
           FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (forget and restore see the row in either state) */
          WHERE t = $1";

const HOT_ROW_FOR_UPDATE_OWNED_SQL: &str = "SELECT handle, t, kind::text, owner_id, \"by\", via, schema_id, source_id, ingest_key, blob_id, origins, refs, goal_refs, sidecar_tables, content_id, occurred_at
           FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (forget and restore see the row in either state) */
          WHERE t = $1 AND owner_id = $2
          FOR UPDATE";

/// Hot row + registered sidecars + embed model ids. No row lock.
pub async fn snapshot_hot(
    conn: &mut PgConnection,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
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
    let detail_dumps = dump_cascaded_details(conn, surfaces, &schema_id, t).await?;
    let embed_spaces = load_embed_spaces(conn, t).await?;
    let sketch = load_sketch_text(conn, t).await?;
    Ok(ColdRecord {
        row,
        schema_id,
        sidecar_dumps,
        detail_dumps,
        embed_spaces,
        sketch,
        format_version: COLD_FORMAT_VERSION,
    })
}

/// `FOR UPDATE` + cooled stub + hot delete. Re-PUTs only when the locked
/// dump differs from `snapshot` (late sidecar). Owner is the permit owner;
/// a concurrent owner transfer that rewrote `owner_id` is `NotFound`.
/// Production callers hold the complete series/grounding advisory set across
/// their PUT, but do not hold this row lock across cold I/O. This function
/// compensates the object if the locator write fails.
///
/// The caller must already hold `snapshot.row.handle`'s handle lock and the
/// grounding lifecycle set — `forget_memory` and
/// `forget_memory_oneshot_in_transaction` take
/// both before the PUT this commits. Re-taking them here would sort a freshly
/// queried set while this transaction already holds locks from the first
/// acquisition, which is exactly the out-of-order extension
/// `lock_forget_footprint_tx` refuses to do.
pub async fn commit_forget(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    cold: &dyn ColdObjectStore,
    object_key: &str,
    snapshot: &ColdRecord,
    expected_owner_id: Uuid,
) -> Result<(), StorageError> {
    let t = snapshot.row.t;
    if snapshot.row.owner_id != expected_owner_id {
        return Err(StorageError::NotFound);
    }
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
    let detail_dumps = dump_cascaded_details(tx.as_mut(), surfaces, &schema_id, t).await?;
    let embed_spaces = load_embed_spaces(tx.as_mut(), t).await?;
    let sketch = load_sketch_text(tx.as_mut(), t).await?;
    let current = ColdRecord {
        row: locked,
        schema_id,
        sidecar_dumps,
        detail_dumps,
        embed_spaces,
        sketch,
        format_version: COLD_FORMAT_VERSION,
    };
    if current != *snapshot {
        cold.put(object_key, &encode_record(&current)?).await?;
    }
    let persist = persist_cooled_after_put(
        tx,
        sidecars,
        surfaces,
        object_key,
        &current,
        expected_owner_id,
    )
    .await;
    if persist.is_err() {
        delete_cold_object(cold, object_key).await;
    }
    persist
}

async fn persist_cooled_after_put(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    object_key: &str,
    current: &ColdRecord,
    expected_owner_id: Uuid,
) -> Result<(), StorageError> {
    let t = current.row.t;
    sqlx::query(
        "INSERT INTO proxima_core.cooled
            (t, handle, owner_id, kind, object_key, blob_id, content_id, source_id, ingest_key,
             origins, refs, goal_refs, cold_digest, \"by\", via)
         VALUES ($1, $2, $3, $4::proxima_core.memory_kind, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
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
    .bind(&current.row.origins)
    .bind(&current.row.refs)
    .bind(&current.row.goal_refs)
    .bind(cold_digest(&encode_record(current)?))
    .bind(current.row.by)
    .bind(current.row.via)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    delete_memory_dependents(
        tx,
        sidecars,
        surfaces,
        &current.row.sidecar_tables,
        t,
        DependentsFor::Forget,
    )
    .await?;
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1 AND owner_id = $2") /* FORGET-POLICY: includes-forgotten (forget and restore see the row in either state) */
        .bind(t)
        .bind(expected_owner_id)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sync_memory_head(tx, current.row.handle).await?;
    // Content stays while the cooled stub names it.

    Ok(())
}

/// Undo the pre-commit PUT of a forget attempt that failed before it
/// committed, unless a committed cooled locator names that exact key.
///
/// Production forgets serialize before PUT. The locator check also makes a
/// raw losing [`commit_forget`] safe without retaining payload after a
/// concurrent owner transfer or hard erase, both of which leave no matching row.
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
    entries: Vec<ColdPurgeEntry>,
}

/// One debt: the key, and the object store that holds those bytes.
///
/// The backend travels with the key because the queue outlives the process
/// that wrote it. See [`proxima_core::UNRECORDED_BACKEND`] for what an empty
/// backend means and why cold Memory objects have one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdPurgeEntry {
    pub object_key: String,
    pub backend: String,
}

impl ColdPurgePlan {
    /// Debts with no recorded backend — cold Memory objects, whose store is
    /// the deployment's one wired `ColdObjectStore`.
    pub(crate) fn from_keys(object_keys: Vec<String>) -> Self {
        Self::from_entries(
            object_keys
                .into_iter()
                .map(|object_key| ColdPurgeEntry {
                    object_key,
                    backend: proxima_core::UNRECORDED_BACKEND.to_owned(),
                })
                .collect(),
        )
    }

    pub(crate) fn from_entries(entries: Vec<ColdPurgeEntry>) -> Self {
        Self { entries }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn entries(&self) -> &[ColdPurgeEntry] {
        &self.entries
    }

    #[must_use]
    pub fn object_keys(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.object_key.clone())
            .collect()
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
/// the cited-object purge follows. Counts reconciled plan entries, including
/// debts already retired by another drain before provider I/O begins.
pub async fn purge_cold_objects_after_commit(
    pool: &PgPool,
    platform: &crate::PgPlatformScope,
    cold: &dyn ColdObjectStore,
    plan: &ColdPurgePlan,
) -> ColdPurgeOutcome {
    purge_cold_objects_after_commit_with_platform(pool, platform, cold, plan).await
}

pub(crate) async fn purge_cold_objects_after_commit_with_platform(
    pool: &PgPool,
    platform: &crate::PgPlatformScope,
    cold: &dyn ColdObjectStore,
    plan: &ColdPurgePlan,
) -> ColdPurgeOutcome {
    let mut outcome = ColdPurgeOutcome {
        attempted: u64::try_from(plan.entries().len()).unwrap_or(u64::MAX),
        ..ColdPurgeOutcome::default()
    };
    for entry in plan.entries() {
        let key = entry.object_key.as_str();
        // Never delete against the wrong store, and never silently drop the
        // debt either: the row stays, `failed` counts it, and the receipt
        // reports the erase as still owing bytes. An operator draining the
        // queue gets the typed refusal with the offending backend named.
        if !proxima_core::cold_backend_matches(cold.backend(), &entry.backend) {
            outcome.failed = outcome.failed.saturating_add(1);
            tracing::warn!(
                key,
                row_backend = entry.backend,
                store_backend = cold.backend(),
                "cold purge debt names a backend this store is not; leaving it queued"
            );
            continue;
        }
        match purge_current_cold_debt(pool, platform, cold, key).await {
            Ok(()) => outcome.purged = outcome.purged.saturating_add(1),
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

/// A queue row's version, used only for the duration of one provider call.
/// These PostgreSQL stamps are not persistent object or transaction IDs.
/// Chosen over a schema `version` column so the guard needs no migration
/// (AGENTS.md migration policy); a row moved by VACUUM FULL or a HOT update
/// simply fails the compare, which leaves the debt queued for retry.
#[derive(sqlx::FromRow)]
struct ColdPurgeVersion {
    backend: String,
    row_xmin: String,
    row_ctid: String,
}

async fn purge_current_cold_debt(
    pool: &PgPool,
    platform: &crate::PgPlatformScope,
    cold: &dyn ColdObjectStore,
    object_key: &str,
) -> Result<(), StorageError> {
    let mut tx = crate::platform_scope::begin_platform_transaction(pool, platform).await?;
    let version = sqlx::query_as::<_, ColdPurgeVersion>(
        "SELECT backend, xmin::text AS row_xmin, ctid::text AS row_ctid
           FROM proxima_core.cold_purge_pending WHERE object_key = $1",
    )
    .bind(object_key)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;
    let Some(version) = version else {
        // Another drain already retired this entry. A stale plan no longer
        // authorizes deleting whatever may now occupy the same object key.
        return Ok(());
    };
    if !proxima_core::cold_backend_matches(cold.backend(), &version.backend) {
        return Err(StorageError::Unavailable(format!(
            "cold purge debt for {object_key} now names backend {:?}, not {:?}",
            version.backend,
            cold.backend()
        )));
    }
    // No database transaction or connection remains held during provider I/O.
    // A producer may publish bytes and renew this debt while delete is in
    // flight; its new row version must survive our earlier acknowledgement.
    match cold.delete(object_key).await {
        Ok(()) | Err(StorageError::NotFound) => {}
        Err(error) => return Err(error),
    }
    if !clear_cold_purge_pending(pool, platform, object_key, &version).await? {
        return Err(StorageError::Unavailable(format!(
            "cold purge debt for {object_key} changed during deletion; leaving it for retry"
        )));
    }
    Ok(())
}

/// A completed provider deletion retires only the queue version it observed.
///
/// The queue IS the debt. `pending_cold_purge_count` answers "is anything
/// owed" by counting rows, not by trusting a flag that a crash between two
/// writes could leave set forever.
async fn clear_cold_purge_pending(
    pool: &PgPool,
    platform: &crate::PgPlatformScope,
    object_key: &str,
    version: &ColdPurgeVersion,
) -> Result<bool, StorageError> {
    let mut tx = crate::platform_scope::begin_platform_transaction(pool, platform).await?;
    let deleted = sqlx::query(
        "DELETE FROM proxima_core.cold_purge_pending
          WHERE object_key = $1 AND backend = $2
            AND xmin::text = $3 AND ctid::text = $4",
    )
    .bind(object_key)
    .bind(&version.backend)
    .bind(&version.row_xmin)
    .bind(&version.row_ctid)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;
    Ok(deleted.rows_affected() == 1)
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

/// Take a sorted, deduplicated advisory-lock set in one round-trip.
///
/// The ordering guarantee — the whole point of these locks — comes from two
/// facts, not from an `ORDER BY`. `unnest` over an array plans as a bare
/// `Function Scan` that yields elements in array order with no sort node
/// above it, and `pg_advisory_xact_lock` is `VOLATILE` and `PARALLEL
/// RESTRICTED`, so the planner can neither hoist the call nor push it into a
/// worker that would evaluate it out of order. The array is therefore sorted
/// here, in Rust, and the statement carries no `ORDER BY`: one would sort the
/// returned rows, which nothing reads, and imply an ordering guarantee the
/// projection does not get from it.
///
/// A per-id statement would be the same locks in the same order; it would
/// just pay a network round-trip for each one, and these sets are as large as
/// an admission's pin count or a series erase's version count.
async fn lock_advisory_ids_tx(
    tx: &mut Transaction<'_, Postgres>,
    namespace: &str,
    ids: &[Uuid],
) -> Result<(), StorageError> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut sorted = ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended($1::text || id::text, 0))
           FROM unnest($2::uuid[]) AS id",
    )
    .bind(namespace)
    .bind(&sorted)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Acquire the one advisory lock vocabulary used by Memory series handles.
///
/// Handle locks are deliberately a different namespace from the `t` locks
/// below: a caller-supplied handle may equal a caller-supplied `t`, and
/// conflating those identities would make an unrelated series and admission
/// serialize. Memory admission, per-entity lifecycle, and series erase acquire
/// their complete handle set in sorted order before any lifecycle `t` lock.
pub(crate) async fn lock_memory_handles_tx(
    tx: &mut Transaction<'_, Postgres>,
    handles: &[Uuid],
) -> Result<(), StorageError> {
    lock_advisory_ids_tx(tx, "proxima-memory-handle:", handles).await
}

/// Acquire the one lifecycle lock vocabulary used by admission, hydration,
/// transfer, forget and erase. Callers hand in the complete target set before
/// taking any row/blob lock; sorting makes crossed declarations wait rather
/// than deadlock. Memory callers acquire all handle locks before entering this
/// helper; this helper itself never acquires a handle lock.
pub(crate) async fn lock_lifecycle_targets_tx(
    tx: &mut Transaction<'_, Postgres>,
    targets: &[Uuid],
) -> Result<(), StorageError> {
    lock_advisory_ids_tx(tx, "proxima-forget:", targets).await
}

/// Lock the source and every hot or cooled non-Fact depender before the source
/// row is locked or cooled. `cooled_forget_grounding` takes the hot set as a
/// SQL backstop; the re-read rejects a growing set rather than extending an
/// already-held advisory set out of UUID order.
async fn lock_forget_footprint_tx(
    tx: &mut Transaction<'_, Postgres>,
    source_t: Uuid,
    source_handle: Uuid,
) -> Result<(), StorageError> {
    // Resolve every dependent handle before taking any lifecycle lock. A
    // dependent append takes its handle first and may name `source_t`; taking
    // only the source handle here would invert the order and leave the
    // dependent handle outside this operation's serialization domain.
    let dependent_handles: Vec<Uuid> = sqlx::query_scalar(
        "SELECT handle FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (forget and restore see the row in either state) */
          WHERE kind <> 'fact'
            AND t <> $1
            AND (origins @> ARRAY[$1]::uuid[] OR refs @> ARRAY[$1]::uuid[])
        UNION
        SELECT handle FROM proxima_core.cooled
          WHERE kind <> 'fact'
            AND t <> $1
            AND (origins @> ARRAY[$1]::uuid[] OR refs @> ARRAY[$1]::uuid[])
          ORDER BY handle",
    )
    .bind(source_t)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    let mut handles = Vec::with_capacity(1 + dependent_handles.len());
    handles.push(source_handle);
    handles.extend(dependent_handles.iter().copied());
    lock_memory_handles_tx(tx, &handles).await?;

    let grounded: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT t, handle FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (forget and restore see the row in either state) */
          WHERE kind <> 'fact'
            AND t <> $1
            AND (origins @> ARRAY[$1]::uuid[] OR refs @> ARRAY[$1]::uuid[])
        UNION
        SELECT t, handle FROM proxima_core.cooled
          WHERE kind <> 'fact'
            AND t <> $1
            AND (origins @> ARRAY[$1]::uuid[] OR refs @> ARRAY[$1]::uuid[])
          ORDER BY t",
    )
    .bind(source_t)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    // The SQL result is sorted; prepending the source made `handles` unsorted.
    // Search its ordered tail to avoid a linear scan for every depender.
    let handle_was_locked =
        |handle: &Uuid| *handle == source_handle || dependent_handles.binary_search(handle).is_ok();
    if grounded
        .iter()
        .any(|(_, handle)| !handle_was_locked(handle))
    {
        return Err(StorageError::Retryable(
            "forget depender handle set grew before lifecycle lock acquisition".into(),
        ));
    }
    let dependents: Vec<Uuid> = grounded.into_iter().map(|(t, _)| t).collect();
    let mut targets = Vec::with_capacity(1 + dependents.len());
    targets.push(source_t);
    targets.extend(dependents.iter().copied());
    lock_lifecycle_targets_tx(tx, &targets).await?;

    let after: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT t, handle FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (forget and restore see the row in either state) */
          WHERE kind <> 'fact'
            AND t <> $1
            AND (origins @> ARRAY[$1]::uuid[] OR refs @> ARRAY[$1]::uuid[])
        UNION
        SELECT t, handle FROM proxima_core.cooled
          WHERE kind <> 'fact'
            AND t <> $1
            AND (origins @> ARRAY[$1]::uuid[] OR refs @> ARRAY[$1]::uuid[])
          ORDER BY t",
    )
    .bind(source_t)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    // Both queries `ORDER BY t` and exclude `source_t`, so target membership is
    // a binary search over `dependents` rather than a scan of `targets`.
    if after.iter().all(|(target, handle)| {
        dependents.binary_search(target).is_ok() && handle_was_locked(handle)
    }) {
        return Ok(());
    }
    Err(StorageError::Retryable(
        "forget depender footprint grew after lifecycle lock acquisition".into(),
    ))
}

/// The owner transfer's half of the forget serialization: the same
/// per-memory advisory lock [`lock_lifecycle_targets_tx`] takes, over every `t`
/// of the series, in sorted order. Forget takes its single lock before any
/// row lock, and the transfer calls this before writing any row, so the two
/// paths cannot form an advisory-lock cycle — a forget on a locked `t`
/// simply queues behind the transfer (and vice versa).
pub(crate) async fn lock_forget_memories_tx(
    tx: &mut Transaction<'_, Postgres>,
    ts: &[Uuid],
) -> Result<(), StorageError> {
    lock_lifecycle_targets_tx(tx, ts).await
}

/// Every row one forgotten `t` takes with it, from two iteration sources
/// and through one statement shape.
///
/// **The stamp, for the `Dumped` legs.** `memory.sidecar_tables` records
/// what THIS row actually wrote, and that is why forget walks it rather than
/// the registry: a registry that gained a sidecar after this row was written
/// must not delete from a table the row never touched, and one that LOST a
/// table must still forget rows written before it went.
/// `forget_dumps_only_stamped_tables_and_skips_unregistered_scan` pins that
/// property by stamping a registered-but-nonexistent table and requiring the
/// forget to succeed. The PG registry is the veto (a stamp naming an
/// unregistered table is a constraint violation) and the owner-pin filter;
/// the DB backs the other direction with `assert_sidecar_stamp_declared`.
///
/// **The contract, for the `Deleted` legs.** The embedding triple and the
/// sketch are derived rows nothing stamps — no lane records them on the
/// memory — so there is no stamp to walk and the declaration is the only
/// list.
///
/// Stamp legs run before declaration legs. Neither set holds a foreign key
/// into the other.
async fn delete_memory_dependents(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    sidecar_tables: &[String],
    t: Uuid,
    lifecycle: DependentsFor,
) -> Result<(), StorageError> {
    let mut legs: Vec<(&str, &str)> = Vec::with_capacity(sidecar_tables.len() + 4);
    for table in sidecar_tables {
        if !sidecars.is_memory_sidecar_table(table) {
            return Err(StorageError::ConstraintViolation(format!(
                "stamped sidecar table {table} is not registered"
            )));
        }
        // See [`is_owner_pinned`]: forgetting a Memory must not delete
        // somebody else's audit rows.
        if is_owner_pinned(sidecars, table) {
            continue;
        }
        // The declaration names the column when it has an opinion. It does
        // not always: a stamp may name a table the PG registry knows and no
        // contract declares, which arrives here as `Unreachable` and is the
        // case the skip-unregistered-scan test installs deliberately. `t` is
        // the fallback because it is what `pg_sidecar!` requires of every
        // memory sidecar it registers.
        //
        // `Kept` is spelled out rather than folded into that fallback, and
        // the fallback DELETES. A `Keep` declaration reaching a delete is
        // the failure this arm exists to make loud instead of silent: the
        // table was declared as one the forget does not touch, and the walk
        // is about to destroy it. `freeze_against`'s
        // `check_keep_is_owner_pinned` makes that unreachable — `Keep`
        // requires `owner_pinned`, and owner-pinned tables were skipped
        // above — so this is the assertion that the boot check ran, not a
        // second policy.
        let key_column = match surfaces.forget_leg(table) {
            ForgetLeg::Dumped { key_column } => key_column,
            ForgetLeg::Kept { why } => {
                return Err(StorageError::Internal(format!(
                    "{table} declares ForgetRule::Keep ({why}) and is not owner-pinned, \
                     so the forget cannot honour it; freeze_against must refuse this registry"
                )));
            }
            ForgetLeg::Deleted { .. }
            | ForgetLeg::DumpedCascade { .. }
            | ForgetLeg::Cascaded { .. }
            | ForgetLeg::Unreachable => "t",
        };
        legs.push((table.as_str(), key_column));
    }
    legs.extend(surfaces.generated_forget_legs());
    if lifecycle == DependentsFor::Erase {
        legs.extend(erase_kept_memory_legs(surfaces));
    }
    for (table, key_column) in legs {
        let sql = forget_leg_sql(table, key_column)?;
        // SQL-POLICY: generated
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(t)
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

/// Which lifecycle verb [`delete_memory_dependents`] serves. A forget keeps
/// what its declarations say it keeps; a hard erase also destroys those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DependentsFor {
    Forget,
    Erase,
}

/// The surfaces [`erase_memory`] deletes with a hand-written statement of
/// its own, because each needs an owner predicate or an ordering the
/// generated `DELETE ... WHERE <key> = $1` does not carry.
const ERASE_MEMORY_EXPLICIT: &[&str] = &[
    "proxima_core.memory",
    "proxima_core.cooled",
    "proxima_core.ingest_keys",
];

/// Every memory-keyed surface a forget KEEPS (its rows outlive a cool) and
/// owner erase deletes by key, minus the ones [`erase_memory`] deletes
/// explicitly. A hard erase of one admission owes these rows the same
/// delete owner erase gives them, and reading the set off the declarations
/// means a newly declared kept surface is reached without a named leg
/// (`memory_external_origin`, #478).
fn erase_kept_memory_legs(surfaces: &OwnerSurfaces) -> Vec<(&'static str, &'static str)> {
    surfaces
        .surfaces()
        .iter()
        .filter_map(|surface| {
            let EraseLeg::Keyed(KeyShape::MemoryT { column }) = surfaces.erase_leg(surface.table)
            else {
                return None;
            };
            (matches!(surfaces.forget_leg(surface.table), ForgetLeg::Kept { .. })
                && !ERASE_MEMORY_EXPLICIT.contains(&surface.table))
            .then_some((surface.table, column))
        })
        .collect()
}

/// The one statement a forget leg runs, over the forgotten `t`.
///
/// Every identifier here is a `&'static str` from a `const` contract or a
/// `pg_sidecar!` registration, both freeze-validated, and every contract one
/// is additionally resolved against `information_schema` by
/// `flavor_contract_acceptance::every_column_a_declaration_names_is_a_column_the_catalog_has`.
/// `PgIdent`'s whitelist is what makes the substitution `%I`-equivalent.
fn forget_leg_sql(table: &str, key_column: &str) -> Result<String, StorageError> {
    let table = PgIdent::table(table)?;
    let key = PgIdent::column(key_column)?;
    // SQL-POLICY: PgIdent
    Ok(format!(
        "DELETE FROM {tbl} WHERE {col} = $1",
        tbl = table.as_str(),
        col = key.as_str()
    ))
}

/// Rewind `memory_head.t` to the latest remaining hot row, or delete the
/// head when the series is empty. `memory.handle` FK requires the memory
/// row to be gone first. The caller must already hold the complete advisory
/// lock for this handle; with that contract, the head is present exactly when
/// a hot row exists and always names the greatest surviving hot `t`.
pub(crate) async fn sync_memory_head(
    tx: &mut Transaction<'_, Postgres>,
    handle: Uuid,
) -> Result<(), StorageError> {
    let remaining: Option<Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_core.memory WHERE handle = $1 ORDER BY t DESC LIMIT 1", /* FORGET-POLICY: includes-forgotten (head sync sees forgotten rows, then skips them) */
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

async fn probe_memory_handle_for_owner(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
    owner_id: Uuid,
) -> Result<Option<Uuid>, StorageError> {
    sqlx::query_scalar(
        "SELECT handle FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (hydrate, removed in S4) */
          WHERE t = $1 AND owner_id = $2
         UNION ALL
         SELECT handle FROM proxima_core.cooled
          WHERE t = $1 AND owner_id = $2
         LIMIT 1",
    )
    .bind(t)
    .bind(owner_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)
}

pub async fn forget_memory(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    cold: &dyn ColdObjectStore,
    object_key: &str,
    t: Uuid,
    expected_owner_id: Uuid,
) -> Result<(), StorageError> {
    let handle = probe_memory_handle_for_owner(tx, t, expected_owner_id)
        .await?
        .ok_or(StorageError::NotFound)?;
    lock_forget_footprint_tx(tx, t, handle).await?;
    let rec = snapshot_hot(tx.as_mut(), sidecars, surfaces, t).await?;
    if rec.row.owner_id != expected_owner_id {
        return Err(StorageError::NotFound);
    }
    cold.put(object_key, &encode_record(&rec)?).await?;
    match commit_forget(
        tx,
        sidecars,
        surfaces,
        cold,
        object_key,
        &rec,
        expected_owner_id,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(err) => {
            compensate_forget_put(tx, cold, object_key, t, &err).await;
            Err(err)
        }
    }
}

pub(crate) async fn forget_memory_oneshot_in_transaction(
    mut tx: Transaction<'static, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    cold: &dyn ColdObjectStore,
    object_key: &str,
    t: Uuid,
    reach: &Reach,
) -> Result<(), StorageError> {
    let expected_owner_id = reach.owner_id;
    let handle = probe_memory_handle_for_owner(&mut tx, t, expected_owner_id)
        .await?
        .ok_or(StorageError::NotFound)?;
    lock_forget_footprint_tx(&mut tx, t, handle).await?;
    let rec = snapshot_hot(tx.as_mut(), sidecars, surfaces, t).await?;
    if rec.row.owner_id != expected_owner_id || !reach.kinds.contains(&rec.row.kind.as_str()) {
        return Err(StorageError::NotFound);
    }
    cold.put(object_key, &encode_record(&rec)?).await?;
    if let Err(err) = commit_forget(
        &mut tx,
        sidecars,
        surfaces,
        cold,
        object_key,
        &rec,
        expected_owner_id,
    )
    .await
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

/// Why a cold record cannot be restored.
///
/// Every variant is produced at a named site and names one export
/// `cold_unavailable` reason. Nothing here is recovered from an error
/// message: a classifier that reads prose is one reword away from silently
/// reclassifying, and it cannot tell a bad object from a lifecycle conflict
/// that happens to share a `StorageError` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColdRejection {
    /// The locator names an object the store does not have.
    MissingObject,
    /// The record predates the database integrity witness, or declares a
    /// format or detail set this binary cannot restore. Not evidence of
    /// corrupt bytes.
    UnsupportedObject,
    /// A witnessed object declares this unsupported format version.
    UnsupportedFormat(u8),
    /// The bytes are present and witnessed but cannot be admitted: a digest
    /// or identity mismatch, an undecodable record, or a payload PostgreSQL
    /// refuses.
    InvalidObject,
}

/// Fetch, verify and decode one cooled row's cold object: THE definition of
/// a valid cold object, as the owner export reads it (#478).
///
/// The outer `Err` is a store or transport fault (anything but `NotFound`);
/// the inner `Err` is a permanent verdict about this object.
pub(crate) async fn read_verified_cold_record(
    cold: &dyn ColdObjectStore,
    cooled: &CooledRow,
) -> Result<Result<ColdRecord, ColdRejection>, StorageError> {
    // A pre-witness locator has neither a digest nor the split pin arrays,
    // and its object predates the stamped cold format. Nothing can prove the
    // bytes belong to this admission, so it fails closed rather than being
    // admitted on the object's own say-so.
    let (Some(origins), Some(refs), Some(goal_refs), Some(expected_digest)) = (
        cooled.origins.as_ref(),
        cooled.refs.as_ref(),
        cooled.goal_refs.as_ref(),
        cooled.cold_digest.as_ref(),
    ) else {
        return Ok(Err(ColdRejection::UnsupportedObject));
    };
    let bytes = match cold.get(&cooled.object_key).await {
        Ok(bytes) => bytes,
        Err(StorageError::NotFound) => return Ok(Err(ColdRejection::MissingObject)),
        Err(error) => return Err(error),
    };
    if expected_digest.len() != 32 || cold_digest(&bytes) != *expected_digest {
        return Ok(Err(ColdRejection::InvalidObject));
    }
    // Format is untrusted object metadata; keep its version in the refusal
    // before the current-format decoder interprets any field.
    let Some(&format_version) = bytes.first() else {
        return Ok(Err(ColdRejection::InvalidObject));
    };
    if format_version != COLD_FORMAT_VERSION {
        return Ok(Err(ColdRejection::UnsupportedFormat(format_version)));
    }
    let Ok(rec) = decode_record(&bytes) else {
        return Ok(Err(ColdRejection::InvalidObject));
    };
    if rec.row.t != cooled.t
        || rec.row.handle != cooled.handle
        || rec.row.kind != cooled.kind
        || rec.row.by != cooled.by
        || rec.row.via != cooled.via
        || rec.row.source_id != cooled.source_id
        || rec.row.ingest_key != cooled.ingest_key
        || rec.row.origins != *origins
        || rec.row.refs != *refs
        || rec.row.goal_refs != *goal_refs
    {
        return Ok(Err(ColdRejection::InvalidObject));
    }
    Ok(Ok(rec))
}

/// Every cooled row of one owner, with its locator, in `t` order. Export
/// reads them in its snapshot; the locators never enter the bundle.
pub(crate) async fn owner_cooled_rows(
    conn: &mut PgConnection,
    owner_id: Uuid,
) -> Result<Vec<CooledRow>, StorageError> {
    sqlx::query_as(
        "SELECT t, handle, owner_id, kind::text, object_key, blob_id, content_id,
                source_id, ingest_key, origins, refs, goal_refs, cold_digest, \"by\", via
           FROM proxima_core.cooled
          WHERE owner_id = $1
          ORDER BY t",
    )
    .bind(owner_id)
    .fetch_all(conn)
    .await
    .map_err(map_err)
}

/// One cooled row by `t`, whoever owns it: export's revalidation re-read.
pub(crate) async fn cooled_row(
    conn: &mut PgConnection,
    t: Uuid,
) -> Result<Option<CooledRow>, StorageError> {
    sqlx::query_as(
        "SELECT t, handle, owner_id, kind::text, object_key, blob_id, content_id,
                source_id, ingest_key, origins, refs, goal_refs, cold_digest, \"by\", via
           FROM proxima_core.cooled
          WHERE t = $1",
    )
    .bind(t)
    .fetch_optional(conn)
    .await
    .map_err(map_err)
}

/// Hard-delete one admission of an abandoned owner.
///
/// Returns the cold objects the erase still owes the object store: the caller
/// commits this transaction and then calls
/// [`purge_cold_objects_after_commit`]. The cold delete cannot run here —
/// a rollback after it would restore the `cooled` locator over destroyed
/// bytes.
#[allow(clippy::too_many_lines)] // the ordered hard-delete transaction keeps atomic DML sequencing visible
pub async fn erase_memory(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    context: &crate::PgHostStateEraseContext,
    owner: &OwnerId,
    t: Uuid,
) -> Result<ColdPurgePlan, StorageError> {
    context.lock_before_physical_erase(tx).await?;
    let surfaces = &context.surfaces;
    // Probe the series identity first. The handle lock is the first
    // serialization boundary; a missing target is not a successful no-op.
    let probed_handle = probe_memory_handle_for_owner(tx, t, owner.into_inner())
        .await?
        .ok_or(StorageError::NotFound)?;
    lock_memory_handles_tx(tx, &[probed_handle]).await?;
    lock_lifecycle_targets_tx(tx, &[t]).await?;
    let handle = probe_memory_handle_for_owner(tx, t, owner.into_inner())
        .await?
        .ok_or(StorageError::NotFound)?;
    if handle != probed_handle {
        return Err(StorageError::Retryable(
            "memory target changed series while erasing".into(),
        ));
    }
    let content_id = sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT content_id FROM proxima_core.memory WHERE t = $1 AND owner_id = $2 /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
         UNION ALL
         SELECT content_id FROM proxima_core.cooled WHERE t = $1 AND owner_id = $2
         LIMIT 1",
    )
    .bind(t)
    .bind(owner.into_inner())
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    .flatten();
    let is_fact: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM proxima_core.memory WHERE t = $1 AND owner_id = $2 AND kind = 'fact' /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
             UNION ALL
             SELECT 1 FROM proxima_core.cooled WHERE t = $1 AND owner_id = $2 AND kind = 'fact'
         )",
    )
    .bind(t)
    .bind(owner.into_inner())
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;
    crate::verbs::owner_erase::invoke_host_lifecycle_exact_erase(
        tx,
        context,
        *owner,
        if is_fact {
            std::slice::from_ref(&t)
        } else {
            &[]
        },
    )
    .await?;
    let pending: Vec<String> = sqlx::query_scalar(
        "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id)
         SELECT c.object_key, c.owner_id
           FROM proxima_core.cooled c
          WHERE c.t = $1 AND c.owner_id = $2
         ON CONFLICT (object_key) DO UPDATE SET enqueued_at = now()
         RETURNING object_key",
    )
    .bind(t)
    .bind(owner.into_inner())
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    let stamped: Vec<String> = sqlx::query_scalar(
        "SELECT unnest(sidecar_tables) FROM proxima_core.memory WHERE t = $1 AND owner_id = $2", /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
    )
    .bind(t)
    .bind(owner.into_inner())
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    // Before the memory and cooled rows: a kept surface with no owner column
    // (memory_external_origin) finds its owner through its memory, hot OR
    // cooled, so its delete must run while that parent is still there.
    delete_memory_dependents(tx, sidecars, surfaces, &stamped, t, DependentsFor::Erase).await?;
    sqlx::query("DELETE FROM proxima_core.cooled WHERE t = $1 AND owner_id = $2")
        .bind(t)
        .bind(owner.into_inner())
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1 AND owner_id = $2") /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
        .bind(t)
        .bind(owner.into_inner())
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    if let Some(id) = content_id {
        super::content::gc_unreferenced_content(tx, id).await?;
    }
    sync_memory_head(tx, handle).await?;
    sqlx::query("DELETE FROM proxima_core.ingest_keys WHERE t = $1 AND owner_id = $2")
        .bind(t)
        .bind(owner.into_inner())
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    // The captured publication, if this Fact had one. Hand-written for the
    // same reason as the receipt above: the row carries no foreign key into
    // `memory` (it must outlive `forget`'s delete of the hot row), so the
    // generated memory-dependent legs do not reach it. Erasing a single
    // admission destroys its captured payload — an undelivered event is
    // still this owner's content, and compliance erasure is the one and
    // only way a record leaves this table.
    //
    // On `t` ALONE, deliberately, and this is the one delete in this
    // function without an `owner_id` predicate. `t` is the outbox's primary
    // key, so the predicate selects exactly this admission's record. Adding
    // `AND owner_id = $2` would look safer and would leak: the surface
    // declares `TransferRule::RetainAtSource`, so a transferred series
    // leaves its already-captured record under the SOURCE owner while the
    // memory row moves to the destination — and the destination erasing the
    // memory would then walk past a record still holding the full typed
    // payload. The caller has already authorized the erase of this `t`; the
    // record is that `t`'s content whichever owner it was captured for.
    sqlx::query("DELETE FROM proxima_core.publication_origin WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.publication_outbox WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(ColdPurgePlan::from_keys(pending))
}

/// Hard-delete every version of every series the given admissions belong to.
///
/// The kernel's own erase, applied to a set. A flavor tearing down one of
/// its scopes — a repo, a workspace, a project — knows which of ITS rows
/// belong to that scope and nothing about the substrate rows behind them;
/// this is the other half, and it is deliberately flavor-agnostic so the
/// flavor never writes a `proxima_core` statement to get it.
///
/// Expansion to the whole series matters: a superseded version that was
/// cooled has no sidecar row left to be found by, so a caller that
/// collected ids from its own tables would erase the live versions and
/// leave the cooled ones behind.
///
/// A cited blob goes when the last admission citing it does. That leg is
/// here rather than in [`erase_memory`] because it is a question about the
/// SET: erasing one version of a series says nothing about whether the
/// blob is still cited, and asking per-version would answer "still cited"
/// for every version but the last. `proxima_core.blob` is referenced by
/// `memory.blob_id`, `cooled.blob_id` and `blob_uploads.blob_id` and by
/// nothing else, so once the admissions are gone the reference question is
/// answerable in one statement, which is what `gc_unreferenced_blobs`
/// below is.
///
/// Goal references are not foreign keys. Erased evidence resolves as
/// unavailable; source-scoped erasure leaves the Goal row intact.
///
/// Returns the number of admissions erased and the cold objects the erase
/// still owes the object store; see [`erase_memory`] for why the caller
/// commits first.
///
/// # Errors
///
/// Returns storage errors from the delete statements.
pub async fn erase_memory_series(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    context: &crate::PgHostStateEraseContext,
    owner: &OwnerId,
    ts: &[Uuid],
) -> Result<(u64, ColdPurgePlan), StorageError> {
    if ts.is_empty() {
        return Ok((0, ColdPurgePlan::default()));
    }
    context.lock_before_physical_erase(tx).await?;
    let owner_id = owner.into_inner();
    // Capture handles and membership in one statement before waiting. The
    // membership is the generation witness: handles are reusable after a
    // complete erase, so a non-empty post-lock series disjoint from this
    // snapshot is a replacement, not a late version of the requested series.
    let before = snapshot_series_for_erase_tx(tx, owner_id, ts).await?;
    erase_memory_series_after_snapshot(tx, sidecars, context, owner, before).await
}

async fn erase_memory_series_after_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    context: &crate::PgHostStateEraseContext,
    owner: &OwnerId,
    before: MemorySeriesSnapshot,
) -> Result<(u64, ColdPurgePlan), StorageError> {
    let owner_id = owner.into_inner();
    let handles = before.handles();
    lock_memory_handles_tx(tx, &handles).await?;
    let after = snapshot_series_for_handles_tx(tx, owner_id, &handles).await?;
    if generation_replaced(&before, &after) {
        return Err(StorageError::Retryable(
            "memory series was replaced while erase waited for its handle lock".into(),
        ));
    }
    let versions = after.versions();

    // Expansion includes every hot and cooled version before any cited blob
    // or row lock. Re-entering these locks in erase_memory is intentional: it
    // protects direct callers while preserving one global lock vocabulary.
    lock_lifecycle_targets_tx(tx, &versions).await?;

    // Captured BEFORE the loop, because the loop is what makes them
    // unreferenced: after it, no row is left to read `blob_id` from.
    let cited = cited_blobs_locked_for_erase(tx, owner, &versions).await?;

    let mut entries = Vec::new();
    let mut erased = 0_u64;
    for t in versions {
        let plan = erase_memory(tx, sidecars, context, owner, t).await?;
        entries.extend_from_slice(plan.entries());
        erased += 1;
    }
    entries.extend(gc_unreferenced_blobs(tx, owner_id, &cited).await?);
    Ok((erased, ColdPurgePlan::from_entries(entries)))
}

/// Every version of every series the given admissions belong to, for this
/// owner.
///
/// The expansion [`erase_memory_series`] performs, exposed so a caller that
/// must reason about the FULL set before touching any of it can ask for it
/// first. A flavor computing an erase footprint needs exactly that: it
/// finds admissions through its own rows, but the substrate erases whole
/// series, and the versions the expansion adds are as much part of the
/// footprint as the ones the flavor named. A footprint computed without
/// this is a footprint that closes and locks the wrong set — the flavor
/// then deletes rows referencing a version it never saw, which is an abort,
/// not a leak.
///
/// Owner-scoped on both legs, so a handle that changed hands never widens
/// one owner's erase into another's rows.
///
/// # Errors
///
/// Returns storage errors from the expansion query.
pub async fn expand_series_for_erase(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    ts: &[Uuid],
) -> Result<Vec<Uuid>, StorageError> {
    if ts.is_empty() {
        return Ok(Vec::new());
    }
    Ok(snapshot_series_for_erase_tx(tx, owner.into_inner(), ts)
        .await?
        .versions())
}

#[derive(Debug)]
struct MemorySeriesSnapshot {
    members: BTreeSet<(Uuid, Uuid)>,
}

impl MemorySeriesSnapshot {
    fn handles(&self) -> Vec<Uuid> {
        self.members
            .iter()
            .map(|(handle, _)| *handle)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn versions(&self) -> Vec<Uuid> {
        self.members
            .iter()
            .map(|(_, t)| *t)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// Did any handle in `after` turn over completely since `before` was taken?
///
/// Arguments are in chronological order. A handle is *witnessed* when at
/// least one of its `(handle, t)` members survived from `before`: the erase
/// is still looking at the same generation of that series, and the members
/// `before` did not have are late versions the expansion legitimately picks
/// up. A handle present in `after` with no surviving member was erased down
/// to nothing and re-admitted under the same handle, so `after` describes a
/// replacement series that this erase was never authorized to touch.
fn generation_replaced(before: &MemorySeriesSnapshot, after: &MemorySeriesSnapshot) -> bool {
    let witnessed = after
        .members
        .intersection(&before.members)
        .map(|(handle, _)| *handle)
        .collect::<BTreeSet<_>>();
    after
        .members
        .iter()
        .any(|(handle, _)| !witnessed.contains(handle))
}

/// Resolve every series and version named by target admissions in one
/// statement snapshot. Callers that mutate acquire `handles` in sorted order
/// and use the member pairs to distinguish late members from handle reuse.
async fn snapshot_series_for_erase_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    ts: &[Uuid],
) -> Result<MemorySeriesSnapshot, StorageError> {
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(SNAPSHOT_SERIES_FOR_ERASE_SQL)
        .bind(ts)
        .bind(owner_id)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(MemorySeriesSnapshot {
        members: rows.into_iter().collect(),
    })
}

const SNAPSHOT_SERIES_FOR_ERASE_SQL: &str = "\
WITH handles AS (
    SELECT handle FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
     WHERE t = ANY($1::uuid[]) AND owner_id = $2
    UNION
    SELECT handle FROM proxima_core.cooled
     WHERE t = ANY($1::uuid[]) AND owner_id = $2
), versions AS (
    SELECT m.handle, m.t FROM proxima_core.memory m /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
      JOIN handles h ON h.handle = m.handle
     WHERE m.owner_id = $2
    UNION
    SELECT c.handle, c.t FROM proxima_core.cooled c
      JOIN handles h ON h.handle = c.handle
     WHERE c.owner_id = $2
)
SELECT handle, t FROM versions ORDER BY handle, t";

async fn snapshot_series_for_handles_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    handles: &[Uuid],
) -> Result<MemorySeriesSnapshot, StorageError> {
    if handles.is_empty() {
        return Ok(MemorySeriesSnapshot {
            members: BTreeSet::new(),
        });
    }
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(SNAPSHOT_SERIES_BY_HANDLES_SQL)
        .bind(handles)
        .bind(owner_id)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(MemorySeriesSnapshot {
        members: rows.into_iter().collect(),
    })
}

const SNAPSHOT_SERIES_BY_HANDLES_SQL: &str = "\
SELECT m.handle, m.t FROM proxima_core.memory m /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
  JOIN unnest($1::uuid[]) h(handle) ON h.handle = m.handle
 WHERE m.owner_id = $2
UNION
SELECT c.handle, c.t FROM proxima_core.cooled c
  JOIN unnest($1::uuid[]) h(handle) ON h.handle = c.handle
 WHERE c.owner_id = $2
 ORDER BY handle, t";

/// Which of `ts` are NOT admissions of `owner`.
///
/// A flavor discovers admissions through its own tables, and a flavor table
/// holds a foreign key on `proxima_core.memory (t)` and nothing that
/// constrains WHOSE memory it names. One owner's row may therefore reference
/// another owner's admission — by writing the id, or, more ordinarily,
/// because the admission was transferred out from under it.
///
/// A scope erase that follows such a reference would delete a principal's
/// rows on the authority of a different principal, and it would do it
/// silently. This is the question that makes that unrepresentable, and it
/// lives here rather than in the flavor for the same reason
/// [`lock_admissions_for_erase`] does: the answer is in `proxima_core`.
///
/// # Errors
///
/// Returns storage errors from the query.
pub async fn admissions_outside_owner(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    ts: &[Uuid],
) -> Result<Vec<Uuid>, StorageError> {
    if ts.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar(ADMISSIONS_OUTSIDE_OWNER_SQL)
        .bind(ts)
        .bind(owner.into_inner())
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)
}

const ADMISSIONS_OUTSIDE_OWNER_SQL: &str = "\
SELECT c.t
  FROM unnest($1::uuid[]) AS c(t)
 WHERE NOT EXISTS (
           SELECT 1 FROM proxima_core.memory m /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
            WHERE m.t = c.t AND m.owner_id = $2
       )
   AND NOT EXISTS (
           SELECT 1 FROM proxima_core.cooled k
            WHERE k.t = c.t AND k.owner_id = $2
       )";

/// Take a row lock on admissions a scope erase is about to delete.
///
/// Not an optimisation and not a formality, and flavor-agnostic for the
/// same reason [`erase_memory_series`] is: a flavor computing which of its
/// rows belong to a scope needs several statements to do it, `READ
/// COMMITTED` gives each of them its own snapshot, and a row committed in
/// any of the gaps references a memory that is about to go. With a
/// `NO ACTION` foreign key that is not a leak — it is an abort, after all
/// the work.
///
/// `FOR UPDATE` on the referenced `memory` rows closes the gaps for real.
/// Inserting a row with a foreign key takes `FOR KEY SHARE` on the row it
/// references, and `FOR UPDATE` conflicts with `FOR KEY SHARE`, so a
/// concurrent writer that would create a dangling pointer blocks until this
/// transaction commits and then fails its own foreign key — in its
/// transaction rather than in the erase.
///
/// CALL IT ONCE, OVER THE WHOLE SET. The locks do last until the
/// transaction ends, so calling it per round of a fixpoint "works" — and
/// puts an arbitrarily long window between the rounds in which this
/// transaction holds some of the rows and is not yet asking for the rest.
/// A writer holding `FOR KEY SHARE` on a later row and waiting on an
/// earlier one turns that window into a deadlock, and `PostgreSQL` picks a
/// victim by which transaction is cheapest to abort, which is the one that
/// has not yet done its deletes: the erase. Computing the full set first
/// and locking it in one statement shrinks that window to the inside of
/// this statement.
///
/// The series handles are acquired before this lifecycle set. `ORDER BY t` is
/// the other half: two erases whose sets overlap then take the shared rows in
/// the same order and one waits instead of both dying.
/// It does not help against a writer, which takes its own locks in its own
/// order — nothing does, which is why a caller must also be prepared to
/// retry a `40P01`.
///
/// Handle resolution and the post-lock presence check are both scoped to
/// `owner`. If transfer wins after the caller's earlier ownership check, the
/// stale erase returns [`StorageError::Retryable`] without serializing the
/// destination owner's series behind its transaction lifetime.
///
/// # Errors
///
/// Returns storage errors from the lock statement.
pub async fn lock_admissions_for_erase(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    ts: &[Uuid],
) -> Result<(), StorageError> {
    if ts.is_empty() {
        return Ok(());
    }
    // This is the mandatory seam before any admission `t` or row lock. The
    // caller has computed its complete footprint but has not started locking
    // it yet; acquire every named series handle here so a later series erase
    // only re-enters handles this transaction already owns.
    let handles: Vec<Uuid> = sqlx::query_scalar(
        "SELECT handle FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
          WHERE t = ANY($1::uuid[]) AND owner_id = $2
        UNION
        SELECT handle FROM proxima_core.cooled
          WHERE t = ANY($1::uuid[]) AND owner_id = $2
          ORDER BY handle",
    )
    .bind(ts)
    .bind(owner.into_inner())
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    lock_memory_handles_tx(tx, &handles).await?;

    let present: Vec<Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
          WHERE t = ANY($1::uuid[]) AND owner_id = $2
        UNION
        SELECT t FROM proxima_core.cooled
          WHERE t = ANY($1::uuid[]) AND owner_id = $2
          ORDER BY t",
    )
    .bind(ts)
    .bind(owner.into_inner())
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    let mut expected = ts.to_vec();
    expected.sort_unstable();
    expected.dedup();
    if present != expected {
        return Err(StorageError::Retryable(
            "erase admission footprint changed before lifecycle lock acquisition".into(),
        ));
    }
    lock_lifecycle_targets_tx(tx, ts).await?;
    sqlx::query(LOCK_ADMISSIONS_SQL)
        .bind(ts)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(())
}

const LOCK_ADMISSIONS_SQL: &str =
    "SELECT t FROM proxima_core.memory WHERE t = ANY($1::uuid[]) ORDER BY t FOR UPDATE"; /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */

/// The blobs the given admissions cite, held.
///
/// Capture and lock in ONE statement, because doing one without the other
/// is never right and a seam between them is a line someone can delete.
/// `gc_unreferenced_blobs` decides "no admission cites this blob" and then
/// deletes it; `memory.blob_id` is `NO ACTION`, so a citation committed
/// between the deciding and the deleting makes the delete wait on the
/// citer's `FOR KEY SHARE`, and then raise `23503` and take the whole erase
/// down with it. `FOR UPDATE` taken here — before the erase loop, while the
/// citing rows are all still there — is what turns that into the citer
/// waiting on us and then failing its own foreign key in its own
/// transaction.
///
/// `FOR UPDATE` and not `FOR NO KEY UPDATE`. `FOR NO KEY UPDATE` is the
/// weaker mode that PERMITS concurrent key-share holders — exactly the
/// writer this has to exclude — so it would leave the race untouched while
/// looking like a fix. Measured on `PostgreSQL` 18, not assumed: against a
/// `FOR NO KEY UPDATE` holder a second session takes `FOR KEY SHARE` on the
/// same row immediately; against a `FOR UPDATE` holder it blocks until the
/// holder's transaction ends.
///
/// Locking every cited blob rather than only the orphans over-locks by
/// design: which of them are orphans is not known until the erase loop has
/// run, and a lock taken after the question is answered is a lock taken too
/// late. They are this owner's blobs and the transaction is short.
///
/// # Errors
///
/// Returns storage errors from the statement.
pub async fn cited_blobs_locked_for_erase(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    ts: &[Uuid],
) -> Result<Vec<Uuid>, StorageError> {
    if ts.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar(CITED_BLOBS_LOCKED_SQL)
        .bind(ts)
        .bind(owner.into_inner())
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)
}

/// The blobs the admissions about to be erased cite, locked against a
/// concurrent citation.
///
/// The `FOR UPDATE` binds to `b` — the one plain table in the outer
/// `FROM` — which is why the two-legged citation question lives in a CTE
/// rather than in a `UNION` the locking clause could not attach to.
const CITED_BLOBS_LOCKED_SQL: &str = "\
WITH cited AS (
    SELECT m.blob_id FROM proxima_core.memory m /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
     WHERE m.t = ANY($1::uuid[]) AND m.owner_id = $2 AND m.blob_id IS NOT NULL
    UNION
    SELECT c.blob_id FROM proxima_core.cooled c
     WHERE c.t = ANY($1::uuid[]) AND c.owner_id = $2 AND c.blob_id IS NOT NULL
)
SELECT b.blob_id FROM proxima_core.blob b
 WHERE b.blob_id IN (SELECT blob_id FROM cited)
   AND b.owner_id = $2
 ORDER BY b.blob_id
   FOR UPDATE";

/// Delete each of `cited` that no admission still cites, with its upload
/// rows, and enqueue the objects that no other upload row names.
///
/// `gc_unreferenced_content`'s idiom, at the blob level, plus the refcount
/// a shared object needs when several upload rows name it.
/// `proxima_core.blob` is
/// referenced by exactly three columns — `memory.blob_id`,
/// `cooled.blob_id`, `blob_uploads.blob_id`, all `NO ACTION` — so "no
/// admission cites it" is the whole reference question. Citation schemas
/// are opaque and have no sidecar table, which is why
/// `OwnerSurfaces::for_registry` finds no cited-object family for
/// core or code to consult.
///
/// The three deletes and the enqueue are one statement so they share one
/// snapshot. That is what makes the object anti-join ask the right
/// question: "does a row OUTSIDE this orphan set name the key", evaluated
/// against the rows as they stood before any of them went, exactly as the
/// owner-erase arm evaluates it.
///
/// One snapshot is not one lock, though: the reference question is asked of
/// this transaction's snapshot and the delete is enforced against the
/// latest one, so a citation committed in between still aborts it. Every
/// candidate is held under `FOR UPDATE` from before the erase loop for
/// that; see [`cited_blobs_locked_for_erase`].
///
/// `b.owner_id = $1` is the Lean citation invariant restated
/// (`memory_cites m b -> memory_owner m = blob_owner b`). It costs nothing
/// when the invariant holds and, if it ever did not, leaves the row rather
/// than deleting another owner's blob.
async fn gc_unreferenced_blobs(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    cited: &[Uuid],
) -> Result<Vec<ColdPurgeEntry>, StorageError> {
    if cited.is_empty() {
        return Ok(Vec::new());
    }
    // One snapshot is still not one lock for the OBJECT question. The
    // anti-join below asks "does a row outside this orphan set name the key",
    // and another owner's erase asking the mirror-image question concurrently
    // would have both withhold and leak the bytes. The object-key fence is
    // what serializes the two; `FOR UPDATE` on `blob` cannot, because the
    // rows racing here belong to a different owner.
    let object_keys = crate::access::owner_columns::blob_upload_object_keys(tx, cited).await?;
    crate::access::owner_columns::lock_object_keys_tx(tx, &object_keys).await?;
    sqlx::query_as::<_, (String, String)>(GC_UNREFERENCED_BLOBS_SQL)
        .bind(cited)
        .bind(owner_id)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)
        .map(|rows| {
            rows.into_iter()
                .map(|(object_key, backend)| ColdPurgeEntry {
                    object_key,
                    backend,
                })
                .collect()
        })
}

const GC_UNREFERENCED_BLOBS_SQL: &str = "\
WITH orphans AS MATERIALIZED (
    SELECT b.blob_id
      FROM proxima_core.blob b
     WHERE b.blob_id = ANY($1::uuid[])
       AND b.owner_id = $2
       AND NOT EXISTS (
               SELECT 1 FROM proxima_core.memory m WHERE m.blob_id = b.blob_id /* FORGET-POLICY: includes-forgotten (a forgotten memory still cites its content) */
           )
       AND NOT EXISTS (
               SELECT 1 FROM proxima_core.cooled c WHERE c.blob_id = b.blob_id
           )
),
enqueued AS (
    INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, backend)
    SELECT DISTINCT ON (u.object_key) u.object_key, u.owner_id, u.bucket
      FROM proxima_core.blob_uploads u
      JOIN orphans o ON o.blob_id = u.blob_id
     WHERE NOT EXISTS (
               SELECT 1
                 FROM proxima_core.blob_uploads other
                WHERE other.object_key = u.object_key
                  AND other.upload_id <> u.upload_id
                  AND NOT EXISTS (
                          SELECT 1 FROM orphans o2 WHERE o2.blob_id = other.blob_id
                      )
           )
     ORDER BY u.object_key, u.upload_id
    ON CONFLICT (object_key) DO UPDATE
       SET enqueued_at = now(), backend = EXCLUDED.backend
    RETURNING object_key, backend
),
d_uploads AS (
    DELETE FROM proxima_core.blob_uploads u
     USING orphans o
     WHERE u.blob_id = o.blob_id
),
d_blobs AS (
    DELETE FROM proxima_core.blob b
     USING orphans o
     WHERE b.blob_id = o.blob_id
)
-- `d_uploads` and `d_blobs` are read by nothing on purpose: a
-- data-modifying WITH clause runs exactly once and to completion whether
-- or not the primary query reads its output.
SELECT object_key, backend FROM enqueued";

// The forget/erase/hydrate suite. Crate-internal because it works at this
// layer deliberately: it stamps `sidecar_tables` lists by hand — including a
// registered table that does not exist in Postgres — to prove forget reads
// the stamp rather than the registry. The governed port would refuse those
// drafts, which is exactly why the test cannot be written above it.
#[cfg(test)]
#[path = "forget_pg_tests.rs"]
pub(crate) mod forget_pg_tests;

#[cfg(test)]
mod tests {
    use super::{ForgetLeg, OwnerSurfaces, forget_leg_sql};

    fn shipped() -> OwnerSurfaces {
        OwnerSurfaces::for_registry(
            &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
        )
    }

    /// The declared walk reaches exactly the four derived-row tables and
    /// nothing else, with the embedding queue first (see
    /// `generated_forget_legs`).
    ///
    /// The key column is pinned too: three of the four are keyed on
    /// `entity_id`, not `t`. A walk that assumed `t` because the sidecar
    /// half does would delete nothing from any of them, and both lanes share
    /// `forget_leg_sql` precisely so that assumption cannot be made in one
    /// place and not the other.
    #[test]
    fn the_declared_forget_walk_is_exactly_the_derived_row_tables() {
        assert_eq!(
            shipped().generated_forget_legs(),
            vec![
                ("proxima_core.embedding_jobs", "entity_id"),
                ("proxima_core.embedding_heads", "entity_id"),
                ("proxima_core.embeddings", "entity_id"),
                ("proxima_core.sketch", "t"),
            ]
        );
    }

    /// No surface resolves to `Unreachable`, and every leg this crate must
    /// execute is one it can build. The forget counterpart of
    /// `owner_erase`'s `every_declared_surface_has_a_leg_this_crate_can_run`.
    #[test]
    fn every_declared_surface_has_a_forget_leg_this_crate_can_run() {
        let surfaces = shipped();
        assert!(surfaces.surfaces().len() > 20);
        for surface in surfaces.surfaces() {
            match surfaces.forget_leg(surface.table) {
                ForgetLeg::Unreachable => panic!(
                    "{} resolves to Unreachable, which freeze should have refused",
                    surface.table
                ),
                ForgetLeg::Dumped { key_column } | ForgetLeg::Deleted { key_column } => {
                    forget_leg_sql(surface.table, key_column).unwrap_or_else(|e| {
                        panic!("{} is a leg this crate cannot build: {e}", surface.table)
                    });
                }
                ForgetLeg::DumpedCascade { .. }
                | ForgetLeg::Cascaded { .. }
                | ForgetLeg::Kept { .. } => {}
            }
        }
    }

    /// The statement, pinned. One shape serves both iteration sources.
    #[test]
    fn a_forget_leg_deletes_the_declared_key_column() {
        assert_eq!(
            forget_leg_sql("proxima_core.embeddings", "entity_id").expect("generates"),
            "DELETE FROM proxima_core.embeddings WHERE entity_id = $1"
        );
        assert!(
            forget_leg_sql("proxima_core.embeddings", "entity_id; DROP TABLE x").is_err(),
            "the identifier whitelist is what makes this %I-equivalent"
        );
    }
}

#[cfg(test)]
mod erase_kept_partition_tests {
    use super::{ERASE_MEMORY_EXPLICIT, erase_kept_memory_legs};
    use proxima_core::flavor::{EraseLeg, ForgetLeg, KeyShape};
    use proxima_core::owner_inverse::OwnerSurfaces;

    /// The hard erase of one admission reaches every memory-keyed surface a
    /// forget keeps: either by a named statement of its own or by the
    /// generated set, never both and never neither.
    #[test]
    fn explicit_and_generated_sets_partition_every_kept_memory_keyed_surface() {
        let surfaces = OwnerSurfaces::for_registry(
            &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
        );
        let generated: Vec<&str> = erase_kept_memory_legs(&surfaces)
            .into_iter()
            .map(|(table, _)| table)
            .collect();
        let kept_keyed: Vec<&str> = surfaces
            .surfaces()
            .iter()
            .filter(|surface| {
                matches!(
                    surfaces.erase_leg(surface.table),
                    EraseLeg::Keyed(KeyShape::MemoryT { .. })
                ) && matches!(surfaces.forget_leg(surface.table), ForgetLeg::Kept { .. })
            })
            .map(|surface| surface.table)
            .collect();
        for table in &kept_keyed {
            let explicit = ERASE_MEMORY_EXPLICIT.contains(table);
            assert!(
                explicit != generated.contains(table),
                "{table} must be reached by exactly one of the explicit and generated sets"
            );
        }
        for table in &generated {
            assert!(
                kept_keyed.contains(table),
                "{table} is not a kept memory-keyed surface"
            );
        }
        assert!(
            generated.contains(&"proxima_core.memory_external_origin"),
            "the other-store origins are reached by the generated set: {generated:?}"
        );
    }
}
