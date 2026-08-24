use std::collections::BTreeMap;

use proxima_core::StorageError;
use proxima_core::flavor::{ExportRule, Surface};
use proxima_core::owner_inverse::{ExportAuthorization, OwnerExportBundle, OwnerSurfaces};
use serde_json::Value;
use sqlx::PgPool;

use crate::access::owner_columns::{owner_binds, sole_owner_column};
use crate::error::map_err;
use crate::pg_ident::PgIdent;

/// One owner's bundle: every surface the contract declares exportable, as
/// table name → rows, plus the pins projected from those rows.
///
/// The statement is generated from the declaration, so a surface is in the
/// bundle iff it declares `Rows` or `Allowlist`, with exactly the fields it
/// names, ordered by the key it names.
pub async fn export_owner_bundle(
    pool: &PgPool,
    auth: &ExportAuthorization,
    surfaces: &OwnerSurfaces,
) -> Result<OwnerExportBundle, StorageError> {
    let owner = auth.audit().owner();
    let (_owner_kind, owner_id) = owner_binds(&owner);
    let mut tables: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for surface in surfaces.surfaces() {
        let Some(sql) = export_statement(surface)? else {
            continue;
        };
        // SQL-POLICY: PgIdent
        let rows: Vec<Value> = sqlx::query_scalar::<_, Value>(sqlx::AssertSqlSafe(sql))
            .bind(owner_id)
            .fetch_all(pool)
            .await
            .map_err(map_err)?;
        tables.insert(surface.table.to_owned(), rows);
    }

    let edges = tables
        .get("proxima_core.memory")
        .map(|memories| pins_from_memories(memories))
        .unwrap_or_default();

    let mut counts: BTreeMap<String, usize> = tables
        .iter()
        .map(|(table, rows)| (table.clone(), rows.len()))
        .collect();
    counts.insert("edges".to_owned(), edges.len());

    Ok(OwnerExportBundle {
        operation_id: auth.audit().operation_id(),
        target: auth.audit().target().clone(),
        owner,
        derived_requester: auth.audit().derived_requester(),
        derived_auth_path: format!("{:?}", auth.audit().derived_auth_path()),
        exported_at: auth.audit().requested_at(),
        counts,
        tables,
        edges,
    })
}

/// The statement one surface's declaration earns it, or `None` when the
/// surface declares itself out of the bundle.
///
/// Two shapes, decided by `owner_columns` — which is a claim, not an
/// omission. A surface carrying its own owner is filtered on it directly:
/// that is the transfer doctrine for owner-pinned rows, whose audit history
/// stays in the bundle of the owner that wrote it after the Memory it
/// describes has been transferred away, and out of the receiving owner's. A
/// surface with EMPTY `owner_columns` asserts it is reached through its
/// key's owner, so the statement joins that key's home table and filters
/// there. `try_freeze` refuses a flavor that declares an exportable surface
/// which is neither, so the `Internal` arm below is unreachable through a
/// frozen registry and is kept for the synthetic surfaces tests build.
fn export_statement(surface: &Surface) -> Result<Option<String>, StorageError> {
    let projection = match surface.export {
        ExportRule::Excluded { .. } => return Ok(None),
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
    let order = order_by(surface)?;
    // SQL-POLICY: PgIdent
    let sql = if surface.owner_columns.is_empty() {
        let Some((base_table, base_column, key_column)) = surface.key.home() else {
            return Err(StorageError::Internal(format!(
                "{} declares no owner column and a key with no home table, so no \
                 export statement can reach its owner",
                surface.table
            )));
        };
        let base_table = PgIdent::table(base_table)?;
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
            base = base_table.as_str(),
            base_column = base_column.as_str(),
            key_column = key_column.as_str(),
        )
    } else {
        // `s.{owner}` is the surface's OWN owner column as it declares it.
        // The branch above joins to the key's home table instead, and
        // `base.owner_id` there is `proxima_core.memory`'s own column,
        // which is fixed.
        let owner = sole_owner_column(surface)?;
        format!(
            "SELECT {projection}
               FROM {table} s
              WHERE s.{owner} IS NOT DISTINCT FROM $1
              ORDER BY {order}",
            table = table.as_str(),
            owner = owner.as_str(),
        )
    };
    Ok(Some(sql))
}

