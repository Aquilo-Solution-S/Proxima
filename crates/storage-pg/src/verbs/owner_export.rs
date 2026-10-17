use std::collections::BTreeMap;

use proxima_core::StorageError;
use proxima_core::flavor::{ExportRule, ForgetLeg, KeyShape, Surface};
use proxima_core::owner_inverse::{ExportAuthorization, OwnerExportBundle, OwnerSurfaces};
use serde_json::Value;
use sqlx::PgPool;

use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::verbs::forget::{ColdRecord, ColdRejection, CooledRow, read_verified_cold_record};
use sqlx::{Connection, Postgres, Transaction};

/// The object stores an export reads beside the database. Change 5 adds the
/// blob store here; today only forgotten memories' cold objects are read.
pub struct ExportObjectSources<'a> {
    pub cold: &'a dyn proxima_core::ColdObjectStore,
}

/// One owner's bundle: every surface the contract declares exportable, as
/// table name → rows, plus the pins projected from those rows.
///
/// The statement is generated from the declaration, so a surface is in the
/// bundle iff it declares `Rows` or `Allowlist`, with exactly the fields it
/// names, ordered by the key it names.
pub async fn export_owner_bundle(
    _pool: &PgPool,
    platform: &crate::PgPlatformScope,
    auth: &ExportAuthorization,
    surfaces: &OwnerSurfaces,
    lifecycle: Option<crate::RegisteredHostStateLifecycle>,
    sources: &ExportObjectSources<'_>,
) -> Result<OwnerExportBundle, StorageError> {
    let owner = auth.audit().owner();
    let owner_id = owner.into_inner();
    validate_lifecycle_dispatch(surfaces, lifecycle.as_ref())?;
    let mut conn = platform.detached_connection().await?;
    // Session locks survive the following transaction and are released when
    // this detached connection is dropped. If cancellation/error interrupts
    // any later await, Drop closes the connection instead of returning a
    // still-locked session to the pool.
    crate::access::owner_columns::lock_host_lifecycle_fence_shared_session(&mut conn).await?;
    crate::access::owner_columns::lock_owner_fence_exclusive_session(&mut conn, &owner).await?;
    // Phases (#478): (a) snapshot every table row and every cooled locator
    // in one REPEATABLE READ READ ONLY transaction, (b) commit it, (c) read
    // the cold objects with no transaction open, still under the session
    // fences this connection holds, (d) merge the decoded rows in.
    let result = async {
        let (mut bundle, snapshot) = export_owner_bundle_snapshot(
            &mut conn,
            platform,
            auth,
            owner,
            owner_id,
            surfaces,
            lifecycle.as_ref(),
        )
        .await?;
        #[cfg(test)]
        crate::pause_point::reach(crate::pause_point::AFTER_EXPORT_SNAPSHOT, owner_id).await;
        let fetched = fetch_cold_records(sources.cold, &snapshot.cooled).await?;
        merge_cold_records(
            &mut conn,
            platform,
            &mut bundle,
            &snapshot,
            fetched,
            surfaces,
        )
        .await?;
        Ok(bundle)
    }
    .await;
    drop(conn);
    result
}

