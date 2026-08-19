use proxima_core::compliance::{
    ComplianceExportBundle, ComplianceExportCounts, ComplianceExportSidecarRows,
    ExportAuthorization,
};
use proxima_core::{OwnerRef, StorageError};
use serde_json::Value;
use sqlx::PgPool;

use crate::access::owner_columns::owner_binds;
use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::verbs::compliance_erase::owner_digest;

pub async fn export_owner_bundle(
    pool: &PgPool,
    auth: &ExportAuthorization,
    fact_sidecar_tables: &[String],
    goal_sidecar_tables: &[String],
    citation_mapping_sidecar_tables: &[String],
    cited_object_sidecar_tables: &[String],
) -> Result<ComplianceExportBundle, StorageError> {
    let owner = auth.audit().owner();
    let memories = owner_rows(pool, owner, OwnerRowsTable::Memories).await?;
    let goals = owner_rows(pool, owner, OwnerRowsTable::Goals).await?;
    let edges = pins_from_memories(&memories);
    let receipts = owner_rows(pool, owner, OwnerRowsTable::Receipts).await?;
    let source_batches = owner_rows(pool, owner, OwnerRowsTable::SourceBatches).await?;
    let source_cursors = owner_rows(pool, owner, OwnerRowsTable::SourceCursors).await?;
    let delegated_authority_grants =
        owner_rows(pool, owner, OwnerRowsTable::DelegatedAuthorityGrants).await?;
    let cooled = owner_rows(pool, owner, OwnerRowsTable::Cooled).await?;
    let sketches = owner_rows(pool, owner, OwnerRowsTable::Sketches).await?;
    let blobs = owner_rows(pool, owner, OwnerRowsTable::Blobs).await?;
    let compliance_audit_rows = audit_rows(pool, owner).await?;
    let sidecars = export_sidecars(
        pool,
        owner,
        SidecarTables {
            fact: fact_sidecar_tables,
            goal: goal_sidecar_tables,
            citation_mapping: citation_mapping_sidecar_tables,
            cited_object: cited_object_sidecar_tables,
        },
    )
    .await?;

    let counts = ComplianceExportCounts {
        memories: memories.len(),
        goals: goals.len(),
        edges: edges.len(),
        receipts: receipts.len(),
        source_batches: source_batches.len(),
        source_cursors: source_cursors.len(),
        delegated_authority_grants: delegated_authority_grants.len(),
        cooled: cooled.len(),
        sketches: sketches.len(),
        blobs: blobs.len(),
        sidecar_rows: sidecars.iter().map(|sidecar| sidecar.rows.len()).sum(),
        compliance_audit_rows: compliance_audit_rows.len(),
    };

    Ok(ComplianceExportBundle {
        operation_id: auth.audit().operation_id(),
        target: auth.audit().target().clone(),
        owner,
        derived_requester: auth.audit().derived_requester(),
        derived_auth_path: format!("{:?}", auth.audit().derived_auth_path()),
        exported_at: auth.audit().requested_at(),
        counts,
        memories,
        goals,
        edges,
        receipts,
        source_batches,
        source_cursors,
        delegated_authority_grants,
        cooled,
        sketches,
        blobs,
        sidecars,
        compliance_audit_rows,
    })
}

struct SidecarTables<'a> {
    fact: &'a [String],
    goal: &'a [String],
    citation_mapping: &'a [String],
    cited_object: &'a [String],
}

#[derive(Clone, Copy)]
enum OwnerRowsTable {
    Memories,
    Goals,
    Receipts,
    SourceBatches,
    SourceCursors,
    DelegatedAuthorityGrants,
    Cooled,
    Sketches,
    Blobs,
}

async fn export_sidecars(
    pool: &PgPool,
    owner: OwnerRef,
    tables: SidecarTables<'_>,
) -> Result<Vec<ComplianceExportSidecarRows>, StorageError> {
    let mut sidecars = Vec::new();
    extend_sidecars(
        pool,
        owner,
        &mut sidecars,
        tables.fact,
        SidecarJoin {
            sidecar_column: "t",
            base_table: "proxima_core.memory",
            base_column: "t",
        },
    )
    .await?;
    extend_sidecars(
        pool,
        owner,
        &mut sidecars,
        tables.goal,
        SidecarJoin {
            sidecar_column: "t",
            base_table: "proxima_core.goal",
            base_column: "t",
        },
    )
    .await?;
    // A v0.0.8 citation *is* the `proxima_core.blob` row a Memory names through
    // `memory.blob_id`: `citation_of_fact` reads the cited-object id and the
    // citation-mapping id off that one row, so both citation sidecar families
    // key on `blob_id` and both are owner-filtered by `blob.owner_id`. Before
    // this, the registered table lists arrived here and were discarded, so a
    // flavor's citation sidecar rows were silently absent from the bundle.
    extend_sidecars(
        pool,
        owner,
        &mut sidecars,
        tables.cited_object,
        SidecarJoin {
            sidecar_column: "cited_object_id",
            base_table: "proxima_core.blob",
            base_column: "blob_id",
        },
    )
    .await?;
    extend_sidecars(
        pool,
        owner,
        &mut sidecars,
        tables.citation_mapping,
        SidecarJoin {
            sidecar_column: "citation_mapping_id",
            base_table: "proxima_core.blob",
            base_column: "blob_id",
        },
    )
    .await?;
    sidecars.sort_by(|left, right| left.table.cmp(&right.table));
    Ok(sidecars)
}

