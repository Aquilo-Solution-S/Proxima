//! Forget / hydrate / erase.
#![allow(clippy::missing_errors_doc, clippy::doc_markdown)]

use std::collections::BTreeSet;

use proxima_core::flavor::ForgetLeg;
use proxima_core::owner_inverse::OwnerSurfaces;
use proxima_core::{
    ColdObjectStore, MemoryHydrationBatchOutcome, MemoryHydrationOutcome, MemoryHydrationStatus,
    Owner, StorageError,
};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::sidecars::PgSidecarRegistryFrozen;

/// Version 6 adds the exact `memory.sidecar_tables` stamp to each object.
/// Version 7 adds contract-declared cascaded detail declarations and their
/// exact rows. A v6 object can only be hydrated for a schema with no such
/// detail surfaces; accepting it for a schema that has one would silently
/// discard rows deleted by the parent FK.
/// Hydration refuses older records because a dump list alone cannot prove
/// which sidecars the original Memory admission declared.
pub const COLD_FORMAT_VERSION: u8 = 7;

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
    goal_refs: Vec<Uuid>,
    sidecar_tables: Vec<String>,
    content_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct CooledRow {
    t: Uuid,
    handle: Uuid,
    owner_id: Uuid,
    kind: String,
    object_key: String,
    blob_id: Option<Uuid>,
    content_id: Option<Uuid>,
    source_id: Option<String>,
    ingest_key: Option<String>,
    origins: Option<Vec<Uuid>>,
    refs: Option<Vec<Uuid>>,
    goal_refs: Option<Vec<Uuid>>,
    cold_digest: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdRecord {
    row: HotRow,
    schema_id: String,
    sidecar_dumps: Vec<(String, String)>,
    /// Contract-declared `ON DELETE CASCADE` detail rows. Each declaration is
    /// present even when it has zero rows, so a cold record cannot add a
    /// forged table or omit a surface during restore.
    detail_dumps: Vec<(String, Vec<String>)>,
    /// Model ids that had vectors. Vectors stay out of the object; hydrate
    /// enqueues embed jobs for these ids.
    embed_models: Vec<String>,
    /// Exact persisted one-liner. v4+; older cold objects restore from sidecar/kind.
    sketch: Option<String>,
    /// Format version this record was decoded from, or the current version
    /// for a record built from a live row. Hydrate consults it because a
    /// pre-v5 object's `refs` still mixes Memory and Goal ids and has to be
    /// separated against the Goal spine before it can be re-inserted, while
    /// pre-v6 objects have no authenticated sidecar stamp and fail closed;
    /// pre-v7 objects have no exact cascaded-detail declaration and fail
    /// closed for schemas that declare one.
    format_version: u8,
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
    write_uuid_list(&mut out, &rec.row.goal_refs)?;
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
    write_str_list(&mut out, &rec.embed_models)?;
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

fn cold_digest(bytes: &[u8]) -> Vec<u8> {
    blake3::hash(bytes).as_bytes().to_vec()
}

fn decode_record(bytes: &[u8]) -> Result<ColdRecord, StorageError> {
    let mut i = 0;
    let version = read_u8(bytes, &mut i)?;
    if !matches!(version, 1..=COLD_FORMAT_VERSION) {
        return Err(StorageError::Internal(format!(
            "unknown cold format {version}"
        )));
    }
    let mut row = HotRow {
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
        // v<=4 predates the pin split, so its `refs` is still a mixed array.
        // It is separated on hydrate against the live Goal spine — the same
        // predicate migration 0004 backfills hot rows with — because the
        // object alone cannot say which ids were Goals.
        goal_refs: if version >= 5 {
            read_uuid_list(bytes, &mut i)?
        } else {
            Vec::new()
        },
        sidecar_tables: Vec::new(),
        content_id: None,
    };
    let schema_id = read_str(bytes, &mut i)?;
    let sidecar_tables = if version >= 6 {
        read_str_list(bytes, &mut i)?
    } else {
        Vec::new()
    };
    row.sidecar_tables = sidecar_tables;
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
    let detail_dumps = if version >= 7 {
        let n = usize::from(read_u16(bytes, &mut i)?);
        let mut dumps = Vec::with_capacity(n);
        for _ in 0..n {
            let table = read_str(bytes, &mut i)?;
            let rows = usize::from(read_u16(bytes, &mut i)?);
            let mut values = Vec::with_capacity(rows);
            for _ in 0..rows {
                values.push(read_str(bytes, &mut i)?);
            }
            dumps.push((table, values));
        }
        dumps
    } else {
        Vec::new()
    };
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
        embed_models,
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

/// The row stamp is the authority for which movable sidecars belong to an
/// admission. A v6 object carries that list beside the row dumps. The
/// registry's retained owner-pinned tables are intentionally absent from the
/// dumps, so they are removed from the expected list before comparison; an
/// owner-pinned dump itself still fails the sidecar preflight below.
fn cold_sidecar_stamp_matches(rec: &ColdRecord, sidecars: &PgSidecarRegistryFrozen) -> bool {
    if rec.format_version < 6 {
        return false;
    }
    let mut seen_stamp = BTreeSet::new();
    if rec
        .row
        .sidecar_tables
        .iter()
        .any(|table| !seen_stamp.insert(table.as_str()))
    {
        return false;
    }
    let expected = dumpable_stamped_tables(&rec.row.sidecar_tables, sidecars);
    let actual = rec.sidecar_dumps.iter().map(|(table, _)| table.as_str());
    expected.into_iter().eq(actual)
}

/// A detail stamp is a declaration, not merely a list of rows. Every
/// contract-declared cascaded table is represented, including an empty one;
/// anything else would let an external object add a table or quietly omit a
/// table whose parent FK deleted rows during cooling.
fn cold_detail_stamp_matches(rec: &ColdRecord, surfaces: &OwnerSurfaces) -> bool {
    let expected = surfaces.cascaded_details_for_schema(&rec.schema_id);
    if rec.format_version < 7 {
        return expected.is_empty() && rec.detail_dumps.is_empty();
    }
    expected.len() == rec.detail_dumps.len()
        && expected
            .iter()
            .zip(&rec.detail_dumps)
            .all(|(expected, (actual_table, _))| expected.table == actual_table)
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

const HOT_ROW_SQL: &str = "SELECT handle, t, kind::text, owner_id, schema_id, source_id, ingest_key, blob_id, origins, refs, goal_refs, sidecar_tables, content_id
           FROM proxima_core.memory
          WHERE t = $1";

const HOT_ROW_FOR_UPDATE_OWNED_SQL: &str = "SELECT handle, t, kind::text, owner_id, schema_id, source_id, ingest_key, blob_id, origins, refs, goal_refs, sidecar_tables, content_id
           FROM proxima_core.memory
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
    let embed_models = load_embed_models(conn, t).await?;
    let sketch = load_sketch_text(conn, t).await?;
    Ok(ColdRecord {
        row,
        schema_id,
        sidecar_dumps,
        detail_dumps,
        embed_models,
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
/// grounding lifecycle set — `forget_memory` and `forget_memory_oneshot` take
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
    let embed_models = load_embed_models(tx.as_mut(), t).await?;
    let sketch = load_sketch_text(tx.as_mut(), t).await?;
    let current = ColdRecord {
        row: locked,
        schema_id,
        sidecar_dumps,
        detail_dumps,
        embed_models,
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
             origins, refs, goal_refs, cold_digest)
         VALUES ($1, $2, $3, $4::proxima_core.memory_kind, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
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
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    delete_memory_dependents(tx, sidecars, surfaces, &current.row.sidecar_tables, t).await?;
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
/// the cited-object purge follows. Returns the number of objects destroyed and
/// cleared.
pub async fn purge_cold_objects_after_commit(
    pool: &PgPool,
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

/// The object is gone, so its debt is gone: one statement, no second write.
///
/// The queue IS the debt. `pending_cold_purge_count` answers "is anything
/// owed" by counting rows, not by trusting a flag that a crash between two
/// writes could leave set forever.
async fn clear_cold_purge_pending(pool: &PgPool, object_key: &str) -> Result<(), StorageError> {
    sqlx::query("DELETE FROM proxima_core.cold_purge_pending WHERE object_key = $1")
        .bind(object_key)
        .execute(pool)
        .await
        .map_err(map_err)?;
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
        "SELECT handle FROM proxima_core.memory
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
        "SELECT t, handle FROM proxima_core.memory
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
    if grounded.iter().any(|(_, handle)| !handles.contains(handle)) {
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
        "SELECT t, handle FROM proxima_core.memory
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
        dependents.binary_search(target).is_ok() && handles.contains(handle)
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

/// Validate every sidecar dump before the hydration atom writes its Memory
/// row. The dump is normally produced by [`dump_stamped_sidecars`], but the
/// cold store is an external durability boundary and must be treated as
/// untrusted input on restore.
///
/// This checks the properties a generic `jsonb_populate_record` call cannot:
/// one table only, a registered and hydratable surface, the declared memory
/// key naming this exact `t`, no owner-bearing audit payload, no unknown
/// columns, and every required column present and non-null. The final
/// `jsonb_populate_record` probe makes PostgreSQL validate UUID, array, enum,
/// and other column types without inserting a row.
async fn validate_cold_sidecars(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    dumps: &[(String, String)],
    t: Uuid,
) -> Result<bool, StorageError> {
    let mut seen_tables = BTreeSet::new();
    for (table, json) in dumps {
        if !sidecars.is_hydratable_memory_sidecar_table(table)
            || is_owner_pinned(sidecars, table)
            || !seen_tables.insert(table.as_str())
        {
            return Ok(false);
        }
        let Some(dump) = decode_cold_sidecar_dump(sidecars, table, json, t) else {
            return Ok(false);
        };
        let columns = declared_columns(tx, dump.schema_name, dump.relation_name).await?;
        if columns.is_empty() || !cold_object_columns_match(&dump.object, &columns) {
            return Ok(false);
        }
        let sql = format!(
            "SELECT jsonb_populate_record(NULL::{table}, $1::jsonb)",
            table = dump.ident.as_str()
        );
        if !probe_cold_sidecar_shape(tx, sql, json).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// One sidecar dump resolved against the frozen registry and decoded.
struct ColdSidecarDump<'a> {
    ident: PgIdent<'a>,
    schema_name: &'a str,
    relation_name: &'a str,
    object: serde_json::Map<String, serde_json::Value>,
}

/// Resolve one dump's table declaration and decode its payload, refusing
/// everything the registry does not describe: a table with no registered
/// memory key, an identifier the splice policy would not accept, a payload
/// that is not a JSON object, a declared key naming some other admission, or
/// an owner-bearing audit payload.
fn decode_cold_sidecar_dump<'a>(
    sidecars: &PgSidecarRegistryFrozen,
    table: &'a str,
    json: &str,
    t: Uuid,
) -> Option<ColdSidecarDump<'a>> {
    let key_column = sidecars.memory_key_column(table)?;
    let ident = PgIdent::table(table).ok()?;
    let (schema_name, relation_name) = table.split_once('.')?;
    let value = serde_json::from_str::<serde_json::Value>(json).ok()?;
    let serde_json::Value::Object(object) = value else {
        return None;
    };
    let key_value = object.get(key_column).and_then(serde_json::Value::as_str)?;
    if key_value.parse::<Uuid>().ok() != Some(t) || object.contains_key("owner_id") {
        return None;
    }
    Some(ColdSidecarDump {
        ident,
        schema_name,
        relation_name,
        object,
    })
}

/// The `column_name`, `is_nullable`, `is_generated` triples PostgreSQL
/// reports for one relation. Both cold validators read the live catalog
/// rather than trusting the external record's own column set.
async fn declared_columns(
    tx: &mut Transaction<'_, Postgres>,
    schema_name: &str,
    relation_name: &str,
) -> Result<Vec<(String, String, String)>, StorageError> {
    sqlx::query_as(
        "SELECT column_name, is_nullable, is_generated
           FROM information_schema.columns
          WHERE table_schema = $1 AND table_name = $2",
    )
    .bind(schema_name)
    .bind(relation_name)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)
}

/// Whether one decoded cold object names only insertable columns and carries
/// a non-null value for every required one. A generated column is neither
/// insertable nor required, so it is excluded on both sides.
fn cold_object_columns_match(
    object: &serde_json::Map<String, serde_json::Value>,
    columns: &[(String, String, String)],
) -> bool {
    let insertable = columns
        .iter()
        .filter(|(_, _, generated)| generated == "NEVER")
        .map(|(name, _, _)| name.as_str())
        .collect::<BTreeSet<_>>();
    if object
        .keys()
        .any(|name| !insertable.contains(name.as_str()))
    {
        return false;
    }
    !columns.iter().any(|(name, nullable, generated)| {
        generated == "NEVER"
            && nullable == "NO"
            && object.get(name).is_none_or(serde_json::Value::is_null)
    })
}

/// Let PostgreSQL validate one sidecar dump's concrete column types by
/// populating a record from it, without inserting a row.
///
/// PostgreSQL aborts the surrounding transaction after a deterministic cast
/// error. Probing behind a savepoint lets a malformed external object become
/// a typed preflight result and the remaining set still be rolled back
/// cleanly, rather than surfacing "current transaction is aborted" from an
/// unrelated statement.
async fn probe_cold_sidecar_shape(
    tx: &mut Transaction<'_, Postgres>,
    sql: String,
    json: &str,
) -> Result<bool, StorageError> {
    sqlx::query("SAVEPOINT cold_sidecar_shape")
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    // SQL-POLICY: PgIdent
    match sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(json)
        .fetch_optional(tx.as_mut())
        .await
    {
        Ok(Some(_)) => {
            sqlx::query("RELEASE SAVEPOINT cold_sidecar_shape")
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
            Ok(true)
        }
        Ok(None) => {
            discard_cold_sidecar_shape(tx).await?;
            Ok(false)
        }
        Err(sqlx::Error::Database(db))
            if db.code().as_deref().is_some_and(is_cold_data_shape_code) =>
        {
            discard_cold_sidecar_shape(tx).await?;
            Ok(false)
        }
        Err(error) => {
            discard_cold_sidecar_shape(tx).await?;
            Err(map_err(error))
        }
    }
}

/// Undo the sidecar shape savepoint after a probe that did not pass.
async fn discard_cold_sidecar_shape(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), StorageError> {
    sqlx::query("ROLLBACK TO SAVEPOINT cold_sidecar_shape")
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query("RELEASE SAVEPOINT cold_sidecar_shape")
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(())
}

/// Validate the exact rows in every contract-declared cascaded detail dump.
/// The table/key declaration comes from the frozen schema contract; the
/// external record may supply only the row values. PostgreSQL's recordset
/// probe checks the concrete column types and constraints without allowing a
/// malformed object to abort the caller's transaction.
async fn validate_cold_details(
    tx: &mut Transaction<'_, Postgres>,
    surfaces: &OwnerSurfaces,
    rec: &ColdRecord,
    t: Uuid,
) -> Result<bool, StorageError> {
    let expected = surfaces.cascaded_details_for_schema(&rec.schema_id);
    if !cold_detail_stamp_matches(rec, surfaces) {
        return Ok(false);
    }
    for (detail, (table_name, rows)) in expected.iter().zip(&rec.detail_dumps) {
        if detail.table != table_name {
            return Ok(false);
        }
        let table = PgIdent::table(detail.table)?;
        let key_column = PgIdent::column(detail.key_column)?;
        let Some((schema_name, relation_name)) = detail.table.split_once('.') else {
            return Ok(false);
        };
        let columns = declared_columns(tx, schema_name, relation_name).await?;
        if columns.is_empty()
            || !columns
                .iter()
                .any(|(name, _, generated)| name == detail.key_column && generated == "NEVER")
        {
            return Ok(false);
        }
        let Some(values) = decode_cold_detail_rows(rows, &columns, detail.key_column, t) else {
            return Ok(false);
        };
        if values.is_empty() {
            continue;
        }
        let json = serde_json::Value::Array(values).to_string();
        let sql = format!(
            "SELECT 1 FROM jsonb_populate_recordset(NULL::{table}, $1::jsonb) AS p
              WHERE p.{key} IS NULL
              LIMIT 1",
            table = table.as_str(),
            key = key_column.as_str(),
        );
        if !probe_cold_detail_shape(tx, sql, &json).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Decode the rows of one cascaded detail dump, holding each to the live
/// column shape, to the declared key naming this exact `t`, and to the
/// no-owner rule. `None` refuses the whole dump.
fn decode_cold_detail_rows(
    rows: &[String],
    columns: &[(String, String, String)],
    key_column: &str,
    t: Uuid,
) -> Option<Vec<serde_json::Value>> {
    let mut values = Vec::with_capacity(rows.len());
    for row in rows {
        let value = serde_json::from_str::<serde_json::Value>(row).ok()?;
        let object = value.as_object()?;
        let key_value = object.get(key_column).and_then(serde_json::Value::as_str)?;
        if key_value.parse::<Uuid>().ok() != Some(t) || object.contains_key("owner_id") {
            return None;
        }
        if !cold_object_columns_match(object, columns) {
            return None;
        }
        values.push(value);
    }
    Some(values)
}

/// Let PostgreSQL evaluate one detail dump's rows against the live relation
/// without inserting them. Same savepoint fence as the sidecar probe: a
/// malformed external row becomes a typed preflight result instead of
/// aborting the caller's transaction.
async fn probe_cold_detail_shape(
    tx: &mut Transaction<'_, Postgres>,
    sql: String,
    json: &str,
) -> Result<bool, StorageError> {
    sqlx::query("SAVEPOINT cold_detail_shape")
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    // SQL-POLICY: PgIdent
    match sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(json)
        .fetch_optional(tx.as_mut())
        .await
    {
        Ok(_) => {
            sqlx::query("RELEASE SAVEPOINT cold_detail_shape")
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
            Ok(true)
        }
        Err(sqlx::Error::Database(db))
            if db.code().as_deref().is_some_and(is_cold_data_shape_code) =>
        {
            discard_cold_detail_shape(tx).await?;
            Ok(false)
        }
        Err(error) => {
            discard_cold_detail_shape(tx).await?;
            Err(map_err(error))
        }
    }
}

/// Undo the detail shape savepoint after a probe that did not pass.
async fn discard_cold_detail_shape(tx: &mut Transaction<'_, Postgres>) -> Result<(), StorageError> {
    sqlx::query("ROLLBACK TO SAVEPOINT cold_detail_shape")
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query("RELEASE SAVEPOINT cold_detail_shape")
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(())
}

async fn restore_registered_sidecars(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    dumps: &[(String, String)],
) -> Result<Option<ColdRejection>, StorageError> {
    let allowed = sidecars.memory_sidecar_tables();
    for (table, json) in dumps {
        if !allowed.contains(&table.as_str())
            || !sidecars.is_hydratable_memory_sidecar_table(table)
            || is_owner_pinned(sidecars, table)
        {
            return Ok(Some(ColdRejection::UnsupportedSidecar));
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
        let result = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(json)
            .execute(tx.as_mut())
            .await;
        if let Err(error) = result {
            if let Some(rejection) = cold_write_rejection(&error) {
                return Ok(Some(rejection));
            }
            return Err(map_err(error));
        }
    }
    Ok(None)
}

async fn restore_cascaded_details(
    tx: &mut Transaction<'_, Postgres>,
    surfaces: &OwnerSurfaces,
    rec: &ColdRecord,
) -> Result<Option<ColdRejection>, StorageError> {
    let expected = surfaces.cascaded_details_for_schema(&rec.schema_id);
    if !cold_detail_stamp_matches(rec, surfaces) {
        return Ok(Some(ColdRejection::UnsupportedObject));
    }
    for (detail, (_, rows)) in expected.iter().zip(&rec.detail_dumps) {
        if rows.is_empty() {
            continue;
        }
        let table = PgIdent::table(detail.table)?;
        let columns = insertable_columns(tx, detail.table).await?;
        if columns.is_empty() {
            return Err(StorageError::Internal(format!(
                "no insertable columns for {}",
                detail.table
            )));
        }
        let col_list = columns.join(", ");
        let Ok(rows) = rows
            .iter()
            .map(|row| serde_json::from_str(row))
            .collect::<Result<Vec<serde_json::Value>, _>>()
        else {
            return Ok(Some(ColdRejection::InvalidObject));
        };
        let values = serde_json::Value::Array(rows).to_string();
        let sql = format!(
            "INSERT INTO {tbl} ({cols})
             SELECT {cols} FROM jsonb_populate_recordset(NULL::{tbl}, $1::jsonb) AS p",
            tbl = table.as_str(),
            cols = col_list,
        );
        // SQL-POLICY: PgIdent
        let result = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(values)
            .execute(tx.as_mut())
            .await;
        if let Err(error) = result {
            if let Some(rejection) = cold_write_rejection(&error) {
                return Ok(Some(rejection));
            }
            return Err(map_err(error));
        }
    }
    Ok(None)
}

/// `owner_id` is the CALLER's, never `rec.row.owner_id`.
///
/// The cold record's embedded owner is the owner at dump time. A transfer
/// rewrites `cooled.owner_id` and leaves the bytes alone, so the two diverge
/// permanently for any series that changed hands while cold. Filing the
/// embedding job under the dumped owner would hand the giver a job for a
/// memory it no longer has — and, if the giver was since erased, the `owners`
/// lookup below is a `fetch_one` against a row that is gone, which fails the
/// whole hydrate.
///
/// `non_embeddable_schemas` is the registry's answer, a parameter because
/// storage does not hold the registry. It is consulted instead of trusting
/// `embed_models` alone: that list records the models this `t` HAD vectors
/// under, so a row under a `Never` schema that carries one — from any write
/// that did not ask the registry — would have the job re-filed here on every
/// hydrate, for a drain that can only drop it.
async fn enqueue_embed_jobs(
    tx: &mut Transaction<'_, Postgres>,
    rec: &ColdRecord,
    owner_id: Uuid,
    non_embeddable_schemas: &[String],
) -> Result<(), StorageError> {
    if rec.embed_models.is_empty() || non_embeddable_schemas.contains(&rec.schema_id) {
        return Ok(());
    }
    let kind = match rec.row.kind.as_str() {
        "abstraction" => proxima_core::EntityKind::Abstraction,
        "perspective" => proxima_core::EntityKind::Perspective,
        _ => proxima_core::EntityKind::Fact,
    };
    let owner_kind: String =
        sqlx::query_scalar("SELECT kind::text FROM proxima_core.owners WHERE owner_id = $1")
            .bind(owner_id)
            .fetch_one(tx.as_mut())
            .await
            .map_err(map_err)?;
    let owner_kind = match owner_kind.as_str() {
        "group" => proxima_core::OwnerRefKind::Group,
        _ => proxima_core::OwnerRefKind::Personal,
    };
    for model_id in &rec.embed_models {
        crate::verbs::fact_embeddings::enqueue_embedding_job_in_tx(
            tx,
            owner_kind,
            Some(owner_id),
            kind,
            rec.row.t,
            model_id,
        )
        .await?;
    }
    Ok(())
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

/// `owner_id` comes from the `cooled` ROW, never from `rec`. The dump is a
/// snapshot of the memory as it was when it cooled; an owner transfer
/// afterwards updates the row and deliberately does not rewrite the bytes.
/// The caller already holds `rec.row.handle`'s advisory lock, so an absent
/// head cannot be recreated concurrently with another series transition.
async fn ensure_memory_head(
    tx: &mut Transaction<'_, Postgres>,
    rec: &ColdRecord,
    owner_id: Uuid,
) -> Result<(), StorageError> {
    let existing: Option<(String, String, Uuid)> = sqlx::query_as(
        "SELECT kind::text, schema_id, owner_id
           FROM proxima_core.memory_head
          WHERE handle = $1
          FOR UPDATE",
    )
    .bind(rec.row.handle)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    if let Some((kind, schema_id, existing_owner)) = existing {
        if kind != rec.row.kind || schema_id != rec.schema_id || existing_owner != owner_id {
            return Err(StorageError::ConstraintViolation(
                "memory_head kind/schema/owner mismatch".into(),
            ));
        }
        // A head can already point at a newer hot version. Hydrating this
        // older cooled version must not rewind that series head.
        return Ok(());
    }
    let inserted = sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, $2::proxima_core.memory_kind, $3, $4, $5)
         ON CONFLICT (handle) DO NOTHING
         RETURNING handle",
    )
    .bind(rec.row.handle)
    .bind(&rec.row.kind)
    .bind(&rec.schema_id)
    .bind(owner_id)
    .bind(rec.row.t)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    if inserted.is_none() {
        // `ON CONFLICT DO NOTHING` can lose to a concurrent empty-head
        // recreation. Re-read the committed winner under its row lock before
        // classifying the result; an absent row means another lifecycle
        // transition removed the head and this hydrate must be retried.
        let winner: Option<(String, String, Uuid)> = sqlx::query_as(
            "SELECT kind::text, schema_id, owner_id
               FROM proxima_core.memory_head
              WHERE handle = $1
              FOR UPDATE",
        )
        .bind(rec.row.handle)
        .fetch_optional(tx.as_mut())
        .await
        .map_err(map_err)?;
        match winner {
            Some((kind, schema_id, existing_owner))
                if kind == rec.row.kind
                    && schema_id == rec.schema_id
                    && existing_owner == owner_id =>
            {
                Ok(())
            }
            Some(_) => Err(StorageError::ConstraintViolation(
                "memory_head kind/schema/owner mismatch".into(),
            )),
            None => Err(StorageError::Retryable(
                "memory_head disappeared during concurrent hydration".into(),
            )),
        }
    } else {
        Ok(())
    }
}

async fn probe_memory_handle_for_owner(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
    owner_id: Uuid,
) -> Result<Option<Uuid>, StorageError> {
    sqlx::query_scalar(
        "SELECT handle FROM proxima_core.memory
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

/// One-shot Engine path: `put` with no open transaction.
pub async fn forget_memory_oneshot(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    cold: &dyn ColdObjectStore,
    object_key: &str,
    t: Uuid,
    expected_owner_id: Uuid,
) -> Result<(), StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    let handle = probe_memory_handle_for_owner(&mut tx, t, expected_owner_id)
        .await?
        .ok_or(StorageError::NotFound)?;
    lock_forget_footprint_tx(&mut tx, t, handle).await?;
    let rec = snapshot_hot(tx.as_mut(), sidecars, surfaces, t).await?;
    if rec.row.owner_id != expected_owner_id {
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
/// Every variant is produced at a named site and maps onto exactly one
/// [`MemoryHydrationStatus`]. Nothing here is recovered from an error
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
    /// The record's sidecar stamp names a set this registry cannot restore.
    UnsupportedSidecar,
    /// The bytes are present and witnessed but cannot be admitted: a digest
    /// or identity mismatch, an undecodable record, or a payload PostgreSQL
    /// refuses.
    InvalidObject,
}

impl ColdRejection {
    const fn status(self) -> MemoryHydrationStatus {
        match self {
            Self::MissingObject => MemoryHydrationStatus::MissingColdObject,
            Self::UnsupportedObject => MemoryHydrationStatus::UnsupportedColdObject,
            Self::UnsupportedSidecar => MemoryHydrationStatus::UnsupportedColdSidecar,
            Self::InvalidObject => MemoryHydrationStatus::InvalidColdObject,
        }
    }
}

/// A cold record read once, verified once, and ready to write.
///
/// The bytes are fetched and digest-checked before the lifecycle locks, and
/// are not fetched again. The digest binds the bytes to the `cooled` row, so
/// once the locked re-read proves that row unchanged, the held record *is*
/// the record the row attests to — whatever now sits at the object key. A
/// second GET would re-read an object no lock covers and could only turn a
/// post-verification overwrite into a spurious failure.
#[derive(Debug)]
struct PreparedHydration {
    cooled: CooledRow,
    rec: ColdRecord,
    origins: Vec<Uuid>,
    refs: Vec<Uuid>,
    goal_refs: Vec<Uuid>,
}

#[derive(Debug)]
enum HydrationPlan {
    Hot,
    NotFound,
    Prepared(Box<PreparedHydration>),
    Rejected(ColdRejection),
}

/// Classify one owner-scoped id and, when it is a restorable cooled
/// admission, do every read and every check the restore depends on.
///
/// This runs before any lifecycle lock. It is the only place that touches
/// the object store, decodes the record, or validates a payload shape.
async fn plan_hydration(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    cold: &dyn ColdObjectStore,
    t: Uuid,
    owner_id: Uuid,
) -> Result<HydrationPlan, StorageError> {
    if owned_hot_exists(tx, t, owner_id).await? {
        return Ok(HydrationPlan::Hot);
    }
    let Some(cooled) = owned_cooled_row(tx, t, owner_id).await? else {
        return Ok(HydrationPlan::NotFound);
    };
    // A pre-witness locator has neither a digest nor the split pin arrays,
    // and its object predates the stamped cold format. Nothing can prove the
    // bytes belong to this admission, so it fails closed rather than being
    // admitted on the object's own say-so.
    let (Some(origins), Some(refs), Some(goal_refs), Some(expected_digest)) = (
        cooled.origins.clone(),
        cooled.refs.clone(),
        cooled.goal_refs.clone(),
        cooled.cold_digest.clone(),
    ) else {
        return Ok(HydrationPlan::Rejected(ColdRejection::UnsupportedObject));
    };

    let bytes = match cold.get(&cooled.object_key).await {
        Ok(bytes) => bytes,
        Err(StorageError::NotFound) => {
            return Ok(HydrationPlan::Rejected(ColdRejection::MissingObject));
        }
        Err(error) => return Err(error),
    };
    if expected_digest.len() != 32 || cold_digest(&bytes) != expected_digest {
        return Ok(HydrationPlan::Rejected(ColdRejection::InvalidObject));
    }
    // The declared version decides supported-vs-invalid before the decoder
    // has an opinion. A record from a newer binary is unsupported, not
    // damaged, and the two are different answers for an operator.
    if bytes
        .first()
        .is_none_or(|version| !(1..=COLD_FORMAT_VERSION).contains(version))
    {
        return Ok(HydrationPlan::Rejected(ColdRejection::UnsupportedObject));
    }
    let Ok(rec) = decode_record(&bytes) else {
        return Ok(HydrationPlan::Rejected(ColdRejection::InvalidObject));
    };
    // The stamp arrived with format 6 and the exact detail declaration with
    // format 7. An older object cannot prove which sidecars its admission
    // declared, so no amount of validation makes it restorable.
    if rec.format_version < 6 {
        return Ok(HydrationPlan::Rejected(ColdRejection::UnsupportedObject));
    }
    if rec.row.t != cooled.t
        || rec.row.handle != cooled.handle
        || rec.row.kind != cooled.kind
        || rec.row.source_id != cooled.source_id
        || rec.row.ingest_key != cooled.ingest_key
        || rec.row.origins != origins
        || rec.row.refs != refs
        || rec.row.goal_refs != goal_refs
    {
        return Ok(HydrationPlan::Rejected(ColdRejection::InvalidObject));
    }
    if !cold_sidecar_stamp_matches(&rec, sidecars) {
        return Ok(HydrationPlan::Rejected(ColdRejection::UnsupportedSidecar));
    }
    if !validate_cold_sidecars(tx, sidecars, &rec.sidecar_dumps, t).await? {
        return Ok(HydrationPlan::Rejected(ColdRejection::InvalidObject));
    }
    if !cold_detail_stamp_matches(&rec, surfaces) {
        return Ok(HydrationPlan::Rejected(ColdRejection::UnsupportedObject));
    }
    if !validate_cold_details(tx, surfaces, &rec, t).await? {
        return Ok(HydrationPlan::Rejected(ColdRejection::InvalidObject));
    }
    Ok(HydrationPlan::Prepared(Box::new(PreparedHydration {
        cooled,
        rec,
        origins,
        refs,
        goal_refs,
    })))
}

/// SQLSTATE families a cold record's own content can raise once PostgreSQL
/// evaluates it for real. Class 22 is a data exception and class 23 an
/// integrity-constraint violation, which together cover a malformed payload,
/// a CHECK or UNIQUE the restored rows break, and the pin-admission triggers
/// refusing a target the record names.
///
/// This is the whole boundary. Every other failure inside the write half is a
/// storage fault or a lifecycle conflict, and stays an error rather than
/// being reported to the caller as a damaged object.
fn is_cold_data_shape_code(code: &str) -> bool {
    code.starts_with("22") || code.starts_with("23")
}

/// Map a write-half failure to a rejection when, and only when, the cold
/// record's own content is what PostgreSQL refused.
fn cold_write_rejection(error: &sqlx::Error) -> Option<ColdRejection> {
    match error {
        sqlx::Error::Database(db) if db.code().as_deref().is_some_and(is_cold_data_shape_code) => {
            Some(ColdRejection::InvalidObject)
        }
        _ => None,
    }
}

/// Write one prepared record back into the hot tables.
///
/// The caller holds the complete handle/lifecycle lock union and has proven
/// the locked `cooled` row equal to the prepared snapshot. `locked` is that
/// row: owner, blob and content are read from it rather than from the
/// snapshot because a transfer may remap exactly those three columns and the
/// append-only trigger permits it.
///
/// The outer `Result` is a storage fault. The inner one is the record's own
/// content being refused, which is a typed per-item outcome rather than a
/// failure of the command.
async fn apply_hydration(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    prepared: &PreparedHydration,
    locked: &CooledRow,
    non_embeddable_schemas: &[String],
) -> Result<Result<u32, ColdRejection>, StorageError> {
    let PreparedHydration {
        rec,
        origins,
        refs,
        goal_refs,
        ..
    } = prepared;
    let t = rec.row.t;

    // Witnesses are lifecycle state, not cold-object metadata. Count them
    // only under the complete lock set, so an erase cannot remove one between
    // the reported count and this commit.
    let witness_targets = origins.iter().chain(refs).copied().collect::<Vec<_>>();
    let Ok(preserved_witnesses) = cold_witness_count(tx, &witness_targets, goal_refs).await? else {
        return Ok(Err(ColdRejection::InvalidObject));
    };

    // `ensure_memory_head` must stay after the lifecycle lock: a bulk erase
    // takes the same set before any head or row lock.
    match ensure_memory_head(tx, rec, locked.owner_id).await {
        Ok(()) => {}
        Err(StorageError::ConstraintViolation(_)) => {
            // The live series disagrees with the record about kind, schema or
            // owner. The record cannot be restored into it.
            return Ok(Err(ColdRejection::InvalidObject));
        }
        Err(error) => return Err(error),
    }
    if let Some(rejection) = insert_hydrated_memory_row(tx, prepared, locked).await? {
        return Ok(Err(rejection));
    }
    if let Some(rejection) = restore_registered_sidecars(tx, sidecars, &rec.sidecar_dumps).await? {
        return Ok(Err(rejection));
    }
    if let Some(rejection) = restore_cascaded_details(tx, surfaces, rec).await? {
        return Ok(Err(rejection));
    }
    rebuild_hydrated_projections(tx, sidecars, rec).await?;
    // `owner_id` from the locked locator, not `rec.row.owner_id`: same reason
    // as the memory INSERT in `insert_hydrated_memory_row`. The sketch is
    // READ owner-scoped, so a sketch written under the dumped owner would
    // let the owner that gave this series away go on recalling it.
    let hydrate_line = hydrate_sketch_line(rec);
    super::sketch::upsert_sketch(tx, locked.owner_id, t, &rec.row.kind, &hydrate_line).await?;
    enqueue_embed_jobs(tx, rec, locked.owner_id, non_embeddable_schemas).await?;

    sqlx::query("DELETE FROM proxima_core.cooled WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;

    sync_memory_head(tx, rec.row.handle).await?;

    // Likewise owner-scoped: the announce feed a client tails is its own.
    // Announcing a hydrate to the pre-transfer owner tells the wrong party
    // that a memory it does not have came back.
    sqlx::query(
        "INSERT INTO proxima_core.announce (owner_id, op, entity, handle, t)
         VALUES ($1, 'append', 'memory', $2, $3)",
    )
    .bind(locked.owner_id)
    .bind(rec.row.handle)
    .bind(t)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(Ok(preserved_witnesses))
}

/// Insert the restored Memory row.
///
/// Owner, blob and content come from the locked locator rather than from the
/// record: a transfer may remap exactly those three columns. `Some` is
/// PostgreSQL refusing the record's own content, which is a typed per-item
/// outcome; every other failure is a storage fault.
async fn insert_hydrated_memory_row(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedHydration,
    locked: &CooledRow,
) -> Result<Option<ColdRejection>, StorageError> {
    let PreparedHydration {
        rec,
        origins,
        refs,
        goal_refs,
        ..
    } = prepared;
    let result = sqlx::query(
        "INSERT INTO proxima_core.memory
            (handle, t, kind, owner_id, schema_id, source_id, ingest_key, blob_id,
             content_id, origins, refs, goal_refs, sidecar_tables)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                 $13)",
    )
    .bind(rec.row.handle)
    .bind(rec.row.t)
    .bind(&rec.row.kind)
    .bind(locked.owner_id)
    .bind(&rec.schema_id)
    .bind(rec.row.source_id.as_deref())
    .bind(rec.row.ingest_key.as_deref())
    .bind(locked.blob_id)
    .bind(locked.content_id)
    .bind(origins)
    .bind(refs)
    .bind(goal_refs)
    .bind(rec.row.sidecar_tables.clone())
    .execute(tx.as_mut())
    .await;
    if let Err(error) = result {
        if let Some(rejection) = cold_write_rejection(&error) {
            return Ok(Some(rejection));
        }
        return Err(map_err(error));
    }
    Ok(None)
}

/// Rebuild the search projection of every restored sidecar.
///
/// The cold record carries no `lexical_language`: the stamp lives on the
/// projection row, and the projection is rebuilt here rather than restored.
/// A hydrated row therefore re-derives its vector at the DEPLOYMENT default
/// rather than at the language the original write asked for. Carrying the
/// language through a forget/hydrate cycle means putting the stamp in the
/// cold record, which is a cold-format change.
async fn rebuild_hydrated_projections(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    rec: &ColdRecord,
) -> Result<(), StorageError> {
    for (table, _) in &rec.sidecar_dumps {
        // `rec.schema_id`, NOT `rec.row.schema_id`. `HotRow` carries a
        // `schema_id` field that `decode_record` leaves EMPTY — the cold
        // format stores the schema id once, on the record — and
        // `insert_hydrated_memory_row` binds `rec.schema_id` for exactly
        // that reason. This has to be the same value or the generator's
        // `memory.schema_id = $3` guard matches nothing and the hydrated
        // row comes back unsearchable.
        sidecars
            .rebuild_projection_for_table(
                tx,
                proxima_core::MemoryId::new(rec.row.t),
                table,
                &rec.schema_id,
                None,
            )
            .await?;
    }
    Ok(())
}

/// The one-liner a hydrated row is recalled by: the record's own persisted
/// sketch, else the first human-readable field of its sidecar dumps, else
/// the memory kind.
fn hydrate_sketch_line(rec: &ColdRecord) -> String {
    rec.sketch.clone().unwrap_or_else(|| {
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
    })
}

async fn owned_cooled_row_for_update(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
    owner_id: Uuid,
) -> Result<Option<CooledRow>, StorageError> {
    sqlx::query_as(
        "SELECT t, handle, owner_id, kind::text, object_key, blob_id, content_id,
                source_id, ingest_key, origins, refs, goal_refs, cold_digest
           FROM proxima_core.cooled
          WHERE t = $1 AND owner_id = $2
          FOR UPDATE",
    )
    .bind(t)
    .bind(owner_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)
}

async fn owned_cooled_row(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
    owner_id: Uuid,
) -> Result<Option<CooledRow>, StorageError> {
    sqlx::query_as(
        "SELECT t, handle, owner_id, kind::text, object_key, blob_id, content_id,
                source_id, ingest_key, origins, refs, goal_refs, cold_digest
           FROM proxima_core.cooled
          WHERE t = $1 AND owner_id = $2",
    )
    .bind(t)
    .bind(owner_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)
}

async fn owned_hot_exists(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
    owner_id: Uuid,
) -> Result<bool, StorageError> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM proxima_core.memory WHERE t = $1 AND owner_id = $2
         )",
    )
    .bind(t)
    .bind(owner_id)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)
}

/// Count the historical target witnesses that exact hydration will preserve,
/// and reject a witness whose closed kind contradicts the cold declaration.
/// Witness rows have no owner by design; the cooled source owner is already
/// established by the permit and the owner-scoped cooled probe.
async fn cold_witness_count(
    tx: &mut Transaction<'_, Postgres>,
    memory_targets: &[Uuid],
    goal_targets: &[Uuid],
) -> Result<Result<u32, ()>, StorageError> {
    let memory_targets = memory_targets.to_vec();
    let goal_targets = goal_targets.to_vec();
    let invalid: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.erased_pin_target e
              WHERE (e.t = ANY($1::uuid[]) AND e.kind = 'goal'::proxima_core.pin_target_kind)
                 OR (e.t = ANY($2::uuid[]) AND e.kind <> 'goal'::proxima_core.pin_target_kind)
         )",
    )
    .bind(&memory_targets)
    .bind(&goal_targets)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;
    if invalid {
        return Ok(Err(()));
    }
    let mut all = BTreeSet::new();
    all.extend(memory_targets);
    all.extend(goal_targets);
    let all = all.into_iter().collect::<Vec<_>>();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.erased_pin_target
          WHERE t = ANY($1::uuid[])",
    )
    .bind(&all)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;
    let count = u32::try_from(count)
        .map_err(|_| StorageError::Internal("hydration witness count overflow".into()))?;
    Ok(Ok(count))
}

/// Test-only single-shot hydration inside the caller's transaction.
///
/// Production hydration is always the bounded owner-authorized set below.
/// These tests drive one admission at a time and need to hold the
/// transaction open around it, so this runs the same plan/lock/apply
/// sequence for one id under the same owner fence rather than keeping a
/// second, laxer entry point alive in shipped code.
#[cfg(test)]
pub(crate) async fn hydrate_one_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    cold: &dyn ColdObjectStore,
    t: Uuid,
    owner_id: Uuid,
    non_embeddable_schemas: &[String],
) -> Result<Result<u32, ColdRejection>, StorageError> {
    let prepared = match plan_hydration(tx, sidecars, surfaces, cold, t, owner_id).await? {
        HydrationPlan::Prepared(prepared) => prepared,
        HydrationPlan::Rejected(rejection) => return Ok(Err(rejection)),
        HydrationPlan::Hot | HydrationPlan::NotFound => return Err(StorageError::NotFound),
    };
    lock_memory_handles_tx(tx, &[prepared.cooled.handle]).await?;
    let targets = std::iter::once(prepared.cooled.t)
        .chain(prepared.origins.iter().copied())
        .chain(prepared.refs.iter().copied())
        .chain(prepared.goal_refs.iter().copied())
        .collect::<Vec<_>>();
    lock_lifecycle_targets_tx(tx, &targets).await?;
    let locked = owned_cooled_row_for_update(tx, prepared.cooled.t, owner_id).await?;
    let Some(locked) = locked.filter(|locked| *locked == prepared.cooled) else {
        return Err(StorageError::Retryable(
            "cooled hydration footprint changed while locking".into(),
        ));
    };
    apply_hydration(
        tx,
        sidecars,
        surfaces,
        &prepared,
        &locked,
        non_embeddable_schemas,
    )
    .await
}

/// Owner-authorized, bounded, all-or-nothing hydration command.
///
/// One pass plans every id — the only object-store read and the only shape
/// validation each id gets. If any plan is a rejection the set commits
/// nothing. Otherwise the whole prepared footprint is locked in one union
/// pass, re-read under that lock, and written from the records already held.
pub(crate) async fn hydrate_memories_oneshot(
    pool: &PgPool,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    cold: &dyn ColdObjectStore,
    owner_id: Uuid,
    memory_ids: &[proxima_core::MemoryId],
    non_embeddable_schemas: &[String],
) -> Result<MemoryHydrationBatchOutcome, StorageError> {
    validate_hydration_request(memory_ids)?;

    let mut tx = pool.begin().await.map_err(map_err)?;
    let plans =
        match plan_hydration_set(&mut tx, sidecars, surfaces, cold, owner_id, memory_ids).await {
            Ok(plans) => plans,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
        };

    if plans
        .iter()
        .any(|plan| matches!(plan, HydrationPlan::Rejected(_)))
    {
        tx.rollback().await.map_err(map_err)?;
        return Ok(uncommitted_outcome(memory_ids, &plans, None));
    }

    let prepared = plans
        .iter()
        .filter_map(|plan| match plan {
            HydrationPlan::Prepared(prepared) => Some(prepared.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    lock_prepared_footprint(&mut tx, &prepared).await?;

    let Some(locked) = relock_prepared_rows(&mut tx, &prepared, owner_id).await? else {
        let _ = tx.rollback().await;
        return Err(StorageError::Retryable(
            "cooled hydration footprint changed while locking".into(),
        ));
    };

    let restored = match apply_prepared_hydrations(
        &mut tx,
        sidecars,
        surfaces,
        &prepared,
        &locked,
        non_embeddable_schemas,
    )
    .await
    {
        Ok(Ok(restored)) => restored,
        Ok(Err((failed, rejection))) => {
            tx.rollback().await.map_err(map_err)?;
            return Ok(uncommitted_outcome(
                memory_ids,
                &plans,
                Some((failed, rejection)),
            ));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    tx.commit().await.map_err(map_err)?;

    Ok(MemoryHydrationBatchOutcome {
        outcomes: hydrated_outcomes(memory_ids, &plans, restored),
        committed: true,
    })
}

/// Refuse a request the command cannot serve at all: more ids than the
/// bounded batch admits, or the same admission named twice.
fn validate_hydration_request(memory_ids: &[proxima_core::MemoryId]) -> Result<(), StorageError> {
    if memory_ids.len() > proxima_core::MAX_MEMORY_HYDRATION_BATCH {
        return Err(StorageError::ConstraintViolation(format!(
            "hydration request exceeds {} ids",
            proxima_core::MAX_MEMORY_HYDRATION_BATCH
        )));
    }
    let mut seen = BTreeSet::new();
    if memory_ids
        .iter()
        .any(|memory_id| !seen.insert(memory_id.into_inner()))
    {
        return Err(StorageError::ConstraintViolation(
            "hydration request contains duplicate memory ids".into(),
        ));
    }
    Ok(())
}

/// Plan every requested id, in request order. This is the one pass that
/// reads the object store and validates a shape.
async fn plan_hydration_set(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    cold: &dyn ColdObjectStore,
    owner_id: Uuid,
    memory_ids: &[proxima_core::MemoryId],
) -> Result<Vec<HydrationPlan>, StorageError> {
    let mut plans = Vec::with_capacity(memory_ids.len());
    for memory_id in memory_ids {
        plans.push(
            plan_hydration(
                tx,
                sidecars,
                surfaces,
                cold,
                memory_id.into_inner(),
                owner_id,
            )
            .await?,
        );
    }
    Ok(plans)
}

/// Take the whole prepared footprint in one union pass.
///
/// The transfer/forget lock law is transaction-wide: all series handles
/// precede all lifecycle targets. Acquiring one item's set per loop would
/// retain the first set while extending it for the next and lets two
/// reversed batches deadlock. One union pass is both sufficient and
/// bounded.
async fn lock_prepared_footprint(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &[&PreparedHydration],
) -> Result<(), StorageError> {
    let handles = prepared
        .iter()
        .map(|prepared| prepared.cooled.handle)
        .collect::<Vec<_>>();
    lock_memory_handles_tx(tx, &handles).await?;
    let targets = prepared
        .iter()
        .flat_map(|prepared| {
            std::iter::once(prepared.cooled.t)
                .chain(prepared.origins.iter().copied())
                .chain(prepared.refs.iter().copied())
                .chain(prepared.goal_refs.iter().copied())
        })
        .collect::<Vec<_>>();
    lock_lifecycle_targets_tx(tx, &targets).await
}

/// Re-read every prepared locator after the complete lock union. Equality
/// over the whole row is deliberate: it covers the remappable owner, blob
/// and content columns as well as the witness, so a transfer that landed
/// during planning restarts the command rather than restoring against a
/// snapshot that no longer describes the row. `None` is that restart.
async fn relock_prepared_rows(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &[&PreparedHydration],
    owner_id: Uuid,
) -> Result<Option<Vec<CooledRow>>, StorageError> {
    let mut locked = Vec::with_capacity(prepared.len());
    for prepared in prepared {
        let current = owned_cooled_row_for_update(tx, prepared.cooled.t, owner_id).await?;
        if current.as_ref() != Some(&prepared.cooled) {
            return Ok(None);
        }
        locked.push(current.expect("the comparison above proved the row present"));
    }
    Ok(Some(locked))
}

/// Write every prepared record under the lock, in request order. The inner
/// `Err` names the first admission whose own content PostgreSQL refused;
/// nothing after it is attempted and the caller decides what an uncommitted
/// set reports.
async fn apply_prepared_hydrations(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    prepared: &[&PreparedHydration],
    locked: &[CooledRow],
    non_embeddable_schemas: &[String],
) -> Result<Result<Vec<u32>, (Uuid, ColdRejection)>, StorageError> {
    let mut restored = Vec::with_capacity(prepared.len());
    for (prepared, locked) in prepared.iter().zip(locked) {
        match apply_hydration(
            tx,
            sidecars,
            surfaces,
            prepared,
            locked,
            non_embeddable_schemas,
        )
        .await?
        {
            Ok(count) => restored.push(count),
            Err(rejection) => return Ok(Err((prepared.cooled.t, rejection))),
        }
    }
    Ok(Ok(restored))
}

/// Report one outcome per requested id, in request order. The restore counts
/// belong to the prepared items and arrive in that same order.
fn hydrated_outcomes(
    memory_ids: &[proxima_core::MemoryId],
    plans: &[HydrationPlan],
    restored: Vec<u32>,
) -> Vec<MemoryHydrationOutcome> {
    let mut restored = restored.into_iter();
    memory_ids
        .iter()
        .zip(plans)
        .map(|(memory_id, plan)| match plan {
            HydrationPlan::Hot => {
                MemoryHydrationOutcome::simple(*memory_id, MemoryHydrationStatus::AlreadyHot)
            }
            HydrationPlan::NotFound => {
                MemoryHydrationOutcome::simple(*memory_id, MemoryHydrationStatus::NotFound)
            }
            HydrationPlan::Prepared(_) => MemoryHydrationOutcome::hydrated(
                *memory_id,
                restored
                    .next()
                    .expect("one restore count per prepared item, in request order"),
            ),
            HydrationPlan::Rejected(_) => unreachable!("a rejected plan never reaches the commit"),
        })
        .collect()
}

/// Results for a set that wrote nothing.
///
/// A prepared item is reported as `NotAttempted` unless it is `failed`, the
/// one id whose own content was refused under the lock.
fn uncommitted_outcome(
    memory_ids: &[proxima_core::MemoryId],
    plans: &[HydrationPlan],
    failed: Option<(Uuid, ColdRejection)>,
) -> MemoryHydrationBatchOutcome {
    let outcomes = memory_ids
        .iter()
        .zip(plans)
        .map(|(memory_id, plan)| {
            let status = match plan {
                HydrationPlan::Hot => MemoryHydrationStatus::AlreadyHot,
                HydrationPlan::NotFound => MemoryHydrationStatus::NotFound,
                HydrationPlan::Rejected(rejection) => rejection.status(),
                HydrationPlan::Prepared(_) => match failed {
                    Some((failed, rejection)) if failed == memory_id.into_inner() => {
                        rejection.status()
                    }
                    _ => MemoryHydrationStatus::NotAttempted,
                },
            };
            MemoryHydrationOutcome::simple(*memory_id, status)
        })
        .collect();
    MemoryHydrationBatchOutcome {
        outcomes,
        committed: false,
    }
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
    surfaces: &OwnerSurfaces,
    owner: &Owner,
    t: Uuid,
) -> Result<ColdPurgePlan, StorageError> {
    // Probe the series identity first. The handle lock is the first
    // serialization boundary; a missing target is not a successful no-op.
    let probed_handle = probe_memory_handle_for_owner(tx, t, owner.stored_owner_id())
        .await?
        .ok_or(StorageError::NotFound)?;
    lock_memory_handles_tx(tx, &[probed_handle]).await?;
    lock_lifecycle_targets_tx(tx, &[t]).await?;
    let handle = probe_memory_handle_for_owner(tx, t, owner.stored_owner_id())
        .await?
        .ok_or(StorageError::NotFound)?;
    if handle != probed_handle {
        return Err(StorageError::Retryable(
            "memory target changed series while erasing".into(),
        ));
    }
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
    delete_memory_dependents(tx, sidecars, surfaces, &stamped, t).await?;
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1 AND owner_id = $2")
        .bind(t)
        .bind(owner.stored_owner_id())
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    if let Some(id) = content_id {
        super::content::gc_unreferenced_content(tx, id).await?;
    }
    sync_memory_head(tx, handle).await?;
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
    // t-shaped handle matches no series.
    .bind(handle)
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
/// What it does NOT reach: unowned `t`-references. `goal.close_fact_t`,
/// `goal.assignment_t`, `goal.write_act_t`, `goal.dependency_t[]`,
/// `goal.evidence_t[]`, `wake_config.trigger_t` and
/// `wake_config.hard_memory_t[]` name memories without a foreign key, so an
/// erase here can leave them pointing at nothing. That is deliberate:
/// [`erase_memory`] leaves them too, and the owner-erase source-scope arm
/// leaves `goal` and `wake_config` untouched — they go only in the owner
/// arm, where the whole row goes rather than one column of it. Nulling them
/// here would make the set-erase do something the single-memory erase does
/// not. A goal whose
/// evidence was erased is a goal with a dangling pointer either way; the
/// reader resolves it to nothing, which is what "erased" should look like.
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
    surfaces: &OwnerSurfaces,
    owner: &Owner,
    ts: &[Uuid],
) -> Result<(u64, ColdPurgePlan), StorageError> {
    if ts.is_empty() {
        return Ok((0, ColdPurgePlan::default()));
    }
    let owner_id = owner.stored_owner_id();
    // Capture handles and membership in one statement before waiting. The
    // membership is the generation witness: handles are reusable after a
    // complete erase, so a non-empty post-lock series disjoint from this
    // snapshot is a replacement, not a late version of the requested series.
    let before = snapshot_series_for_erase_tx(tx, owner_id, ts).await?;
    erase_memory_series_after_snapshot(tx, sidecars, surfaces, owner, before).await
}

async fn erase_memory_series_after_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    owner: &Owner,
    before: MemorySeriesSnapshot,
) -> Result<(u64, ColdPurgePlan), StorageError> {
    let owner_id = owner.stored_owner_id();
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
        let plan = erase_memory(tx, sidecars, surfaces, owner, t).await?;
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
    owner: &Owner,
    ts: &[Uuid],
) -> Result<Vec<Uuid>, StorageError> {
    if ts.is_empty() {
        return Ok(Vec::new());
    }
    Ok(
        snapshot_series_for_erase_tx(tx, owner.stored_owner_id(), ts)
            .await?
            .versions(),
    )
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
    SELECT handle FROM proxima_core.memory
     WHERE t = ANY($1::uuid[]) AND owner_id = $2
    UNION
    SELECT handle FROM proxima_core.cooled
     WHERE t = ANY($1::uuid[]) AND owner_id = $2
), versions AS (
    SELECT m.handle, m.t FROM proxima_core.memory m
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
SELECT m.handle, m.t FROM proxima_core.memory m
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
    owner: &Owner,
    ts: &[Uuid],
) -> Result<Vec<Uuid>, StorageError> {
    if ts.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar(ADMISSIONS_OUTSIDE_OWNER_SQL)
        .bind(ts)
        .bind(owner.stored_owner_id())
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)
}

const ADMISSIONS_OUTSIDE_OWNER_SQL: &str = "\
SELECT c.t
  FROM unnest($1::uuid[]) AS c(t)
 WHERE NOT EXISTS (
           SELECT 1 FROM proxima_core.memory m
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
    owner: &Owner,
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
        "SELECT handle FROM proxima_core.memory
          WHERE t = ANY($1::uuid[]) AND owner_id = $2
        UNION
        SELECT handle FROM proxima_core.cooled
          WHERE t = ANY($1::uuid[]) AND owner_id = $2
          ORDER BY handle",
    )
    .bind(ts)
    .bind(owner.stored_owner_id())
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    lock_memory_handles_tx(tx, &handles).await?;

    let present: Vec<Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_core.memory
          WHERE t = ANY($1::uuid[]) AND owner_id = $2
        UNION
        SELECT t FROM proxima_core.cooled
          WHERE t = ANY($1::uuid[]) AND owner_id = $2
          ORDER BY t",
    )
    .bind(ts)
    .bind(owner.stored_owner_id())
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
    "SELECT t FROM proxima_core.memory WHERE t = ANY($1::uuid[]) ORDER BY t FOR UPDATE";

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
    owner: &Owner,
    ts: &[Uuid],
) -> Result<Vec<Uuid>, StorageError> {
    if ts.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar(CITED_BLOBS_LOCKED_SQL)
        .bind(ts)
        .bind(owner.stored_owner_id())
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
    SELECT m.blob_id FROM proxima_core.memory m
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
/// admission cites it" is the whole reference question. Citation sidecars
/// hold no foreign key on it: they are opaque payload tables, which is why
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
               SELECT 1 FROM proxima_core.memory m WHERE m.blob_id = b.blob_id
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
mod forget_pg_tests;

#[cfg(test)]
mod tests {
    use super::{ForgetLeg, OwnerSurfaces, StorageError, forget_leg_sql, write_bytes};

    fn shipped() -> OwnerSurfaces {
        OwnerSurfaces::for_registry(
            &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
        )
    }

    /// The declared walk reaches exactly the four derived-row tables and
    /// nothing else.
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
                ("proxima_core.embedding_heads", "entity_id"),
                ("proxima_core.embedding_jobs", "entity_id"),
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

    fn cold_row(refs: Vec<uuid::Uuid>, goal_refs: Vec<uuid::Uuid>) -> super::ColdRecord {
        super::ColdRecord {
            row: super::HotRow {
                handle: uuid::Uuid::now_v7(),
                t: uuid::Uuid::now_v7(),
                kind: "fact".to_owned(),
                owner_id: uuid::Uuid::now_v7(),
                schema_id: String::new(),
                source_id: Some("src".to_owned()),
                ingest_key: None,
                blob_id: None,
                origins: Vec::new(),
                refs,
                goal_refs,
                sidecar_tables: Vec::new(),
                content_id: None,
            },
            schema_id: "core/upload-v1".to_owned(),
            sidecar_dumps: Vec::new(),
            detail_dumps: Vec::new(),
            embed_models: Vec::new(),
            sketch: None,
            format_version: super::COLD_FORMAT_VERSION,
        }
    }

    #[test]
    fn a_v7_cold_object_round_trips_pins_and_sidecar_stamp() {
        let memory = uuid::Uuid::now_v7();
        let goal = uuid::Uuid::now_v7();
        let mut rec = cold_row(vec![memory], vec![goal]);
        rec.row.sidecar_tables = vec!["proxima_core.agent_note_v1".to_owned()];
        rec.sidecar_dumps = vec![(rec.row.sidecar_tables[0].clone(), "{}".to_owned())];
        let decoded =
            super::decode_record(&super::encode_record(&rec).expect("encode")).expect("v7 decodes");
        assert_eq!(decoded.row.refs, vec![memory]);
        assert_eq!(decoded.row.goal_refs, vec![goal]);
        assert_eq!(decoded.row.sidecar_tables, rec.row.sidecar_tables);
        assert_eq!(decoded.format_version, 7);
        let sidecars = crate::core_pg_sidecars();
        assert!(super::cold_sidecar_stamp_matches(&decoded, &sidecars));
        let mut retained = decoded.clone();
        retained
            .row
            .sidecar_tables
            .push("proxima_core.mcp_call_logged_v1".to_owned());
        assert!(
            super::cold_sidecar_stamp_matches(&retained, &sidecars),
            "declared retained owner-pinned dumps are intentionally omitted"
        );
    }

    /// Cold objects written before the split have no `goal_refs` field at
    /// all, and their `refs` is still the mixed array. Decode must not
    /// invent the field, and must report the version it read so hydrate
    /// knows to separate `refs` against the live Goal spine rather than
    /// re-inserting a Goal id into the Memory column, where the pin check
    /// would now reject it.
    #[test]
    fn a_pre_split_cold_object_decodes_with_an_unsplit_refs_array() {
        let rec = cold_row(Vec::new(), Vec::new());
        let mixed = vec![uuid::Uuid::now_v7(), uuid::Uuid::now_v7()];

        // The v4 layout, written out by hand: it is identical to v5 except
        // that no goal_refs list follows refs.
        let mut v4 = vec![4_u8];
        super::write_uuid(&mut v4, rec.row.handle);
        super::write_uuid(&mut v4, rec.row.t);
        super::write_str(&mut v4, &rec.row.kind).expect("kind");
        super::write_uuid(&mut v4, rec.row.owner_id);
        super::write_opt_str(&mut v4, rec.row.source_id.as_deref()).expect("source");
        super::write_opt_str(&mut v4, rec.row.ingest_key.as_deref()).expect("ingest key");
        super::write_opt_uuid(&mut v4, rec.row.blob_id);
        super::write_uuid_list(&mut v4, &rec.row.origins).expect("origins");
        super::write_uuid_list(&mut v4, &mixed).expect("refs");
        super::write_str(&mut v4, &rec.schema_id).expect("schema");
        super::write_count(&mut v4, 0).expect("no sidecars");
        super::write_str_list(&mut v4, &rec.embed_models).expect("embed models");
        super::write_opt_str(&mut v4, None).expect("sketch");

        let decoded = super::decode_record(&v4).expect("v4 decodes");
        assert_eq!(decoded.format_version, 4);
        assert_eq!(
            decoded.row.refs, mixed,
            "a pre-split object's refs must survive decode untouched"
        );
        assert!(
            decoded.row.goal_refs.is_empty(),
            "decode must not guess which legacy refs were Goals"
        );
    }

    #[test]
    fn a_legacy_cold_object_has_no_authenticated_sidecar_stamp() {
        let rec = cold_row(Vec::new(), Vec::new());
        let mut v5 = vec![5_u8];
        super::write_uuid(&mut v5, rec.row.handle);
        super::write_uuid(&mut v5, rec.row.t);
        super::write_str(&mut v5, &rec.row.kind).expect("kind");
        super::write_uuid(&mut v5, rec.row.owner_id);
        super::write_opt_str(&mut v5, rec.row.source_id.as_deref()).expect("source");
        super::write_opt_str(&mut v5, rec.row.ingest_key.as_deref()).expect("ingest key");
        super::write_opt_uuid(&mut v5, rec.row.blob_id);
        super::write_uuid_list(&mut v5, &rec.row.origins).expect("origins");
        super::write_uuid_list(&mut v5, &rec.row.refs).expect("refs");
        super::write_uuid_list(&mut v5, &rec.row.goal_refs).expect("goal refs");
        super::write_str(&mut v5, &rec.schema_id).expect("schema");
        super::write_count(&mut v5, 0).expect("no sidecars");
        super::write_str_list(&mut v5, &rec.embed_models).expect("embed models");
        super::write_opt_str(&mut v5, None).expect("sketch");
        let decoded = super::decode_record(&v5).expect("v5 decodes");
        assert_eq!(decoded.format_version, 5);
        assert!(decoded.row.sidecar_tables.is_empty());
        assert!(!super::cold_sidecar_stamp_matches(
            &decoded,
            &crate::core_pg_sidecars()
        ));
    }

    #[test]
    fn an_unknown_cold_format_is_refused() {
        let future = super::COLD_FORMAT_VERSION.saturating_add(1);
        let err = super::decode_record(&[future]).expect_err("future format is not shipped");
        assert!(
            matches!(err, StorageError::Internal(ref msg) if msg.contains(&format!("unknown cold format {future}"))),
            "got {err:?}"
        );
    }

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