async fn export_owner_bundle_snapshot(
    conn: &mut sqlx::postgres::PgConnection,
    platform: &crate::PgPlatformScope,
    auth: &ExportAuthorization,
    owner: proxima_core::OwnerId,
    owner_id: uuid::Uuid,
    surfaces: &OwnerSurfaces,
    lifecycle: Option<&crate::RegisteredHostStateLifecycle>,
) -> Result<(OwnerExportBundle, ExportSnapshot), StorageError> {
    let mut tx = conn.begin().await.map_err(map_err)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    platform.bind_transaction(tx.as_mut()).await?;
    let mut tables: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut primary_keys: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for surface in surfaces.generic_surfaces() {
        if !surface.export.carries_rows() {
            continue;
        }
        let primary_key = primary_key_columns(&mut tx, surface.table).await?;
        let statement = export_statement(surface, &primary_key)?;
        primary_keys.insert(surface.table.to_owned(), primary_key);
        let Some(sql) = statement else {
            continue;
        };
        // SQL-POLICY: PgIdent
        let rows: Vec<Value> = sqlx::query_scalar::<_, Value>(sqlx::AssertSqlSafe(sql))
            .bind(owner_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(map_err)?;
        tables.insert(surface.table.to_owned(), rows);
    }

    invoke_host_lifecycle_export(&mut tx, surfaces, owner, lifecycle, &mut tables).await?;
    let expected_tables = surfaces
        .surfaces()
        .iter()
        .filter(|surface| surface.export.carries_rows())
        .map(|surface| surface.table)
        .collect::<std::collections::BTreeSet<_>>();
    let actual_tables = tables
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    if actual_tables != expected_tables {
        return Err(StorageError::Internal(
            "owner export did not return the exact included declared surface set".into(),
        ));
    }

    // Pins come from hot memory rows AND cooled stubs, so a forgotten
    // memory's origins and refs stay in the graph (#478). A pre-witness
    // cooled row has NULL arrays and contributes nothing.
    let pinned_rows: Vec<Value> = ["proxima_core.memory", "proxima_core.cooled"]
        .iter()
        .filter_map(|table| tables.get(*table))
        .flatten()
        .cloned()
        .collect();
    let edges = pins_from_memories(&pinned_rows);

    let mut counts: BTreeMap<String, usize> = tables
        .iter()
        .map(|(table, rows)| (table.clone(), rows.len()))
        .collect();
    counts.insert("edges".to_owned(), edges.len());

    let bundle = OwnerExportBundle {
        operation_id: auth.audit().operation_id(),
        target: auth.audit().target().clone(),
        owner,
        derived_requester: auth.audit().derived_requester(),
        derived_auth_path: format!("{:?}", auth.audit().derived_auth_path()),
        exported_at: auth.audit().requested_at(),
        counts,
        tables,
        edges,
        cold_unavailable: Vec::new(),
    };
    // The locators stay in memory; they never enter the bundle.
    let cooled = crate::verbs::forget::owner_cooled_rows(&mut tx, owner_id).await?;
    let keys: Vec<&str> = cooled.iter().map(|row| row.object_key.as_str()).collect();
    let purge_pending: std::collections::BTreeSet<String> = sqlx::query_scalar(
        "SELECT object_key FROM proxima_core.cold_purge_pending WHERE object_key = ANY($1)",
    )
    .bind(&keys)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_err)?
    .into_iter()
    .collect();
    tx.commit().await.map_err(map_err)?;
    Ok((
        bundle,
        ExportSnapshot {
            cooled,
            purge_pending,
            primary_keys,
        },
    ))
}

/// What the snapshot transaction saw beside the bundle rows. Change 5 adds
/// the blob locators here.
struct ExportSnapshot {
    cooled: Vec<CooledRow>,
    purge_pending: std::collections::BTreeSet<String>,
    primary_keys: BTreeMap<String, Vec<String>>,
}

/// Phase (c): locator + expected digest -> verified record, or a permanent
/// verdict. Sequential, and never under an open transaction. A store or
/// transport error fails the whole export: no partial bundle.
async fn fetch_cold_records(
    cold: &dyn proxima_core::ColdObjectStore,
    cooled: &[CooledRow],
) -> Result<Vec<(CooledRow, Result<ColdRecord, ColdRejection>)>, StorageError> {
    let mut fetched = Vec::with_capacity(cooled.len());
    for row in cooled {
        let verdict = read_verified_cold_record(cold, row).await?;
        fetched.push((row.clone(), verdict));
    }
    Ok(fetched)
}