async fn owner_rows(
    pool: &PgPool,
    owner: OwnerRef,
    table: OwnerRowsTable,
) -> Result<Vec<Value>, StorageError> {
    let (owner_kind, owner_id) = owner_binds(&owner);
    match table {
        OwnerRowsTable::Memories => sqlx::query_scalar::<_, Value>(MEMORY_ROWS_SQL)
            .bind(owner_kind)
            .bind(owner_id)
            .fetch_all(pool)
            .await
            .map_err(map_err),
        OwnerRowsTable::Goals => sqlx::query_scalar::<_, Value>(GOAL_ROWS_SQL)
            .bind(owner_kind)
            .bind(owner_id)
            .fetch_all(pool)
            .await
            .map_err(map_err),
        OwnerRowsTable::Receipts => sqlx::query_scalar::<_, Value>(RECEIPT_ROWS_SQL)
            .bind(owner_kind)
            .bind(owner_id)
            .fetch_all(pool)
            .await
            .map_err(map_err),
        OwnerRowsTable::SourceBatches => sqlx::query_scalar::<_, Value>(SOURCE_BATCH_ROWS_SQL)
            .bind(owner_kind)
            .bind(owner_id)
            .fetch_all(pool)
            .await
            .map_err(map_err),
        OwnerRowsTable::SourceCursors => sqlx::query_scalar::<_, Value>(SOURCE_CURSOR_ROWS_SQL)
            .bind(owner_kind)
            .bind(owner_id)
            .fetch_all(pool)
            .await
            .map_err(map_err),
        OwnerRowsTable::DelegatedAuthorityGrants => {
            sqlx::query_scalar::<_, Value>(DELEGATED_AUTHORITY_GRANT_ROWS_SQL)
                .bind(owner_kind)
                .bind(owner_id)
                .fetch_all(pool)
                .await
                .map_err(map_err)
        }
        OwnerRowsTable::Cooled => sqlx::query_scalar::<_, Value>(COOLED_ROWS_SQL)
            .bind(owner_kind)
            .bind(owner_id)
            .fetch_all(pool)
            .await
            .map_err(map_err),
        OwnerRowsTable::Sketches => sqlx::query_scalar::<_, Value>(SKETCH_ROWS_SQL)
            .bind(owner_kind)
            .bind(owner_id)
            .fetch_all(pool)
            .await
            .map_err(map_err),
        OwnerRowsTable::Blobs => sqlx::query_scalar::<_, Value>(BLOB_ROWS_SQL)
            .bind(owner_kind)
            .bind(owner_id)
            .fetch_all(pool)
            .await
            .map_err(map_err),
    }
}

async fn audit_rows(pool: &PgPool, owner: OwnerRef) -> Result<Vec<Value>, StorageError> {
    sqlx::query_scalar::<_, Value>(
        "SELECT to_jsonb(a)
           FROM proxima_core.compliance_audit_log a
          WHERE a.owner_ref_digest = $1
          ORDER BY a.requested_at, a.operation_id",
    )
    .bind(owner_digest(owner))
    .fetch_all(pool)
    .await
    .map_err(map_err)
}

#[derive(Clone, Copy)]
struct SidecarJoin {
    sidecar_column: &'static str,
    base_table: &'static str,
    base_column: &'static str,
}

async fn extend_sidecars(
    pool: &PgPool,
    owner: OwnerRef,
    sidecars: &mut Vec<ComplianceExportSidecarRows>,
    tables: &[String],
    join: SidecarJoin,
) -> Result<(), StorageError> {
    for table in tables {
        let rows = sidecar_rows(pool, owner, table, join).await?;
        if rows.is_empty() {
            continue;
        }
        sidecars.push(ComplianceExportSidecarRows {
            table: table.clone(),
            rows,
        });
    }
    Ok(())
}