/// Row order is part of the bundle's bytes, so it comes off the declared
/// key.
///
/// THE ORDER IS NOT ALWAYS TOTAL. Five of flavor #0's twenty-eight declared
/// keys are backed by no unique index — `embeddings`, `embedding_heads` and
/// `embedding_jobs` on `entity_id`, `projection` on `memory_id`, and
/// `ingest_keys` on `t`, whose primary key is
/// `(owner_id, source_id, ingest_key)`.
///
/// Four of the five are `ExportRule::Excluded`, so they never reach this
/// function. `ingest_keys` is `ExportRule::Rows`, and it is the one that
/// matters: one memory may hold several admission receipts — a different
/// `(source_id, ingest_key)` pair each — and for those rows `ORDER BY s.t`
/// leaves the bundle's byte order to whatever the executor returns.
/// `every_declared_key_that_is_unique_is_unique_in_the_catalog` pins that
/// set, so the exception is recorded rather than assumed away.
///
/// THE ERASE IS UNAFFECTED. `WHERE t = ANY(...)` destroys every receipt of
/// an erased memory, which is what an erase owes, and it does not care what
/// order it finds them in. That predicate is a `Seq Scan`: `ingest_keys`
/// carries one index and it is the primary key.
///
/// A total export order needs the surface to carry a tiebreak the erase does
/// not use, because the two verbs want different things from one `key`: the
/// erase wants the memory column, the export wants a unique row identity.
fn order_by(surface: &Surface) -> Result<String, StorageError> {
    let mut parts = Vec::new();
    for column in surface.key.columns() {
        parts.push(format!("s.{}", PgIdent::column(column)?.as_str()));
    }
    Ok(parts.join(", "))
}

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
        let sql = export_statement(&surface("proxima_core.blob"))
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

    /// A surface with EMPTY `owner_columns` claims it is reached through its
    /// key's owner, and the generated join is what makes the claim true.
    #[test]
    fn a_keyed_sidecar_reaches_the_owner_through_its_home_table() {
        let sql = export_statement(&surface("proxima_core.agent_note_v1"))
            .expect("generates")
            .expect("the note sidecar is exported");
        assert!(sql.contains("JOIN proxima_core.memory base"), "{sql}");
        assert!(
            sql.contains("WHERE base.owner_id IS NOT DISTINCT FROM $1"),
            "{sql}"
        );
        assert!(sql.contains("to_jsonb(s)"), "{sql}");
    }

    /// The owner-pinned shape: no join, because joining `memory` would drop
    /// the rows whose Memory has been transferred away — exactly the history
    /// this owner is entitled to a copy of.
    #[test]
    fn an_owner_pinned_sidecar_is_filtered_on_its_own_owner() {
        let sql = export_statement(&surface("proxima_core.mcp_call_logged_v1"))
            .expect("generates")
            .expect("the call log is exported");
        assert!(!sql.contains("JOIN"), "{sql}");
        assert!(
            sql.contains("WHERE s.owner_id IS NOT DISTINCT FROM $1"),
            "{sql}"
        );
    }

    #[test]
    fn a_surface_declared_excluded_generates_nothing() {
        for table in [
            "proxima_core.content",
            "proxima_core.blob_uploads",
            "proxima_core.wake_config",
            "proxima_core.announce",
            "proxima_core.owners",
        ] {
            let declared = surface(table);
            assert!(matches!(declared.export, ExportRule::Excluded { .. }));
            assert!(
                export_statement(&declared).expect("generates").is_none(),
                "{table} declares Excluded and must emit no statement"
            );
        }
    }

    /// The collision-safety rule, as a property of every generated
    /// statement.
    #[test]
    fn no_generated_statement_aliases_a_table_t() {
        for surface in FLAVOR_0.all_surfaces() {
            let Some(sql) = export_statement(&surface).expect("generates") else {
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