/// Phase (d): decoded rows into their declared tables, verdicts into
/// `cold_unavailable`, then re-sort what changed and recount.
///
/// The fence rule. The export holds the host-lifecycle fence shared and the
/// owner fence exclusive for its whole run, which every owner-fenced verb
/// (ingest, write sessions, goal and memory timeseries, goal writes, owner
/// erase) waits on. Transfer takes both owners' fences exclusive
/// (`owner_columns::transfer_to_owner`), so it cannot move a row
/// mid-export. Forget and hydrate do not take the owner fence, so a
/// `missing` or `invalid` verdict is only reported after the cooled row is
/// re-read unchanged in a fresh transaction; a row that is gone or names
/// another object means one of them got in between the snapshot and the
/// read, and the export answers `Retryable` rather than a bundle that
/// disagrees with itself. A present, digest-verified object is exported
/// even when a purge is queued for it: the digest decides.
async fn merge_cold_records(
    conn: &mut sqlx::postgres::PgConnection,
    platform: &crate::PgPlatformScope,
    bundle: &mut OwnerExportBundle,
    snapshot: &ExportSnapshot,
    fetched: Vec<(CooledRow, Result<ColdRecord, ColdRejection>)>,
    surfaces: &OwnerSurfaces,
) -> Result<(), StorageError> {
    let mut touched = std::collections::BTreeSet::new();
    let mut unavailable = Vec::new();
    for (row, verdict) in fetched {
        let (reason, revalidate) = match verdict {
            Ok(rec) => match decoded_rows(&rec, &row, surfaces) {
                Some(rows) => {
                    for (table, value) in rows {
                        bundle
                            .tables
                            .entry(table.to_owned())
                            .or_default()
                            .push(value);
                        touched.insert(table);
                    }
                    enrich_cooled_row(bundle, &row, &rec);
                    continue;
                }
                None => ("invalid".to_owned(), true),
            },
            Err(ColdRejection::MissingObject)
                if snapshot.purge_pending.contains(&row.object_key) =>
            {
                ("purge_pending".to_owned(), false)
            }
            Err(ColdRejection::MissingObject) => ("missing".to_owned(), true),
            Err(ColdRejection::InvalidObject) => ("invalid".to_owned(), true),
            Err(ColdRejection::UnsupportedFormat(version)) => {
                (format!("unsupported_format({version})"), false)
            }
            Err(ColdRejection::UnsupportedSidecar) => ("unsupported_sidecar".to_owned(), false),
            // The shared verifier answers this for a pre-witness locator only.
            Err(ColdRejection::UnsupportedObject) => ("pre_witness".to_owned(), false),
        };
        if revalidate {
            revalidate_cooled_row(conn, platform, &row).await?;
        }
        unavailable.push(serde_json::json!({"t": row.t.to_string(), "reason": reason}));
    }
    for table in touched {
        let Some(surface) = surfaces
            .surfaces()
            .iter()
            .find(|surface| surface.table == table)
        else {
            continue;
        };
        let mut columns: Vec<String> = surface
            .key
            .columns()
            .into_iter()
            .map(str::to_owned)
            .collect();
        for column in snapshot.primary_keys.get(table).into_iter().flatten() {
            if !columns.contains(column) {
                columns.push(column.clone());
            }
        }
        if let Some(rows) = bundle.tables.get_mut(table) {
            rows.sort_by(|a, b| {
                columns
                    .iter()
                    .map(|column| json_order(&a[column.as_str()], &b[column.as_str()]))
                    .find(|ordering| ordering.is_ne())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
    }
    bundle.cold_unavailable = unavailable;
    let mut counts: BTreeMap<String, usize> = bundle
        .tables
        .iter()
        .map(|(table, rows)| (table.clone(), rows.len()))
        .collect();
    counts.insert("edges".to_owned(), bundle.edges.len());
    counts.insert("cold_unavailable".to_owned(), bundle.cold_unavailable.len());
    bundle.counts = counts;
    Ok(())
}

/// The rows one verified record contributes, each through its surface's
/// export rule, or `None` when the record names a table that is not a
/// declared exported surface (the object is then `invalid`). All or nothing.
fn decoded_rows(
    rec: &ColdRecord,
    row: &CooledRow,
    surfaces: &OwnerSurfaces,
) -> Option<Vec<(&'static str, Value)>> {
    let mut out = Vec::new();
    for (table, json) in &rec.sidecar_dumps {
        let surface = carried_surface(surfaces, table)?;
        let value: Value = serde_json::from_str(json).ok()?;
        out.push((surface.table, project(surface, &value)?));
    }
    for (table, rows) in &rec.detail_dumps {
        let surface = carried_surface(surfaces, table)?;
        for json in rows {
            let value: Value = serde_json::from_str(json).ok()?;
            out.push((surface.table, project(surface, &value)?));
        }
    }
    if let Some(text) = &rec.sketch {
        let surface = carried_surface(surfaces, "proxima_core.sketch")?;
        let value = serde_json::json!({
            "t": row.t.to_string(),
            "owner_id": row.owner_id.to_string(),
            "kind": row.kind,
            "text": text,
        });
        out.push((surface.table, project(surface, &value)?));
    }
    Some(out)
}

fn carried_surface<'a>(surfaces: &'a OwnerSurfaces, table: &str) -> Option<&'a Surface> {
    surfaces
        .surfaces()
        .iter()
        .find(|surface| surface.table == table && surface.export.carries_rows())
}

/// A decoded row through its surface's export rule. A dump omits generated
/// columns (forget strips them), so an allowlisted field the dump lacks is
/// carried as null rather than refusing the object.
fn project(surface: &Surface, value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    match surface.export {
        ExportRule::Rows => Some(value.clone()),
        ExportRule::Allowlist(fields) => Some(Value::Object(
            fields
                .iter()
                .map(|field| {
                    (
                        (*field).to_owned(),
                        object.get(*field).cloned().unwrap_or(Value::Null),
                    )
                })
                .collect(),
        )),
        ExportRule::Excluded { .. } | ExportRule::DerivedFrom { .. } => None,
    }
}

/// The cooled stub gains what only its object knew: the schema and the
/// event time.
fn enrich_cooled_row(bundle: &mut OwnerExportBundle, row: &CooledRow, rec: &ColdRecord) {
    let t = row.t.to_string();
    let Some(rows) = bundle.tables.get_mut("proxima_core.cooled") else {
        return;
    };
    let Some(object) = rows
        .iter_mut()
        .filter_map(Value::as_object_mut)
        .find(|object| object.get("t").and_then(Value::as_str) == Some(t.as_str()))
    else {
        return;
    };
    object.insert("schema_id".to_owned(), Value::String(rec.schema_id.clone()));
    let occurred_at = rec
        .row
        .occurred_at
        .and_then(|at| {
            at.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .map_or(Value::Null, Value::String);
    object.insert("occurred_at".to_owned(), occurred_at);
}

/// Re-read one cooled row in a fresh read-only transaction. Gone, or naming
/// another object or digest: a lifecycle verb raced the read, so retry.
async fn revalidate_cooled_row(
    conn: &mut sqlx::postgres::PgConnection,
    platform: &crate::PgPlatformScope,
    row: &CooledRow,
) -> Result<(), StorageError> {
    let mut tx = conn.begin().await.map_err(map_err)?;
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    platform.bind_transaction(tx.as_mut()).await?;
    let now = crate::verbs::forget::cooled_row(&mut tx, row.t).await?;
    tx.commit().await.map_err(map_err)?;
    match now {
        Some(now) if now.object_key == row.object_key && now.cold_digest == row.cold_digest => {
            Ok(())
        }
        _ => Err(StorageError::Retryable(format!(
            "the cooled memory {} changed while its cold object was read; retry the export",
            row.t
        ))),
    }
}

/// A total order over the JSON scalars a key or primary-key column holds.
/// UUIDs render lowercase with fixed hyphens, so string order is byte order.
fn json_order(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .partial_cmp(&b.as_f64())
            .unwrap_or(Ordering::Equal),
        _ => a.to_string().cmp(&b.to_string()),
    }
}

fn validate_lifecycle_dispatch(
    surfaces: &OwnerSurfaces,
    lifecycle: Option<&crate::RegisteredHostStateLifecycle>,
) -> Result<(), StorageError> {
    use std::collections::BTreeSet;
    let expected = surfaces
        .host_lifecycle_surfaces()
        .iter()
        .map(|policy| policy.table)
        .collect::<BTreeSet<_>>();
    let Some(lifecycle) = lifecycle else {
        return if expected.is_empty() {
            Ok(())
        } else {
            Err(StorageError::Internal(
                "managed host lifecycle surface has no registered callback; refusing export".into(),
            ))
        };
    };
    let declared = lifecycle
        .descriptor
        .tables()
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if expected.is_empty()
        || lifecycle.descriptor.tables().len() != declared.len()
        || declared != expected
    {
        return Err(StorageError::Internal(
            "registered host lifecycle descriptor does not exactly match export surfaces".into(),
        ));
    }
    Ok(())
}

async fn invoke_host_lifecycle_export(
    tx: &mut Transaction<'_, Postgres>,
    surfaces: &OwnerSurfaces,
    owner: proxima_core::OwnerId,
    lifecycle: Option<&crate::RegisteredHostStateLifecycle>,
    tables: &mut BTreeMap<String, Vec<Value>>,
) -> Result<(), StorageError> {
    validate_lifecycle_dispatch(surfaces, lifecycle)?;
    let Some(lifecycle) = lifecycle else {
        return Ok(());
    };
    let included_tables = surfaces
        .host_lifecycle_surfaces()
        .iter()
        .filter(|policy| policy.owner_export.carries_rows())
        .map(|policy| policy.table)
        .collect::<Vec<_>>();
    let receipt = lifecycle
        .port
        .export(
            tx,
            proxima_core::storage_ports::HostStateExportRequest::new(
                lifecycle.descriptor.participant_id(),
                owner,
                included_tables.clone(),
            ),
        )
        .await?;
    if receipt.participant != lifecycle.descriptor.participant_id() || receipt.owner != owner {
        return Err(StorageError::Internal(
            "host lifecycle export receipt has a foreign participant or owner".into(),
        ));
    }
    let expected = included_tables
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let mut received = std::collections::BTreeMap::new();
    for table in receipt.tables {
        if received.insert(table.table, table.rows).is_some() {
            return Err(StorageError::Internal(
                "host lifecycle export receipt contains a duplicate table".into(),
            ));
        }
    }
    let actual = received
        .keys()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    if actual != expected {
        return Err(StorageError::Internal(
            "host lifecycle export receipt does not contain the exact included table set".into(),
        ));
    }
    for (table, rows) in received {
        let policy = surfaces
            .host_lifecycle_surfaces()
            .iter()
            .find(|policy| policy.table == table)
            .expect("exact receipt table set was checked against frozen policies");
        validate_host_export_rows(table.as_str(), policy.owner_export, &rows)?;
        tables.insert(table.as_str().to_owned(), rows);
    }
    Ok(())
}

fn validate_host_export_rows(
    table: &str,
    policy: ExportRule,
    rows: &[Value],
) -> Result<(), StorageError> {
    for row in rows {
        let Some(object) = row.as_object() else {
            return Err(StorageError::Internal(format!(
                "host lifecycle export row for {table} is not a JSON object"
            )));
        };
        match policy {
            ExportRule::Rows => {}
            ExportRule::Allowlist(fields) => {
                if object.len() != fields.len()
                    || fields.iter().any(|field| !object.contains_key(*field))
                {
                    return Err(StorageError::Internal(format!(
                        "host lifecycle export row for {table} differs from its field allowlist"
                    )));
                }
            }
            ExportRule::Excluded { .. } | ExportRule::DerivedFrom { .. } => {
                return Err(StorageError::Internal(format!(
                    "host lifecycle table {table} carries no rows in the bundle but appeared \
                     in the export receipt"
                )));
            }
        }
    }
    Ok(())
}

/// The statement one surface's declaration earns it, or `None` when the
/// surface declares itself out of the bundle.
///
/// Two shapes, decided by `owner_column` — which is a claim, not an
/// omission. A surface carrying its own owner is filtered on it directly:
/// that is the transfer doctrine for owner-pinned rows, whose audit history
/// stays in the bundle of the owner that wrote it after the Memory it
/// describes has been transferred away, and out of the receiving owner's. A
/// surface with a `None` `owner_column` asserts it is reached through its
/// key's owner, so the statement joins that key's home table and filters
/// there. `try_freeze` refuses a flavor that declares an exportable surface
/// which is neither, so the `Internal` arm below is unreachable through a
/// frozen registry and is kept for the synthetic surfaces tests build.
fn export_statement(
    surface: &Surface,
    primary_key: &[String],
) -> Result<Option<String>, StorageError> {
    let projection = match surface.export {
        ExportRule::Excluded { .. } | ExportRule::DerivedFrom { .. } => return Ok(None),
        // The surface is aliased `s`, never `t`. With a single range table
        // in scope Postgres resolves a bare `t` in `to_jsonb(t)` to *the
        // column*, exporting a JSON string holding the uuid where the whole
        // row belongs — silent data loss in a portability bundle — and with
        // a joined base table that also has a `t` it raises `column
        // reference "t" is ambiguous`.
        ExportRule::Rows => "to_jsonb(s)".to_owned(),
        // An explicit field allowlist: the table is an unsupported
        // persistence detail, the bundle is a supported serialized contract,
        // and a storage-only column added later must not leak into it merely
        // because the table changed. `jsonb_build_object` normalizes key
        // order exactly as `to_jsonb` does, so an allowlist naming every
        // column is byte-identical to the row.
        ExportRule::Allowlist(fields) => {
            let mut parts = Vec::with_capacity(fields.len());
            for field in fields {
                let column = PgIdent::column(field)?;
                parts.push(format!("'{}', s.{}", field, column.as_str()));
            }
            format!("jsonb_build_object({})", parts.join(", "))
        }
    };
    let table = PgIdent::table(surface.table)?;
    let order = order_by(surface, primary_key)?;
    // SQL-POLICY: PgIdent
    let sql = if let Some(owner_column) = surface.owner_column {
        // `s.{owner}` is the surface's OWN owner column as it declares it.
        // The branch below joins to the key's home table instead, and
        // `base.owner_id` there is `proxima_core.memory`'s own column,
        // which is fixed.
        let owner = PgIdent::column(owner_column)?;
        format!(
            "SELECT {projection}
               FROM {table} s
              WHERE s.{owner} IS NOT DISTINCT FROM $1
              ORDER BY {order}",
            table = table.as_str(),
            owner = owner.as_str(),
        )
    } else {
        let Some((base_table, base_column, key_column)) = surface.key.home() else {
            return Err(StorageError::Internal(format!(
                "{} declares no owner column and a key with no home table, so no \
                 export statement can reach its owner",
                surface.table
            )));
        };
        // A memory-keyed surface whose rows outlive a cool (forget keeps
        // them) belongs to a memory that may now be a `cooled` stub, so its
        // home is the memory spine hot OR cooled. Derived from the
        // declaration (see `KeyShape::home`), never from a table name.
        let base = if cooled_rows_reach_home(surface) {
            "(SELECT t, owner_id FROM proxima_core.memory \
              UNION ALL SELECT t, owner_id FROM proxima_core.cooled)"
                .to_owned()
        } else {
            PgIdent::table(base_table)?.as_str().to_owned()
        };
        let base_column = PgIdent::column(base_column)?;
        let key_column = PgIdent::column(key_column)?;
        format!(
            "SELECT {projection}
               FROM {table} s
               JOIN {base} base
                 ON base.{base_column} = s.{key_column}
              WHERE base.owner_id IS NOT DISTINCT FROM $1
              ORDER BY {order}",
            table = table.as_str(),
            base_column = base_column.as_str(),
            key_column = key_column.as_str(),
        )
    };
    Ok(Some(sql))
}

/// Whether an owner-less surface reaches its owner through the memory
/// spine hot OR cooled: keyed on a memory `t`, and kept by forget, so its
/// rows outlive the hot row they hang off.
fn cooled_rows_reach_home(surface: &Surface) -> bool {
    surface.owner_column.is_none()
        && matches!(surface.key, KeyShape::MemoryT { .. })
        && matches!(ForgetLeg::derive(surface), ForgetLeg::Kept { .. })
}

/// Row order is part of the bundle's bytes, so it is TOTAL: the declared
/// key first, then the table's remaining primary-key columns in index order.
///
/// A declared key need not be unique. It is what the erase and transfer
/// select by (`ingest_keys` on `t`, `memory_external_origin` on `t`), and one
/// memory may hold several such rows. Ordering by the key alone would leave
/// their byte order to whatever the executor returns; the primary key breaks
/// every tie, so the bundle is a function of the rows (#478). The columns
/// come from the catalog (`primary_key_columns`), never from a table name.
///
/// A surface with no primary key has no total order to offer and is refused
/// rather than exported in an order nobody chose.
fn order_by(surface: &Surface, primary_key: &[String]) -> Result<String, StorageError> {
    if primary_key.is_empty() {
        return Err(StorageError::Internal(format!(
            "{} has no primary key, so its export order would not be total",
            surface.table
        )));
    }
    let key = surface.key.columns();
    let mut parts = Vec::new();
    let tiebreak = primary_key
        .iter()
        .map(String::as_str)
        .filter(|column| !key.contains(column));
    for column in key.iter().copied().chain(tiebreak) {
        parts.push(format!("s.{}", PgIdent::column(column)?.as_str()));
    }
    Ok(parts.join(", "))
}

/// The table's primary-key columns in index order, read from the catalog.
async fn primary_key_columns(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
) -> Result<Vec<String>, StorageError> {
    sqlx::query_scalar(
        "SELECT a.attname::text
           FROM pg_index i
          CROSS JOIN LATERAL unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord)
           JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
          WHERE i.indrelid = $1::regclass
            AND i.indisprimary
          ORDER BY k.ord",
    )
    .bind(table)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)
}

