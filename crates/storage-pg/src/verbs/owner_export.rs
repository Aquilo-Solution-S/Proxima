use std::collections::BTreeMap;

use proxima_core::StorageError;
use proxima_core::flavor::{ExportRule, ForgetLeg, KeyShape, Surface};
use proxima_core::owner_inverse::{ExportAuthorization, OwnerExportBundle, OwnerSurfaces};
use serde_json::Value;
use sqlx::PgPool;

use crate::error::map_err;
use crate::pg_ident::PgIdent;
use sqlx::{Connection, Postgres, Transaction};

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
    let result = export_owner_bundle_snapshot(
        &mut conn,
        platform,
        auth,
        owner,
        owner_id,
        surfaces,
        lifecycle.as_ref(),
    )
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
) -> Result<OwnerExportBundle, StorageError> {
    let mut tx = conn.begin().await.map_err(map_err)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    platform.bind_transaction(tx.as_mut()).await?;
    let mut tables: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for surface in surfaces.generic_surfaces() {
        if !surface.export.carries_rows() {
            continue;
        }
        let primary_key = primary_key_columns(&mut tx, surface.table).await?;
        let Some(sql) = export_statement(surface, &primary_key)? else {
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
    };
    tx.commit().await.map_err(map_err)?;
    Ok(bundle)
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