async fn sidecar_rows(
    pool: &PgPool,
    owner: OwnerRef,
    table: &str,
    join: SidecarJoin,
) -> Result<Vec<Value>, StorageError> {
    let table = PgIdent::table(table)?;
    let sidecar_column = PgIdent::column(join.sidecar_column)?;
    let base_table = PgIdent::table(join.base_table)?;
    let base_column = PgIdent::column(join.base_column)?;
    let (owner_kind, owner_id) = owner_binds(&owner);
    // SQL-POLICY: PgIdent
    let sql = format!(
        "SELECT to_jsonb(t)
           FROM {table} t
           JOIN {base_table} base
             ON base.{base_column} = t.{sidecar_column}
          WHERE base.owner_id IS NOT DISTINCT FROM $2
          ORDER BY t.{sidecar_column}",
        table = table.as_str(),
        base_table = base_table.as_str(),
        base_column = base_column.as_str(),
        sidecar_column = sidecar_column.as_str(),
    );
    // SQL-POLICY: PgIdent
    sqlx::query_scalar::<_, Value>(sqlx::AssertSqlSafe(sql))
        .bind(owner_kind)
        .bind(owner_id)
        .fetch_all(pool)
        .await
        .map_err(map_err)
}

const MEMORY_ROWS_SQL: &str = "
SELECT to_jsonb(m)
  FROM proxima_core.memory m
 WHERE m.owner_id IS NOT DISTINCT FROM $2
 ORDER BY m.t";

const GOAL_ROWS_SQL: &str = "
SELECT to_jsonb(g)
  FROM proxima_core.goal g
 WHERE g.owner_id IS NOT DISTINCT FROM $2
 ORDER BY g.t";

// A cooled admission's content has left `memory` for the object store, so
// omitting this table returned an incomplete owner bundle for every forgotten
// admission. The row is exported as a locator manifest — `object_key` names
// where the dumped payload lives — and the bundle deliberately does not stream
// cold-store bytes: it is a database export, and the payload is recoverable by
// hydrating the admission.
const COOLED_ROWS_SQL: &str = "
SELECT to_jsonb(c)
  FROM proxima_core.cooled c
 WHERE c.owner_id IS NOT DISTINCT FROM $2
 ORDER BY c.t";

// The derived one-liner of each of the owner's memories and goals. `search_tsv`
// is a generated lexical-index column over `text`, not owner data, so it is
// dropped rather than dumped into a portability bundle.
const SKETCH_ROWS_SQL: &str = "
SELECT to_jsonb(s) - 'search_tsv'
  FROM proxima_core.sketch s
 WHERE s.owner_id IS NOT DISTINCT FROM $2
 ORDER BY s.t";

// Keep this an explicit field allowlist: the blob row is the authoritative
// cited-object identity even for opaque schemas, while upload coordinates and
// object-store bytes are not part of a database compliance export.
const BLOB_ROWS_SQL: &str = "
SELECT jsonb_build_object(
           'blob_id', b.blob_id,
           'schema_id', b.schema_id,
           'content_hash', b.content_hash
       )
  FROM proxima_core.blob b
 WHERE b.owner_id IS NOT DISTINCT FROM $2
 ORDER BY b.blob_id";

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

const RECEIPT_ROWS_SQL: &str = "
SELECT to_jsonb(ik)
  FROM proxima_core.ingest_keys ik
 WHERE ik.owner_id IS NOT DISTINCT FROM $2
 ORDER BY ik.t";

const SOURCE_BATCH_ROWS_SQL: &str = "
SELECT to_jsonb(a)
  FROM proxima_core.announce a
 WHERE FALSE
 ORDER BY a.seq";

const SOURCE_CURSOR_ROWS_SQL: &str = "
SELECT to_jsonb(sc)
  FROM proxima_core.source_cursors sc
 WHERE sc.owner_kind = $1
   AND sc.owner_id IS NOT DISTINCT FROM $2
 ORDER BY sc.source";

// Keep this an explicit field allowlist: the durable grant table is an
// unsupported persistence detail, while the compliance bundle is a supported
// serialized contract. A future storage-only column must not leak into export
// merely because the table changed.
const DELEGATED_AUTHORITY_GRANT_ROWS_SQL: &str = "
SELECT jsonb_build_object(
           'subject_user_id', dag.subject_user_id,
           'owner_kind', dag.owner_kind,
           'owner_id', dag.owner_id,
           'tool_name', dag.tool_name,
           'action_name', dag.action_name,
           'read_ceiling', dag.read_ceiling,
           'write_ceiling', dag.write_ceiling,
           'expires_at', dag.expires_at,
           'auth_epoch', dag.auth_epoch,
           'issued_at', dag.issued_at,
           'revoked_at', dag.revoked_at,
           'revoked_by_user_id', dag.revoked_by_user_id
       )
  FROM proxima_core.delegated_authority_grants dag
 WHERE dag.owner_kind = $1
   AND dag.owner_id IS NOT DISTINCT FROM $2
 ORDER BY dag.issued_at, dag.delegation_id";

#[cfg(test)]
mod tests {
    use super::{pin_sort_key, pins_from_memories};
    use serde_json::json;

    #[test]
    fn export_sql_does_not_rebuild_an_edge_table() {
        let src = include_str!("compliance_export.rs");
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
}