/// Pin edges of memory-shaped rows (hot `memory` or `cooled`).
fn pins_from_memories(memories: &[Value]) -> Vec<Value> {
    let mut edges = Vec::new();
    for memory in memories {
        let Some(source_t) = memory.get("t") else {
            continue;
        };
        push_pins(&mut edges, source_t, memory.get("origins"), "origin");
        push_pins(&mut edges, source_t, memory.get("refs"), "reference");
    }
    edges.sort_by_key(pin_sort_key);
    edges
}

fn pin_sort_key(edge: &Value) -> (String, String, String) {
    (
        edge.get("source_t")
            .map(ToString::to_string)
            .unwrap_or_default(),
        edge.get("target_t")
            .map(ToString::to_string)
            .unwrap_or_default(),
        edge.get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    )
}

fn push_pins(edges: &mut Vec<Value>, source_t: &Value, pins: Option<&Value>, kind: &'static str) {
    let Some(Value::Array(pins)) = pins else {
        return;
    };
    for pin in pins {
        edges.push(serde_json::json!({
            "source_t": source_t,
            "target_t": pin,
            "kind": kind,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::{export_statement, pin_sort_key, pins_from_memories};
    use proxima_core::FLAVOR_0;
    use proxima_core::flavor::{ExportRule, Surface};
    use serde_json::json;

    fn pk(columns: &[&str]) -> Vec<String> {
        columns.iter().map(|column| (*column).to_owned()).collect()
    }

    /// The declared key comes first; the rest of the primary key breaks its
    /// ties, so a non-unique key still gives a total order.
    #[test]
    fn export_order_is_the_key_then_the_rest_of_the_primary_key() {
        let ingest = surface("proxima_core.ingest_keys");
        let sql = export_statement(&ingest, &pk(&["owner_id", "source_id", "ingest_key"]))
            .expect("generates")
            .expect("ingest keys are exported");
        assert!(
            sql.contains("ORDER BY s.t, s.owner_id, s.source_id, s.ingest_key"),
            "{sql}"
        );
        assert!(
            export_statement(&ingest, &[]).is_err(),
            "a surface with no primary key has no total order and is refused"
        );
    }

    fn surface(table: &str) -> Surface {
        FLAVOR_0
            .all_surfaces()
            .find(|surface| surface.table == table)
            .unwrap_or_else(|| panic!("flavor #0 declares {table}"))
    }

    #[test]
    fn export_sql_does_not_rebuild_an_edge_table() {
        let src = include_str!("owner_export.rs");
        let needle = format!("{}{}", "JOIN unnest", "(src.origins)");
        assert!(
            !src.contains(&needle),
            "export must project pins from memory rows, not unnest a second Edge scan"
        );
    }

    #[test]
    fn a_cooled_row_with_null_pins_contributes_no_edges() {
        let source = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let edges = pins_from_memories(&[json!({
            "t": source,
            "origins": null,
            "refs": null,
        })]);
        assert!(edges.is_empty());
    }

    #[test]
    fn pins_come_from_memory_origin_and_ref_arrays() {
        let source = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let origin = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        let reference = "cccccccc-cccc-cccc-cccc-cccccccccccc";
        let edges = pins_from_memories(&[json!({
            "t": source,
            "origins": [origin],
            "refs": [reference],
        })]);
        assert_eq!(
            edges,
            vec![
                json!({"source_t": source, "target_t": origin, "kind": "origin"}),
                json!({"source_t": source, "target_t": reference, "kind": "reference"}),
            ]
        );
        assert!(pin_sort_key(&edges[0]) < pin_sort_key(&edges[1]));
    }

    /// Pinning the generated text is what keeps an allowlist an allowlist
    /// rather than a comment.
    #[test]
    fn an_allowlisted_surface_generates_exactly_its_declared_fields() {
        let sql = export_statement(&surface("proxima_core.blob"), &pk(&["blob_id"]))
            .expect("generates")
            .expect("blob is exported");
        assert!(
            sql.contains(
                "jsonb_build_object('blob_id', s.blob_id, 'schema_id', s.schema_id, \
                 'content_hash', s.content_hash)"
            ),
            "{sql}"
        );
        assert!(sql.contains("ORDER BY s.blob_id"), "{sql}");
    }

    /// A surface with a `None` `owner_column` claims it is reached through its
    /// key's owner, and the generated join is what makes the claim true.
    #[test]
    fn a_keyed_sidecar_reaches_the_owner_through_its_home_table() {
        let sql = export_statement(&surface("proxima_core.agent_note_v1"), &pk(&["t"]))
            .expect("generates")
            .expect("the note sidecar is exported");
        assert!(sql.contains("JOIN proxima_core.memory base"), "{sql}");
        assert!(
            sql.contains("WHERE base.owner_id IS NOT DISTINCT FROM $1"),
            "{sql}"
        );
        assert!(sql.contains("to_jsonb(s)"), "{sql}");
    }

    /// Other-store origins outlive a cool, so the export finds their owner
    /// on the memory spine hot OR cooled, not on `memory` alone (#478).
    #[test]
    fn external_origins_reach_the_owner_through_hot_or_cooled_memories() {
        let sql = export_statement(
            &surface("proxima_core.memory_external_origin"),
            &pk(&["t", "memory"]),
        )
        .expect("generates")
        .expect("external origins are exported");
        let flat = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(
            flat,
            "SELECT to_jsonb(s) FROM proxima_core.memory_external_origin s \
             JOIN (SELECT t, owner_id FROM proxima_core.memory UNION ALL \
             SELECT t, owner_id FROM proxima_core.cooled) base ON base.t = s.t \
             WHERE base.owner_id IS NOT DISTINCT FROM $1 ORDER BY s.t, s.memory"
        );
    }

    #[test]
    fn a_surface_that_carries_no_rows_generates_nothing() {
        for table in [
            "proxima_core.owners",
            "proxima_core.memory_head",
            "proxima_core.embeddings",
        ] {
            let declared = surface(table);
            assert!(!declared.export.carries_rows());
            assert!(
                export_statement(&declared, &[])
                    .expect("generates")
                    .is_none(),
                "{table} is Excluded or DerivedFrom and must emit no statement"
            );
        }
        // content and blob_uploads were declared gaps; #478 carries them.
        for table in ["proxima_core.content", "proxima_core.blob_uploads"] {
            assert!(
                matches!(surface(table).export, ExportRule::Allowlist(_)),
                "{table}"
            );
        }
    }

    /// The collision-safety rule, as a property of every generated
    /// statement.
    #[test]
    fn no_generated_statement_aliases_a_table_t() {
        for surface in FLAVOR_0.all_surfaces() {
            let key = surface
                .key
                .columns()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let Some(sql) = export_statement(&surface, &key).expect("generates") else {
                continue;
            };
            assert!(
                !sql.contains(" t\n") && !sql.contains("to_jsonb(t)"),
                "{}: an alias of `t` makes to_jsonb resolve the COLUMN, not the row: {sql}",
                surface.table
            );
        }
    }
}

#[cfg(test)]
#[path = "owner_export_cold_pg_tests.rs"]
mod owner_export_cold_pg_tests;
